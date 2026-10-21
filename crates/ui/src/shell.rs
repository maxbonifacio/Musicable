//! The app shell: the menu bar, the transport bar, the tracks and a status
//! bar, with the keyboard shortcuts window and the command palette.

use std::path::Path;

use egui::{Align, Button, Color32, DragValue, Grid, Layout, RichText, ScrollArea, Ui};
use musicable_commands::{
    Command, CommandId, KeyChord, Keymap, Problem, Registry, options, transport,
};
use musicable_model::time::{Beats, TempoMap};
use musicable_model::{Project, TrackKind};

use crate::menus::menus;
use crate::palette::Palette;
use crate::shortcuts::Shortcuts;
use crate::widgets::{Icon, clock, icon_button, meter, position};

/// What the shell shows. The app fills one in for each frame.
pub struct View<'a> {
    pub project: &'a Project,
    /// The project's name: its file's name, or "Untitled".
    pub name: &'a str,
    /// Whether the project changed since it was opened or saved.
    pub modified: bool,
    pub commands: &'a Registry,
    pub keymap: &'a Keymap,
    /// The keymap Musicable starts from, which the user's keymap file
    /// changes.
    pub default_keymap: &'a Keymap,
    pub playback: Playback,
    pub audio: Audio<'a>,
    /// The user's keymap file.
    pub keymap_file: KeymapStatus<'a>,
    /// Errors and notices for the user, oldest first.
    pub messages: &'a [String],
}

/// The transport, as the engine last reported it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Playback {
    pub playing: bool,
    /// Where the playhead is in the song.
    pub playhead: Beats,
    /// The same, in seconds from the start of the song.
    pub seconds: f64,
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
    /// Set the tempo at the playhead, from the tempo field.
    SetTempo(f64),
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

/// The shell's own state: its windows and what they show.
#[derive(Debug, Default)]
pub struct Shell {
    shortcuts: Shortcuts,
    palette: Palette,
}

impl Shell {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows the shell in the window's root `ui`, and returns what the user
    /// asked for.
    pub fn show(&mut self, ui: &mut Ui, view: &View<'_>) -> Vec<Action> {
        let mut actions = Vec::new();
        egui::Panel::top("menu_bar").show(ui, |ui| {
            menu_bar(ui, view, &mut actions);
        });
        egui::Panel::top("transport_bar").show(ui, |ui| {
            ui.add_space(4.0);
            transport_bar(ui, view, &mut actions);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            status_bar(ui, view, &mut actions);
        });
        egui::CentralPanel::default().show(ui, |ui| tracks(ui, view.project));
        // The palette goes first, so the keys it takes while it's open, such
        // as Escape, don't reach the window under it.
        self.palette.show(ui.ctx(), view, &mut actions);
        self.shortcuts.show(ui.ctx(), view, &mut actions);
        actions
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
        self.palette.is_open() || self.shortcuts.asking()
    }
}

/// The menus, with every command and its keys, and the meters on the right.
fn menu_bar(ui: &mut Ui, view: &View<'_>, actions: &mut Vec<Action>) {
    egui::MenuBar::new().ui(ui, |ui| {
        for menu in menus(view.commands) {
            ui.menu_button(menu.title.as_str(), |ui| {
                for item in &menu.items {
                    match item {
                        Some(command) => command_button(ui, view, command, actions),
                        None => {
                            ui.separator();
                        }
                    }
                }
            });
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            meters(ui, &view.audio, actions);
        });
    });
}

/// A menu entry for a command, showing its keys.
fn command_button(ui: &mut Ui, view: &View<'_>, command: &Command, actions: &mut Vec<Action>) {
    let button = with_keys(
        Button::new(command.title.as_str()),
        view.keymap,
        &command.id,
    );
    if ui.add(button).clicked() {
        actions.push(Action::Run(command.id.clone()));
    }
}

fn transport_bar(ui: &mut Ui, view: &View<'_>, actions: &mut Vec<Action>) {
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
        let start = icon_button(ui, Icon::ToStart, false);
        run(start, transport::GO_TO_START, actions);
        let play = icon_button(ui, Icon::Play, view.playback.playing);
        run(play, transport::PLAY, actions);
        run(icon_button(ui, Icon::Stop, false), transport::STOP, actions);
        ui.separator();

        let region = project.loop_region;
        let looping = ui
            .add(Button::selectable(project.looping, "Loop"))
            .on_hover_text(format!(
                "{}\nLoops {} to {}",
                hint(view, &transport::TOGGLE_LOOP),
                position(tempo, region.start),
                position(tempo, region.end)
            ));
        if looping.clicked() {
            actions.push(Action::Run(transport::TOGGLE_LOOP));
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
        if field.changed() {
            actions.push(Action::SetTempo(bpm));
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
                .size(18.0)
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
fn meters(ui: &mut Ui, audio: &Audio<'_>, actions: &mut Vec<Action>) {
    let dropouts = if audio.dropouts == 0 {
        RichText::new("No dropouts")
    } else {
        let plural = if audio.dropouts == 1 { "" } else { "s" };
        RichText::new(format!("{} dropout{plural}", audio.dropouts))
            .color(Color32::from_rgb(235, 90, 80))
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
    meter(ui, audio.load).on_hover_text(audio.details);
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

fn tracks(ui: &mut Ui, project: &Project) {
    if project.tracks.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(
                RichText::new("No tracks yet. File > New Demo Project has a dozen test tones.")
                    .weak(),
            );
        });
        return;
    }
    ScrollArea::vertical().show(ui, |ui| {
        Grid::new("tracks")
            .num_columns(5)
            .striped(true)
            .spacing([28.0, 6.0])
            .show(ui, |ui| {
                for heading in ["Track", "Kind", "Test tone", "Gain", ""] {
                    ui.label(RichText::new(heading).strong());
                }
                ui.end_row();
                for track in &project.tracks {
                    ui.label(&track.name);
                    ui.label(match track.kind {
                        TrackKind::Audio => "Audio",
                        TrackKind::Midi => "MIDI",
                        TrackKind::Return => "Return",
                        TrackKind::Group => "Group",
                    });
                    ui.label(match track.tone {
                        Some(tone) => {
                            format!("{:.2} Hz at {}", tone.frequency, decibels(tone.level))
                        }
                        None => "None".to_owned(),
                    });
                    ui.label(decibels(track.gain));
                    if track.muted {
                        ui.label(RichText::new("Muted").color(ui.visuals().warn_fg_color));
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
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

/// A linear gain in decibels: "-6.0 dB".
fn decibels(gain: f32) -> String {
    if gain > 0.0 {
        let db = 20.0 * gain.log10();
        // No "-0.0 dB" for gains a hair under unity.
        format!("{:.1} dB", if db.abs() < 0.05 { 0.0 } else { db })
    } else {
        "-inf dB".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{file, view};

    use super::*;
    use crate::testing::harness;

    #[test]
    fn the_transport_bar_runs_commands() {
        let mut harness = harness(Project::new());
        for label in ["Go to Start", "Play", "Stop", "Loop", "Metronome", "+"] {
            harness.get_by_label(label).click();
            harness.run();
        }
        let ran = [
            transport::GO_TO_START,
            transport::PLAY,
            transport::STOP,
            transport::TOGGLE_LOOP,
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
        let menu_bar: Vec<_> = ["File", "Transport", "View", "Options"]
            .into_iter()
            .map(|title| harness.query_by_label(title).is_some())
            .collect();
        assert_eq!(menu_bar, [true; 4]);
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
    fn the_tracks_are_listed() {
        let mut project = Project::new();
        let id = project.add_track("A4", TrackKind::Audio);
        project.track_mut(id).unwrap().gain = 0.5;
        let harness = harness(project);
        harness.get_by_label("A4");
        harness.get_by_label("-6.0 dB");
        assert!(harness.query_by_label_contains("No tracks yet").is_none());
        let empty = self::harness(Project::new());
        empty.get_by_label_contains("No tracks yet");
    }

    #[test]
    fn gains_read_in_decibels() {
        assert_eq!(decibels(1.0), "0.0 dB");
        assert_eq!(decibels(0.5), "-6.0 dB");
        assert_eq!(decibels(0.08), "-21.9 dB");
        assert_eq!(decibels(2.0), "6.0 dB");
        assert_eq!(decibels(0.0), "-inf dB");
    }
}
