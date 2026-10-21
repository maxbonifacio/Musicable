//! The app shell: the menu bar, the transport bar, the tracks, a status bar
//! and the keyboard shortcuts window.

use std::path::Path;

use egui::{Align, Button, Color32, DragValue, Grid, Layout, RichText, ScrollArea, Ui};
use musicable_commands::{CommandId, Keymap, Registry, file, transport};
use musicable_model::time::{Beats, TempoMap};
use musicable_model::{Project, TrackKind};

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
    pub playback: Playback,
    pub audio: Audio<'a>,
    /// Where the user's keymap file goes, if Musicable found a place for it.
    pub keymap_file: Option<&'a Path>,
    /// What was wrong with the keymap file when it was last read.
    pub keymap_problems: &'a [String],
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

/// Something the user asked for, for the app to carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Run a command.
    Run(CommandId),
    /// Set the tempo at the playhead, from the tempo field.
    SetTempo(f64),
    /// Show the audio settings.
    AudioSettings,
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

/// The menus that list commands, with their commands in order. `None` is a
/// separator. Commands missing from a menu go at its end, and an area of
/// commands without a menu gets one, so every command can be reached.
const MENUS: [(&str, &str, &[Option<CommandId>]); 2] = [
    (
        "File",
        "file",
        &[
            Some(file::NEW),
            Some(file::NEW_DEMO),
            Some(file::OPEN),
            None,
            Some(file::SAVE),
            Some(file::SAVE_AS),
            None,
            Some(file::QUIT),
        ],
    ),
    (
        "Transport",
        "transport",
        &[
            Some(transport::PLAY_STOP),
            Some(transport::PLAY),
            Some(transport::STOP),
            Some(transport::GO_TO_START),
            None,
            Some(transport::TOGGLE_LOOP),
            Some(transport::TOGGLE_METRONOME),
            None,
            Some(transport::TEMPO_UP),
            Some(transport::TEMPO_DOWN),
        ],
    ),
];

/// The shell's own state: which of its windows are open.
#[derive(Debug, Default)]
pub struct Shell {
    shortcuts_open: bool,
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
            self.menu_bar(ui, view, &mut actions);
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
        self.shortcuts_window(ui.ctx(), view, &mut actions);
        actions
    }

    fn menu_bar(&mut self, ui: &mut Ui, view: &View<'_>, actions: &mut Vec<Action>) {
        egui::MenuBar::new().ui(ui, |ui| {
            for (title, area, items) in MENUS {
                ui.menu_button(title, |ui| {
                    for item in items {
                        match item {
                            Some(id) => command_button(ui, view, id, actions),
                            None => {
                                ui.separator();
                            }
                        }
                    }
                    let rest: Vec<_> = view
                        .commands
                        .iter()
                        .filter(|c| area_of(&c.id) == area && !items.contains(&Some(c.id.clone())))
                        .collect();
                    if !rest.is_empty() {
                        ui.separator();
                        for command in rest {
                            command_button(ui, view, &command.id, actions);
                        }
                    }
                });
            }
            let mut others: Vec<&str> = view
                .commands
                .iter()
                .map(|c| area_of(&c.id))
                .filter(|area| MENUS.iter().all(|(_, a, _)| a != area))
                .collect();
            others.dedup();
            for area in others {
                ui.menu_button(capitalized(area), |ui| {
                    for command in view.commands.iter().filter(|c| area_of(&c.id) == area) {
                        command_button(ui, view, &command.id, actions);
                    }
                });
            }
            ui.menu_button("Options", |ui| {
                if ui.button("Audio Settings...").clicked() {
                    actions.push(Action::AudioSettings);
                }
                if ui.button("Keyboard Shortcuts...").clicked() {
                    self.shortcuts_open = true;
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                meters(ui, &view.audio, actions);
            });
        });
    }

    fn shortcuts_window(
        &mut self,
        ctx: &egui::Context,
        view: &View<'_>,
        actions: &mut Vec<Action>,
    ) {
        egui::Window::new("Keyboard Shortcuts")
            .open(&mut self.shortcuts_open)
            .default_width(480.0)
            .show(ctx, |ui| {
                ui.label(
                    "Every shortcut runs a command. To change one, edit your keymap file, then \
                     reload it here.",
                );
                ui.add_space(6.0);
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    Grid::new("shortcuts")
                        .num_columns(3)
                        .striped(true)
                        .spacing([18.0, 4.0])
                        .show(ui, |ui| {
                            for command in view.commands.iter() {
                                ui.label(&command.title);
                                ui.label(
                                    RichText::new(chords(view.keymap, &command.id)).monospace(),
                                );
                                ui.label(RichText::new(command.id.as_str()).weak().small());
                                ui.end_row();
                            }
                        });
                });
                ui.separator();
                match view.keymap_file {
                    Some(path) => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Your keymap file:");
                            ui.add(
                                egui::Label::new(
                                    RichText::new(path.display().to_string()).monospace(),
                                )
                                .selectable(true),
                            );
                        });
                        ui.horizontal(|ui| {
                            if path.exists() {
                                if ui.button("Reload").clicked() {
                                    actions.push(Action::ReloadKeymap);
                                }
                            } else if ui
                                .button("Create It")
                                .on_hover_text("Creates the file with no changes in it yet")
                                .clicked()
                            {
                                actions.push(Action::CreateKeymap);
                            }
                        });
                    }
                    None => {
                        ui.label("Musicable found no folder for your keymap file.");
                    }
                }
                for problem in view.keymap_problems {
                    ui.label(RichText::new(problem).color(ui.visuals().warn_fg_color));
                }
            });
    }
}

/// A menu entry for a command, showing its keys.
fn command_button(ui: &mut Ui, view: &View<'_>, id: &CommandId, actions: &mut Vec<Action>) {
    let Some(command) = view.commands.get(id) else {
        return;
    };
    let button = Button::new(command.title.as_str()).shortcut_text(chords(view.keymap, id));
    if ui.add(button).clicked() {
        actions.push(Action::Run(id.clone()));
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
                actions.push(Action::AudioSettings);
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
fn chords(keymap: &Keymap, id: &CommandId) -> String {
    let chords: Vec<_> = keymap.chords_for(id).map(|c| c.to_string()).collect();
    chords.join(", ")
}

/// The area a command belongs to: `transport` for `transport.play`.
fn area_of(id: &CommandId) -> &str {
    id.as_str().split('.').next().unwrap_or_default()
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
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
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};

    use super::*;

    /// What the shell shows in the tests, and what it hands back.
    struct State {
        shell: Shell,
        project: Project,
        commands: Registry,
        keymap: Keymap,
        actions: Vec<Action>,
    }

    fn harness(project: Project) -> Harness<'static, State> {
        let mut commands = Registry::new();
        file::register(&mut commands);
        transport::register(&mut commands);
        let state = State {
            shell: Shell::new(),
            project,
            commands,
            keymap: Keymap::default_keymap(),
            actions: Vec::new(),
        };
        Harness::builder()
            .with_size([1100.0, 640.0])
            .build_ui_state(
                |ui, state: &mut State| {
                    let view = View {
                        project: &state.project,
                        name: "Untitled",
                        modified: false,
                        commands: &state.commands,
                        keymap: &state.keymap,
                        playback: Playback {
                            playing: false,
                            playhead: Beats::ZERO,
                            seconds: 0.0,
                        },
                        audio: Audio {
                            summary: "48 kHz · 128 samples · 5.3 ms",
                            running: true,
                            load: Some(0.25),
                            dropouts: 0,
                            details: "",
                        },
                        keymap_file: None,
                        keymap_problems: &[],
                        messages: &[],
                    };
                    let actions = state.shell.show(ui, &view);
                    state.actions.extend(actions);
                },
                state,
            )
    }

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

    #[test]
    fn areas_get_menus() {
        assert_eq!(area_of(&transport::PLAY), "transport");
        assert_eq!(capitalized("transport"), "Transport");
        assert_eq!(capitalized(""), "");
    }
}
