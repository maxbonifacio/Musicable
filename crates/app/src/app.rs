//! The window: the UI crate's shell, wired to the session, the audio device
//! and the project's file.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use musicable_audio::InputKind;
use musicable_commands::{Args, CommandError, Focus, Request, arrange, browser, device};
use musicable_engine::{EngineHandle, Meter, Meters};
use musicable_io::project::{self, EXTENSION};
use musicable_library::Queue;
use musicable_model::param::{ParamSpec, Scale};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{DeviceId, Project, TrackId};
use musicable_plugins::{ParamInfo, PluginInfo, Waker};
use musicable_ui::{
    Action, Audio, InputChoice, KeyPresses, KeymapStatus, Level, Levels, MidiChoice, PIANO_ROLL,
    Playback, PluginEntry, PluginParam, PluginStatus, Plugins, SaveChoice, Shell, View,
};

use crate::appearance_window::{AppearanceWindow, Asked as AppearanceAsked};
use crate::audio_files::{AudioFiles, clip_path};
use crate::audio_window::{AudioWindow, Change};
use crate::export::{self, Export, ExportSettings, Job, Report};
use crate::export_window::{Asked, ExportWindow};
use crate::keymap_file::UserKeymap;
use crate::library::{self, LIBRARY_FILE, Library, Listing};
use crate::plugin_scan::{PLUGINS_FILE, PluginScan};
use crate::plugin_windows::PluginWindows;
use crate::settings::{self, AudioSettings, KEYMAP_FILE, SETTINGS_FILE, Settings, THEMES_FOLDER};
use crate::theme_file::{Choice, UserTheme};
use crate::{Output, Session, capital};

/// How often the window redraws while the transport plays or the mixer's
/// meters move, and while neither does, to update the CPU meter.
const PLAYING_FRAME: Duration = Duration::from_millis(16);
const STOPPED_FRAME: Duration = Duration::from_millis(250);
/// How often the app looks for a graph while one compiles, to send it to the
/// engine soon after it's ready.
const COMPILING_FRAME: Duration = Duration::from_millis(5);
/// The audio files the Import Audio and Load Sample dialogs show. The app
/// reads a file by what's in it, whatever it's called.
const AUDIO_EXTENSIONS: &[&str] = &["wav", "wave", "aif", "aiff", "aifc", "flac"];

/// The app's window, and everything it drives.
pub struct App {
    session: Session,
    /// The plugins' own windows. After the session, so the plugins close
    /// their windows before the windows they're in go.
    plugin_windows: PluginWindows,
    plugin_scan: PluginScan,
    /// The plugins installed, as the browser lists them.
    plugin_entries: Vec<PluginEntry>,
    /// How each plugin device's plugin is doing, as of this frame.
    plugin_status: BTreeMap<DeviceId, PluginStatus>,
    /// The parameters automation can move, for each loaded plugin.
    plugin_params: BTreeMap<DeviceId, Listed>,
    output: Output,
    /// The output's stream whose inputs the session has.
    inputs_started: u64,
    /// The inputs the mixer's input menus offer, as last listed.
    input_choices: Vec<InputChoice>,
    /// The MIDI inputs the mixer's MIDI input menus offer, as last listed.
    midi_choices: Vec<MidiChoice>,
    shell: Shell,
    /// The headers and the waveforms of the project's audio files.
    audio_files: AudioFiles,
    /// The session's revision whose audio files were asked for.
    files_found: Option<u64>,
    keys: KeyPresses,
    audio_window: AudioWindow,
    appearance_window: AppearanceWindow,
    export_window: ExportWindow,
    /// The export under way, if one is.
    export: Option<Export>,
    /// Where the last export went, where the next one's dialog starts.
    export_folder: Option<PathBuf>,
    /// The project's file, once it has one.
    path: Option<PathBuf>,
    /// Where the file dialogs start: the folder a project was last opened
    /// from or saved to.
    folder: Option<PathBuf>,
    /// Errors and notices for the status bar, oldest first.
    messages: Vec<String>,
    /// The mixer's meters, as last read.
    levels: Levels,
    /// Where the settings and the user's keymap live.
    config_dir: Option<PathBuf>,
    settings: Settings,
    /// The user's keymap file, which every change of keys is saved to.
    keymap_file: UserKeymap,
    /// The theme Musicable draws with, and the file it's from, which is
    /// followed as it changes.
    theme_file: UserTheme,
    /// What the status bar says about the theme's file, as last said.
    theme_message: Option<String>,
    /// What waits for the user to say whether to save their changes first.
    pending: Option<Request>,
    /// Set once the user has said what to do with their changes, so the
    /// window closes.
    closing: bool,
    /// The window's title, as last set.
    title: String,
}

impl App {
    /// Opens the audio device and the project at `path`, or a new project,
    /// in the window whose egui context is `ctx`.
    pub fn new(path: Option<PathBuf>, ctx: &egui::Context) -> Self {
        follow_system_double_clicks(ctx);
        let mut messages = Vec::new();
        let config_dir = settings::config_dir();
        let settings = match &config_dir {
            Some(dir) => Settings::read(&dir.join(SETTINGS_FILE)).unwrap_or_else(|error| {
                messages.push(format!(
                    "{}; Musicable uses the default settings.",
                    capital(error.to_string().trim_end())
                ));
                Settings::default()
            }),
            None => Settings::default(),
        };
        let (output, handle) = Output::open(settings.audio.request(), &mut messages);
        let mut app = Self::with_output(path, config_dir, settings, output, handle, messages, ctx);
        app.scan_plugins(false, ctx);
        app
    }

    /// Opens the project at `path`, or a new project, playing through
    /// `output`, whose engine `handle` drives. The settings and the user's
    /// keymap live in `config_dir`.
    fn with_output(
        path: Option<PathBuf>,
        config_dir: Option<PathBuf>,
        settings: Settings,
        output: Output,
        handle: EngineHandle,
        mut messages: Vec<String>,
        ctx: &egui::Context,
    ) -> Self {
        let (keymap_file, keymap) =
            UserKeymap::read(config_dir.as_ref().map(|dir| dir.join(KEYMAP_FILE)));
        if !keymap_file.problems().is_empty() || keymap_file.error().is_some() {
            messages.push(
                "Your keymap file has problems: see Options > Keyboard Shortcuts.".to_owned(),
            );
        }
        let (project, path) = match path {
            Some(path) => match project::load(&path) {
                Ok(project) => (project, Some(path)),
                Err(error) => {
                    messages.push(format!("Couldn't open {}: {error}", path.display()));
                    (Project::new(), None)
                }
            },
            None => (Project::new(), None),
        };
        let folder = path.as_deref().and_then(Path::parent).map(Path::to_owned);
        let mut session = Session::new(project, folder.as_deref(), keymap, handle);
        session.editor_mut().behaviour = settings.behaviour;
        session.set_capture_seconds(u64::from(settings.recording.capture_minutes) * 60);
        session.set_plugin_waker(waker(ctx));
        // The browser's library, whose file work runs on a queue of its own.
        let queue = Arc::new(Queue::for_this_machine());
        let library_file = config_dir.as_ref().map(|dir| dir.join(LIBRARY_FILE));
        let presets = (settings.browser.presets_folder.clone()).or_else(library::presets_folder);
        session.set_library(Library::open(
            library_file,
            presets,
            queue,
            Some(waker(ctx)),
        ));
        session.set_preview_gain(settings.browser.preview_gain());
        let plugin_scan = PluginScan::new(config_dir.as_ref().map(|dir| dir.join(PLUGINS_FILE)));
        let theme_file = UserTheme::new(
            config_dir.as_ref().map(|dir| dir.join(THEMES_FOLDER)),
            Choice::from_name(&settings.appearance.theme),
        );
        // What the last scan found, while the next one runs. Before the
        // first, plugin devices wait for it.
        let known = plugin_scan.known();
        let mut app = Self {
            session,
            plugin_windows: PluginWindows::default(),
            plugin_scan,
            plugin_entries: Vec::new(),
            plugin_status: BTreeMap::new(),
            plugin_params: BTreeMap::new(),
            output,
            inputs_started: 0,
            input_choices: Vec::new(),
            midi_choices: Vec::new(),
            shell: Shell::new(),
            audio_files: AudioFiles::new(),
            files_found: None,
            keys: KeyPresses::new(),
            audio_window: AudioWindow::new(),
            appearance_window: AppearanceWindow::new(),
            export_window: ExportWindow::new(),
            export: None,
            export_folder: None,
            path,
            folder,
            messages,
            levels: Levels::default(),
            config_dir,
            settings,
            keymap_file,
            theme_file,
            theme_message: None,
            pending: None,
            closing: false,
            title: String::new(),
        };
        app.use_theme(ctx);
        if let Some(known) = known {
            app.set_installed_plugins(known);
        }
        app.list_inputs();
        app
    }

    /// Scans for plugins in the background, unless a scan is running.
    /// Libraries left out as bad are loaded again only if `retry_bad`.
    fn scan_plugins(&mut self, retry_bad: bool, ctx: &egui::Context) {
        if let Err(error) = self.plugin_scan.start(retry_bad, waker(ctx)) {
            self.messages.push(capital(&format!("{error}.")));
        }
    }

    /// Finds the project's plugins among `installed` from now on, and lists
    /// them in the browser.
    fn set_installed_plugins(&mut self, installed: Vec<PluginInfo>) {
        let mut entries: Vec<PluginEntry> = installed
            .iter()
            .map(|plugin| PluginEntry {
                format: plugin.format,
                id: plugin.id.clone(),
                name: plugin.name.clone(),
                vendor: plugin.vendor.clone(),
                instrument: plugin.is_instrument(),
            })
            .collect();
        entries.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.vendor.clone()));
        self.plugin_entries = entries;
        self.session.set_installed_plugins(installed);
    }

    /// How each plugin device's plugin is doing, for the devices panel, and
    /// its parameters, for automation lanes.
    fn update_plugin_status(&mut self) {
        self.plugin_status.clear();
        let devices = self
            .session
            .project()
            .chains()
            .flat_map(|(_, devices)| devices);
        for device in devices.filter(|d| d.kind.plugin().is_some()) {
            let instance = self.session.plugin(device.id);
            let params = match instance {
                Some(instance) => {
                    let list = instance.params();
                    let from = (list.as_ptr() as usize, list.len());
                    let listed = self.plugin_params.entry(device.id).or_default();
                    if listed.from != from {
                        *listed = Listed {
                            from,
                            params: automatable(list),
                        };
                    }
                    listed.params.clone()
                }
                None => Arc::default(),
            };
            let status = PluginStatus {
                loaded: instance.is_some(),
                has_window: instance.is_some_and(|i| i.has_window()),
                window_open: self.plugin_windows.is_open(device.id),
                problem: self.session.plugin_problem(device.id).map(str::to_owned),
                params,
            };
            self.plugin_status.insert(device.id, status);
        }
        let status = &self.plugin_status;
        self.plugin_params
            .retain(|device, _| status.get(device).is_some_and(|status| status.loaded));
    }

    /// The project's name: its file's name without the extension.
    fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_stem)
            .map_or("Untitled".to_owned(), |stem| {
                stem.to_string_lossy().into_owned()
            })
    }

    fn act(&mut self, action: Action, ctx: &egui::Context, frame: &eframe::Frame) {
        match action {
            // The shell offers the session's own commands.
            Action::Run(id) => {
                let _ = self.session.run(&id);
            }
            Action::RunWith(id, args) => match self.session.run_with(&id, &args) {
                Ok(_) | Err(CommandError::NotNow(_)) => {}
                Err(error) => self.messages.push(capital(&format!("{error}."))),
            },
            Action::DropFiles { files, track, at } => self.insert_audio(files, track, at),
            Action::SetTempo(bpm) => self.session.set_tempo(bpm),
            Action::BeginGesture => self.session.begin_gesture(),
            Action::EndGesture => self.session.end_gesture(),
            Action::Mixer(change) => self.session.change_mixer(change),
            Action::ListInputs => self.list_inputs(),
            Action::ClearPeaks => self.session.meters().clear_highest(),
            Action::SetKeymap(keymap) => {
                self.keymap_file.save(&keymap);
                self.session.set_keymap(keymap);
            }
            Action::ResetKeymap => self.session.set_keymap(self.keymap_file.reset()),
            Action::ImportKeymap => {
                let picked = self.keymap_dialog(frame).pick_file();
                if let Some(keymap) = picked.and_then(|path| self.keymap_file.import(&path)) {
                    self.session.set_keymap(keymap);
                }
            }
            Action::ExportKeymap => {
                let dialog = self.keymap_dialog(frame).set_file_name(KEYMAP_FILE);
                if let Some(path) = dialog.save_file() {
                    self.keymap_file.export(self.session.keymap(), &path);
                }
            }
            Action::ReloadKeymap => self.session.set_keymap(self.keymap_file.reload()),
            // The file starts with the keys in use: the default ones, as the
            // file doesn't exist yet.
            Action::CreateKeymap => self.keymap_file.save(self.session.keymap()),
            Action::ResetDropouts => self.output.reset_dropouts(),
            Action::SetAutoPreview(on) => {
                self.settings.browser.auto_preview = on;
                self.save_settings();
            }
            Action::SetPreviewVolume(volume) => {
                self.settings.browser.preview_volume = volume;
                self.session
                    .set_preview_gain(self.settings.browser.preview_gain());
                self.save_settings();
            }
            Action::DismissMessages => self.messages.clear(),
        }
        self.handle_requests(ctx, frame);
    }

    /// Carries out what the commands run so far asked for. What would lose
    /// unsaved changes waits for the user to say whether to save them.
    fn handle_requests(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        for request in self.session.take_requests() {
            let loses_changes = matches!(
                request,
                Request::New | Request::NewDemo | Request::Open | Request::Quit
            );
            if self.pending.is_some() {
                // One question at a time.
            } else if loses_changes && self.session.is_modified() {
                self.pending = Some(request);
            } else {
                self.carry_out(request, ctx, frame);
            }
        }
    }

    fn carry_out(&mut self, request: Request, ctx: &egui::Context, frame: &eframe::Frame) {
        match request {
            Request::New => self.open_project(Project::new(), None),
            Request::NewDemo => self.open_project(crate::demo_project(), None),
            Request::Open => {
                let Some(path) = self.file_dialog(frame).pick_file() else {
                    return;
                };
                match project::load(&path) {
                    Ok(project) => self.open_project(project, Some(path)),
                    Err(error) => self
                        .messages
                        .push(format!("Couldn't open {}: {error}", path.display())),
                }
            }
            Request::Save => {
                self.save(false, frame);
            }
            Request::SaveAs => {
                self.save(true, frame);
            }
            Request::Quit => {
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Request::CommandPalette => self.shell.open_palette(),
            Request::AudioSettings => self.audio_window.open(&self.output),
            Request::KeyboardShortcuts => self.shell.open_shortcuts(),
            Request::Appearance => self.appearance_window.open(),
            Request::ImportAudio => {
                if let Some(files) = self.audio_dialog(frame).pick_files() {
                    self.insert_audio(files, None, None);
                }
            }
            Request::ExportAudio => self.export_window.open(
                &self.settings.export,
                self.session.project(),
                self.session.editor(),
                self.export.is_some(),
            ),
            Request::TogglePanel(panel) => {
                self.shell.toggle_panel(panel);
                self.focus_in_sight();
            }
            Request::ShowPanel(panel) => {
                self.shell.show_panel(panel);
                self.focus_in_sight();
            }
            Request::SwitchDevicesPianoRoll => {
                self.shell.switch_devices_piano_roll();
                self.focus_in_sight();
            }
            Request::Rename(named) => self.shell.rename(named),
            Request::PluginWindow { device, show: true } => {
                self.show_plugin_window(device, ctx, frame);
            }
            Request::PluginWindow {
                device,
                show: false,
            } => self.plugin_windows.hide(&mut self.session, device),
            Request::RescanPlugins => self.scan_plugins(true, ctx),
            Request::Capture => self.session.capture(),
            Request::LoadSample(device) => {
                if let Some(path) = self.audio_dialog(frame).pick_file() {
                    self.set_sample(device, &path);
                }
            }
            Request::AddLibraryFolder => self.add_library_folder(frame),
            Request::FocusBrowserSearch => self.shell.focus_browser_search(),
        }
    }

    /// Gives the sampler `device` the audio file at `path`, through
    /// [`device::SET_SAMPLE`]. The sampler reads it when the graph is
    /// built, and the status bar says if it can't.
    fn set_sample(&mut self, device: DeviceId, path: &Path) {
        let file = clip_path(path, self.project_folder());
        let args = Args::new().with("device", device).with("file", file);
        if let Err(error) = self.session.run_with(&device::SET_SAMPLE, &args) {
            self.messages
                .push(format!("Couldn't load the sample: {error}."));
        }
    }

    /// Asks for a folder of samples, and adds it to the browser's library.
    fn add_library_folder(&mut self, frame: &eframe::Frame) {
        let dialog = rfd::FileDialog::new()
            .set_parent(frame)
            .set_title("Add Folder to Library");
        let Some(folder) = dialog.pick_folder() else {
            return;
        };
        let args = Args::new().with("folder", folder.to_string_lossy().as_ref());
        if let Err(error) = self.session.run_with(&browser::ADD_FOLDER, &args) {
            self.messages.push(capital(&format!("{error}.")));
        }
    }

    /// Gives the arrangement the focus when the piano roll has it but is
    /// hidden, so the keys act on what's in sight.
    fn focus_in_sight(&mut self) {
        let editor = self.session.editor_mut();
        if editor.focus == Focus::PianoRoll && !self.shell.panel_shown(PIANO_ROLL) {
            editor.focus = Focus::Arrangement;
        }
    }

    /// Shows the device `device`'s plugin's own window, in front of the
    /// app's.
    fn show_plugin_window(&mut self, device: DeviceId, ctx: &egui::Context, frame: &eframe::Frame) {
        // The display's scale, without egui's zoom, which the plugin
        // doesn't know of.
        let scale = ctx.native_pixels_per_point().unwrap_or(1.0);
        let shown = self.plugin_windows.show(
            &mut self.session,
            device,
            owner(frame),
            f64::from(scale),
            waker(ctx),
        );
        if let Err(error) = shown {
            self.messages.push(capital(&format!("{error}.")));
        }
    }

    fn open_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.session
            .open(project, path.as_deref().and_then(Path::parent));
        self.audio_files.forget();
        self.set_path(path);
    }

    /// The folder of the project's file, which audio clips' relative paths
    /// start from. A project that isn't saved has none.
    fn project_folder(&self) -> Option<&Path> {
        self.path.as_deref().and_then(Path::parent)
    }

    /// Puts the audio files at `paths` on `track` from `at`, one after
    /// another, through [`arrange::INSERT_AUDIO`], which says where they go
    /// without those. Files that can't be read are left out, and the status
    /// bar says why.
    fn insert_audio(&mut self, paths: Vec<PathBuf>, track: Option<TrackId>, at: Option<Beats>) {
        let folder = self.project_folder().map(Path::to_owned);
        let mut files = Vec::new();
        for path in paths {
            let name = clip_path(&path, folder.as_deref());
            match self.audio_files.read_now(&name, &path) {
                Ok(file) => {
                    self.session.editor_mut().files.insert(name.clone(), file);
                    files.push(name);
                }
                Err(error) => self.messages.push(format!(
                    "Couldn't read the audio file {}: {error}",
                    path.display()
                )),
            }
        }
        if files.is_empty() {
            return;
        }
        let mut args = Args::new().with("files", files);
        if let Some(track) = track {
            args.set("track", track);
        }
        if let Some(at) = at {
            args.set("at", at);
        }
        if let Err(error) = self.session.run_with(&arrange::INSERT_AUDIO, &args) {
            self.messages
                .push(format!("Couldn't insert the audio files: {error}."));
        }
    }

    /// Asks for the headers and the waveforms of the project's audio files
    /// when the project has changed, and takes in those read since.
    fn read_audio_files(&mut self, ctx: &egui::Context) {
        let revision = self.session.revision();
        if self.files_found != Some(revision) {
            self.files_found = Some(revision);
            let folder = self.project_folder().map(Path::to_owned);
            self.audio_files
                .find(self.session.project(), folder.as_deref());
        }
        if self.audio_files.receive(self.session.editor_mut()) {
            ctx.request_repaint();
        }
    }

    fn set_path(&mut self, path: Option<PathBuf>) {
        if let Some(folder) = path.as_deref().and_then(Path::parent) {
            self.folder = Some(folder.to_owned());
        }
        self.path = path;
    }

    /// Saves the project to its file, or to one the user picks if it has
    /// none yet or `choose` is set. Returns whether it saved.
    fn save(&mut self, choose: bool, frame: &eframe::Frame) -> bool {
        let path = match &self.path {
            Some(path) if !choose => path.clone(),
            _ => {
                let dialog = self
                    .file_dialog(frame)
                    .set_file_name(format!("{}.{EXTENSION}", self.name()));
                match dialog.save_file() {
                    Some(path) => with_extension(path),
                    None => return false,
                }
            }
        };
        match project::save(&path, &self.session.saved_project()) {
            Ok(()) => {
                self.session.mark_saved();
                self.set_path(Some(path));
                true
            }
            Err(error) => {
                self.messages
                    .push(format!("Couldn't save {}: {error}", path.display()));
                false
            }
        }
    }

    /// Asks where to export `range` of the song with `settings`, the tracks
    /// `tracks` on their own if the settings want stems, and starts the
    /// export. The settings are kept for the next one.
    fn start_export(
        &mut self,
        range: BeatRange,
        settings: ExportSettings,
        tracks: &BTreeSet<TrackId>,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        let format = settings.format;
        let dialog = rfd::FileDialog::new()
            .set_parent(frame)
            .add_filter(format.name(), format.extensions())
            .set_file_name(format!("{}.{}", self.name(), format.extensions()[0]));
        let folder = self.export_folder.as_deref();
        let dialog = match folder.or(self.project_folder()).or(self.folder.as_deref()) {
            Some(folder) => dialog.set_directory(folder),
            None => dialog,
        };
        if let Some(file) = dialog.save_file() {
            self.export_to(&file, range, settings, tracks, ctx);
        }
    }

    /// Exports as [`start_export`](Self::start_export) does, once the
    /// dialog has said where: to `file`, and the stems next to it.
    fn export_to(
        &mut self,
        file: &Path,
        range: BeatRange,
        settings: ExportSettings,
        tracks: &BTreeSet<TrackId>,
        ctx: &egui::Context,
    ) {
        self.export_folder = file.parent().map(Path::to_owned);
        self.settings.export = settings;
        self.save_settings();
        let project = self.session.saved_project();
        let (mixdown, stems) = export::files_for(file, &project, &settings, tracks);
        let job = Job {
            project,
            folder: self.project_folder().map(Path::to_owned),
            range,
            settings,
            config: self.session.engine_config(),
            installed: self.session.installed_plugins().to_vec(),
            waker: Some(waker(ctx)),
            mixdown,
            stems,
        };
        match Export::start(job) {
            Ok(export) => self.export = Some(export),
            Err(error) => self.export_window.finished(Report {
                failed: Some(error),
                ..Report::default()
            }),
        }
    }

    /// Carries the export under way on, and once it's finished, says how it
    /// went.
    fn tick_export(&mut self) {
        let Some(export) = &mut self.export else {
            return;
        };
        export.tick();
        let Some(report) = export.report().cloned() else {
            return;
        };
        self.export = None;
        let message = match (&report.failed, report.cancelled, report.files.len()) {
            (Some(failed), ..) => format!("The export stopped: {failed}."),
            (None, true, _) => "The export was cancelled.".to_owned(),
            (None, false, 1) => format!("Exported {}.", report.files[0].path.display()),
            (None, false, files) => format!("Exported {files} files."),
        };
        self.messages.push(message);
        self.export_window.finished(report);
    }

    /// A dialog for picking a project file, in the folder of the last one.
    fn file_dialog(&self, frame: &eframe::Frame) -> rfd::FileDialog {
        let dialog = rfd::FileDialog::new()
            .set_parent(frame)
            .add_filter("Musicable project", &[EXTENSION]);
        match &self.folder {
            Some(folder) => dialog.set_directory(folder),
            None => dialog,
        }
    }

    /// A dialog for picking audio files, in the project's folder.
    fn audio_dialog(&self, frame: &eframe::Frame) -> rfd::FileDialog {
        let dialog = rfd::FileDialog::new()
            .set_parent(frame)
            .add_filter("Audio files", AUDIO_EXTENSIONS);
        match self.project_folder().or(self.folder.as_deref()) {
            Some(folder) => dialog.set_directory(folder),
            None => dialog,
        }
    }

    /// A dialog for picking a keymap file. It starts in the folder of the
    /// user's keymap file, where its backup is.
    fn keymap_dialog(&self, frame: &eframe::Frame) -> rfd::FileDialog {
        let dialog = rfd::FileDialog::new()
            .set_parent(frame)
            .add_filter("Musicable keymap", &["toml"]);
        match &self.config_dir {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        }
    }

    /// Plays through the stream `request` asks for, and keeps the settings
    /// if it opens.
    fn apply_audio(&mut self, request: musicable_audio::StreamRequest) {
        let outcome = match self.output.apply(request, &mut self.session) {
            Ok(()) => {
                self.settings.audio = AudioSettings::from_request(self.output.request());
                self.save_settings();
                Ok(format!("Playing through {}.", self.output.device_name()))
            }
            Err(error) => Err(format!(
                "The device didn't open with these settings: {error}. The ones before are back."
            )),
        };
        self.audio_window.applied(outcome);
    }

    /// Sets how late recordings through the device playing land, and keeps
    /// it in the settings.
    fn set_recording_offset(&mut self, frames: i64) {
        let Some(info) = self.output.info() else {
            return;
        };
        self.settings.recording.set_offset_for(info, frames);
        self.session.set_recording_offset(frames);
        self.save_settings();
    }

    /// Opens the audio and MIDI inputs the tracks want with the stream,
    /// unless that would cut a recording short, and hands a new stream's
    /// inputs to the session.
    fn follow_inputs(&mut self) {
        let recording = self.session.is_recording() && self.session.stats().playing;
        if !recording {
            let wanted = self.session.wanted_inputs();
            let midi = self.session.wanted_midi_inputs();
            self.output.want_inputs(&wanted, &midi, &mut self.session);
        }
        if self.output.started() != self.inputs_started {
            self.inputs_started = self.output.started();
            let layout = self.output.input_layout();
            let (buffers, midi) = (self.output.input_buffers(), self.output.midi_buffers());
            self.session.set_inputs(layout, buffers, midi);
            self.messages.extend(self.output.input_problems());
            self.list_inputs();
        }
    }

    /// Lists the inputs the mixer's input menus offer. An ASIO driver's
    /// channels are only known once its stream opens.
    fn list_inputs(&mut self) {
        let devices = match musicable_audio::input_devices() {
            Ok(devices) => devices,
            Err(error) => {
                self.messages
                    .push(format!("The audio inputs couldn't be listed: {error}."));
                Vec::new()
            }
        };
        let open = self.output.info().map_or(&[][..], |info| &info.inputs);
        self.input_choices = devices
            .into_iter()
            .map(|device| {
                let channels = match device.channels {
                    0 => open
                        .iter()
                        .find(|input| input.id == device.id)
                        .map_or(0, |input| input.channels),
                    channels => channels,
                };
                let name = match device.kind {
                    InputKind::Loopback => format!("Loopback: {}", device.name),
                    _ => device.name,
                };
                InputChoice {
                    id: device.id,
                    name,
                    channels,
                }
            })
            .collect();
        let midi = match musicable_audio::midi_input_devices() {
            Ok(devices) => devices,
            Err(error) => {
                self.messages
                    .push(format!("The MIDI inputs couldn't be listed: {error}."));
                Vec::new()
            }
        };
        self.midi_choices = midi
            .into_iter()
            .map(|device| MidiChoice {
                id: device.id,
                name: device.name,
            })
            .collect();
    }

    /// Draws with the theme in use from the next frame on, and says in the
    /// status bar when its file couldn't be used as it is.
    fn use_theme(&mut self, ctx: &egui::Context) {
        if let Some(fonts) = self.theme_file.take_fonts() {
            ctx.set_fonts(fonts);
        }
        let theme = self.theme_file.theme();
        musicable_ui::apply_theme(ctx, theme);
        self.shell.set_theme(theme.clone());
        let message = self.theme_file.message();
        if message != self.theme_message {
            if let Some(said) = self.theme_message.take() {
                self.messages.retain(|message| *message != said);
            }
            self.messages.extend(message.clone());
            self.theme_message = message;
        }
        ctx.request_repaint();
    }

    /// Does what the user asked for in the appearance window, and keeps the
    /// theme picked in the settings.
    fn appearance(&mut self, asked: AppearanceAsked, ctx: &egui::Context, frame: &eframe::Frame) {
        match asked {
            AppearanceAsked::Choose(choice) => self.theme_file.choose(choice),
            AppearanceAsked::Export => {
                if let Some(path) = self.theme_dialog(frame).save_file() {
                    let path = match path.extension() {
                        Some(_) => path,
                        None => path.with_extension("toml"),
                    };
                    self.theme_file.export(&path);
                }
            }
            AppearanceAsked::OpenFolder => self.theme_file.open_folder(),
            AppearanceAsked::Reload => self.theme_file.reload(),
        }
        let chosen = self.theme_file.choice().name();
        if chosen != self.settings.appearance.theme {
            self.settings.appearance.theme = chosen.to_owned();
            self.save_settings();
        }
        self.use_theme(ctx);
    }

    /// A dialog for saving a theme file, in the themes folder.
    fn theme_dialog(&self, frame: &eframe::Frame) -> rfd::FileDialog {
        let dialog = rfd::FileDialog::new()
            .set_parent(frame)
            .add_filter("Musicable theme", &["toml"])
            .set_file_name("My Theme.toml");
        match self.theme_file.folder() {
            Some(folder) if fs::create_dir_all(folder).is_ok() => dialog.set_directory(folder),
            _ => dialog,
        }
    }

    fn save_settings(&mut self) {
        let Some(path) = self.config_dir.as_ref().map(|dir| dir.join(SETTINGS_FILE)) else {
            return;
        };
        if let Err(error) = self.settings.write(&path) {
            self.messages.push(format!(
                "Couldn't save the settings to {}: {error}",
                path.display()
            ));
        }
    }

    /// Asks whether to save the changes, while a request waits for that, and
    /// carries the request out once the user says.
    fn ask_to_save(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(request) = self.pending else {
            return;
        };
        let Some(choice) = musicable_ui::ask_to_save(ctx, &self.name()) else {
            return;
        };
        self.pending = None;
        let go_ahead = match choice {
            SaveChoice::Save => self.save(false, frame),
            SaveChoice::Discard => true,
            SaveChoice::Cancel => false,
        };
        if go_ahead {
            self.carry_out(request, ctx, frame);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let modified = if self.session.is_modified() { "*" } else { "" };
        let title = format!("{}{modified} - Musicable", self.name());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for App {
    /// Key presses go to the keymap before egui sees them, so a bound key
    /// runs its command wherever the focus is, unless a text field has it.
    /// A key whose command doesn't apply now goes on to egui: with no notes
    /// selected, the arrow keys move the keyboard focus between controls.
    /// While the keyboard shortcuts window waits for the keys to give a
    /// command, they go to it instead.
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        if self.pending.is_none() && self.shell.capturing() {
            if let Some(chord) = self.keys.capture(input) {
                self.shell.captured(chord);
            }
            return;
        }
        let session = &mut self.session;
        // While the app asks something, keys answer it, and none run
        // commands while the command palette is open either.
        let asking = self.pending.is_some() || self.shell.asking();
        self.keys.take(ctx, input, |chord, repeat| {
            if asking || !session.takes_key(chord) {
                return false;
            }
            if !repeat {
                session.key_pressed(chord);
            }
            true
        });
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.theme_file.poll() {
            self.use_theme(ctx);
        }
        if let Some(scanned) = self.plugin_scan.receive() {
            self.messages.extend(scanned.problems);
            self.set_installed_plugins(scanned.plugins);
        }
        self.session.tick();
        self.plugin_windows.tick(&mut self.session);
        self.tick_export();
        self.messages.extend(self.session.take_problems());
        self.output.tick(&mut self.session, &mut self.messages);
        self.follow_inputs();
        // Recordings through the device playing make up for its offset.
        let info = self.output.info();
        let offset = info.map_or(0, |info| self.settings.recording.offset_for(info));
        self.session.set_recording_offset(offset);
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && !self.closing && self.session.is_modified() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending.get_or_insert(Request::Quit);
        }
        self.read_audio_files(ctx);
        self.levels = levels(self.session.meters(), self.session.project());
        // The meters fall back for a few seconds after the music stops. They
        // only move while a device plays.
        let metering = self.output.info().is_some() && self.levels.moving();
        // Also while the window is hidden, so the engine's old graphs get
        // dropped and a lost device gets tried again.
        let mut frame = if self.session.compiling() {
            COMPILING_FRAME
        } else if self.session.stats().playing || self.session.stats().preview.is_some() || metering
        {
            PLAYING_FRAME
        } else {
            STOPPED_FRAME
        };
        // The export's progress, and its session, which sends its engine
        // the graphs as they compile.
        if let Some(export) = &self.export {
            frame = frame.min(match export.stage() {
                export::Stage::Preparing { .. } => COMPILING_FRAME,
                _ => PLAYING_FRAME,
            });
        }
        // A plugin's timer.
        if let Some(due) = self.session.next_plugin_timer() {
            frame = frame.min(due.saturating_duration_since(Instant::now()));
        }
        ctx.request_repaint_after(frame);
    }

    /// The plugins close their windows while the app's window is still
    /// there.
    fn on_exit(&mut self) {
        self.plugin_windows.close_all(&mut self.session);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Commands that keys ran before this frame.
        self.handle_requests(&ctx, frame);
        self.update_plugin_status();

        let stats = self.session.stats();
        let heard = self.session.heard_position(self.output.latency());
        let rate = self.session.engine_config().sample_rate;
        let summary = self.output.summary();
        let details = self.output.details();
        let name = self.name();
        let recordings = recordings(&self.session);
        let library = self.session.library();
        let scanning = library.scanning();
        let (found, total) = library.found();
        let view = View {
            project: self.session.project(),
            name: &name,
            modified: self.session.is_modified(),
            commands: self.session.commands(),
            keymap: self.session.keymap(),
            default_keymap: self.keymap_file.defaults(),
            undo: self.session.undo_name(),
            redo: self.session.redo_name(),
            editor: self.session.editor(),
            playback: Playback {
                playing: stats.playing,
                record: self.session.is_recording(),
                playhead: self.session.beats_at(heard),
                seconds: heard as f64 / f64::from(rate),
            },
            recordings: &recordings,
            levels: &self.levels,
            inputs: &self.input_choices,
            midi_inputs: &self.midi_choices,
            waveforms: self.audio_files.waveforms(),
            audio: Audio {
                summary: &summary,
                running: self.output.info().is_some(),
                load: self.output.load(),
                dropouts: self.output.dropouts(),
                details: &details,
            },
            keymap_file: KeymapStatus {
                path: self.keymap_file.path(),
                problems: self.keymap_file.problems(),
                error: self.keymap_file.error(),
                notice: self.keymap_file.notice(),
            },
            messages: &self.messages,
            plugins: Plugins {
                installed: &self.plugin_entries,
                scanning: self.plugin_scan.is_running(),
                status: &self.plugin_status,
            },
            native: musicable_engine::devices::BUILT,
            library: musicable_ui::Library {
                index: library.index(),
                loaded: library.is_loaded(),
                scanning: &scanning,
                listing: match library.listing() {
                    Listing::Search(text) => musicable_ui::Listing::Search(text),
                    Listing::Similar(file) => musicable_ui::Listing::Similar(file),
                },
                found,
                total,
                presets: library.presets(),
                finding_presets: library.is_finding_presets(),
                preview: library
                    .preview_playing()
                    .map(|preview| musicable_ui::Preview {
                        path: &preview.path,
                        at: (stats.preview)
                            .filter(|at| at.id == preview.id)
                            .map(|at| at.frame),
                        frames: preview.frames,
                        peaks: preview.peaks.as_deref(),
                    }),
                auto_preview: self.settings.browser.auto_preview,
                preview_volume: self.settings.browser.preview_volume,
            },
        };
        let actions = self.shell.show(ui, &view);
        for action in actions {
            self.act(action, &ctx, frame);
        }
        let offset = self.session.recording_offset();
        match self.audio_window.show(&ctx, &self.output, offset) {
            Some(Change::Stream(request)) => self.apply_audio(request),
            Some(Change::RecordingOffset(frames)) => self.set_recording_offset(frames),
            None => {}
        }
        if let Some(asked) = self.appearance_window.show(&ctx, &self.theme_file) {
            self.appearance(asked, &ctx, frame);
        }
        let project = self.session.project();
        let editor = self.session.editor();
        let export = self.export.as_ref();
        match self.export_window.show(&ctx, project, editor, rate, export) {
            Some(Asked::Export {
                range,
                settings,
                tracks,
            }) => self.start_export(range, settings, &tracks, &ctx, frame),
            Some(Asked::Cancel) => {
                if let Some(export) = &mut self.export {
                    export.cancel();
                }
                self.tick_export();
            }
            None => {}
        }
        self.ask_to_save(&ctx, frame);
        self.update_title(&ctx);
    }
}

/// A plugin's parameters that automation can move, as last listed.
#[derive(Default)]
struct Listed {
    /// Where the plugin kept the list they came from, and how long it was,
    /// to know when the plugin describes them again.
    from: (usize, usize),
    params: Arc<[PluginParam]>,
}

/// The parameters of `params` automation can move: those the plugin
/// neither hides nor keeps to itself.
fn automatable(params: &[ParamInfo]) -> Arc<[PluginParam]> {
    params
        .iter()
        .filter(|param| param.automatable && !param.hidden && !param.read_only)
        .map(|param| PluginParam {
            id: param.id,
            spec: ParamSpec {
                name: param.name.clone(),
                min: param.min,
                max: param.max,
                default: param.default,
                scale: Scale::Linear,
            },
        })
        .collect()
}

/// The readings of the meters of `project`'s channels, for the mixer.
fn levels(meters: &Meters, project: &Project) -> Levels {
    let read = |meter: &Meter| {
        let level = meter.level();
        Level {
            peak: level.peak,
            rms: level.rms,
            highest: level.highest,
        }
    };
    let tracks = project
        .tracks
        .iter()
        .filter_map(|track| Some((track.id, read(meters.track(track.id)?))))
        .collect();
    Levels {
        tracks,
        master: read(meters.master()),
    }
}

/// What the armed tracks have recorded so far, in the song's beats.
fn recordings(session: &Session) -> Vec<musicable_ui::Recording> {
    let recordings = session.recordings().into_iter();
    recordings
        .map(|recording| {
            let start = session.beats_at(recording.start);
            let end = session.beats_at(recording.start + recording.frames);
            musicable_ui::Recording {
                track: recording.track,
                range: BeatRange::new(start, end),
            }
        })
        .collect()
}

/// Wakes the window whose context is `ctx`, from any thread, so a frame
/// carries out what a plugin asked for.
fn waker(ctx: &egui::Context) -> Waker {
    let ctx = ctx.clone();
    Arc::new(move || ctx.request_repaint())
}

/// The app's main window, as Win32's `HWND`, for plugin windows to stay in
/// front of.
#[cfg(windows)]
fn owner(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(windows))]
fn owner(_frame: &eframe::Frame) -> Option<isize> {
    None
}

/// Gives two clicks as long to make a double-click as the system does, where
/// it says, rather than egui's 0.3 seconds. Read as the app starts.
fn follow_system_double_clicks(ctx: &egui::Context) {
    if let Some(time) = double_click_time() {
        ctx.options_mut(|options| options.input_options.max_double_click_delay = time);
    }
}

/// The double-click time in Windows' mouse settings, in seconds: 0.5 unless
/// it's been changed.
#[cfg(windows)]
fn double_click_time() -> Option<f64> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
    // SAFETY: a plain Win32 call that reads a setting.
    let millis = unsafe { GetDoubleClickTime() };
    (millis > 0).then(|| f64::from(millis) / 1000.0)
}

#[cfg(not(windows))]
fn double_click_time() -> Option<f64> {
    None
}

/// `path`, with the project file extension added unless it has it.
fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|ext| ext == EXTENSION) {
        path
    } else {
        let mut path = path.into_os_string();
        path.push(".");
        path.push(EXTENSION);
        path.into()
    }
}

#[cfg(test)]
mod arranging;
#[cfg(test)]
mod browsing;
#[cfg(test)]
mod built_in_devices;
#[cfg(test)]
mod exporting;
#[cfg(test)]
mod plugins_on_tracks;
#[cfg(test)]
mod remapping;
#[cfg(test)]
mod testing;
#[cfg(test)]
mod theming;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_clicks_take_as_long_as_the_system_gives_them() {
        let ctx = egui::Context::default();
        follow_system_double_clicks(&ctx);
        let delay = ctx.options(|options| options.input_options.max_double_click_delay);
        if cfg!(windows) {
            let time = double_click_time().expect("Windows has a double-click time");
            assert!(
                (0.1..=5.0).contains(&time),
                "a double-click time of {time} s"
            );
            assert_eq!(delay, time);
        } else {
            assert_eq!(delay, egui::InputOptions::default().max_double_click_delay);
        }
    }

    #[test]
    fn saved_projects_get_the_extension() {
        let saved = |path: &str| with_extension(PathBuf::from(path));
        assert_eq!(saved("song"), PathBuf::from("song.musicable"));
        assert_eq!(saved("song.musicable"), PathBuf::from("song.musicable"));
        assert_eq!(saved("song.v2"), PathBuf::from("song.v2.musicable"));
    }
}
