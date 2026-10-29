//! The shell as the UI's tests drive it, standing in for the app, and for
//! the system where windows of their own open.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{
    Event, ImmediateViewport, Pos2, RawInput, Rect, ViewportBuilder, ViewportCommand, ViewportId,
    ViewportInfo, pos2, vec2,
};
use egui_kittest::{Harness, HarnessBuilder};
use musicable_commands::{
    Document, Editor, Focus, Keymap, Problem, Registry, Transport, browser, history, view,
};
use musicable_library::{FileId, Index, Preset};
use musicable_model::native::NativeKind;
use musicable_model::time::Beats;
use musicable_model::{DeviceId, Project};

use crate::{
    Action, Audio, DEFAULT_WORKSPACE, KeymapStatus, Levels, Library, Listing, Peaks, Performance,
    Playback, PluginEntry, PluginStatus, Plugins, Preview, Shell, View, Waveforms, Workspaces,
};

/// What the shell shows in the tests, and what it hands back.
pub(crate) struct State {
    pub shell: Shell,
    pub project: Project,
    pub editor: Editor,
    pub waveforms: Waveforms,
    pub commands: Registry,
    /// The keys in use. A new keymap from the shell replaces them, as it does
    /// in the app.
    pub keymap: Keymap,
    pub default_keymap: Keymap,
    pub keymap_path: Option<PathBuf>,
    pub keymap_problems: Vec<Problem>,
    /// The undo history the Edit menu and the History panel show, in a
    /// document of its own: the tests make its steps with [`step`](Self::step),
    /// and it goes to the step the History panel asks for.
    pub history: Document,
    /// What the meters read.
    pub levels: Levels,
    /// What the CPU figures say.
    pub performance: Performance,
    /// The plugins installed, whether a scan is running, and how each
    /// plugin device's plugin is doing.
    pub plugins: Vec<PluginEntry>,
    pub scanning: bool,
    pub plugin_status: BTreeMap<DeviceId, PluginStatus>,
    /// The built-in devices the browser lists: all of them.
    pub native: Vec<NativeKind>,
    pub library: LibraryState,
    /// The workspaces' names, by slot, and the slot of the one in use.
    pub workspaces: Vec<Option<String>>,
    pub workspace: usize,
    /// Whether the Keys page shows, in a window of its own, as the app's
    /// Settings window would show it.
    pub keys_page: bool,
    pub actions: Vec<Action>,
}

impl State {
    /// Makes a step called `name` in the undo history: one that turns the
    /// metronome on or off.
    pub fn step(&mut self, name: &str) {
        let on = !self.history.project().metronome;
        self.history.transact(name, |tx| tx.set_metronome(on));
    }
}

/// A transport that plays along: locating moves its playhead.
#[derive(Debug, Default)]
pub(crate) struct Player {
    pub playing: bool,
    pub recording: bool,
    pub playhead: Beats,
}

impl Transport for Player {
    fn is_playing(&self) -> bool {
        self.playing
    }

    fn play(&mut self) {
        self.playing = true;
    }

    fn stop(&mut self) {
        self.playing = false;
        self.recording = false;
    }

    fn locate(&mut self, at: Beats) {
        self.playhead = at;
    }

    fn playhead(&self) -> Beats {
        self.playhead
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn set_recording(&mut self, on: bool) {
        self.recording = on;
    }
}

/// The browser's library, as the app would have it.
#[derive(Debug)]
pub(crate) struct LibraryState {
    pub index: Index,
    pub loaded: bool,
    pub scanning: Vec<(PathBuf, usize)>,
    pub reading: Vec<(PathBuf, usize)>,
    /// The last search: searches the shell asks for are run on the index,
    /// as the app runs them.
    pub search: String,
    /// The file Find Similar listed files like, if it did last.
    pub similar: Option<PathBuf>,
    pub found: Vec<FileId>,
    pub presets: Vec<Preset>,
    pub finding_presets: bool,
    pub preview: Option<Previewing>,
    pub auto_preview: bool,
    pub preview_volume: i32,
}

/// The sample previewing.
#[derive(Debug)]
pub(crate) struct Previewing {
    pub path: PathBuf,
    pub at: Option<u64>,
    pub frames: Option<u64>,
    pub peaks: Option<Arc<Peaks>>,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            index: Index::new(),
            loaded: true,
            scanning: Vec::new(),
            reading: Vec::new(),
            search: String::new(),
            similar: None,
            found: Vec::new(),
            presets: Vec::new(),
            finding_presets: false,
            preview: None,
            auto_preview: true,
            preview_volume: 0,
        }
    }
}

impl LibraryState {
    fn view(&self) -> Library<'_> {
        Library {
            index: &self.index,
            loaded: self.loaded,
            scanning: &self.scanning,
            reading: &self.reading,
            listing: match &self.similar {
                Some(like) => Listing::Similar(like),
                None => Listing::Search(&self.search),
            },
            found: &self.found,
            total: self.found.len(),
            presets: &self.presets,
            finding_presets: self.finding_presets,
            preview: self.preview.as_ref().map(|preview| Preview {
                path: &preview.path,
                at: preview.at,
                frames: preview.frames,
                peaks: preview.peaks.as_deref(),
            }),
            auto_preview: self.auto_preview,
            preview_volume: self.preview_volume,
        }
    }

    /// Lists the files whose paths have every word of `text` in them, in
    /// the index's order.
    pub fn search(&mut self, text: &str) {
        self.search = text.to_owned();
        self.similar = None;
        let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
        self.found.clear();
        if words.is_empty() {
            return;
        }
        for (place, found) in self.index.places().iter().enumerate() {
            for (file, record) in found.files().iter().enumerate() {
                let path = record.path.to_lowercase();
                if words.iter().all(|word| path.contains(word)) {
                    self.found.push(FileId { place, file });
                }
            }
        }
    }

    /// Lists every other file as like `file`.
    fn find_similar(&mut self, file: &Path) {
        let like = self.index.find(file);
        self.similar = Some(file.to_owned());
        self.found.clear();
        for (place, found) in self.index.places().iter().enumerate() {
            for file in 0..found.files().len() {
                let id = FileId { place, file };
                if Some(id) != like {
                    self.found.push(id);
                }
            }
        }
    }
}

/// Every command the app has.
pub(crate) fn commands() -> Registry {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    commands
}

/// The shell in a window, showing `project` with the app's commands and the
/// default keymap.
pub(crate) fn harness(project: Project) -> Harness<'static, State> {
    build(Harness::builder(), project)
}

/// Like [`harness`], with frames as quick as a screen's, so two clicks make
/// a double click.
pub(crate) fn quick_harness(project: Project) -> Harness<'static, State> {
    let builder = Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(40);
    build(builder, project)
}

fn build(builder: HarnessBuilder<State>, project: Project) -> Harness<'static, State> {
    let state = State {
        shell: Shell::new(),
        project,
        editor: Editor::default(),
        waveforms: Waveforms::default(),
        commands: commands(),
        keymap: Keymap::default_keymap(),
        default_keymap: Keymap::default_keymap(),
        keymap_path: None,
        keymap_problems: Vec::new(),
        history: Document::default(),
        levels: Levels::default(),
        performance: Performance::default(),
        plugins: Vec::new(),
        scanning: false,
        plugin_status: BTreeMap::new(),
        native: NativeKind::ALL.to_vec(),
        library: LibraryState::default(),
        workspaces: vec![Some(DEFAULT_WORKSPACE.to_owned())],
        workspace: 0,
        keys_page: false,
        actions: Vec::new(),
    };
    builder.with_size([1100.0, 720.0]).build_ui_state(
        |ui, state: &mut State| {
            let view = View {
                project: &state.project,
                editor: &state.editor,
                waveforms: &state.waveforms,
                name: "Untitled",
                modified: false,
                commands: &state.commands,
                keymap: &state.keymap,
                default_keymap: &state.default_keymap,
                history: state.history.steps(),
                playback: Playback {
                    playing: false,
                    record: false,
                    playhead: Beats::ZERO,
                    seconds: 0.0,
                },
                recordings: &[],
                levels: &state.levels,
                performance: &state.performance,
                inputs: &[],
                midi_inputs: &[],
                audio: Audio {
                    summary: "48 kHz · 128 samples · 5.3 ms",
                    running: true,
                    load: Some(0.25),
                    dropouts: 0,
                    details: "",
                },
                keymap_file: KeymapStatus {
                    path: state.keymap_path.as_deref(),
                    problems: &state.keymap_problems,
                    error: None,
                    notice: None,
                },
                messages: &[],
                agent: None,
                analysing: None,
                rendering: None,
                plugins: Plugins {
                    installed: &state.plugins,
                    scanning: state.scanning,
                    status: &state.plugin_status,
                },
                native: &state.native,
                library: state.library.view(),
                workspaces: Workspaces {
                    slots: &state.workspaces,
                    current: state.workspace,
                },
                monitors: &[],
            };
            let mut actions = state.shell.show(ui, &view);
            if state.keys_page {
                let shell = &mut state.shell;
                // Below the transport bar, where the app's window opens.
                let top = ui.ctx().content_rect().center_top() + egui::vec2(0.0, 72.0);
                egui::Window::new("Keyboard Shortcuts")
                    .pivot(egui::Align2::CENTER_TOP)
                    .default_pos(top)
                    .default_width(680.0)
                    .show(ui.ctx(), |ui| actions.extend(shell.show_keys(ui, &view)));
            } else {
                state.shell.hide_keys();
            }
            for action in &actions {
                match action {
                    Action::SetKeymap(keymap) => {
                        state.keymap = keymap.clone();
                        state.keymap_problems.clear();
                    }
                    // What the browser lists is what the library found.
                    Action::RunWith(id, args) if *id == browser::SEARCH => {
                        state.library.search(&args.get::<String>("text").unwrap());
                    }
                    Action::RunWith(id, args) if *id == browser::FIND_SIMILAR => {
                        let file: String = args.get("file").unwrap();
                        state.library.find_similar(Path::new(&file));
                    }
                    Action::RunWith(id, args) if *id == history::GO_TO => {
                        let step = args.get::<i64>("step").unwrap();
                        assert!(state.history.go_to(step as usize), "{step}");
                    }
                    _ => {}
                }
            }
            // As the app would, keep the zoom and scrolling the
            // arrangement and the piano roll ask for, and give the focus to
            // the panel clicked in, as the tests look for the editor's
            // state, not these.
            actions.retain(|action| match action {
                Action::RunWith(id, args) if *id == view::SET_FOCUS => {
                    let panel = args.get::<String>("panel").unwrap();
                    state.editor.focus = Focus::on(&panel).unwrap();
                    false
                }
                Action::RunWith(id, args)
                    if *id == view::SET_VIEWPORT || *id == view::SET_PIANO_ROLL_VIEWPORT =>
                {
                    let number = |name| args.get::<f64>(name).unwrap_or_default();
                    let roll = &mut state.editor.piano_roll;
                    let viewport = match *id == view::SET_VIEWPORT {
                        true => &mut state.editor.viewport,
                        false => {
                            roll.height = number("height") as f32;
                            roll.key_height = number("key_height") as f32;
                            &mut roll.viewport
                        }
                    };
                    viewport.start = args.get("start").unwrap_or_default();
                    viewport.zoom = number("zoom");
                    viewport.top = number("top") as f32;
                    viewport.width = number("width") as f32;
                    false
                }
                _ => true,
            });
            state.actions.extend(actions);
        },
        state,
    )
}

/// A window of its own, as a test has the system show it.
#[derive(Debug, Clone)]
pub(crate) struct Own {
    pub id: ViewportId,
    /// Where it is, how big, and whether it has the focus, as egui
    /// describes it to the window's passes.
    pub info: ViewportInfo,
    /// What the shell made it with, as of its last pass.
    pub builder: ViewportBuilder,
    /// The events for its next pass.
    pub events: Vec<Event>,
    /// The commands its own passes sent it, which moved and sized it.
    pub commands: Vec<ViewportCommand>,
}

thread_local! {
    /// The windows of their own the system shows, in the order they opened.
    static OWN: RefCell<Vec<Own>> = const { RefCell::new(Vec::new()) };
}

/// Like [`harness`], with windows of their own as the system shows them,
/// rather than inside the main window: each has passes of its own, with
/// the input [`with_own`] gives it, as eframe gives them. The main window
/// is at the desktop's corner, at 100%, with the system's focus.
pub(crate) fn own_windows_harness(project: Project) -> Harness<'static, State> {
    OWN.with_borrow_mut(Vec::clear);
    let mut harness = harness(project);
    harness.ctx.set_embed_viewports(false);
    egui::Context::set_immediate_viewport_renderer(show_own);
    let size = harness.ctx.content_rect().size();
    let main = (harness.input_mut().viewports)
        .entry(ViewportId::ROOT)
        .or_default();
    main.inner_rect = Some(Rect::from_min_size(Pos2::ZERO, size));
    main.outer_rect = main.inner_rect;
    main.native_pixels_per_point = Some(1.0);
    main.focused = Some(true);
    harness.run();
    harness
}

/// The windows of their own the system shows, in the order they opened.
pub(crate) fn own_windows() -> Vec<Own> {
    OWN.with_borrow(Clone::clone)
}

/// Changes the window of its own `id` with `change`, and tells the main
/// window where it is, as the system would.
pub(crate) fn with_own(
    harness: &mut Harness<'_, State>,
    id: ViewportId,
    change: impl FnOnce(&mut Own),
) {
    let info = OWN.with_borrow_mut(|windows| {
        let window = (windows.iter_mut()).find(|window| window.id == id)?;
        change(window);
        Some(window.info.clone())
    });
    let info = info.expect("a window of its own");
    harness.input_mut().viewports.insert(id, info);
}

/// Clicks with the mouse at `at`.
pub(crate) fn click_at(harness: &mut Harness<'_, State>, at: Pos2) {
    harness.hover_at(at);
    harness.run();
    harness.drag_at(at);
    harness.run();
    harness.drop_at(at);
    harness.run();
}

/// A key pressed and let go of, as a window of its own gets it.
pub(crate) fn key(key: egui::Key) -> [Event; 2] {
    [true, false].map(|pressed| Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    })
}

/// Gives the main window the system's focus, or takes it away.
pub(crate) fn focus_main(harness: &mut Harness<'_, State>, focused: bool) {
    let main = harness
        .input_mut()
        .viewports
        .entry(ViewportId::ROOT)
        .or_default();
    main.focused = Some(focused);
}

/// Shows a window of its own, as eframe does: a pass of its own, nested in
/// the main window's, with the input the test gave it. It goes where the
/// shell moves it and is as big as the shell makes it, as at 100%, with no
/// frame.
fn show_own(ctx: &egui::Context, viewport: ImmediateViewport<'_>) {
    let ImmediateViewport {
        ids,
        builder,
        mut viewport_ui_cb,
    } = viewport;
    let id = ids.this;
    let mut viewports = ctx.input(|i| i.raw.viewports.clone());
    let time = ctx.input(|i| i.time);
    if id == crate::ghost::id() {
        // The ghost of a dragged tab isn't a window of its own: the tests
        // find it in the shell. It's drawn all the same.
        OWN.with_borrow(|windows| {
            for window in windows {
                viewports.insert(window.id, window.info.clone());
            }
        });
        let size = builder.inner_size.unwrap_or(vec2(100.0, 30.0));
        let at = builder.position.unwrap_or_default();
        let rect = Rect::from_min_size(at, size);
        let info = ViewportInfo {
            inner_rect: Some(rect),
            outer_rect: Some(rect),
            native_pixels_per_point: Some(1.0),
            ..ViewportInfo::default()
        };
        viewports.insert(id, info);
        let input = RawInput {
            viewport_id: id,
            viewports,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(time),
            ..RawInput::default()
        };
        let output = ctx.run_ui(input, |ui| viewport_ui_cb(ui));
        output.drop_without_applying_deltas();
        return;
    }
    let input = OWN.with_borrow_mut(|windows| {
        if !windows.iter().any(|window| window.id == id) {
            let at = builder.position.unwrap_or(pos2(1200.0, 0.0));
            let size = builder.inner_size.unwrap_or(vec2(400.0, 300.0));
            let rect = Rect::from_min_size(at, size);
            windows.push(Own {
                id,
                info: ViewportInfo {
                    inner_rect: Some(rect),
                    outer_rect: Some(rect),
                    native_pixels_per_point: Some(1.0),
                    focused: Some(false),
                    minimized: Some(false),
                    maximized: Some(false),
                    ..ViewportInfo::default()
                },
                builder: builder.clone(),
                events: Vec::new(),
                commands: Vec::new(),
            });
        }
        for window in windows.iter() {
            viewports.insert(window.id, window.info.clone());
        }
        let window = (windows.iter_mut()).find(|window| window.id == id)?;
        window.builder = builder;
        let size = window
            .info
            .inner_rect
            .map_or(vec2(400.0, 300.0), |rect| rect.size());
        // Its events, such as Close, go to one pass.
        window.info.events.clear();
        Some(RawInput {
            viewport_id: id,
            viewports,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(time),
            focused: window.info.focused == Some(true),
            events: std::mem::take(&mut window.events),
            ..RawInput::default()
        })
    });
    let Some(input) = input else {
        return;
    };
    let output = ctx.run_ui(input, |ui| viewport_ui_cb(ui));
    let commands = (output.viewport_output.get(&id)).map(|output| output.commands.clone());
    output.drop_without_applying_deltas();
    OWN.with_borrow_mut(|windows| {
        let Some(window) = (windows.iter_mut()).find(|window| window.id == id) else {
            return;
        };
        for command in commands.into_iter().flatten() {
            let rect = window.info.inner_rect.unwrap_or(Rect::ZERO);
            let rect = match command {
                ViewportCommand::OuterPosition(at) => Some(Rect::from_min_size(at, rect.size())),
                ViewportCommand::InnerSize(size) => Some(Rect::from_min_size(rect.min, size)),
                ViewportCommand::Maximized(maximized) => {
                    window.info.maximized = Some(maximized);
                    None
                }
                _ => None,
            };
            if let Some(rect) = rect {
                window.info.inner_rect = Some(rect);
                window.info.outer_rect = Some(rect);
            }
            window.commands.push(command);
        }
    });
}
