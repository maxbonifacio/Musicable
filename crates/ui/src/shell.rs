//! The app shell: the menu bar, the transport bar, the panels docked under
//! them, and a status bar, with the command palette, the box that renames
//! things, and the Keys page of the app's Settings window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::{Align, Button, Layout, RichText, Ui};
use musicable_commands::editor::Lane;
use musicable_commands::mixer::Change;
use musicable_commands::{
    Args, Command, CommandId, Editor, Focus, KeyChord, Keymap, Named, Problem, Registry, Steps,
    Value, arrange, controllers, edit, locator, notes, options, take, track, transport, view,
    workspace,
};
use musicable_model::clip::ClipId;
use musicable_model::native::NativeKind;
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Chain, DeviceId, Project, TrackId};

use crate::browser::{self, Library};
use crate::carry::Carry;
use crate::devices::Plugins;
use crate::dock::{Dock, Draw, Move};
use crate::ghost;
use crate::menus::menus;
use crate::mixer::{InputChoice, Levels, MidiChoice};
use crate::mouse::MouseMap;
use crate::palette::Palette;
use crate::panels::{
    self, ARRANGEMENT, BROWSER, Cx, DEVICES, HISTORY, MIXER, PIANO_ROLL, Panels, SAMPLE_EDITOR,
};
use crate::parts::{Parts, Views};
use crate::shortcuts::Shortcuts;
use crate::theme::Theme;
use crate::transport_bar;
use crate::waveform::Waveforms;
use crate::widgets::{checked_name_box, confirm_box, name_box};
use crate::windows::{Monitor, Place, Placement, Window, on_desktop, unscaled};
use crate::workspace::{Workspace, check_name};

/// What the shell shows. The app fills one in for each frame.
pub struct View<'a> {
    pub project: &'a Project,
    /// What's selected, the zoom, the grid and the rest of the editor's
    /// state.
    pub editor: &'a Editor,
    /// The waveforms of the audio files the clips play, as far as the app
    /// has read them.
    pub waveforms: &'a Waveforms,
    /// The project's name: its file's name, or "Untitled".
    pub name: &'a str,
    /// Whether the project changed since it was opened or saved.
    pub modified: bool,
    pub commands: &'a Registry,
    pub keymap: &'a Keymap,
    /// The keymap Musicable starts from, which the user's keymap file
    /// changes.
    pub default_keymap: &'a Keymap,
    /// The steps of the undo history, by name, for the History panel and
    /// the Edit menu, which says what Undo and Redo would change, such as
    /// "Undo Set Tempo".
    pub history: Steps<'a>,
    pub playback: Playback,
    /// What the armed tracks have recorded so far, while they record.
    pub recordings: &'a [Recording],
    /// The meters' readings, as the engine last measured them.
    pub levels: &'a Levels,
    /// The audio inputs tracks can record from, for the mixer's input
    /// menus.
    pub inputs: &'a [InputChoice],
    /// The MIDI inputs, for the mixer's MIDI input menus.
    pub midi_inputs: &'a [MidiChoice],
    pub audio: Audio<'a>,
    /// The user's keymap file.
    pub keymap_file: KeymapStatus<'a>,
    /// Errors and notices for the user, oldest first.
    pub messages: &'a [String],
    /// What the status bar says while an agent controls Musicable, such as
    /// "Agent: Move Notes": its last change. Clicked, it shows the Settings
    /// window's Agents page.
    pub agent: Option<&'a str>,
    /// The plugins installed, and how the project's are doing.
    pub plugins: Plugins<'a>,
    /// Musicable's own devices that play, which the browser lists: the
    /// engine's `devices::BUILT`. The others wait for their sound.
    pub native: &'a [NativeKind],
    /// The browser's library of samples and presets, and the preview.
    pub library: Library<'a>,
    /// The user's workspaces, for the Workspaces menu.
    pub workspaces: Workspaces<'a>,
    /// The monitors, where windows of their own go: a window whose monitor
    /// is gone opens on the main one. None where the app can't tell.
    pub monitors: &'a [Monitor],
}

/// The transport, as the engine last reported it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub playing: bool,
    /// Whether recording is on: whether playing records the armed tracks.
    pub record: bool,
    /// Where the playhead is in the song: what's heard, so the latency of
    /// plugins and of the audio device is taken off where the engine is.
    pub playhead: Beats,
    /// The same, in seconds from the start of the song.
    pub seconds: f64,
}

/// A stretch of the song a track has recorded so far, while it records.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recording {
    pub track: TrackId,
    pub range: BeatRange,
}

/// The audio device, as the corner of the transport bar shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Audio<'a> {
    /// What's playing, such as "48 kHz · 128 samples · 5.3 ms", or why
    /// nothing is.
    pub summary: &'a str,
    /// Whether a device is playing.
    pub running: bool,
    /// The share of the time the audio thread was busy rendering lately:
    /// over 1.0 it can't keep up. `None` without a device.
    pub load: Option<f32>,
    /// Dropouts since the device started or the count was reset.
    pub dropouts: u64,
    /// More about the device, the load and the dropouts, for a tooltip.
    pub details: &'a str,
}

/// The user's keymap file, as the Keys page shows it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KeymapStatus<'a> {
    /// Where the file goes, if Musicable found a place for it.
    pub path: Option<&'a Path>,
    /// What was wrong with its lines when it was last read.
    pub problems: &'a [Problem],
    /// Why it couldn't be used at all, if it couldn't.
    pub error: Option<&'a str>,
    /// How the last change to the keys went, or the last import or export.
    pub notice: Option<&'a str>,
}

/// The user's workspaces, as the Workspaces menu lists them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Workspaces<'a> {
    /// The name of each slot's workspace, the first being Default's, or
    /// `None` where a slot has none.
    pub slots: &'a [Option<String>],
    /// The slot of the workspace in use, from 0.
    pub current: usize,
}

impl Workspaces<'_> {
    /// The name of the workspace in slot `slot`, from 0, if it has one.
    pub fn name(&self, slot: usize) -> Option<&str> {
        self.slots.get(slot)?.as_deref()
    }

    /// The name of the workspace in use.
    fn current_name(&self) -> &str {
        self.name(self.current).unwrap_or(DEFAULT_WORKSPACE)
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.slots.iter().flatten().map(String::as_str)
    }
}

/// The name of the first workspace, which every user has.
pub const DEFAULT_WORKSPACE: &str = "Default";

/// Something the user asked for, for the app to carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Run a command.
    Run(CommandId),
    /// Run a command with arguments, as the arrangement's gestures and
    /// context menus do.
    RunWith(CommandId, Args),
    /// Read these audio files and insert them, as
    /// [`arrange::INSERT_AUDIO`] does: on `track` at `at` when they're
    /// dropped there.
    DropFiles {
        files: Vec<PathBuf>,
        track: Option<TrackId>,
        at: Option<Beats>,
    },
    /// Give the sampler `device` the audio file at `file`, with
    /// [`device::SET_SAMPLE`](musicable_commands::device::SET_SAMPLE): it
    /// was dropped on it.
    LoadSample { device: DeviceId, file: PathBuf },
    /// Set the tempo at the playhead, from the tempo field.
    SetTempo(f64),
    /// The user started dragging a control, such as the tempo field. Until
    /// [`EndGesture`](Action::EndGesture), its changes are one step in the
    /// undo history.
    BeginGesture,
    /// The user let go of the control.
    EndGesture,
    /// Make a change from the mixer, such as moving a fader.
    Mixer(Change),
    /// List the audio and MIDI inputs again, for the mixer's input menus:
    /// the user opened one, maybe after plugging a device in.
    ListInputs,
    /// Clear the highest peak of every channel's meter.
    ClearPeaks,
    /// Use this keymap from now on, and save how it differs from the default
    /// keymap: the user changed keys on the Keys page.
    SetKeymap(Keymap),
    /// Give every command its default keys again.
    ResetKeymap,
    /// Ask for a keymap file, and use its keys in place of the user's.
    ImportKeymap,
    /// Ask where to save a copy of the user's keymap file, and save it there.
    ExportKeymap,
    /// Read the keymap file again.
    ReloadKeymap,
    /// Create the keymap file, with no changes in it yet.
    CreateKeymap,
    /// Count dropouts from zero again.
    ResetDropouts,
    /// Turn on or off previewing samples when they're picked in the
    /// browser.
    SetAutoPreview(bool),
    /// Play previews this loud from now on, in dB.
    SetPreviewVolume(i32),
    /// Clear the messages.
    DismissMessages,
    /// Keep the panels' layout as a new workspace with this name, and use
    /// it from now on.
    SaveWorkspaceAs(String),
    /// Give the workspace in use this name.
    RenameWorkspace(String),
    /// Delete the workspace in use, as the user said to.
    DeleteWorkspace,
}

/// What to do with changes that haven't been saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Discard,
    Cancel,
}

/// The shell's own state: its windows and what they show, how it looks,
/// and what the mouse does.
#[derive(Debug)]
pub struct Shell {
    shortcuts: Shortcuts,
    palette: Palette,
    /// The kinds of panel there are.
    registry: panels::Registry,
    panels: Panels,
    /// How the panels are laid out.
    dock: Dock,
    theme: Theme,
    mouse: MouseMap,
    /// The parts of the transport bar, the track headers and the mixer
    /// strips, as the user has them.
    parts: Parts,
    /// What the devices and the piano roll follow, as last seen.
    followed: Option<Followed>,
    /// What's being renamed, and the name typed so far.
    rename: Option<Rename>,
    /// What the box asking about a workspace asks, while it's open.
    asking_workspace: Option<WorkspaceBox>,
    /// Where the main window is, for the workspace.
    main: Placement,
    /// The panel with the focus while the user is in the main window.
    main_focus: &'static str,
    /// Whether the main window had the system's focus, as last seen.
    main_focused: bool,
    /// The windows of their own that the user moved panels out to.
    windows: Vec<Window>,
    /// How many windows of their own have opened, so each new one is a new
    /// viewport.
    opened: u64,
    /// The panels commands asked to move to another window since the last
    /// frame.
    asked: Vec<Move>,
    /// The panel whose tab is dragged out past the edge of its window, and
    /// where the pointer is on the desktop, in pixels, as of the last
    /// frame: the shell shows its ghost there.
    ghost: Option<(&'static str, [i32; 2])>,
}

impl Default for Shell {
    fn default() -> Self {
        let registry = panels::Registry::new();
        Self {
            shortcuts: Shortcuts::default(),
            palette: Palette::default(),
            dock: Dock::new(&registry),
            registry,
            panels: Panels::default(),
            theme: Theme::default(),
            mouse: MouseMap::live(),
            parts: Parts::default(),
            followed: None,
            rename: None,
            asking_workspace: None,
            main: Placement::default(),
            main_focus: ARRANGEMENT,
            main_focused: false,
            windows: Vec::new(),
            opened: 0,
            asked: Vec::new(),
            ghost: None,
        }
    }
}

/// What the box that asks about a workspace asks for.
#[derive(Debug)]
enum WorkspaceBox {
    /// A name for a new workspace, and the name typed so far.
    SaveAs(String),
    /// A new name for the workspace in use: `None` until the box has shown
    /// the current one.
    Rename(Option<String>),
    /// Whether to delete the workspace in use.
    Delete,
}

/// What the tabs of the devices and the piano roll follow: selecting the
/// MIDI clip the piano roll shows brings it forward, and selecting another
/// track the devices.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Followed {
    /// The editor that fits the clip selected or opened last, the clip it
    /// shows, and whether that's selected.
    editor: &'static str,
    clip: Option<ClipId>,
    selected: bool,
    track: Option<Chain>,
}

/// A rename in progress.
#[derive(Debug)]
struct Rename {
    named: Named,
    /// The name typed so far: `None` until the box has shown the current
    /// one.
    text: Option<String>,
}

/// A panel the user moved to another window, and where it goes.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Moving {
    /// To a new window of its own, there.
    Out(&'static str, Place),
    /// Back to where it was in the main window.
    Back(&'static str),
    /// Into the window with that viewport, its tab dragged there, which
    /// put it where it was let go of.
    In(&'static str, egui::ViewportId),
}

impl Shell {
    /// The shell in the dark theme, with Live's mouse.
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows the shell in the window's root `ui`, and the windows of their
    /// own the user moved panels out to, and returns what the user asked
    /// for.
    pub fn show(&mut self, ui: &mut Ui, view: &View<'_>) -> Vec<Action> {
        let mut actions = Vec::new();
        let ctx = ui.ctx().clone();
        // Drags from the browser go from window to window.
        ctx.plugin_or_default::<Carry>();
        let zoom = ctx.zoom_factor();
        let main = ctx.input(|i| i.viewport().clone());
        self.follow(view.editor);
        self.main.follow(&main, zoom, ctx.input(|i| i.time));
        self.remember_focus(view.editor);
        // The user came back to the main window.
        let focused = main.focused == Some(true);
        if focused && !self.main_focused {
            self.focus_main(view.editor, &mut actions);
        }
        self.main_focused = focused;
        for moving in self.asked_moves(&ctx, &main, zoom) {
            self.carry(moving);
        }
        let shown: BTreeSet<_> = (self.registry.iter())
            .map(|panel| panel.id)
            .filter(|id| self.panel_shown(id))
            .collect();
        let focus = view.editor.focus.panel();
        let state = MenuState {
            shown: &shown,
            to_new_window: self.can_move_to_new_window(focus),
            to_main_window: self.window_of(focus).is_some(),
        };
        let theme = &self.theme;
        let parts = &self.parts;
        egui::Panel::top("menu_bar").show(ui, |ui| {
            menu_bar(ui, view, &state, &mut actions);
        });
        egui::Panel::top("transport_bar").show(ui, |ui| {
            transport_bar::show(ui, view, theme, parts, &mut actions);
        });
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            status_bar(ui, view, &mut actions);
        });
        // The devices panel shows the selected track's devices.
        let chain = view.editor.selected_track;
        let drawn = actions.len();
        let mut draw = Draw {
            registry: &self.registry,
            panels: &mut self.panels,
            cx: Cx {
                view,
                theme,
                mouse: &self.mouse,
                parts,
                chain,
                actions: &mut actions,
                browse: false,
            },
            edited: false,
            closed: Vec::new(),
            focused: None,
            moves: Vec::new(),
            detached: false,
            shown: self.dock.count_shown(),
            bounds: ctx.content_rect(),
            ghost: None,
            visited: false,
        };
        let dock = &mut self.dock;
        egui::CentralPanel::no_frame().show(ui, |ui| dock.show(ui, &mut draw));
        if let Some(panel) = draw.focused {
            focus_on(panel, view.editor, drawn, draw.cx.actions);
        }
        let mut browse = draw.cx.browse;
        // A tab dragged out past the edge of the window it's in has its
        // ghost by the pointer, in a window of its own.
        let ghost = draw.ghost;
        self.ghost = ghost.and_then(|(panel, at)| Some((panel, on_desktop(at, &main, zoom)?)));
        let root = egui::ViewportId::ROOT;
        let mut moves = moving(draw.moves, root, &main, self.main.place, zoom);
        self.show_windows(&ctx, view, chain, &mut actions, &mut browse, &mut moves);
        for moving in moves {
            self.carry(moving);
        }
        if let Some((panel, at)) = self.ghost {
            let title = self.registry.get(panel).map_or(panel, |panel| panel.title);
            ghost::show(&ctx, title, at);
        }
        // Add Device shows the browser's devices.
        if browse {
            self.panels.browser.show_devices();
            if !self.panel_shown(BROWSER) {
                self.show_panel(BROWSER);
                ctx.request_repaint();
            }
        }
        self.parts.carry_out();
        // The boxes that take keys go first, so the keys they take while
        // they're open, such as Escape, don't reach the windows under them.
        self.show_rename(&ctx, view, &mut actions);
        self.show_workspace_box(&ctx, view, &mut actions);
        self.palette.show(&ctx, view, &mut actions);
        self.parts.show_customise(&ctx, &self.theme);
        browser::show_dragged(&ctx);
        actions
    }

    /// Shows each window of its own, with its panels, and takes the keys
    /// pressed in it, as the app takes the main window's. Panels moved out
    /// of them go in `moves`, and Add Device clicked in one sets `browse`.
    /// A window the user closed, or that's left with nothing to show,
    /// closes, and its panels go back to the main window.
    fn show_windows(
        &mut self,
        ctx: &egui::Context,
        view: &View<'_>,
        chain: Option<Chain>,
        actions: &mut Vec<Action>,
        browse: &mut bool,
        moves: &mut Vec<Moving>,
    ) {
        let zoom = ctx.zoom_factor();
        // The main window's scaling, a guess at that of the monitor a new
        // window is made on, before it moves where it goes.
        let scale = ctx.input(|i| i.viewport().native_pixels_per_point);
        let scale = scale.unwrap_or(1.0);
        let asking = self.asking();
        let editor = view.editor;
        let Self {
            windows,
            registry,
            panels,
            theme,
            mouse,
            parts,
            shortcuts,
            ghost,
            ..
        } = self;
        let (registry, theme, mouse, parts) = (&*registry, &*theme, &*mouse, &*parts);
        let mut closed = Vec::new();
        for (index, window) in windows.iter_mut().enumerate() {
            let title = title(&window.dock, registry, view.name);
            let builder = window.builder(&title, view.monitors, scale, zoom);
            if std::mem::take(&mut window.raise) {
                ctx.send_viewport_cmd_to(window.id, egui::ViewportCommand::Focus);
            }
            let shown = window.shown();
            let close = ctx.show_viewport_immediate(window.id, builder, |ui, class| {
                let ctx = ui.ctx().clone();
                // An OS window of its own: tests embed it in the main one.
                let own = class == egui::ViewportClass::Immediate;
                let viewport = ctx.input(|i| i.viewport().clone());
                if own {
                    match window.follow_goal(&viewport, zoom) {
                        Some(command) => ctx.send_viewport_cmd(command),
                        None if !window.placing() => {
                            let now = ctx.input(|i| i.time);
                            window.placement.follow(&viewport, zoom, now);
                        }
                        None => {}
                    }
                    // The user came to this window.
                    let focused = viewport.focused == Some(true);
                    if focused && !window.focused {
                        focus_window(window, editor, actions);
                    }
                    window.focused = focused;
                    take_keys(&ctx, window, view, asking, shortcuts, actions);
                }
                let drawn = actions.len();
                let mut draw = Draw {
                    registry,
                    panels: &mut *panels,
                    cx: Cx {
                        view,
                        theme,
                        mouse,
                        parts,
                        chain,
                        actions: &mut *actions,
                        browse: false,
                    },
                    edited: false,
                    closed: Vec::new(),
                    focused: None,
                    moves: Vec::new(),
                    detached: true,
                    shown,
                    bounds: if own {
                        ctx.content_rect()
                    } else {
                        ui.max_rect()
                    },
                    ghost: None,
                    visited: false,
                };
                window.dock.show(ui, &mut draw);
                if let Some(panel) = draw.focused {
                    focus_on(panel, editor, drawn, draw.cx.actions);
                }
                *browse |= draw.cx.browse;
                if let Some((panel, at)) = draw.ghost
                    && let Some(at) = on_desktop(at, &viewport, zoom)
                {
                    *ghost = Some((panel, at));
                }
                let from = window.placement.place;
                moves.extend(moving(draw.moves, window.id, &viewport, from, zoom));
                if own {
                    browser::show_dragged(&ctx);
                }
                own && viewport.close_requested()
            });
            if close || window.shown() == 0 {
                closed.push(index);
            }
        }
        for index in closed.into_iter().rev() {
            self.close_window(index);
        }
    }

    /// Brings the piano roll forward when the clip it shows is selected,
    /// or the Sample Editor when its clip is, and the devices when another
    /// track is, if they're not hidden.
    fn follow(&mut self, editor: &Editor) {
        let panel = editor.clip_editor;
        let clip = match panel {
            SAMPLE_EDITOR => editor.sample_editor.clip,
            _ => editor.piano_roll.clip,
        };
        let now = Followed {
            editor: panel,
            clip,
            selected: clip.is_some_and(|clip| editor.selection.clips.contains(&clip)),
            track: editor.selected_track,
        };
        let Some(last) = self.followed.replace(now) else {
            return;
        };
        let other = now.clip != last.clip || now.editor != last.editor;
        if now.selected && (!last.selected || other) {
            self.bring_forward(panel);
        } else if now.track != last.track {
            self.bring_forward(DEVICES);
        }
    }

    /// Notes which window the panel with the focus is in, so that window
    /// gives it the focus again when the user comes back to it.
    fn remember_focus(&mut self, editor: &Editor) {
        let panel = editor.focus.panel();
        match self.window_of(panel) {
            Some(index) => self.windows[index].focus = panel,
            None => self.main_focus = panel,
        }
    }

    /// The user came back to the main window: the panel that had the focus
    /// there last has it again, if the focus is in another window.
    fn focus_main(&self, editor: &Editor, actions: &mut Vec<Action>) {
        if self.window_of(editor.focus.panel()).is_none() {
            return;
        }
        let panel = Some(self.main_focus)
            .filter(|&panel| self.window_of(panel).is_none() && self.dock.shown(panel))
            .unwrap_or(ARRANGEMENT);
        focus_on(panel, editor, actions.len(), actions);
    }

    /// Shows the panel with id `id`, such as [`MIXER`], if it's hidden or
    /// behind another tab, or else hides it. A hidden panel comes back
    /// where it was.
    pub fn toggle_panel(&mut self, id: &str) {
        if self.panel_shown(id) {
            self.hide_panel(id);
        } else {
            self.show_panel(id);
        }
    }

    /// Shows the panel with id `id`, in front of the tabs beside it, in the
    /// window it's in, which comes to the front.
    pub fn show_panel(&mut self, id: &str) {
        match self.window_of(id) {
            Some(index) => {
                let window = &mut self.windows[index];
                window.dock.show_panel(id);
                window.raise = true;
            }
            None => self.dock.show_panel(id),
        }
    }

    /// Hides the panel with id `id`, unless it's one that stays, such as
    /// the arrangement. A window of its own left with nothing to show
    /// closes.
    pub fn hide_panel(&mut self, id: &str) {
        if !self.registry.get(id).is_some_and(|panel| panel.hides) {
            return;
        }
        match self.window_of(id) {
            Some(index) => {
                self.windows[index].dock.hide_panel(id);
                if self.windows[index].shown() == 0 {
                    self.close_window(index);
                }
            }
            None => self.dock.hide_panel(id),
        }
    }

    /// Puts the panel with id `id` in front of the tabs beside it, if it
    /// isn't hidden.
    fn bring_forward(&mut self, id: &str) {
        match self.window_of(id) {
            Some(index) => self.windows[index].dock.bring_forward(id),
            None => self.dock.bring_forward(id),
        }
    }

    /// Brings the devices forward, unless they're hidden, scrolled to
    /// `device`, which was just added, in case it went behind the piano
    /// roll or past the end of the panel. The window they're in stays
    /// where it is, so the keys stay where they were, such as in the
    /// browser.
    pub fn show_device(&mut self, device: DeviceId) {
        self.bring_forward(DEVICES);
        self.panels.devices.reveal(device);
    }

    /// Brings the clip editor with id `editor`, the piano roll or the
    /// Sample Editor, forward, or the devices if it's in front, as Live's
    /// F12 switches between its clip view and its device view. Either comes
    /// back if it's hidden.
    pub fn switch_devices_clip_editor(&mut self, editor: &str) {
        if self.panel_shown(editor) {
            self.show_panel(DEVICES);
        } else {
            self.show_panel(editor);
        }
    }

    /// Whether the panel with id `id` is in sight, in whichever window it's
    /// in: not hidden, nor behind another tab.
    pub fn panel_shown(&self, id: &str) -> bool {
        match self.window_of(id) {
            Some(index) => self.windows[index].dock.shown(id),
            None => self.dock.shown(id),
        }
    }

    /// Shows the browser, and puts the keys in the search field of the list
    /// it shows.
    pub fn focus_browser_search(&mut self) {
        self.show_panel(BROWSER);
        self.panels.browser.focus_search();
    }

    /// The window of its own the panel with id `id` is in, if it's not in
    /// the main window.
    fn window_of(&self, id: &str) -> Option<usize> {
        (self.windows.iter()).position(|window| window.dock.has(id))
    }

    /// The viewport of the window the panel with id `id` is in: the main
    /// window's, [`egui::ViewportId::ROOT`], unless the user moved it to a
    /// window of its own. Plugin windows open from the devices there.
    pub fn viewport_of(&self, id: &str) -> egui::ViewportId {
        let window = self.window_of(id);
        window.map_or(egui::ViewportId::ROOT, |index| self.windows[index].id)
    }

    /// How many windows of their own are open.
    pub fn windows(&self) -> usize {
        self.windows.len()
    }

    /// Moves the panel with id `id` to a new window of its own, a little
    /// down and right of the window it's in, as big as it is there, from
    /// the next frame on. Returns why it can't, if it can't: the arrangement
    /// stays in the main window, and the last panel in a window of its own
    /// has it to itself already.
    pub fn move_to_new_window(&mut self, id: &str) -> Result<(), String> {
        let panel = self
            .registry
            .get(id)
            .ok_or_else(|| format!("There's no panel {id}."))?;
        let title = panel.title;
        if !panel.hides {
            return Err(format!("{title} stays in the main window."));
        }
        if !self.can_move_to_new_window(id) {
            return Err(format!("{title} has its window to itself already."));
        }
        let asked = Move::ToNewWindow {
            panel: panel.id,
            at: None,
            size: None,
        };
        self.asked.push(asked);
        Ok(())
    }

    /// Sends the panel with id `id` back to where it was in the main
    /// window, from the next frame on. Returns why it can't, if it's in the
    /// main window already.
    pub fn move_to_main_window(&mut self, id: &str) -> Result<(), String> {
        let panel = self
            .registry
            .get(id)
            .ok_or_else(|| format!("There's no panel {id}."))?;
        if self.window_of(id).is_none() {
            return Err(format!("{} is in the main window already.", panel.title));
        }
        self.asked.push(Move::ToMainWindow(panel.id));
        Ok(())
    }

    /// Whether the panel with id `id` can move to a new window: not the
    /// arrangement, which stays in the main window, nor the last panel of
    /// a window of its own.
    pub fn can_move_to_new_window(&self, id: &str) -> bool {
        let hides = self.registry.get(id).is_some_and(|panel| panel.hides);
        let last = (self.window_of(id)).is_some_and(|index| self.windows[index].shown() <= 1);
        hides && !last
    }

    /// The moves commands asked for since the last frame, and where the
    /// panels go.
    fn asked_moves(
        &mut self,
        ctx: &egui::Context,
        main: &egui::ViewportInfo,
        zoom: f32,
    ) -> Vec<Moving> {
        let mut moves = Vec::new();
        for asked in std::mem::take(&mut self.asked) {
            let panel = match asked {
                Move::ToNewWindow { panel, .. } | Move::ToMainWindow(panel) | Move::Into(panel) => {
                    panel
                }
            };
            let (viewport, from, dock) = match self.window_of(panel) {
                Some(index) => {
                    let window = &self.windows[index];
                    let viewport = ctx.input_for(window.id, |i| i.viewport().clone());
                    (viewport, window.place(), &window.dock)
                }
                None => (main.clone(), self.main.place, &self.dock),
            };
            let asked = match asked {
                Move::ToNewWindow { panel, at, .. } => Move::ToNewWindow {
                    panel,
                    at,
                    size: dock.panel_size(panel, &self.theme),
                },
                back => back,
            };
            let here = self.viewport_of(panel);
            moves.extend(moving(vec![asked], here, &viewport, from, zoom));
        }
        moves
    }

    /// Carries out a move of a panel to another window.
    fn carry(&mut self, moving: Moving) {
        match moving {
            Moving::Out(panel, place) => self.open_window(panel, place),
            Moving::Back(panel) => self.send_back(panel),
            Moving::In(panel, into) => self.take_in(panel, into),
        }
    }

    /// Moves the panel `panel` out of the window it's in to a new window of
    /// its own at `place`. Out of the main window, it keeps its place there,
    /// hidden, to go back to.
    fn open_window(&mut self, panel: &'static str, place: Place) {
        if !self.can_move_to_new_window(panel) {
            return;
        }
        match self.window_of(panel) {
            Some(index) => self.windows[index].dock.remove_panel(panel),
            None => self.dock.hide_panel(panel),
        }
        self.opened += 1;
        let id = egui::ViewportId::from_hash_of(("panels", self.opened));
        let window = Window::new(id, Dock::of_panel(panel), place, panel);
        self.windows.push(window);
        self.dock.mark_changed();
    }

    /// Sends the panel `panel` back to where it was in the main window, out
    /// of the window of its own it's in, shown if it was shown there.
    fn send_back(&mut self, panel: &'static str) {
        let Some(index) = self.window_of(panel) else {
            return;
        };
        let dock = &mut self.windows[index].dock;
        let shown = (dock.panels().iter()).any(|&(other, shown)| other == panel && shown);
        dock.remove_panel(panel);
        if shown {
            self.dock.show_panel(panel);
        }
        if self.windows[index].shown() == 0 {
            self.close_window(index);
        }
        self.dock.mark_changed();
    }

    /// Takes the panel `panel` out of the window it was in, now that its tab
    /// was dragged into the window with viewport `into`, which put it where
    /// it was let go of. Into a window of its own from the main window, it
    /// keeps its place there, hidden, to go back to. The window it was in
    /// closes if it has nothing else to show.
    fn take_in(&mut self, panel: &'static str, into: egui::ViewportId) {
        let from =
            (self.windows.iter()).position(|window| window.id != into && window.dock.has(panel));
        if let Some(index) = from {
            self.windows[index].dock.remove_panel(panel);
            if self.windows[index].shown() == 0 {
                self.close_window(index);
            }
        }
        if into != egui::ViewportId::ROOT {
            self.dock.hide_panel(panel);
        }
        self.dock.mark_changed();
    }

    /// Closes the window of its own `index`, and sends its panels back to
    /// where they were in the main window: those it showed are shown there,
    /// and those it hid stay hidden.
    fn close_window(&mut self, index: usize) {
        let window = self.windows.remove(index);
        for (panel, shown) in window.dock.panels() {
            if shown {
                self.dock.show_panel(panel);
            }
        }
        self.dock.mark_changed();
    }

    /// How the panels are laid out, where the windows are, the main one
    /// first, and the parts of the transport bar, the track headers and the
    /// mixer strips, for the app to save. The app gives it its slot.
    pub fn workspace(&self) -> Workspace {
        let mut main = self.dock.layout(&self.registry, &self.theme);
        self.main.place.put_in(&mut main);
        let mut windows = vec![main];
        for window in &self.windows {
            let mut layout = window.dock.layout(&self.registry, &self.theme);
            window.place().put_in(&mut layout);
            windows.push(layout);
        }
        Workspace {
            slot: None,
            views: self.parts.views(),
            windows,
        }
    }

    /// The workspace, if the user changed the layout or the views' parts,
    /// or moved a window, since it was last handed out, for the app to
    /// save. A window being moved or resized is saved once it has stayed
    /// put for a second.
    pub fn changed_workspace(&mut self) -> Option<Workspace> {
        self.hand_out(false)
    }

    /// The same, without waiting for the windows to stay put: before the
    /// app switches to another workspace, or closes.
    pub fn unsaved_workspace(&mut self) -> Option<Workspace> {
        self.hand_out(true)
    }

    /// The workspace, if it changed, waiting for windows the user moves to
    /// stay put unless `now`.
    fn hand_out(&mut self, now: bool) -> Option<Workspace> {
        // Each is asked, so none tells of the same change again.
        let mut changed = self.dock.take_changed() | self.parts.take_changed();
        for window in &mut self.windows {
            changed |= window.dock.take_changed();
        }
        let moved =
            self.main.moved(now) || (self.windows.iter()).any(|window| window.placement.moved(now));
        if !changed && !moved {
            return None;
        }
        self.main.saved();
        for window in &mut self.windows {
            window.placement.saved();
        }
        Some(self.workspace())
    }

    /// Lays the panels out and shows the views' parts as `workspace` says,
    /// in the windows it has, and takes the main window's place from it,
    /// which the app moves the window to. The panels in windows of their
    /// own keep their places in the main window, hidden, to go back to.
    pub fn set_workspace(&mut self, workspace: &Workspace) {
        let main = workspace.windows.first();
        self.dock = match main {
            Some(window) => Dock::from_layout(window, &self.registry, &self.theme),
            None => Dock::new(&self.registry),
        };
        self.parts.set_views(&workspace.views);
        match main.filter(|window| window.at.is_some() || window.size.is_some()) {
            Some(window) => self.main.set(Place::of(window)),
            None => self.main.forget(),
        }
        self.windows.clear();
        let mut placed = Vec::new();
        for layout in workspace.windows.iter().skip(1) {
            let registry = &self.registry;
            let Some(dock) = Dock::of_window(layout, registry, &self.theme, &mut placed) else {
                continue;
            };
            let panels = dock.panels();
            for &(panel, _) in &panels {
                self.dock.hide_panel(panel);
            }
            let shown = panels.iter().find(|(_, shown)| *shown);
            let focus = shown.or(panels.first()).map_or(MIXER, |&(panel, _)| panel);
            self.opened += 1;
            let id = egui::ViewportId::from_hash_of(("panels", self.opened));
            self.windows
                .push(Window::new(id, dock, Place::of(layout), focus));
        }
        // Laid out as it was saved, there's nothing to save.
        self.dock.take_changed();
    }

    /// Lays the panels out as at first, with all of them in the main window,
    /// which stays where it is, shows the views' parts as at first, and
    /// saves that as the workspace in use.
    pub fn reset_workspace(&mut self) {
        self.dock = Dock::new(&self.registry);
        self.windows.clear();
        self.dock.mark_changed();
        self.parts.set_views(&Views::default());
    }

    /// Asks for a name to keep the panels' layout and the views' parts
    /// under, as a new workspace.
    pub fn save_workspace_as(&mut self) {
        self.asking_workspace = Some(WorkspaceBox::SaveAs(String::new()));
    }

    /// Asks for a new name for the workspace in use.
    pub fn rename_workspace(&mut self) {
        self.asking_workspace = Some(WorkspaceBox::Rename(None));
    }

    /// Asks whether to delete the workspace in use.
    pub fn delete_workspace(&mut self) {
        self.asking_workspace = Some(WorkspaceBox::Delete);
    }

    /// Asks the user for a new name for `named`.
    pub fn rename(&mut self, named: Named) {
        self.rename = Some(Rename { named, text: None });
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Draws with `theme` from now on.
    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
    }

    pub fn mouse_map(&self) -> &MouseMap {
        &self.mouse
    }

    /// Does what `mouse` says mouse gestures do from now on.
    pub fn set_mouse_map(&mut self, mouse: MouseMap) {
        self.mouse = mouse;
    }

    /// The box that renames a track, a clip, a locator or a take, while
    /// one is renamed. Enter runs the command that renames it.
    fn show_rename(&mut self, ctx: &egui::Context, view: &View<'_>, actions: &mut Vec<Action>) {
        let Some(rename) = &mut self.rename else {
            return;
        };
        let project = view.project;
        let (title, current, command, arg, value): (_, _, _, _, Value) = match rename.named {
            Named::Track(id) => (
                "Rename Track",
                project.track(id).map(|t| t.name.clone()),
                track::RENAME,
                "track",
                id.into(),
            ),
            Named::Clip(id) => (
                "Rename Clip",
                project.find_clip(id).map(|(_, c)| c.name.clone()),
                arrange::RENAME,
                "clip",
                id.into(),
            ),
            Named::Locator(id) => (
                "Rename Locator",
                project.locator(id).map(|l| l.name.clone()),
                locator::RENAME,
                "locator",
                id.into(),
            ),
            Named::Take(id) => (
                "Rename Take",
                project.find_take(id).map(|(_, t)| t.name.clone()),
                take::RENAME,
                "take",
                id.into(),
            ),
        };
        // It went while the box was open.
        let Some(current) = current else {
            self.rename = None;
            return;
        };
        let first = rename.text.is_none();
        let text = rename.text.get_or_insert_with(|| current.clone());
        let Some(confirmed) = name_box(ctx, "rename", title, "Rename", text, first) else {
            return;
        };
        let name = text.trim().to_owned();
        if confirmed && !name.is_empty() && name != current {
            let args = Args::new().with(arg, value).with("name", name);
            actions.push(Action::RunWith(command, args));
        }
        self.rename = None;
    }

    /// The box that asks for a workspace's name, or whether to delete it,
    /// while it's open.
    fn show_workspace_box(
        &mut self,
        ctx: &egui::Context,
        view: &View<'_>,
        actions: &mut Vec<Action>,
    ) {
        let Some(asking) = &mut self.asking_workspace else {
            return;
        };
        let workspaces = view.workspaces;
        let current = workspaces.current_name();
        // A name the others don't have.
        let others = || workspaces.names().filter(|&name| name != current);
        let done = match asking {
            WorkspaceBox::SaveAs(text) => {
                let full = workspaces.names().count() >= usize::from(workspace::SLOTS);
                let check = |text: &str| match full {
                    true => Err(format!(
                        "There's room for {} workspaces: delete one first.",
                        workspace::SLOTS
                    )),
                    false => check_name(text, workspaces.names()),
                };
                let title = "Save Workspace As";
                let problem = |text: &str| check(text).err();
                checked_name_box(ctx, "workspace name", title, "Save", text, false, problem).map(
                    |confirmed| {
                        if let (true, Ok(name)) = (confirmed, check(text)) {
                            actions.push(Action::SaveWorkspaceAs(name));
                        }
                    },
                )
            }
            WorkspaceBox::Rename(text) => {
                let first = text.is_none();
                let text = text.get_or_insert_with(|| current.to_owned());
                let title = format!("Rename {current}");
                let problem = |text: &str| check_name(text, others()).err();
                checked_name_box(
                    ctx,
                    "workspace name",
                    &title,
                    "Rename",
                    text,
                    first,
                    problem,
                )
                .map(|confirmed| match (confirmed, check_name(text, others())) {
                    (true, Ok(name)) if name != current => {
                        actions.push(Action::RenameWorkspace(name));
                    }
                    _ => {}
                })
            }
            WorkspaceBox::Delete => confirm_box(
                ctx,
                &format!("Delete the workspace {current}?"),
                &format!("Its layout is lost, and the window switches to {DEFAULT_WORKSPACE}."),
                "Delete",
            )
            .map(|confirmed| {
                if confirmed {
                    actions.push(Action::DeleteWorkspace);
                }
            }),
        };
        if done.is_some() {
            self.asking_workspace = None;
        }
    }

    /// Shows the command palette, which finds any command and runs it.
    pub fn open_palette(&mut self) {
        self.shortcuts.cancel();
        self.palette.open();
    }

    /// Draws the Settings window's Keys page in `ui`: every command's keys,
    /// to change, and the user's keymap file. The app draws it in each frame
    /// the page is in sight, and calls [`hide_keys`](Self::hide_keys) in
    /// those it isn't.
    pub fn show_keys(&mut self, ui: &mut Ui, view: &View<'_>) -> Vec<Action> {
        let mut actions = Vec::new();
        (self.shortcuts).show(ui, view, &self.registry, &mut actions);
        actions
    }

    /// The Keys page is out of sight: it stops giving a command keys.
    pub fn hide_keys(&mut self) {
        self.shortcuts.hide();
    }

    /// Whether the Keys page is giving a command keys, which Escape and a
    /// click outside the window stop, rather than closing it.
    pub fn giving_keys(&self) -> bool {
        self.shortcuts.editing()
    }

    /// Stops giving a command keys.
    pub fn stop_giving_keys(&mut self) {
        self.shortcuts.cancel();
    }

    /// Whether the Keys page waits for the keys to give a command. The next
    /// chord pressed goes to [`captured`](Self::captured) then, not to the
    /// keymap: [`KeyPresses::capture`](crate::KeyPresses::capture) takes it.
    pub fn capturing(&self) -> bool {
        self.shortcuts.capturing()
    }

    /// The chord pressed while the Keys page waited for one.
    pub fn captured(&mut self, chord: KeyChord) {
        self.shortcuts.captured(chord);
    }

    /// Whether the shell asks something that keys answer, or has the command
    /// palette open. Keys don't run commands while it does.
    pub fn asking(&self) -> bool {
        self.palette.is_open()
            || self.shortcuts.asking()
            || self.rename.is_some()
            || self.asking_workspace.is_some()
            || self.panels.devices.asking()
    }
}

/// What the menus tick and grey out that the shell knows.
struct MenuState<'a> {
    /// The ids of the panels in sight.
    shown: &'a BTreeSet<&'static str>,
    /// Whether the panel with the focus can move to a new window.
    to_new_window: bool,
    /// Whether it's in a window of its own, to send back to the main one.
    to_main_window: bool,
}

/// The menus, with every command and its keys.
fn menu_bar(ui: &mut Ui, view: &View<'_>, state: &MenuState<'_>, actions: &mut Vec<Action>) {
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in menus(view.commands) {
            ui.menu_button(menu.title.as_str(), |ui| {
                for item in &menu.items {
                    match item {
                        Some(command) => command_button(ui, view, state, command, actions),
                        None => {
                            ui.separator();
                        }
                    }
                }
            });
        }
    });
}

/// A menu entry for a command, showing its keys, ticked if it's a toggle
/// that's on, and greyed out when it doesn't apply. Undo and Redo say what
/// they'd change.
fn command_button(
    ui: &mut Ui,
    view: &View<'_>,
    state: &MenuState<'_>,
    command: &Command,
    actions: &mut Vec<Action>,
) {
    let id = &command.id;
    let step = match id {
        id if *id == edit::UNDO => Some(view.history.undo_name()),
        id if *id == edit::REDO => Some(view.history.redo_name()),
        _ => None,
    };
    let workspaces = view.workspaces;
    let slot = workspace::slot_of(id).map(|slot| usize::from(slot) - 1);
    let (title, enabled) = match (step, slot) {
        (Some(Some(name)), _) => (format!("{} {name}", command.title), true),
        (Some(None), _) => (command.title.clone(), false),
        // Each slot with a workspace shows its name.
        (None, Some(slot)) => match workspaces.name(slot) {
            Some(name) => (name.to_owned(), true),
            None => return,
        },
        // Default keeps its name and can't be deleted.
        (None, None) if *id == workspace::RENAME || *id == workspace::DELETE => {
            (command.title.clone(), workspaces.current != 0)
        }
        (None, None) if *id == view::MOVE_TO_NEW_WINDOW => {
            (command.title.clone(), state.to_new_window)
        }
        (None, None) if *id == view::MOVE_TO_MAIN_WINDOW => {
            (command.title.clone(), state.to_main_window)
        }
        (None, None) => (
            command.title.clone(),
            command.when.applies(view.project, view.editor),
        ),
    };
    let mut button = with_keys(Button::new(title), view, id);
    let shown = state.shown;
    let on = match id {
        id if *id == view::TOGGLE_MIXER => Some(shown.contains(MIXER)),
        id if *id == view::TOGGLE_DEVICES => Some(shown.contains(DEVICES)),
        id if *id == view::TOGGLE_PIANO_ROLL => Some(shown.contains(PIANO_ROLL)),
        id if *id == view::TOGGLE_SAMPLE_EDITOR => Some(shown.contains(SAMPLE_EDITOR)),
        id if *id == view::TOGGLE_CLIP_EDITOR => Some(shown.contains(view.editor.clip_editor)),
        id if *id == view::TOGGLE_BROWSER => Some(shown.contains(BROWSER)),
        id if *id == view::TOGGLE_HISTORY => Some(shown.contains(HISTORY)),
        _ if slot.is_some() => slot.map(|slot| slot == workspaces.current),
        id => toggled(view, id),
    };
    if let Some(on) = on {
        button = button.selected(on);
    }
    if ui.add_enabled(enabled, button).clicked() {
        actions.push(Action::Run(id.clone()));
    }
}

/// Whether `focus` can have the focus: not the piano roll or the Sample
/// Editor with no clip to show.
fn can_focus(focus: Focus, editor: &Editor) -> bool {
    match focus {
        Focus::PIANO_ROLL => editor.piano_roll.clip.is_some(),
        Focus::SAMPLE_EDITOR => editor.sample_editor.clip.is_some(),
        _ => true,
    }
}

/// Gives the panel `id` the focus, before what the panels asked for from
/// `drawn` on, unless it has the focus, or asked for it, or is the piano
/// roll or the Sample Editor with no clip to show. What a click does in a
/// panel can move the focus on, as opening a clip in the piano roll does.
fn focus_on(id: &'static str, editor: &Editor, drawn: usize, actions: &mut Vec<Action>) {
    let Some(focus) = Focus::on(id).filter(|&focus| focus != editor.focus) else {
        return;
    };
    if !can_focus(focus, editor) {
        return;
    }
    let asked = (actions[drawn..].iter())
        .any(|action| matches!(action, Action::RunWith(id, _) if *id == view::SET_FOCUS));
    if !asked {
        let args = Args::new().with("panel", id);
        actions.insert(drawn, Action::RunWith(view::SET_FOCUS, args));
    }
}

/// Where the panels `moves` moved out of the window egui describes as
/// `viewport` go, or into it, the window being `here` and at `from`, and
/// egui zooming by `zoom`. A tab dragged out goes where it was let go of,
/// with the new window's title bar under the pointer, and a panel moved
/// from its tab's menu goes a little down and right of the window. Either
/// is as big as it was.
fn moving(
    moves: Vec<Move>,
    here: egui::ViewportId,
    viewport: &egui::ViewportInfo,
    from: Place,
    zoom: f32,
) -> Vec<Moving> {
    // Title bars and the like are in pixels the monitor scales.
    let scale = viewport.native_pixels_per_point.unwrap_or(1.0);
    let pixels = |points: f32| (points * scale).round() as i32;
    (moves.into_iter())
        .map(|asked| match asked {
            Move::ToNewWindow { panel, at, size } => {
                let dropped = (at.and_then(|at| on_desktop(at, viewport, zoom)))
                    .map(|[x, y]| [x - pixels(60.0), y - pixels(12.0)]);
                let beside = (from.at).map(|[x, y]| [x + pixels(40.0), y + pixels(40.0)]);
                let size = size.map(|size| unscaled(size.max(egui::vec2(320.0, 200.0)), zoom));
                let place = Place {
                    at: dropped.or(beside),
                    size,
                    maximized: false,
                };
                Moving::Out(panel, place)
            }
            Move::ToMainWindow(panel) => Moving::Back(panel),
            Move::Into(panel) => Moving::In(panel, here),
        })
        .collect()
}

/// The title of a window of its own: the panels it shows, then the
/// project's name `name`.
fn title(dock: &Dock, registry: &panels::Registry, name: &str) -> String {
    let panels: Vec<_> = (dock.panels().into_iter())
        .filter(|&(_, shown)| shown)
        .filter_map(|(id, _)| registry.get(id).map(|panel| panel.title))
        .collect();
    format!("{} - {name} - Musicable", panels.join(", "))
}

/// Where keys pressed in `window` go: to the panel with the focus if it's
/// there, or else to the one that gets it as the user comes to the window,
/// the one that had it last if it's in sight, or the first in sight. `None`
/// when none can take it, as the piano roll and the Sample Editor can't with
/// no clip, so only keys that work everywhere work.
fn window_focus(window: &Window, editor: &Editor) -> Option<Focus> {
    if window.dock.has(editor.focus.panel()) {
        return Some(editor.focus);
    }
    let in_sight = |panel: &&'static str| window.dock.shown(panel);
    let mut panels = (window.dock.panels().into_iter()).map(|(panel, _)| panel);
    let panel = (Some(window.focus).filter(in_sight)).or_else(|| panels.find(in_sight))?;
    Focus::on(panel).filter(|&focus| can_focus(focus, editor))
}

/// The user came to `window`: the panel that had the focus there last has
/// it again, unless the focus is there already.
fn focus_window(window: &Window, editor: &Editor, actions: &mut Vec<Action>) {
    if let Some(focus) = window_focus(window, editor) {
        focus_on(focus.panel(), editor, actions.len(), actions);
    }
}

/// Hands the keys pressed in `window` to the keymap before its panels see
/// them, as the app hands it the main window's: a chord bound in the panel
/// the keys go to, or everywhere, runs its command, unless a text field
/// has the focus or the shell is `asking` something. While the Settings
/// window's Keys page waits for keys, they go to it instead.
fn take_keys(
    ctx: &egui::Context,
    window: &mut Window,
    view: &View<'_>,
    asking: bool,
    shortcuts: &mut Shortcuts,
    actions: &mut Vec<Action>,
) {
    let mut events = ctx.input_mut(|i| std::mem::take(&mut i.events));
    if shortcuts.capturing() {
        if let Some(chord) = window.keys.capture_events(&mut events) {
            shortcuts.captured(chord);
        }
    } else if !asking {
        let editor = view.editor;
        let focus = window_focus(window, editor);
        // The command runs once the focus is there.
        let moved = focus
            .filter(|&focus| focus != editor.focus)
            .map(|focus| Editor {
                focus,
                ..editor.clone()
            });
        let there = moved.as_ref().unwrap_or(editor);
        let text_field = ctx.text_edit_focused();
        let took = window
            .keys
            .take_events(ctx, &mut events, text_field, |chord, repeat| {
                let found = match focus {
                    Some(focus) => view.keymap.lookup_focused(chord, focus),
                    None => view.keymap.lookup(chord),
                };
                let applies = |id: &CommandId| {
                    (view.commands.get(id))
                        .is_some_and(|command| command.when.applies(view.project, there))
                };
                let Some(id) = found.filter(|id| applies(id)) else {
                    return false;
                };
                if !repeat {
                    actions.push(Action::Run(id.clone()));
                }
                true
            });
        // A key taken doesn't move the keyboard focus, as egui would have
        // once the pass began.
        if took {
            ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
    }
    ctx.input_mut(|i| i.events = events);
}

/// Whether the command `id` is a toggle that's on, such as Loop or Snap to
/// Grid, for menus to show. `None` for commands that aren't toggles, and for
/// the panels, which the shell keeps.
pub(crate) fn toggled(view: &View<'_>, id: &CommandId) -> Option<bool> {
    let project = view.project;
    let editor = view.editor;
    let toggles = [
        (transport::RECORD, view.playback.record),
        (transport::TOGGLE_PUNCH_IN, project.punch_in),
        (transport::TOGGLE_PUNCH_OUT, project.punch_out),
        (transport::TOGGLE_LOOP, project.looping),
        (transport::TOGGLE_METRONOME, project.metronome),
        (view::TOGGLE_FOLLOW, editor.follow),
        (view::TOGGLE_SNAP, editor.grid.snap),
        (view::TRIPLET_GRID, editor.grid.triplet),
        (view::FIXED_GRID, editor.grid.fixed.is_some()),
        (notes::TOGGLE_DRAW, editor.piano_roll.draw),
        (
            controllers::SHOW_VELOCITIES,
            editor.piano_roll.lane == Lane::Velocity,
        ),
    ];
    toggles
        .into_iter()
        .find(|(toggle, _)| toggle == id)
        .map(|(_, on)| on)
}

fn status_bar(ui: &mut Ui, view: &View<'_>, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        match view.messages {
            [] => {
                ui.label(RichText::new("Ready").weak());
            }
            [.., last] => {
                if ui.small_button("\u{d7}").on_hover_text("Dismiss").clicked() {
                    actions.push(Action::DismissMessages);
                }
                let text = match view.messages.len() {
                    1 => last.clone(),
                    n => format!("{last} (and {} more)", n - 1),
                };
                ui.label(text).on_hover_text(view.messages.join("\n"));
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let name = if view.modified {
                format!("{} (not saved)", view.name)
            } else {
                view.name.to_owned()
            };
            ui.label(RichText::new(name).weak());
            ui.separator();
            let summary = RichText::new(view.audio.summary);
            let summary = if view.audio.running {
                summary
            } else {
                summary.color(ui.visuals().warn_fg_color)
            };
            let device = ui
                .add(Button::new(summary).frame(false))
                .on_hover_text("Audio settings");
            if device.clicked() {
                actions.push(Action::Run(options::AUDIO_SETTINGS));
            }
            if let Some(agent) = view.agent {
                ui.separator();
                let agent = ui
                    .add(Button::new(agent).frame(false))
                    .on_hover_text("An agent controls Musicable: see the Agents settings");
                if agent.clicked() {
                    actions.push(Action::Run(options::AGENTS));
                }
            }
        });
    });
}

/// Asks whether to save the changes to the project called `name` before
/// they're lost. Call it each frame until it returns a choice.
pub fn ask_to_save(ctx: &egui::Context, name: &str) -> Option<SaveChoice> {
    let modal = egui::Modal::new(egui::Id::unique("ask_to_save")).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.heading(format!("Save the changes to {name}?"));
        ui.label("Your changes are lost if you don't save them.");
        ui.add_space(10.0);
        let mut choice = None;
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                choice = Some(SaveChoice::Save);
            }
            if ui.button("Don't Save").clicked() {
                choice = Some(SaveChoice::Discard);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(SaveChoice::Cancel);
            }
        });
        choice
    });
    if modal.inner.is_none() && modal.should_close() {
        return Some(SaveChoice::Cancel);
    }
    modal.inner
}

/// The chords that run `id` in the panel with the focus, as keymap files
/// write them: the panel's own, and those that work everywhere that the
/// panel doesn't take for another command.
pub(crate) fn chords(view: &View<'_>, id: &CommandId) -> String {
    let chords = view.keymap.chords_focused(id, view.editor.focus);
    let chords: Vec<_> = chords.iter().map(KeyChord::to_string).collect();
    chords.join(", ")
}

/// `button` with the chords that run `id` in the panel with the focus at
/// its right, if any do.
pub(crate) fn with_keys<'a>(button: Button<'a>, view: &View<'_>, id: &CommandId) -> Button<'a> {
    match chords(view, id) {
        keys if keys.is_empty() => button,
        keys => button.shortcut_text(keys),
    }
}

#[cfg(test)]
mod tests {
    use egui::accesskit::{Role, Toggled};
    use egui::vec2;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{Scope, file, track, view};
    use musicable_model::native::NativeDevice;
    use musicable_model::{DeviceKind, TrackId, TrackKind};

    use super::*;
    use crate::browser::Dragged;
    use crate::panels::ARRANGEMENT;
    use crate::testing::{
        State, click_at, focus_main, harness, key, own_windows, own_windows_harness, with_own,
    };
    use crate::windows::SETTLE;
    use crate::workspace::{Layout, Split, WindowLayout};

    type Harness = egui_kittest::Harness<'static, State>;

    /// Drags with the mouse from `from` to `to`, a few steps at a time.
    fn drag(harness: &mut Harness, from: egui::Pos2, to: egui::Pos2) {
        harness.hover_at(from);
        harness.run();
        harness.drag_at(from);
        harness.run();
        for step in 1..=5 {
            harness.hover_at(from + (to - from) * step as f32 / 5.0);
            harness.run();
        }
        harness.drop_at(to);
        harness.run();
    }

    /// The main window's layout.
    fn layout(harness: &Harness) -> WindowLayout {
        let mut workspace = harness.state().shell.workspace();
        workspace.windows.remove(0)
    }

    fn tabs(ids: &[&str], front: &str) -> Layout {
        Layout {
            front: Some(front.to_owned()),
            ..Layout::new(Split::Tabs(ids.iter().map(|&id| id.to_owned()).collect()))
        }
    }

    fn across(parts: Vec<Layout>) -> Layout {
        Layout::new(Split::Across(parts))
    }

    fn down(parts: Vec<Layout>) -> Layout {
        Layout::new(Split::Down(parts))
    }

    #[test]
    fn the_transport_bar_runs_commands() {
        let mut harness = harness(Project::new());
        let labels = [
            "Go to Start",
            "Play",
            "Stop",
            "Record",
            "Capture",
            "Loop",
            "Punch In",
            "Punch Out",
            "Metronome",
            "+",
        ];
        for label in labels {
            harness.get_by_label(label).click();
            harness.run();
        }
        let ran = [
            transport::GO_TO_START,
            transport::PLAY,
            transport::STOP,
            transport::RECORD,
            transport::CAPTURE,
            transport::TOGGLE_LOOP,
            transport::TOGGLE_PUNCH_IN,
            transport::TOGGLE_PUNCH_OUT,
            transport::TOGGLE_METRONOME,
            transport::TEMPO_UP,
        ];
        let expected: Vec<_> = ran.into_iter().map(Action::Run).collect();
        assert_eq!(harness.state().actions, expected);
    }

    #[test]
    fn menus_list_commands_with_their_keys() {
        let mut harness = harness(Project::new());
        harness.get_by_label("File").click();
        harness.run();
        // Each entry shows its keys.
        let labels: Vec<_> = harness
            .get_all_by_label_contains("Save Project")
            .filter_map(|node| node.accesskit_node().label())
            .collect();
        assert_eq!(
            labels,
            ["Save Project Ctrl+S", "Save Project As... Ctrl+Shift+S"]
        );
        harness.get_by_label("Save Project Ctrl+S").click();
        harness.run();
        assert_eq!(harness.state().actions, [Action::Run(file::SAVE)]);
    }

    #[test]
    fn menus_show_the_keys_that_work_where_the_focus_is() {
        let mut harness = harness(Project::new());
        // In the mixer, Ctrl+S plays instead of saving, and F5 saves.
        let keymap = &mut harness.state_mut().keymap;
        let mixer = Scope::Panel(MIXER);
        keymap.bind_in(mixer, "Ctrl+S".parse().unwrap(), transport::PLAY);
        keymap.bind_in(mixer, "F5".parse().unwrap(), file::SAVE);
        harness.get_by_label("File").click();
        harness.run();
        harness.get_by_label("Save Project Ctrl+S");
        harness.state_mut().editor.focus = Focus::MIXER;
        harness.run();
        harness.get_by_label("Save Project F5");
    }

    #[test]
    fn clicking_in_a_panel_gives_it_the_focus() {
        let mut project = Project::new();
        project.add_track("Drums", TrackKind::Audio);
        let mut harness = harness(project);
        let focus = |harness: &Harness| harness.state().editor.focus;
        // Under the mixer's tab, in the mixer.
        let tab = harness.get_by_label("Mixer panel").rect();
        click_at(&mut harness, tab.left_bottom() + vec2(60.0, 80.0));
        assert_eq!(focus(&harness), Focus::MIXER);
        assert_eq!(harness.state().actions, []);

        // Not through a window over the panels, though.
        harness.state_mut().keys_page = true;
        harness.run();
        let window = harness.get_by_label("Keyboard Shortcuts").rect();
        let at = window.center_top() + vec2(0.0, 40.0);
        click_at(&mut harness, at);
        assert_eq!(focus(&harness), Focus::MIXER);
        harness.state_mut().keys_page = false;
        harness.run();
        // Without it, that's the arrangement.
        click_at(&mut harness, at);
        assert_eq!(focus(&harness), Focus::ARRANGEMENT);

        // Clicking a tab gives its panel the focus, and so does moving the
        // keyboard focus into a panel, as Tab does.
        harness.get_by_label("Devices panel").click();
        harness.run();
        assert_eq!(focus(&harness), Focus::DEVICES);
        harness.get_by_label("Mute Drums").focus();
        harness.run();
        assert_eq!(focus(&harness), Focus::MIXER);
    }

    #[test]
    fn the_piano_roll_takes_the_focus_once_it_shows_a_clip() {
        let mut harness = harness(Project::new());
        harness.state_mut().shell.show_panel(PIANO_ROLL);
        harness.run();
        let tab = harness.get_by_label("Piano Roll panel").rect();
        click_at(&mut harness, tab.left_bottom() + vec2(60.0, 60.0));
        // With no clip, it has nothing for the keys to act on.
        assert_eq!(harness.state().editor.focus, Focus::ARRANGEMENT);
    }

    #[test]
    fn the_windows_open_from_commands() {
        let mut harness = harness(Project::new());
        let menu_bar: Vec<_> = ["File", "Edit", "Transport", "View", "Options"]
            .into_iter()
            .map(|title| harness.query_by_label(title).is_some())
            .collect();
        assert_eq!(menu_bar, [true; 5]);
        harness.get_by_label("Options").click();
        harness.run();
        harness.get_by_label("Keyboard Shortcuts... Ctrl+K").click();
        harness.run();
        harness.get_by_label("View").click();
        harness.run();
        harness
            .get_by_label("Command Palette... Ctrl+Shift+P")
            .click();
        harness.run();
        // The device in the status bar opens the audio settings.
        harness
            .get_by_label("48 kHz · 128 samples · 5.3 ms")
            .click();
        harness.run();
        assert_eq!(
            harness.state().actions,
            [
                Action::Run(options::KEYBOARD_SHORTCUTS),
                Action::Run(view::COMMAND_PALETTE),
                Action::Run(options::AUDIO_SETTINGS),
            ]
        );
    }

    #[test]
    fn the_edit_menu_says_what_undo_and_redo_change() {
        let mut harness = harness(Project::new());
        harness.state_mut().step("Set Tempo");
        harness.get_by_label("Edit").click();
        harness.run();
        harness.get_by_label("Undo Set Tempo Ctrl+Z").click();
        harness.run();
        assert_eq!(harness.state().actions, [Action::Run(edit::UNDO)]);

        // Nothing to redo, so Redo is greyed out.
        harness.get_by_label("Edit").click();
        harness.run();
        let redo = harness.get_by_label("Redo Ctrl+Y, Ctrl+Shift+Z");
        assert!(redo.accesskit_node().is_disabled());
        redo.click();
        harness.run();
        assert_eq!(harness.state().actions, [Action::Run(edit::UNDO)]);
    }

    #[test]
    fn dragging_the_tempo_is_one_gesture() {
        let mut harness = harness(Project::new());
        let at = harness.get_by_value("120.00 BPM").rect().center();
        harness.hover_at(at);
        harness.run();
        harness.drag_at(at);
        harness.run();
        for x in 1..=3 {
            harness.hover_at(at + egui::vec2(20.0 * x as f32, 0.0));
            harness.run();
        }
        harness.drop_at(at + egui::vec2(60.0, 0.0));
        harness.run();
        let actions = &harness.state().actions;
        assert_eq!(actions.first(), Some(&Action::BeginGesture));
        assert_eq!(actions.last(), Some(&Action::EndGesture));
        let tempos: Vec<_> = actions[1..actions.len() - 1]
            .iter()
            .map(|action| match action {
                Action::SetTempo(bpm) => *bpm,
                other => panic!("{other:?} in the middle of a drag"),
            })
            .collect();
        assert!(!tempos.is_empty() && tempos.is_sorted(), "{tempos:?}");
    }

    #[test]
    fn the_window_shows_the_mixer() {
        let mut project = Project::new();
        project.add_track("A4", TrackKind::Audio);
        let mut harness = harness(project);
        harness.get_by_label("A4");
        harness.get_by_role_and_label(Role::Slider, "A4 Volume");
        harness.get_by_role_and_label(Role::Slider, "Master Volume");
        // Its changes come back as actions, and it takes the focus.
        harness.get_by_label("Mute A4").click();
        harness.run();
        let mute = Change::Muted(TrackId(0), true);
        assert_eq!(harness.state().actions, [Action::Mixer(mute)]);
        assert_eq!(harness.state().editor.focus, Focus::MIXER);
    }

    #[test]
    fn the_piano_roll_and_the_devices_share_tabs() {
        let mut shell = Shell::new();
        assert!(shell.panel_shown(DEVICES));
        assert!(!shell.panel_shown(PIANO_ROLL));
        // F12 brings one forward, then the other.
        shell.switch_devices_clip_editor(PIANO_ROLL);
        assert!(shell.panel_shown(PIANO_ROLL));
        assert!(!shell.panel_shown(DEVICES));
        shell.switch_devices_clip_editor(PIANO_ROLL);
        assert!(shell.panel_shown(DEVICES));
        assert!(!shell.panel_shown(PIANO_ROLL));
        // Its toggle brings the piano roll forward, and then hides it, and
        // the devices are in front again.
        shell.toggle_panel(PIANO_ROLL);
        assert!(shell.panel_shown(PIANO_ROLL));
        assert!(!shell.panel_shown(DEVICES));
        shell.toggle_panel(PIANO_ROLL);
        assert!(!shell.panel_shown(PIANO_ROLL));
        assert!(shell.panel_shown(DEVICES));
        // With neither shown, switching shows the piano roll.
        shell.toggle_panel(DEVICES);
        assert!(!shell.panel_shown(DEVICES));
        shell.switch_devices_clip_editor(PIANO_ROLL);
        assert!(shell.panel_shown(PIANO_ROLL));
        shell.show_panel(MIXER);
        assert!(shell.panel_shown(PIANO_ROLL));
        // The arrangement stays.
        shell.toggle_panel(ARRANGEMENT);
        assert!(shell.panel_shown(ARRANGEMENT));
    }

    #[test]
    fn panels_drag_into_splits_and_tabs() {
        let mut harness = harness(Project::new());
        // The mixer to the right of the arrangement.
        let lanes = harness.get_by_label("Arrangement").rect();
        let tab = harness.get_by_label("Mixer panel").rect().center();
        drag(
            &mut harness,
            tab,
            egui::pos2(lanes.right() - 10.0, lanes.center().y),
        );
        let devices = tabs(&[DEVICES, PIANO_ROLL], DEVICES);
        let beside = across(vec![Layout::panel(ARRANGEMENT), Layout::panel(MIXER)]);
        let browser = tabs(&[BROWSER, HISTORY], BROWSER);
        let hidden = vec![BROWSER.into(), PIANO_ROLL.into(), HISTORY.into()];
        let expected = WindowLayout {
            hidden: hidden.clone(),
            layout: across(vec![browser.clone(), down(vec![beside, devices])]),
            ..WindowLayout::default()
        };
        assert_eq!(layout(&harness), expected);
        assert!(harness.state_mut().shell.changed_workspace().is_some());
        // And into a tab beside the devices.
        let tab = harness.get_by_label("Mixer panel").rect().center();
        let devices = harness.get_by_label("Devices panel").rect();
        let to = egui::pos2(devices.right() + 20.0, devices.center().y);
        drag(&mut harness, tab, to);
        let expected = WindowLayout {
            hidden,
            layout: across(vec![
                browser,
                down(vec![
                    Layout::panel(ARRANGEMENT),
                    tabs(&[DEVICES, PIANO_ROLL, MIXER], MIXER),
                ]),
            ]),
            ..WindowLayout::default()
        };
        assert_eq!(layout(&harness), expected);
        assert!(harness.state().shell.panel_shown(MIXER));
        assert!(!harness.state().shell.panel_shown(DEVICES));
    }

    #[test]
    fn panels_keep_their_sizes_as_the_window_grows() {
        let mut harness = harness(Project::new());
        // Tall enough for the panels to have their sizes, with room left
        // for the arrangement.
        harness.set_size(egui::vec2(1100.0, 900.0));
        harness.run();
        let window = harness.ctx.content_rect();
        let from_bottom = |harness: &Harness| {
            let top = harness.get_by_label("Mixer panel").rect().top();
            harness.ctx.content_rect().bottom() - top
        };
        let mixer = from_bottom(&harness);
        harness.set_size(window.size() + egui::vec2(200.0, 300.0));
        harness.run();
        assert_eq!(from_bottom(&harness), mixer);
        // Dragging the edge over the mixer makes it taller, from then on.
        let edge = harness.get_by_label("Mixer panel").rect().top() - 1.0;
        let x = harness.get_by_label("Arrangement").rect().center().x;
        drag(
            &mut harness,
            egui::pos2(x, edge),
            egui::pos2(x, edge - 50.0),
        );
        let taller = from_bottom(&harness);
        assert!((taller - mixer - 50.0).abs() < 2.0, "{mixer} to {taller}");
        harness.set_size(window.size());
        harness.run();
        assert_eq!(from_bottom(&harness), taller);
        let layout = layout(&harness).layout;
        let Split::Across(parts) = &layout.split else {
            panic!("{layout:?}");
        };
        let Split::Down(parts) = &parts[1].split else {
            panic!("{layout:?}");
        };
        let tab_bar = Theme::default().dock.tab_bar;
        // Within a point: the edge goes where the pointer is, between
        // pixels.
        let size = 300.0 + tab_bar + taller - mixer;
        let written = parts[1].size.expect("the mixer's size") as f32;
        assert!((written - size).abs() <= 1.0, "{written} for {size}");
    }

    #[test]
    fn a_closed_tab_hides_its_panel_until_the_view_menu_shows_it() {
        let mut harness = harness(Project::new());
        let tab = harness.get_by_label("Mixer panel").rect();
        let close = harness
            .get_all_by_label("Close")
            .find(|close| tab.contains_rect(close.rect()))
            .unwrap();
        close.click();
        harness.run();
        assert!(!harness.state().shell.panel_shown(MIXER));
        assert!(harness.query_by_label("Mixer panel").is_none());
        assert_eq!(
            layout(&harness).hidden,
            [BROWSER, MIXER, PIANO_ROLL, HISTORY]
        );
        // The arrangement's tab has no close button.
        let tab = harness.get_by_label("Arrangement panel").rect();
        let closes = harness.get_all_by_label("Close");
        assert!(
            closes
                .into_iter()
                .all(|close| !tab.contains_rect(close.rect()))
        );
        harness.state_mut().shell.toggle_panel(MIXER);
        harness.run();
        assert!(harness.state().shell.panel_shown(MIXER));
        harness.get_by_label("Mixer panel");
    }

    #[test]
    fn the_devices_and_the_piano_roll_follow_the_selection() {
        let mut project = Project::new();
        let drums = project.add_track("Drums", TrackKind::Audio);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let content = musicable_model::clip::ClipContent::Midi(Default::default());
        let chords = project.new_clip("Chords", range, content);
        let id = chords.id;
        project.track_mut(keys).unwrap().clips.push(chords);
        let mut harness = harness(project);
        // A hidden piano roll stays hidden.
        let select = |harness: &mut Harness, track, clips: &[ClipId]| {
            let editor = &mut harness.state_mut().editor;
            editor.selected_track = Some(Chain::Track(track));
            editor.selection = musicable_commands::editor::Selection::clips(clips.to_vec());
            if let Some(&clip) = clips.first() {
                editor.piano_roll.clip = Some(clip);
            }
            harness.run();
        };
        select(&mut harness, keys, &[id]);
        assert!(!harness.state().shell.panel_shown(PIANO_ROLL));
        harness.state_mut().shell.switch_devices_clip_editor(PIANO_ROLL);
        select(&mut harness, drums, &[]);
        assert!(harness.state().shell.panel_shown(DEVICES));
        select(&mut harness, keys, &[id]);
        assert!(harness.state().shell.panel_shown(PIANO_ROLL));
        select(&mut harness, drums, &[]);
        assert!(harness.state().shell.panel_shown(DEVICES));
    }

    #[test]
    fn a_device_added_comes_forward_scrolled_into_sight() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        // More than fit across the panel, the synth last.
        let devices: Vec<_> = (NativeKind::ALL.iter())
            .map(|&kind| project.new_device(DeviceKind::Native(NativeDevice::new(kind))))
            .collect();
        let synth = devices.last().unwrap().id;
        project.track_mut(keys).unwrap().devices = devices;
        let mut harness = harness(project);
        harness.state_mut().editor.selected_track = Some(Chain::Track(keys));
        harness.run();
        let width = harness.ctx.content_rect().width();
        let in_sight = |harness: &Harness| {
            let title = harness.get_by_label("Device Synth").rect();
            title.left() >= 0.0 && title.right() <= width
        };
        assert!(!in_sight(&harness));

        // Behind the piano roll, the devices come forward, scrolled to it.
        harness.state_mut().shell.switch_devices_clip_editor(PIANO_ROLL);
        harness.run();
        assert!(!harness.state().shell.panel_shown(DEVICES));
        harness.state_mut().shell.show_device(synth);
        harness.run();
        assert!(harness.state().shell.panel_shown(DEVICES));
        assert!(!harness.state().shell.panel_shown(PIANO_ROLL));
        assert!(in_sight(&harness));

        // Hidden, they stay hidden.
        harness.state_mut().shell.toggle_panel(DEVICES);
        harness.state_mut().shell.show_device(synth);
        harness.run();
        assert!(!harness.state().shell.panel_shown(DEVICES));
    }

    #[test]
    fn workspaces_lay_the_panels_out() {
        let mut harness = harness(Project::new());
        let window = WindowLayout {
            hidden: vec![MIXER.into(), HISTORY.into()],
            layout: across(vec![
                down(vec![Layout::panel(ARRANGEMENT), Layout::panel(MIXER)]),
                Layout {
                    size: Some(300),
                    ..tabs(&[BROWSER, DEVICES, PIANO_ROLL, HISTORY], DEVICES)
                },
            ]),
            ..WindowLayout::default()
        };
        let workspace = Workspace {
            slot: None,
            windows: vec![window.clone()],
            ..Workspace::default()
        };
        harness.state_mut().shell.set_workspace(&workspace);
        harness.run();
        assert_eq!(layout(&harness), window);
        // The devices are on the right.
        let devices = harness.get_by_label("Devices panel").rect();
        assert!(devices.left() > harness.get_by_label("Arrangement").rect().center().x);
        assert!(harness.query_by_label("Mixer panel").is_none());
        // Setting it isn't a change to save.
        assert!(harness.state_mut().shell.changed_workspace().is_none());
    }

    #[test]
    fn the_view_menu_ticks_the_panels_shown() {
        let mut project = Project::new();
        project.add_track("Keys", TrackKind::Midi);
        let mut harness = harness(project);
        harness.state_mut().shell.toggle_panel(PIANO_ROLL);
        harness.run();
        // The piano roll is in front of the devices, with nothing to show.
        harness.get_by_label("Piano Roll");
        harness.get_by_label("View").click();
        harness.run();
        let ticked = |harness: &egui_kittest::Harness<'_, crate::testing::State>, label| {
            harness.get_by_label(label).accesskit_node().toggled()
        };
        assert_eq!(
            ticked(&harness, "Piano Roll Ctrl+Alt+3"),
            Some(Toggled::True)
        );
        assert_eq!(ticked(&harness, "Devices Ctrl+Alt+L"), Some(Toggled::False));
        harness
            .get_by_label("Switch Devices and Piano Roll F12")
            .click();
        harness.run();
        assert_eq!(
            harness.state().actions,
            [Action::Run(view::SWITCH_DEVICES_PIANO_ROLL)]
        );
    }

    #[test]
    fn the_track_commands_have_a_menu() {
        let mut harness = harness(Project::new());
        harness.get_by_label("Track").click();
        harness.run();
        harness.get_by_label("New Audio Track Ctrl+T").click();
        harness.run();
        assert_eq!(
            harness.state().actions,
            [Action::Run(track::NEW_AUDIO_TRACK)]
        );
    }

    /// The shell with the workspaces Default, Mixing in the third slot and
    /// Editing in the fourth, Mixing in use.
    fn with_workspaces() -> Harness {
        let mut harness = harness(Project::new());
        let state = harness.state_mut();
        let names = ["Default", "", "Mixing", "Editing"];
        state.workspaces = (names.iter())
            .map(|name| (!name.is_empty()).then(|| (*name).to_owned()))
            .collect();
        state.workspace = 2;
        harness.run();
        harness
    }

    fn type_name(harness: &mut Harness, text: &str) {
        harness.get_by_role(Role::TextInput).type_text(text);
        harness.run();
    }

    #[test]
    fn the_workspaces_menu_switches_between_them_by_name() {
        let mut harness = with_workspaces();
        harness.get_by_label("Workspaces").click();
        harness.run();
        // Each slot's workspace, with Alt and the slot's number, and a tick
        // on the one in use.
        let ticked =
            |harness: &Harness, label| harness.get_by_label(label).accesskit_node().toggled();
        assert_eq!(ticked(&harness, "Default Alt+1"), Some(Toggled::False));
        assert_eq!(ticked(&harness, "Mixing Alt+3"), Some(Toggled::True));
        assert!(harness.query_by_label_contains("Workspace 2").is_none());
        harness.get_by_label("Editing Alt+4").click();
        harness.run();
        assert_eq!(harness.state().actions, [Action::Run(workspace::SWITCH_4)]);

        // Default can't be renamed or deleted.
        harness.state_mut().workspace = 0;
        harness.get_by_label("Workspaces").click();
        harness.run();
        for label in ["Rename Workspace...", "Delete Workspace..."] {
            assert!(harness.get_by_label(label).accesskit_node().is_disabled());
        }
        assert!(
            !harness
                .get_by_label("Reset Workspace")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn a_new_workspace_needs_a_name_the_others_dont_have() {
        let mut harness = with_workspaces();
        harness.state_mut().shell.save_workspace_as();
        harness.run();
        assert!(harness.state().shell.asking(), "keys don't run commands");
        type_name(&mut harness, "mixing");
        harness.get_by_label("There's a workspace called mixing already.");
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert_eq!(harness.state().actions, []);
        type_name(&mut harness, " 2");
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert_eq!(
            harness.state().actions,
            [Action::SaveWorkspaceAs("mixing 2".to_owned())]
        );
        assert!(!harness.state().shell.asking());

        // With every slot taken, there's no room for another.
        let state = harness.state_mut();
        state.workspaces = (1..=9).map(|n| Some(format!("Mine {n}"))).collect();
        state.shell.save_workspace_as();
        harness.run();
        type_name(&mut harness, "Tracking");
        harness.get_by_label("There's room for 9 workspaces: delete one first.");
        assert!(harness.get_by_label("Save").accesskit_node().is_disabled());
    }

    #[test]
    fn renaming_starts_from_the_name_and_deleting_asks_first() {
        let mut harness = with_workspaces();
        harness.state_mut().shell.rename_workspace();
        harness.run();
        harness.get_by_label("Rename Mixing");
        // Its own name in other letters is no clash.
        type_name(&mut harness, "MIXING");
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert_eq!(
            harness.state().actions,
            [Action::RenameWorkspace("MIXING".to_owned())]
        );
        harness.state_mut().actions.clear();
        // Nor is it a change.
        harness.state_mut().shell.rename_workspace();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert_eq!(harness.state().actions, []);

        harness.state_mut().shell.delete_workspace();
        harness.run();
        harness.get_by_label("Delete the workspace Mixing?");
        harness.get_by_label("Cancel").click();
        harness.run();
        assert_eq!(harness.state().actions, []);
        harness.state_mut().shell.delete_workspace();
        harness.run();
        harness.get_by_label("Delete").click();
        harness.run();
        assert_eq!(harness.state().actions, [Action::DeleteWorkspace]);
    }

    /// Opens the menu of the tab `tab`, and clicks `item` in it.
    fn tab_menu(harness: &mut Harness, tab: &str, item: &str) {
        harness.get_by_label(tab).click_secondary();
        harness.run();
        harness.get_by_label(item).click();
        harness.run();
    }

    #[test]
    fn a_tabs_menu_moves_its_panel_to_a_window_of_its_own_and_back() {
        let mut project = Project::new();
        project.add_track("Drums", TrackKind::Audio);
        let mut harness = harness(project);
        let before = layout(&harness);
        tab_menu(&mut harness, "Mixer panel", "Move to New Window");
        let shell = &harness.state().shell;
        assert_eq!(shell.windows(), 1);
        assert!(shell.panel_shown(MIXER));
        // A window named after it, which the tests show in the main one.
        // A press in its panel, at its foot, gives the panel the focus.
        let window = harness.get_by_label("Mixer - Untitled - Musicable").rect();
        let inside = egui::pos2(window.center().x, window.bottom() - 20.0);
        harness.drag_at(inside);
        harness.drop_at(inside);
        harness.run();
        assert_eq!(harness.state().editor.focus, Focus::MIXER);
        // The main window keeps its place, hidden, and the workspace has
        // the window, as big as the mixer was with its tab bar: 1084 by 289
        // points, and 22.
        let workspace = harness.state().shell.workspace();
        let [main, window] = &workspace.windows[..] else {
            panic!("{workspace:?}");
        };
        assert!(main.hidden.iter().any(|id| id == MIXER));
        assert_eq!(window.layout, Layout::panel(MIXER));
        assert_eq!(window.size, Some([1084, 311]));
        assert!(harness.state_mut().shell.changed_workspace().is_some());
        // The last panel in a window stays there.
        harness.get_by_label("Mixer panel").click_secondary();
        harness.run();
        let item = harness.get_by_label("Move to New Window");
        assert!(item.accesskit_node().is_disabled());
        harness.get_by_label("Move to Main Window").click();
        harness.run();
        assert_eq!(harness.state().shell.windows(), 0);
        assert_eq!(layout(&harness), before);
    }

    #[test]
    fn the_arrangement_stays_in_the_main_window() {
        let mut harness = harness(Project::new());
        harness.get_by_label("Arrangement panel").click_secondary();
        harness.run();
        let item = harness.get_by_label("Move to New Window");
        assert!(item.accesskit_node().is_disabled());
        assert!(harness.query_by_label("Move to Main Window").is_none());
        harness.key_press(egui::Key::Escape);
        harness.run();
        // Nor do the commands move it, and they say why.
        let shell = &mut harness.state_mut().shell;
        assert_eq!(
            shell.move_to_new_window(ARRANGEMENT),
            Err("Arrangement stays in the main window.".to_owned())
        );
        assert_eq!(
            shell.move_to_main_window(MIXER),
            Err("Mixer is in the main window already.".to_owned())
        );
        // The View menu greys out what doesn't apply to the panel with the
        // focus.
        let disabled = |harness: &mut Harness, label| {
            harness.get_by_label("View").click();
            harness.run();
            let disabled = harness.get_by_label(label).accesskit_node().is_disabled();
            harness.key_press(egui::Key::Escape);
            harness.run();
            disabled
        };
        assert!(disabled(&mut harness, "Move Panel to New Window"));
        harness.state_mut().editor.focus = Focus::MIXER;
        assert!(!disabled(&mut harness, "Move Panel to New Window"));
        assert!(disabled(&mut harness, "Move Panel to Main Window"));
    }

    #[test]
    fn a_tab_dragged_out_of_the_main_window_keeps_its_place_there() {
        let mut harness = harness(Project::new());
        let before = layout(&harness);
        // Let go of past the left edge, nearer the browser's place than its
        // own.
        let tab = harness.get_by_label("Devices panel").rect().center();
        drag(&mut harness, tab, egui::pos2(-40.0, 60.0));
        assert_eq!(harness.state().shell.windows(), 1);
        // Back from its window, it's where it was.
        let shell = &mut harness.state_mut().shell;
        shell.move_to_main_window(DEVICES).unwrap();
        harness.run();
        assert_eq!(harness.state().shell.windows(), 0);
        assert_eq!(layout(&harness).layout, before.layout);
    }

    /// A window of its own with the mixer, right of the main window, which
    /// is 1100 points wide: at 1200, 0, and 400 by 300 points.
    fn mixer_window(harness: &mut Harness) -> egui::ViewportId {
        harness.state_mut().shell.move_to_new_window(MIXER).unwrap();
        harness.run();
        let window = own_windows()[0].id;
        with_own(harness, window, |own| {
            let rect = egui::Rect::from_min_size(egui::pos2(1200.0, 0.0), vec2(400.0, 300.0));
            own.info.inner_rect = Some(rect);
            own.info.outer_rect = Some(rect);
        });
        harness.run();
        window
    }

    /// Moves the pointer in the window of its own `window` to `at`, in its
    /// points, and presses or lets go of the mouse button there.
    fn pointer_in(
        harness: &mut Harness,
        window: egui::ViewportId,
        at: egui::Pos2,
        pressed: Option<bool>,
    ) {
        with_own(harness, window, |own| {
            own.events.push(egui::Event::PointerMoved(at));
            if let Some(pressed) = pressed {
                own.events.push(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        });
        harness.run();
    }

    #[test]
    fn a_tab_dragged_from_a_window_of_its_own_goes_in_the_main_window() {
        let mut harness = own_windows_harness(Project::new());
        let window = mixer_window(&mut harness);
        // The mixer's tab, at the left of its window's tab bar, dragged
        // over the main window, after the arrangement's tab, in the mixer's
        // window's points, as the system has the drag there.
        let tab = egui::pos2(20.0, 11.0);
        let arrangement = harness.get_by_label("Arrangement panel").rect();
        let after = egui::pos2(arrangement.right() + 6.0, arrangement.center().y);
        let corner = vec2(1200.0, 0.0);
        pointer_in(&mut harness, window, tab, Some(true));
        for step in 1..=5 {
            let at = tab + (after - corner - tab) * step as f32 / 5.0;
            pointer_in(&mut harness, window, at, None);
        }
        // Out of its window, its ghost follows the pointer on the desktop.
        let ghost = [after.x.round() as i32, after.y.round() as i32];
        assert_eq!(harness.state().shell.ghost, Some((MIXER, ghost)));
        pointer_in(&mut harness, window, after - corner, Some(false));
        let shell = &harness.state().shell;
        assert_eq!(shell.windows(), 0, "its window had nothing else");
        assert_eq!(shell.ghost, None);
        // In a tab after the arrangement's, in front.
        let Split::Across(parts) = layout(&harness).layout.split else {
            panic!("a split across");
        };
        let Split::Down(column) = &parts[1].split else {
            panic!("a split down");
        };
        assert_eq!(column[0], tabs(&[ARRANGEMENT, MIXER], MIXER));
    }

    /// A window of its own with the mixer, and the browser beside it,
    /// hidden, right of the main window: at 1200, 0, and 400 by 300 points.
    fn mixer_and_browser_window(harness: &mut Harness) -> egui::ViewportId {
        let mut workspace = Shell::new().workspace();
        workspace.windows.push(WindowLayout {
            at: Some([1200, 0]),
            size: Some([400, 300]),
            hidden: vec![BROWSER.into()],
            layout: Layout::new(Split::Across(vec![
                Layout::panel(MIXER),
                Layout::panel(BROWSER),
            ])),
            ..WindowLayout::default()
        });
        harness.state_mut().shell.set_workspace(&workspace);
        harness.run();
        // The main window learns where it is.
        let window = own_windows()[0].id;
        with_own(harness, window, |_| {});
        harness.run();
        window
    }

    #[test]
    fn a_tab_dragged_from_the_main_window_goes_in_a_window_of_its_own() {
        let mut harness = own_windows_harness(Project::new());
        mixer_and_browser_window(&mut harness);
        let tab = harness.get_by_label("Devices panel").rect().center();
        // Right of the mixer's tab, in its window.
        let over = egui::pos2(1270.0, 11.0);
        let before = layout(&harness);
        drag(&mut harness, tab, over);
        // In a tab after the mixer's, in front, and the browser stays
        // hidden beside them.
        let shell = &harness.state().shell;
        assert_eq!(shell.ghost, None);
        let window = &shell.workspace().windows[1];
        let mixer = tabs(&[MIXER, DEVICES], DEVICES);
        let across = Split::Across(vec![mixer, Layout::panel(BROWSER)]);
        assert_eq!(window.layout, Layout::new(across));
        assert_eq!(window.hidden, [BROWSER]);
        assert_eq!(shell.viewport_of(DEVICES), shell.viewport_of(MIXER));
        // It keeps its place in the main window, to go back to.
        let shell = &mut harness.state_mut().shell;
        shell.move_to_main_window(DEVICES).unwrap();
        harness.run();
        assert_eq!(layout(&harness).layout, before.layout);
    }

    #[test]
    fn a_tab_dragged_over_a_window_of_its_own_and_away_leaves_it_as_it_was() {
        let mut harness = own_windows_harness(Project::new());
        mixer_and_browser_window(&mut harness);
        let before = harness.state().shell.workspace();
        let tab = harness.get_by_label("Devices panel").rect().center();
        let over = egui::pos2(1270.0, 11.0);
        harness.hover_at(tab);
        harness.run();
        harness.drag_at(tab);
        harness.run();
        for step in 1..=5 {
            harness.hover_at(tab + (over - tab) * step as f32 / 5.0);
            harness.run();
        }
        // Out there, its ghost follows the pointer on the desktop.
        assert_eq!(harness.state().shell.ghost, Some((DEVICES, [1270, 11])));
        // Back in the main window, and let go of where it was.
        harness.hover_at(tab);
        harness.run();
        assert_eq!(harness.state().shell.ghost, None);
        harness.drop_at(tab);
        harness.run();
        assert_eq!(harness.state().shell.workspace(), before);
        // Over it again, and Escape pressed.
        harness.drag_at(tab);
        harness.run();
        for step in 1..=5 {
            harness.hover_at(tab + (over - tab) * step as f32 / 5.0);
            harness.run();
        }
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert_eq!(harness.state().shell.ghost, None);
        harness.drop_at(over);
        harness.run();
        assert_eq!(harness.state().shell.workspace(), before);
    }

    #[test]
    fn a_tab_let_go_of_as_it_gets_to_a_window_of_its_own_goes_in_it() {
        let mut harness = own_windows_harness(Project::new());
        mixer_and_browser_window(&mut harness);
        let tab = harness.get_by_label("Devices panel").rect().center();
        harness.hover_at(tab);
        harness.run();
        harness.drag_at(tab);
        harness.run();
        harness.hover_at(tab - vec2(0.0, 20.0));
        harness.run();
        // Over the window and let go of there in one go, before it could
        // show where the panel would go.
        let over = egui::pos2(1270.0, 11.0);
        harness.input_mut().events.extend([
            egui::Event::PointerMoved(over),
            egui::Event::PointerButton {
                pos: over,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        harness.run();
        let shell = &harness.state().shell;
        assert_eq!(shell.viewport_of(DEVICES), shell.viewport_of(MIXER));
        // It's dropped there and then, and not dragged on in the window, to
        // go wherever the button is let go of next.
        harness.run();
        assert!(!egui::DragAndDrop::has_any_payload(&harness.ctx));
    }

    #[test]
    fn a_tab_dragged_out_of_the_window_gets_a_window_of_its_own() {
        let mut harness = harness(Project::new());
        let tab = harness.get_by_label("Mixer panel").rect().center();
        let below = egui::pos2(tab.x, harness.ctx.content_rect().bottom() + 40.0);
        drag(&mut harness, tab, below);
        let shell = &harness.state().shell;
        assert_eq!(shell.windows(), 1);
        assert!(shell.panel_shown(MIXER));
        assert!(layout(&harness).hidden.iter().any(|id| id == MIXER));
        // Dragged about inside, a tab docks as ever.
        let tab = harness.get_by_label("Devices panel").rect().center();
        let lanes = harness.get_by_label("Arrangement").rect();
        drag(
            &mut harness,
            tab,
            egui::pos2(lanes.right() - 10.0, lanes.center().y),
        );
        assert_eq!(harness.state().shell.windows(), 1);
    }

    #[test]
    fn workspaces_keep_the_windows_of_their_own() {
        let mut harness = harness(Project::new());
        let window = WindowLayout {
            at: Some([3900, 100]),
            size: Some([800, 500]),
            hidden: vec![BROWSER.into()],
            layout: tabs(&[MIXER, BROWSER], MIXER),
            ..WindowLayout::default()
        };
        let mut workspace = Shell::new().workspace();
        workspace.windows.push(window.clone());
        harness.state_mut().shell.set_workspace(&workspace);
        harness.run();
        let shell = &harness.state().shell;
        assert_eq!(shell.windows(), 1);
        assert!(shell.panel_shown(MIXER));
        assert!(!shell.panel_shown(BROWSER));
        assert_eq!(shell.workspace().windows[1], window);
        assert!(harness.state_mut().shell.changed_workspace().is_none());
        // The browser comes back in that window, and goes again, and the
        // window closes once it has nothing to show.
        let shell = &mut harness.state_mut().shell;
        shell.toggle_panel(BROWSER);
        assert!(shell.panel_shown(BROWSER));
        assert_eq!(shell.viewport_of(BROWSER), shell.viewport_of(MIXER));
        shell.toggle_panel(BROWSER);
        shell.toggle_panel(MIXER);
        assert_eq!(shell.windows(), 0);
        assert!(!shell.panel_shown(MIXER));
        assert_eq!(shell.viewport_of(MIXER), egui::ViewportId::ROOT);
        // A panel in two windows of a workspace goes in the first, and the
        // arrangement in none.
        let mut workspace = Shell::new().workspace();
        let only = |id: &str| WindowLayout {
            layout: Layout::panel(id),
            ..WindowLayout::default()
        };
        workspace
            .windows
            .extend([only(MIXER), only(MIXER), only(ARRANGEMENT)]);
        shell.set_workspace(&workspace);
        assert_eq!(shell.windows(), 1);
        assert!(shell.panel_shown(ARRANGEMENT));
    }

    #[test]
    fn a_window_of_its_own_has_a_focus_and_keys_of_its_own() {
        let mut project = Project::new();
        project.add_track("Drums", TrackKind::Audio);
        let mut harness = own_windows_harness(project);
        harness.state_mut().shell.move_to_new_window(MIXER).unwrap();
        harness.run();
        let [window] = &own_windows()[..] else {
            panic!("one window of its own");
        };
        let window = window.id;
        assert_eq!(harness.state().shell.viewport_of(MIXER), window);
        // The user goes to it, and its panel has the focus.
        focus_main(&mut harness, false);
        with_own(&mut harness, window, |own| own.info.focused = Some(true));
        harness.run();
        assert_eq!(harness.state().editor.focus, Focus::MIXER);
        // Keys there run commands, and the keys that work everywhere work
        // there.
        with_own(&mut harness, window, |own| {
            own.events.extend(key(egui::Key::Space))
        });
        harness.run();
        assert_eq!(harness.state().actions, [Action::Run(transport::PLAY_STOP)]);
        // Back in the main window, the arrangement has the focus again.
        with_own(&mut harness, window, |own| own.info.focused = Some(false));
        focus_main(&mut harness, true);
        harness.run();
        assert_eq!(harness.state().editor.focus, Focus::ARRANGEMENT);
        // And in the window again, the mixer.
        focus_main(&mut harness, false);
        with_own(&mut harness, window, |own| own.info.focused = Some(true));
        harness.run();
        assert_eq!(harness.state().editor.focus, Focus::MIXER);
    }

    #[test]
    fn a_text_field_in_a_window_of_its_own_takes_every_key() {
        let mut harness = own_windows_harness(Project::new());
        let shell = &mut harness.state_mut().shell;
        shell.move_to_new_window(BROWSER).unwrap();
        shell.focus_browser_search();
        harness.run();
        let window = own_windows()[0].id;
        focus_main(&mut harness, false);
        with_own(&mut harness, window, |own| own.info.focused = Some(true));
        harness.run();
        with_own(&mut harness, window, |own| {
            own.events.extend(key(egui::Key::Space))
        });
        harness.run();
        let actions = &harness.state().actions;
        assert!(
            !actions.contains(&Action::Run(transport::PLAY_STOP)),
            "{actions:?}"
        );
    }

    #[test]
    fn closing_a_window_of_its_own_sends_its_panels_back() {
        let mut harness = own_windows_harness(Project::new());
        let before = layout(&harness);
        let mut workspace = Shell::new().workspace();
        workspace.windows.push(WindowLayout {
            at: Some([1200, 0]),
            size: Some([800, 500]),
            layout: tabs(&[MIXER, BROWSER], BROWSER),
            ..WindowLayout::default()
        });
        harness.state_mut().shell.set_workspace(&workspace);
        harness.run();
        let window = own_windows()[0].clone();
        // It went where the workspace has it, as big.
        let inner = window.info.inner_rect.unwrap();
        assert_eq!(
            (inner.min, inner.size()),
            (egui::pos2(1200.0, 0.0), vec2(800.0, 500.0))
        );
        assert_eq!(
            window.builder.title.as_deref(),
            Some("Mixer, Browser - Untitled - Musicable")
        );
        with_own(&mut harness, window.id, |own| {
            own.info.events.push(egui::ViewportEvent::Close);
        });
        harness.run();
        let shell = &harness.state().shell;
        assert_eq!(shell.windows(), 0);
        // Both are shown where they were in the main window.
        assert!(shell.panel_shown(MIXER) && shell.panel_shown(BROWSER));
        let mut after = layout(&harness);
        after.hidden = before.hidden.clone();
        assert_eq!(
            after,
            WindowLayout {
                hidden: before.hidden.clone(),
                ..before
            }
        );
    }

    #[test]
    fn a_sample_dragged_from_a_browser_of_its_own_goes_on_a_track() {
        let mut project = Project::new();
        let drums = project.add_track("Drums", TrackKind::Audio);
        let mut harness = own_windows_harness(project);
        harness
            .state_mut()
            .shell
            .move_to_new_window(BROWSER)
            .unwrap();
        harness.run();
        // The browser's window is right of the main window, which is 1100
        // points wide.
        let window = own_windows()[0].id;
        with_own(&mut harness, window, |own| {
            let rect = egui::Rect::from_min_size(egui::pos2(1200.0, 0.0), vec2(400.0, 600.0));
            own.info.inner_rect = Some(rect);
            own.info.outer_rect = Some(rect);
        });
        harness.run();
        // Over the drums' lane in the main window, in the browser's
        // window's points, as the system has the drag there.
        let lanes = harness.get_by_label("Arrangement").rect();
        let row = harness.get_by_label("Track Drums").rect();
        let over = egui::pos2(lanes.center().x, row.center().y) - vec2(1200.0, 0.0);
        let sample = PathBuf::from("/samples/kick.wav");
        // Pressed in the browser's window, where the drag starts.
        pointer_in(&mut harness, window, egui::pos2(200.0, 300.0), Some(true));
        egui::DragAndDrop::set_payload(&harness.ctx, Dragged::Sample(sample.clone()));
        with_own(&mut harness, window, |own| {
            own.events.push(egui::Event::PointerMoved(over))
        });
        harness.run();
        let release = egui::Event::PointerButton {
            pos: over,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        with_own(&mut harness, window, |own| own.events.push(release));
        harness.run();
        let dropped = (harness.state().actions.iter()).find_map(|action| match action {
            Action::DropFiles { files, track, .. } => Some((files.clone(), *track)),
            _ => None,
        });
        assert_eq!(dropped, Some((vec![sample], Some(drums))));
    }

    #[test]
    fn a_window_moved_about_is_saved_once_it_stays_put() {
        let mut harness = harness(Project::new());
        harness.run();
        let shell = &mut harness.state_mut().shell;
        assert!(shell.changed_workspace().is_none(), "nothing's changed");
        // The window as egui describes it, at 100%, at the time `now`.
        let seen = |shell: &mut Shell, x: f32, now: f64| {
            let rect = egui::Rect::from_min_size(egui::pos2(x, 100.0), egui::vec2(1000.0, 700.0));
            let viewport = egui::ViewportInfo {
                native_pixels_per_point: Some(1.0),
                outer_rect: Some(rect),
                inner_rect: Some(rect),
                ..egui::ViewportInfo::default()
            };
            shell.main.follow(&viewport, 1.0, now);
        };
        seen(shell, 200.0, 10.0);
        assert!(shell.changed_workspace().is_none(), "it's still moving");
        seen(shell, 200.0, 10.0 + SETTLE);
        let saved = shell.changed_workspace().expect("it stayed put");
        assert_eq!(saved.windows[0].at, Some([200, 100]));
        assert_eq!(saved.windows[0].size, Some([1000, 700]));
        assert!(shell.changed_workspace().is_none(), "it's saved");
        // Before switching, it's saved at once.
        seen(shell, 300.0, 20.0);
        assert!(shell.changed_workspace().is_none());
        assert!(shell.unsaved_workspace().is_some());
        assert!(shell.unsaved_workspace().is_none());
        // Laid out from a workspace, it's where the workspace has it.
        let mut workspace = shell.workspace();
        workspace.windows[0].at = Some([-1500, 20]);
        shell.set_workspace(&workspace);
        assert_eq!(shell.workspace().windows[0].at, Some([-1500, 20]));
        assert!(shell.changed_workspace().is_none());
        // Reset, the panels are as at first, and that's saved.
        shell.toggle_panel(MIXER);
        shell.reset_workspace();
        let reset = shell.changed_workspace().expect("a reset is saved");
        let first = Shell::new().workspace();
        assert_eq!(reset.windows[0].layout, first.windows[0].layout);
        assert_eq!(reset.windows[0].hidden, first.windows[0].hidden);
    }
}
