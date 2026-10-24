//! The app shell: the menu bar, the transport bar, the panels docked under
//! them, and a status bar, with the command palette, the box that renames
//! things, and the Keys page of the app's Settings window.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::{Align, Button, Layout, RichText, Ui};
use musicable_commands::editor::Lane;
use musicable_commands::mixer::Change;
use musicable_commands::{
    Args, Command, CommandId, Editor, Focus, KeyChord, Keymap, Named, Problem, Registry, Value,
    arrange, controllers, device, edit, locator, notes, options, take, track, transport, view,
    workspace,
};
use musicable_model::clip::ClipId;
use musicable_model::native::NativeKind;
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Project, TrackId};

use crate::browser::{self, Library};
use crate::devices::Plugins;
use crate::dock::{Dock, Draw};
use crate::menus::menus;
use crate::mixer::{InputChoice, Levels, MidiChoice};
use crate::mouse::MouseMap;
use crate::palette::Palette;
use crate::panels::{self, BROWSER, Cx, DEVICES, MIXER, PIANO_ROLL, Panels};
use crate::parts::{Parts, Views};
use crate::shortcuts::Shortcuts;
use crate::theme::Theme;
use crate::transport_bar;
use crate::waveform::Waveforms;
use crate::widgets::{checked_name_box, name_box};
use crate::workspace::{WindowLayout, Workspace, check_name};

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
    /// The name of the change Undo would take back, such as "Set Tempo", for
    /// the Edit menu. `None` when there's nothing to undo.
    pub undo: Option<&'a str>,
    /// The name of the change Redo would make again.
    pub redo: Option<&'a str>,
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
    /// The plugins installed, and how the project's are doing.
    pub plugins: Plugins<'a>,
    /// Musicable's own devices that play, which the browser lists: the
    /// engine's `devices::BUILT`. The others wait for their sound.
    pub native: &'a [NativeKind],
    /// The browser's library of samples and presets, and the preview.
    pub library: Library<'a>,
    /// The user's workspaces, for the Workspaces menu.
    pub workspaces: Workspaces<'a>,
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
    /// Where the main window is, as last seen, for the workspace.
    place: Place,
    /// When the main window got there, in egui's time.
    placed_at: f64,
    /// Where the main window was when the workspace was last handed out to
    /// be saved: `None` until the window has been seen since it was laid
    /// out.
    saved_place: Option<Place>,
    /// The time of the frame drawn last, in egui's time.
    now: f64,
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
            place: Place::default(),
            placed_at: 0.0,
            saved_place: None,
            now: 0.0,
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

/// How long a window that the user moves or resizes stays put before its
/// place is saved, in seconds, so it isn't saved at every step.
const SETTLE: f64 = 1.0;

/// Where a window is, and how big: its place in its [`WindowLayout`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Place {
    /// Where its top left corner is on the desktop, in pixels.
    at: Option<[i32; 2]>,
    /// How big its inside is, in pixels at 100% scaling: its monitor's
    /// scaling makes it bigger.
    size: Option<[u32; 2]>,
    maximized: bool,
}

impl Place {
    /// Where the window egui describes as `viewport` is, now that egui
    /// zooms by `zoom`, when it was at `self` before. A minimized window
    /// stays where it was, and a maximized one keeps the place it goes back
    /// to, but for the monitor: one moved to another monitor while it was
    /// maximized goes back there.
    fn seen(self, viewport: &egui::ViewportInfo, zoom: f32) -> Self {
        if viewport.minimized == Some(true) {
            return self;
        }
        // Pixels to an egui point, on the window's monitor.
        let scale = viewport.native_pixels_per_point.unwrap_or(1.0);
        let pixels = scale * zoom;
        let outer = viewport.outer_rect.map(|rect| rect * pixels);
        let at = outer.map(|rect| [rect.min.x.round() as i32, rect.min.y.round() as i32]);
        let size = (viewport.inner_rect).map(|rect| {
            let size = (rect.size() * zoom).round();
            [size.x.max(1.0) as u32, size.y.max(1.0) as u32]
        });
        if viewport.maximized != Some(true) {
            return Self {
                at: at.or(self.at),
                size: size.or(self.size),
                maximized: false,
            };
        }
        let mut back = Self {
            at: self.at.or(at),
            size: self.size.or(size),
            maximized: true,
        };
        if let (Some(outer), Some([x, y]), Some([width, height])) = (outer, back.at, back.size) {
            let middle = egui::pos2(
                x as f32 + width as f32 * scale / 2.0,
                y as f32 + height as f32 * scale / 2.0,
            );
            if !outer.contains(middle) {
                let corner = (outer.min + egui::vec2(40.0, 40.0) * scale).round();
                back.at = Some([corner.x as i32, corner.y as i32]);
            }
        }
        back
    }

    fn of(window: &WindowLayout) -> Self {
        Self {
            at: window.at,
            size: window.size,
            maximized: window.maximized,
        }
    }

    fn put_in(self, window: &mut WindowLayout) {
        window.at = self.at;
        window.size = self.size;
        window.maximized = self.maximized;
    }
}

/// What the tabs of the devices and the piano roll follow: selecting the
/// MIDI clip the piano roll shows brings it forward, and selecting another
/// track the devices.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Followed {
    /// The clip the piano roll shows, and whether it's selected.
    clip: Option<ClipId>,
    selected: bool,
    track: Option<TrackId>,
}

/// A rename in progress.
#[derive(Debug)]
struct Rename {
    named: Named,
    /// The name typed so far: `None` until the box has shown the current
    /// one.
    text: Option<String>,
}

impl Shell {
    /// The shell in the dark theme, with Live's mouse.
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows the shell in the window's root `ui`, and returns what the user
    /// asked for.
    pub fn show(&mut self, ui: &mut Ui, view: &View<'_>) -> Vec<Action> {
        let mut actions = Vec::new();
        self.follow(view.editor);
        self.follow_place(ui.ctx());
        let shown: BTreeSet<_> = (self.registry.iter())
            .map(|panel| panel.id)
            .filter(|id| self.dock.shown(id))
            .collect();
        let theme = &self.theme;
        let parts = &self.parts;
        egui::Panel::top("menu_bar").show(ui, |ui| {
            menu_bar(ui, view, &shown, &mut actions);
        });
        egui::Panel::top("transport_bar").show(ui, |ui| {
            transport_bar::show(ui, view, theme, parts, &mut actions);
        });
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            status_bar(ui, view, &mut actions);
        });
        let chain = (self.panels.devices).chain(view.project, view.editor.selected_track);
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
        };
        let dock = &mut self.dock;
        egui::CentralPanel::no_frame().show(ui, |ui| dock.show(ui, &mut draw));
        if let Some(panel) = draw.focused {
            focus_on(panel, view.editor, drawn, draw.cx.actions);
        }
        // Add Device shows the browser's devices.
        if draw.cx.browse {
            self.panels.browser.show_devices();
            if !self.dock.shown(BROWSER) {
                self.dock.show_panel(BROWSER);
                ui.ctx().request_repaint();
            }
        }
        // A device added from the browser brings the devices forward.
        let adds = [device::ADD, device::ADD_PLUGIN, device::ADD_PRESET];
        let added = (actions.iter()).any(|action| match action {
            Action::Run(id) | Action::RunWith(id, _) => adds.contains(id),
            _ => false,
        });
        if added {
            self.dock.bring_forward(DEVICES);
        }
        self.parts.carry_out();
        // The boxes that take keys go first, so the keys they take while
        // they're open, such as Escape, don't reach the windows under them.
        self.show_rename(ui.ctx(), view, &mut actions);
        self.show_workspace_box(ui.ctx(), view, &mut actions);
        self.palette.show(ui.ctx(), view, &mut actions);
        self.parts.show_customise(ui.ctx(), &self.theme);
        browser::show_dragged(ui.ctx());
        actions
    }

    /// Brings the piano roll forward when the clip it shows is selected,
    /// and the devices when another track is, if they're not hidden.
    fn follow(&mut self, editor: &Editor) {
        let clip = editor.piano_roll.clip;
        let now = Followed {
            clip,
            selected: clip.is_some_and(|clip| editor.selection.clips.contains(&clip)),
            track: editor.selected_track,
        };
        let Some(last) = self.followed.replace(now) else {
            return;
        };
        if now.selected && (!last.selected || now.clip != last.clip) {
            self.dock.bring_forward(PIANO_ROLL);
        } else if now.track != last.track {
            self.dock.bring_forward(DEVICES);
        }
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

    /// Shows the panel with id `id`, in front of the tabs beside it.
    pub fn show_panel(&mut self, id: &str) {
        self.dock.show_panel(id);
    }

    /// Hides the panel with id `id`, unless it's one that stays, such as
    /// the arrangement.
    pub fn hide_panel(&mut self, id: &str) {
        if self.registry.get(id).is_some_and(|panel| panel.hides) {
            self.dock.hide_panel(id);
        }
    }

    /// Brings the piano roll forward, or the devices if the piano roll is
    /// in front, as Live's F12 switches between its clip view and its
    /// device view. Either comes back if it's hidden.
    pub fn switch_devices_piano_roll(&mut self) {
        if self.panel_shown(PIANO_ROLL) {
            self.show_panel(DEVICES);
        } else {
            self.show_panel(PIANO_ROLL);
        }
    }

    /// Whether the panel with id `id` is in sight: not hidden, nor behind
    /// another tab.
    pub fn panel_shown(&self, id: &str) -> bool {
        self.dock.shown(id)
    }

    /// Shows the browser, and puts the keys in the search field of the list
    /// it shows.
    pub fn focus_browser_search(&mut self) {
        self.dock.show_panel(BROWSER);
        self.panels.browser.focus_search();
    }

    /// Follows where the main window is, as egui says in `ctx`.
    fn follow_place(&mut self, ctx: &egui::Context) {
        let zoom = ctx.zoom_factor();
        let (place, now) = ctx.input(|input| (self.place.seen(input.viewport(), zoom), input.time));
        self.now = now;
        if place != self.place {
            self.place = place;
            self.placed_at = now;
        }
        // Where the window is once it's laid out is where it was saved.
        self.saved_place.get_or_insert(place);
    }

    /// How the panels are laid out, where the window is, and the parts of
    /// the transport bar, the track headers and the mixer strips, for the
    /// app to save. The app gives it its slot.
    pub fn workspace(&self) -> Workspace {
        let mut main = self.dock.layout(&self.registry, &self.theme);
        self.place.put_in(&mut main);
        Workspace {
            slot: None,
            views: self.parts.views(),
            windows: vec![main],
        }
    }

    /// The workspace, if the user changed the layout or the views' parts,
    /// or moved the window, since it was last handed out, for the app to
    /// save. A window being moved or resized is saved once it has stayed
    /// put for a second.
    pub fn changed_workspace(&mut self) -> Option<Workspace> {
        let moved = self.saved_place.is_some_and(|saved| saved != self.place)
            && self.now - self.placed_at >= SETTLE;
        // Both are asked, so neither tells of the same change again.
        let changed = self.dock.take_changed() | self.parts.take_changed();
        (changed | moved).then(|| self.take_workspace())
    }

    /// The same, without waiting for the window to stay put: before the
    /// app switches to another workspace, or closes.
    pub fn unsaved_workspace(&mut self) -> Option<Workspace> {
        let moved = self.saved_place.is_some_and(|saved| saved != self.place);
        let changed = self.dock.take_changed() | self.parts.take_changed();
        (changed | moved).then(|| self.take_workspace())
    }

    fn take_workspace(&mut self) -> Workspace {
        self.saved_place = Some(self.place);
        self.workspace()
    }

    /// Lays the panels out and shows the views' parts as `workspace` says,
    /// and takes the main window's place from it, which the app moves the
    /// window to.
    pub fn set_workspace(&mut self, workspace: &Workspace) {
        let main = workspace.windows.first();
        self.dock = match main {
            Some(window) => Dock::from_layout(window, &self.registry, &self.theme),
            None => Dock::new(&self.registry),
        };
        self.parts.set_views(&workspace.views);
        if let Some(window) = main.filter(|window| window.at.is_some() || window.size.is_some()) {
            self.place = Place::of(window);
        }
        self.saved_place = None;
    }

    /// Lays the panels out as at first, with all of them in the main window,
    /// which stays where it is, shows the views' parts as at first, and
    /// saves that as the workspace in use.
    pub fn reset_workspace(&mut self) {
        self.dock = Dock::new(&self.registry);
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

/// The menus, with every command and its keys. `shown` has the ids of the
/// panels in sight.
fn menu_bar(ui: &mut Ui, view: &View<'_>, shown: &BTreeSet<&str>, actions: &mut Vec<Action>) {
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in menus(view.commands) {
            ui.menu_button(menu.title.as_str(), |ui| {
                for item in &menu.items {
                    match item {
                        Some(command) => command_button(ui, view, shown, command, actions),
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
    shown: &BTreeSet<&str>,
    command: &Command,
    actions: &mut Vec<Action>,
) {
    let id = &command.id;
    let step = match id {
        id if *id == edit::UNDO => Some(view.undo),
        id if *id == edit::REDO => Some(view.redo),
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
        (None, None) => (
            command.title.clone(),
            command.when.applies(view.project, view.editor),
        ),
    };
    let mut button = with_keys(Button::new(title), view, id);
    let on = match id {
        id if *id == view::TOGGLE_MIXER => Some(shown.contains(MIXER)),
        id if *id == view::TOGGLE_DEVICES => Some(shown.contains(DEVICES)),
        id if *id == view::TOGGLE_PIANO_ROLL => Some(shown.contains(PIANO_ROLL)),
        id if *id == view::TOGGLE_BROWSER => Some(shown.contains(BROWSER)),
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

/// Gives the panel `id` the focus, before what the panels asked for from
/// `drawn` on, unless it has the focus, or asked for it, or is the piano
/// roll with no clip to show. What a click does in a panel can move the
/// focus on, as opening a clip in the piano roll does.
fn focus_on(id: &'static str, editor: &Editor, drawn: usize, actions: &mut Vec<Action>) {
    let Some(focus) = Focus::on(id).filter(|&focus| focus != editor.focus) else {
        return;
    };
    if focus == Focus::PIANO_ROLL && editor.piano_roll.clip.is_none() {
        return;
    }
    let asked = (actions[drawn..].iter())
        .any(|action| matches!(action, Action::RunWith(id, _) if *id == view::SET_FOCUS));
    if !asked {
        let args = Args::new().with("panel", id);
        actions.insert(drawn, Action::RunWith(view::SET_FOCUS, args));
    }
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

/// A box over the window that asks `question`, with `detail` under it,
/// and buttons to `confirm` or cancel. Escape or a click outside cancels.
/// Returns whether the user confirmed, once they have or have cancelled.
fn confirm_box(ctx: &egui::Context, question: &str, detail: &str, confirm: &str) -> Option<bool> {
    let modal = egui::Modal::new(egui::Id::unique(question)).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.heading(question);
        ui.label(detail);
        ui.add_space(10.0);
        let mut choice = None;
        ui.horizontal(|ui| {
            if ui.button(confirm).clicked() {
                choice = Some(true);
            }
            if ui.button("Cancel").clicked() {
                choice = Some(false);
            }
        });
        choice
    });
    if modal.inner.is_none() && modal.should_close() {
        return Some(false);
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
    use musicable_model::{TrackId, TrackKind};

    use super::*;
    use crate::panels::ARRANGEMENT;
    use crate::testing::{State, harness};
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

    /// Clicks with the mouse at `at`.
    fn click_at(harness: &mut Harness, at: egui::Pos2) {
        harness.hover_at(at);
        harness.run();
        harness.drag_at(at);
        harness.run();
        harness.drop_at(at);
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
        harness.state_mut().undo = Some("Set Tempo");
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
        shell.switch_devices_piano_roll();
        assert!(shell.panel_shown(PIANO_ROLL));
        assert!(!shell.panel_shown(DEVICES));
        shell.switch_devices_piano_roll();
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
        shell.switch_devices_piano_roll();
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
        let expected = WindowLayout {
            hidden: vec![BROWSER.into(), PIANO_ROLL.into()],
            layout: across(vec![Layout::panel(BROWSER), down(vec![beside, devices])]),
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
            hidden: vec![BROWSER.into(), PIANO_ROLL.into()],
            layout: across(vec![
                Layout::panel(BROWSER),
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
        assert_eq!(layout(&harness).hidden, [BROWSER, MIXER, PIANO_ROLL]);
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
            editor.selected_track = Some(track);
            editor.selection = musicable_commands::editor::Selection::clips(clips.to_vec());
            if let Some(&clip) = clips.first() {
                editor.piano_roll.clip = Some(clip);
            }
            harness.run();
        };
        select(&mut harness, keys, &[id]);
        assert!(!harness.state().shell.panel_shown(PIANO_ROLL));
        harness.state_mut().shell.switch_devices_piano_roll();
        select(&mut harness, drums, &[]);
        assert!(harness.state().shell.panel_shown(DEVICES));
        select(&mut harness, keys, &[id]);
        assert!(harness.state().shell.panel_shown(PIANO_ROLL));
        select(&mut harness, drums, &[]);
        assert!(harness.state().shell.panel_shown(DEVICES));
    }

    #[test]
    fn workspaces_lay_the_panels_out() {
        let mut harness = harness(Project::new());
        let window = WindowLayout {
            hidden: vec![MIXER.into()],
            layout: across(vec![
                down(vec![Layout::panel(ARRANGEMENT), Layout::panel(MIXER)]),
                Layout {
                    size: Some(300),
                    ..tabs(&[BROWSER, DEVICES, PIANO_ROLL], DEVICES)
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

    #[test]
    fn a_window_keeps_the_place_it_goes_back_to() {
        let viewport = |min: [f32; 2], size: [f32; 2], maximized| egui::ViewportInfo {
            native_pixels_per_point: Some(1.5),
            outer_rect: Some(egui::Rect::from_min_size(min.into(), size.into())),
            inner_rect: Some(egui::Rect::from_min_size(
                egui::pos2(min[0] + 5.0, min[1] + 20.0),
                egui::vec2(size[0] - 10.0, size[1] - 25.0),
            )),
            maximized: Some(maximized),
            ..egui::ViewportInfo::default()
        };
        // Its corner in pixels, and its inside's size at 100% scaling.
        let place = Place::default().seen(&viewport([100.0, 50.0], [810.0, 625.0], false), 1.0);
        let normal = Place {
            at: Some([150, 75]),
            size: Some([800, 600]),
            maximized: false,
        };
        assert_eq!(place, normal);
        // Maximized on the same monitor, it goes back to where it was.
        let maximized = place.seen(&viewport([-5.0, -5.0], [1290.0, 700.0], true), 1.0);
        assert_eq!(
            maximized,
            Place {
                maximized: true,
                ..normal
            }
        );
        // Minimized, it's where it was.
        let minimized = egui::ViewportInfo {
            minimized: Some(true),
            ..viewport([-32000.0, -32000.0], [160.0, 28.0], false)
        };
        assert_eq!(maximized.seen(&minimized, 1.0), maximized);
        // Maximized on another monitor, it goes back to that one.
        let moved = maximized.seen(&viewport([1275.0, -5.0], [1290.0, 700.0], true), 1.0);
        assert_eq!(moved.at, Some([1973, 53]), "40 points in from its corner");
        assert_eq!(moved.size, normal.size);
        // egui's zoom makes points bigger, but not pixels.
        let zoomed = Place::default().seen(&viewport([100.0, 50.0], [810.0, 625.0], false), 2.0);
        assert_eq!(zoomed.at, Some([300, 150]));
        assert_eq!(zoomed.size, Some([1600, 1200]));
    }

    #[test]
    fn a_window_moved_about_is_saved_once_it_stays_put() {
        let mut harness = harness(Project::new());
        harness.run();
        let shell = &mut harness.state_mut().shell;
        assert!(shell.changed_workspace().is_none(), "nothing's changed");
        let place = Place {
            at: Some([200, 100]),
            size: Some([1000, 700]),
            maximized: false,
        };
        shell.place = place;
        shell.placed_at = shell.now;
        assert!(shell.changed_workspace().is_none(), "it's still moving");
        shell.now += SETTLE;
        let saved = shell.changed_workspace().expect("it stayed put");
        assert_eq!(saved.windows[0].at, Some([200, 100]));
        assert_eq!(saved.windows[0].size, Some([1000, 700]));
        assert!(shell.changed_workspace().is_none(), "it's saved");
        // Before switching, it's saved at once.
        shell.place.at = Some([300, 100]);
        shell.placed_at = shell.now;
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
