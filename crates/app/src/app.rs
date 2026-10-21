//! The window: the UI crate's shell, wired to the session, the audio device
//! and the project's file.

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui;
use musicable_commands::Request;
use musicable_engine::EngineHandle;
use musicable_io::project::{self, EXTENSION};
use musicable_model::Project;
use musicable_ui::{Action, Audio, KeyPresses, KeymapStatus, Playback, SaveChoice, Shell, View};

use crate::audio_window::AudioWindow;
use crate::keymap_file::UserKeymap;
use crate::settings::{self, AudioSettings, KEYMAP_FILE, SETTINGS_FILE, Settings};
use crate::{Output, Session, capital};

/// How often the window redraws while the transport plays, to move the
/// playhead, and while it doesn't, to update the meters.
const PLAYING_FRAME: Duration = Duration::from_millis(16);
const STOPPED_FRAME: Duration = Duration::from_millis(250);
/// How often the app looks for a graph while one compiles, to send it to the
/// engine soon after it's ready.
const COMPILING_FRAME: Duration = Duration::from_millis(5);

/// The app's window, and everything it drives.
pub struct App {
    session: Session,
    output: Output,
    shell: Shell,
    keys: KeyPresses,
    audio_window: AudioWindow,
    /// The project's file, once it has one.
    path: Option<PathBuf>,
    /// Where the file dialogs start: the folder a project was last opened
    /// from or saved to.
    folder: Option<PathBuf>,
    /// Errors and notices for the status bar, oldest first.
    messages: Vec<String>,
    /// Where the settings and the user's keymap live.
    config_dir: Option<PathBuf>,
    settings: Settings,
    /// The user's keymap file, which every change of keys is saved to.
    keymap_file: UserKeymap,
    /// What waits for the user to say whether to save their changes first.
    pending: Option<Request>,
    /// Set once the user has said what to do with their changes, so the
    /// window closes.
    closing: bool,
    /// The window's title, as last set.
    title: String,
}

impl App {
    /// Opens the audio device and the project at `path`, or a new project.
    pub fn new(path: Option<PathBuf>) -> Self {
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
        Self::with_output(path, config_dir, settings, output, handle, messages)
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
        Self {
            session: Session::new(project, keymap, handle),
            output,
            shell: Shell::new(),
            keys: KeyPresses::new(),
            audio_window: AudioWindow::new(),
            path,
            folder,
            messages,
            config_dir,
            settings,
            keymap_file,
            pending: None,
            closing: false,
            title: String::new(),
        }
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
            Action::SetTempo(bpm) => self.session.set_tempo(bpm),
            Action::BeginGesture => self.session.begin_gesture(),
            Action::EndGesture => self.session.end_gesture(),
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
        }
    }

    fn open_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.session.open(project);
        self.set_path(path);
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
        match project::save(&path, self.session.project()) {
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
            if asking || session.keymap().lookup(chord).is_none() {
                return false;
            }
            if !repeat {
                session.key_pressed(chord);
            }
            true
        });
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.session.tick();
        self.output.tick(&mut self.session, &mut self.messages);
        let close = ctx.input(|i| i.viewport().close_requested());
        if close && !self.closing && self.session.is_modified() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending.get_or_insert(Request::Quit);
        }
        // Also while the window is hidden, so the engine's old graphs get
        // dropped and a lost device gets tried again.
        let frame = if self.session.compiling() {
            COMPILING_FRAME
        } else if self.session.stats().playing {
            PLAYING_FRAME
        } else {
            STOPPED_FRAME
        };
        ctx.request_repaint_after(frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Commands that keys ran before this frame.
        self.handle_requests(&ctx, frame);

        let stats = self.session.stats();
        let rate = self.session.engine_config().sample_rate;
        let summary = self.output.summary();
        let details = self.output.details();
        let name = self.name();
        let view = View {
            project: self.session.project(),
            name: &name,
            modified: self.session.is_modified(),
            commands: self.session.commands(),
            keymap: self.session.keymap(),
            default_keymap: self.keymap_file.defaults(),
            undo: self.session.undo_name(),
            redo: self.session.redo_name(),
            playback: Playback {
                playing: stats.playing,
                playhead: self.session.playhead(),
                seconds: stats.position as f64 / f64::from(rate),
            },
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
        };
        let actions = self.shell.show(ui, &view);
        for action in actions {
            self.act(action, &ctx, frame);
        }
        if let Some(request) = self.audio_window.show(&ctx, &self.output) {
            self.apply_audio(request);
        }
        self.ask_to_save(&ctx, frame);
        self.update_title(&ctx);
    }
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
mod remapping;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_projects_get_the_extension() {
        let saved = |path: &str| with_extension(PathBuf::from(path));
        assert_eq!(saved("song"), PathBuf::from("song.musicable"));
        assert_eq!(saved("song.musicable"), PathBuf::from("song.musicable"));
        assert_eq!(saved("song.v2"), PathBuf::from("song.v2.musicable"));
    }
}
