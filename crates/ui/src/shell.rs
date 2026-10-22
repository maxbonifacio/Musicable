//! The app shell: the menu bar, the transport bar, the arrangement with the
//! mixer and the devices under it and the plugin browser beside it, and a
//! status bar, with the keyboard shortcuts window, the command palette, and
//! the box that renames things.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use egui::{Align, Button, DragValue, Id, Key, Layout, Modal, Modifiers, RichText, TextEdit, Ui};
use musicable_commands::mixer::Change;
use musicable_commands::{
    Args, Command, CommandId, Editor, KeyChord, Keymap, Named, Problem, Registry, Value, arrange,
    edit, locator, options, take, track, transport, view,
};
use musicable_model::native::NativeKind;
use musicable_model::time::{BeatRange, Beats, TempoMap};
use musicable_model::{Project, TrackId};

use crate::arrangement::Arrangement;
use crate::devices::{BROWSER, Browser, DEVICES, DevicesPanel, Plugins};
use crate::menus::menus;
use crate::mixer::{self, Choices, InputChoice, Levels, MidiChoice};
use crate::mouse::MouseMap;
use crate::palette::Palette;
use crate::shortcuts::Shortcuts;
use crate::theme::Theme;
use crate::waveform::Waveforms;
use crate::widgets::{Icon, clock, icon_button, meter, position};

/// The mixer's panel, under the arrangement.
pub const MIXER: &str = "mixer";

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

/// The user's keymap file, as the keyboard shortcuts window shows it.
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
    /// keymap: the user changed keys in the keyboard shortcuts window.
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
    /// Clear the messages.
    DismissMessages,
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
    arrangement: Arrangement,
    devices: DevicesPanel,
    browser: Browser,
    theme: Theme,
    mouse: MouseMap,
    /// The panels hidden, by id.
    hidden: BTreeSet<&'static str>,
    /// What's being renamed, and the name typed so far.
    rename: Option<Rename>,
}

impl Default for Shell {
    fn default() -> Self {
        Self {
            shortcuts: Shortcuts::default(),
            palette: Palette::default(),
            arrangement: Arrangement::default(),
            devices: DevicesPanel::default(),
            browser: Browser::default(),
            theme: Theme::default(),
            mouse: MouseMap::live(),
            // The plugin browser waits to be asked for.
            hidden: BTreeSet::from([BROWSER]),
            rename: None,
        }
    }
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
        let theme = &self.theme;
        egui::Panel::top("menu_bar").show(ui, |ui| {
            menu_bar(ui, view, &self.hidden, theme, &mut actions);
        });
        egui::Panel::top("transport_bar").show(ui, |ui| {
            ui.add_space(4.0);
            transport_bar(ui, view, theme, &mut actions);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            status_bar(ui, view, &mut actions);
        });
        let chain = self.devices.chain(view.project, view.editor.selected_track);
        if !self.hidden.contains(BROWSER) {
            egui::Panel::left(BROWSER)
                .resizable(true)
                .default_size(theme.devices.browser)
                .min_size(theme.devices.browser / 2.0)
                .show(ui, |ui| {
                    self.browser.show(ui, view, chain, theme, &mut actions);
                });
        }
        // Under the mixer, as Live's Detail View is.
        if !self.hidden.contains(DEVICES) {
            let browse = egui::Panel::bottom(DEVICES)
                .resizable(true)
                .default_size(theme.devices.panel)
                .min_size(theme.devices.panel / 2.0)
                .show(ui, |ui| {
                    let mouse = &self.mouse;
                    self.devices
                        .show(ui, view, chain, theme, mouse, &mut actions)
                })
                .inner;
            if browse && self.hidden.remove(BROWSER) {
                ui.ctx().request_repaint();
            }
        }
        if !self.hidden.contains(MIXER) {
            egui::Panel::bottom(MIXER)
                .resizable(true)
                .default_size(theme.mixer.panel)
                .min_size(theme.mixer.min_fader)
                .show(ui, |ui| {
                    mixer::show(
                        ui,
                        view.project,
                        view.levels,
                        Choices {
                            audio: view.inputs,
                            midi: view.midi_inputs,
                        },
                        theme,
                        &self.mouse,
                        &mut actions,
                    );
                });
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.arrangement
                .show(ui, view, theme, &self.mouse, &mut actions);
        });
        // The boxes that take keys go first, so the keys they take while
        // they're open, such as Escape, don't reach the windows under them.
        self.show_rename(ui.ctx(), view, &mut actions);
        self.palette.show(ui.ctx(), view, &mut actions);
        self.shortcuts.show(ui.ctx(), view, &mut actions);
        actions
    }

    /// Shows the panel with id `id`, such as [`MIXER`], if it's hidden, or
    /// hides it.
    pub fn toggle_panel(&mut self, id: &'static str) {
        if !self.hidden.remove(id) {
            self.hidden.insert(id);
        }
    }

    pub fn panel_shown(&self, id: &str) -> bool {
        !self.hidden.contains(id)
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
        let (enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let mut done = None;
        let modal = Modal::new(Id::unique("rename")).show(ctx, |ui| {
            ui.set_width(ui.spacing().text_edit_width);
            ui.heading(title);
            let field = ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY));
            if first {
                // The whole name, ready to be typed over.
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), field.id) {
                    let all = egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(text.chars().count()),
                    );
                    state.cursor.set_char_range(Some(all));
                    state.store(ui.ctx(), field.id);
                }
            }
            field.request_focus();
            ui.horizontal(|ui| {
                if ui.button("Rename").clicked() {
                    done = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    done = Some(false);
                }
            });
        });
        if enter {
            done = Some(true);
        } else if escape || modal.backdrop_response.clicked() {
            done = done.or(Some(false));
        }
        let Some(confirmed) = done else {
            return;
        };
        let name = text.trim().to_owned();
        if confirmed && !name.is_empty() && name != current {
            let args = Args::new().with(arg, value).with("name", name);
            actions.push(Action::RunWith(command, args));
        }
        self.rename = None;
    }

    /// Shows the command palette, which finds any command and runs it.
    pub fn open_palette(&mut self) {
        self.shortcuts.cancel();
        self.palette.open();
    }

    /// Shows the keyboard shortcuts window, where keys are changed.
    pub fn open_shortcuts(&mut self) {
        self.shortcuts.open();
    }

    /// Whether the keyboard shortcuts window waits for the keys to give a
    /// command. The next chord pressed goes to [`captured`](Self::captured)
    /// then, not to the keymap: [`KeyPresses::capture`](crate::KeyPresses::capture)
    /// takes it.
    pub fn capturing(&self) -> bool {
        self.shortcuts.capturing()
    }

    /// The chord pressed while the keyboard shortcuts window waited for one.
    pub fn captured(&mut self, chord: KeyChord) {
        self.shortcuts.captured(chord);
    }

    /// Whether the shell asks something that keys answer, or has the command
    /// palette open. Keys don't run commands while it does.
    pub fn asking(&self) -> bool {
        self.palette.is_open() || self.shortcuts.asking() || self.rename.is_some()
    }
}

/// The menus, with every command and its keys, and the meters on the right.
fn menu_bar(
    ui: &mut Ui,
    view: &View<'_>,
    hidden: &BTreeSet<&'static str>,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in menus(view.commands) {
            ui.menu_button(menu.title.as_str(), |ui| {
                for item in &menu.items {
                    match item {
                        Some(command) => command_button(ui, view, hidden, command, actions),
                        None => {
                            ui.separator();
                        }
                    }
                }
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            meters(ui, &view.audio, theme, actions);
        });
    });
}

/// A menu entry for a command, showing its keys, ticked if it's a toggle
/// that's on, and greyed out when it doesn't apply. Undo and Redo say what
/// they'd change.
fn command_button(
    ui: &mut Ui,
    view: &View<'_>,
    hidden: &BTreeSet<&'static str>,
    command: &Command,
    actions: &mut Vec<Action>,
) {
    let id = &command.id;
    let step = match id {
        id if *id == edit::UNDO => Some(view.undo),
        id if *id == edit::REDO => Some(view.redo),
        _ => None,
    };
    let (title, enabled) = match step {
        Some(Some(name)) => (format!("{} {name}", command.title), true),
        Some(None) => (command.title.clone(), false),
        None => (
            command.title.clone(),
            command.when.applies(view.project, view.editor),
        ),
    };
    let mut button = with_keys(Button::new(title), view.keymap, id);
    let on = match id {
        id if *id == view::TOGGLE_MIXER => Some(!hidden.contains(MIXER)),
        id if *id == view::TOGGLE_DEVICES => Some(!hidden.contains(DEVICES)),
        id if *id == view::TOGGLE_BROWSER => Some(!hidden.contains(BROWSER)),
        id => toggled(view, id),
    };
    if let Some(on) = on {
        button = button.selected(on);
    }
    if ui.add_enabled(enabled, button).clicked() {
        actions.push(Action::Run(id.clone()));
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
    ];
    toggles
        .into_iter()
        .find(|(toggle, _)| toggle == id)
        .map(|(_, on)| on)
}

fn transport_bar(ui: &mut Ui, view: &View<'_>, theme: &Theme, actions: &mut Vec<Action>) {
    let project = view.project;
    let tempo = &project.tempo;
    let playhead = view.playback.playhead;
    // Runs `id` when `response` was clicked, with its keys in the tooltip.
    let run = |response: egui::Response, id: CommandId, actions: &mut Vec<Action>| {
        if response.on_hover_text(hint(view, &id)).clicked() {
            actions.push(Action::Run(id));
        }
    };
    ui.horizontal(|ui| {
        let start = icon_button(ui, Icon::ToStart, false, theme);
        run(start, transport::GO_TO_START, actions);
        let play = icon_button(ui, Icon::Play, view.playback.playing, theme);
        run(play, transport::PLAY, actions);
        let stop = icon_button(ui, Icon::Stop, false, theme);
        run(stop, transport::STOP, actions);
        let record = icon_button(ui, Icon::Record, view.playback.record, theme);
        run(record, transport::RECORD, actions);
        ui.separator();

        // Punching in and out happens at the loop's ends.
        let region = project.loop_region;
        let ends = |toggle: &CommandId, what: &str| {
            format!(
                "{}\n{what} {} to {}",
                hint(view, toggle),
                position(tempo, region.start),
                position(tempo, region.end)
            )
        };
        let looping = ui
            .add(Button::selectable(project.looping, "Loop"))
            .on_hover_text(ends(&transport::TOGGLE_LOOP, "Loops"));
        if looping.clicked() {
            actions.push(Action::Run(transport::TOGGLE_LOOP));
        }
        let punch_in = ui
            .add(Button::selectable(project.punch_in, "Punch In"))
            .on_hover_text(format!(
                "{}\nRecords from {} on",
                hint(view, &transport::TOGGLE_PUNCH_IN),
                position(tempo, region.start)
            ));
        if punch_in.clicked() {
            actions.push(Action::Run(transport::TOGGLE_PUNCH_IN));
        }
        let punch_out = ui
            .add(Button::selectable(project.punch_out, "Punch Out"))
            .on_hover_text(format!(
                "{}\nRecords up to {}",
                hint(view, &transport::TOGGLE_PUNCH_OUT),
                position(tempo, region.end)
            ));
        if punch_out.clicked() {
            actions.push(Action::Run(transport::TOGGLE_PUNCH_OUT));
        }
        ui.separator();

        let metronome = ui.add(Button::selectable(project.metronome, "Metronome"));
        run(metronome, transport::TOGGLE_METRONOME, actions);
        ui.separator();

        let mut bpm = tempo.bpm_at(playhead);
        let field = ui
            .add(
                DragValue::new(&mut bpm)
                    .speed(0.1)
                    .range(TempoMap::MIN_BPM..=TempoMap::MAX_BPM)
                    .fixed_decimals(2)
                    .suffix(" BPM")
                    // A typed tempo applies on Enter, not at each digit.
                    .update_while_editing(false),
            )
            .on_hover_text("Drag, or click and type, to set the tempo");
        // One drag is one step in the undo history.
        if field.drag_started() {
            actions.push(Action::BeginGesture);
        }
        if field.changed() {
            actions.push(Action::SetTempo(bpm));
        }
        if field.drag_stopped() {
            actions.push(Action::EndGesture);
        }
        run(ui.small_button("\u{2212}"), transport::TEMPO_DOWN, actions);
        run(ui.small_button("+"), transport::TEMPO_UP, actions);
        let signature = tempo.signature_at(playhead);
        ui.label(format!(
            "{}/{}",
            signature.numerator(),
            signature.denominator()
        ));
        ui.separator();

        ui.label(
            RichText::new(position(tempo, playhead))
                .monospace()
                .size(theme.position_text)
                .strong(),
        )
        .on_hover_text("Bars, beats and sixteenths");
        ui.label(
            RichText::new(clock(view.playback.seconds))
                .monospace()
                .weak(),
        );
    });
}

/// The CPU meter and the dropout counter, laid out right to left.
fn meters(ui: &mut Ui, audio: &Audio<'_>, theme: &Theme, actions: &mut Vec<Action>) {
    let dropouts = if audio.dropouts == 0 {
        RichText::new("No dropouts")
    } else {
        let plural = if audio.dropouts == 1 { "" } else { "s" };
        RichText::new(format!("{} dropout{plural}", audio.dropouts)).color(theme.warning)
    };
    let counter = ui
        .add(Button::new(dropouts).frame(false))
        .on_hover_text(format!(
            "{}\n\nClick to count from zero again.",
            audio.details
        ));
    if counter.clicked() {
        actions.push(Action::ResetDropouts);
    }
    ui.separator();
    meter(ui, audio.load, theme).on_hover_text(audio.details);
    let load = audio.load.map_or("CPU --".to_owned(), |load| {
        format!("CPU {:.0}%", load * 100.0)
    });
    ui.label(RichText::new(load).monospace())
        .on_hover_text(audio.details);
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

/// A command's title and keys, for a tooltip: "Play (Shift+Space)".
fn hint(view: &View<'_>, id: &CommandId) -> String {
    let title = view
        .commands
        .get(id)
        .map_or(id.as_str(), |c| c.title.as_str());
    match chords(view.keymap, id).as_str() {
        "" => title.to_owned(),
        chords => format!("{title} ({chords})"),
    }
}

/// The chords that run `id`, as keymap files write them.
pub(crate) fn chords(keymap: &Keymap, id: &CommandId) -> String {
    let chords: Vec<_> = keymap.chords_for(id).map(|c| c.to_string()).collect();
    chords.join(", ")
}

/// `button` with the chords that run `id` at its right, if any do.
pub(crate) fn with_keys<'a>(button: Button<'a>, keymap: &Keymap, id: &CommandId) -> Button<'a> {
    match chords(keymap, id) {
        keys if keys.is_empty() => button,
        keys => button.shortcut_text(keys),
    }
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{file, track, view};
    use musicable_model::{TrackId, TrackKind};

    use super::*;
    use crate::testing::harness;

    #[test]
    fn the_transport_bar_runs_commands() {
        let mut harness = harness(Project::new());
        let labels = [
            "Go to Start",
            "Play",
            "Stop",
            "Record",
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
        // Its changes come back as actions.
        harness.get_by_label("Mute A4").click();
        harness.run();
        let mute = Change::Muted(TrackId(0), true);
        assert_eq!(harness.state().actions, [Action::Mixer(mute)]);
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
}
