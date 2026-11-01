//! The mixer: a channel strip for each track, in the project's order, and
//! the master's on the right.
//!
//! A strip has, from the top: the track's name, where an audio or MIDI
//! track records from, an interface's input or for an audio track another
//! track's tap or Resampling, and when it plays its input, where its output
//! goes, a bus or Sends Only, a send to each return track, the pan, the
//! fader beside the meter, the gain and the highest peak, mute, solo and
//! arm, and the clip stage. Each is a part the user can hide or move, and
//! every strip shows the same parts, so their faders line up. Its controls
//! hand back [`Change`]s for the app to make.
//!
//! Clicking a strip where it has no control selects its track, or the
//! master, as clicking its header in the arrangement does, and a selected
//! track's strip is lit, as its header is. Devices and presets dragged onto
//! a strip, from the browser or the devices panel, go on its track.
//!
//! An automated fader, pan or send shows the value its automation gives it
//! at the playhead, with a dot in the automation's colour, grey once
//! someone overrode it. Its context menu shows its automation lane, clears
//! the automation, or re-enables it. One that modulators move shows how
//! far they can move it, and where they have it, and its menu lists them
//! ([`modulators`]). A modulator's output dragged onto one moves it.
//!
//! The Routing panel ([`routing`]) lists the strips' connections, and the
//! keys wired into devices, a row each, with the strips' own controls.

mod routing;

use std::collections::BTreeMap;
use std::iter;
use std::ops::RangeInclusive;

use egui::accesskit::{Action as Request, ActionData};
use egui::{
    Align, Align2, AsIdSalt, Button, Color32, ComboBox, DragValue, FontId, Frame, Label, Layout,
    Painter, Rangef, Rect, Response, RichText, Role, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, WidgetInfo, pos2, vec2,
};
use musicable_commands::editor::Editor;
use musicable_commands::mixer::Change;
use musicable_commands::{Args, automation, edit, track};
use musicable_model::mixer::Bus;
use musicable_model::modulation::Output;
use musicable_model::param::ParamAddress;
use musicable_model::plugin::NoteTiming;
use musicable_model::routing::{InputError, Tap, Wire};
use musicable_model::time::Beats;
use musicable_model::{
    AudioInput, Chain, ClipType, DeviceId, Input, MidiInput, Monitor, Project, Track, TrackId,
    TrackKind,
};

use crate::browser::{self, Destination};
use crate::modulators::{self, Travel};
use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::parts::{Draw, MIXER_STRIP, Part, Parts, Registry, Shown, Size};
use crate::performance::{self, Performance};
use crate::shell::Action;
use crate::theme::{MixerSizes, Theme};
use crate::widgets::{Face, placeholder, toggle};

pub(crate) use routing::RoutingPanel;

/// A channel's meter readings, linear: 1.0 is full scale.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Level {
    /// Left and right, falling back once the signal drops.
    pub peak: [f32; 2],
    /// Left and right, averaged over a fraction of a second.
    pub rms: [f32; 2],
    /// The highest sample since the peaks were last cleared.
    pub highest: f32,
}

/// An audio input tracks can record from, as the input menus offer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputChoice {
    /// What tracks name it by: see [`AudioInput::device`].
    pub id: String,
    pub name: String,
    /// How many channels it has, or 0 when that's only known once it opens.
    pub channels: u16,
}

/// A MIDI input tracks can record from, as the MIDI input menus offer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiChoice {
    /// What tracks name it by: see [`MidiInput::device`].
    pub id: String,
    pub name: String,
}

/// What the input menus offer.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Choices<'a> {
    pub audio: &'a [InputChoice],
    pub midi: &'a [MidiChoice],
    /// How each plugin device's plugin is doing, for what a MIDI track
    /// taking a plugin's notes says of them.
    pub plugins:
        Option<&'a std::collections::BTreeMap<musicable_model::DeviceId, crate::PluginStatus>>,
}

/// The readings of the project's meters, as the engine last measured them,
/// and of its modulators, as their channels' nodes last worked them out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Levels {
    pub tracks: BTreeMap<TrackId, Level>,
    pub master: Level,
    /// Where each modulator's outputs are: from -1 to 1 for a bipolar
    /// output, and from 0 to 1 for the others.
    pub outputs: BTreeMap<Output, f32>,
    /// How far the routings that play move each parameter they move, all
    /// together: as a share of its range, or in a plugin's own units for a
    /// plugin's parameter.
    pub moved: BTreeMap<ParamAddress, f32>,
}

impl Levels {
    /// The readings of the track with `id`: silence if it has no meter.
    pub fn track(&self, id: TrackId) -> Level {
        self.tracks.get(&id).copied().unwrap_or_default()
    }

    /// Whether a meter shows a signal. The meters fall back for a few
    /// seconds after the music stops, and the window has to keep redrawing
    /// until they're down.
    pub fn moving(&self) -> bool {
        self.tracks.values().chain([&self.master]).any(|level| {
            let readings = level.peak.iter().chain(&level.rms);
            readings
                .into_iter()
                .any(|&reading| decibels_of(reading) > FLOOR)
        })
    }
}

/// The quietest level the meters and the gain fields show, in dB. Below
/// it, they read silence.
const FLOOR: f32 = -70.0;
/// The top of the faders and the meters, in dB. Live's faders go as high.
const FADER_TOP: f32 = 6.0;
/// The top of the sends, in dB.
const SEND_TOP: f32 = 0.0;
/// Where the meters turn from green to yellow, in dB. Over full scale, they
/// turn red.
const LOUD: f32 = -6.0;
/// The marks on the meters' scale, in dB.
const MARKS: [f32; 6] = [6.0, 0.0, -6.0, -12.0, -24.0, -48.0];

/// What the mixer shows without tracks.
const NO_TRACKS: &str = "No tracks yet. Track > New Audio Track adds one, and File > New Demo \
                         Project has a dozen test tones.";

/// Shows the mixer for `project`, with the strips of the tracks `editor`
/// selects lit, the meters reading `levels`, the CPU rows `performance`,
/// the input menus offering `inputs`, automated controls where `playhead`
/// has them and the strips' parts as `parts` has them, and adds what the
/// user changed to `actions`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    ui: &mut Ui,
    project: &Project,
    editor: &Editor,
    levels: &Levels,
    performance: &Performance,
    inputs: Choices<'_>,
    playhead: Beats,
    theme: &Theme,
    mouse: &MouseMap,
    parts: &Parts,
    actions: &mut Vec<Action>,
) {
    let sizes = &theme.mixer;
    let returns: Vec<&Track> = project
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Return)
        .collect();
    let mut mixer = Mixer {
        project,
        editor,
        returns,
        inputs,
        playhead,
        levels,
        performance,
        fader: 0.0,
        theme,
        mouse,
        parts,
    };
    // Every strip has the same rows, so the faders line up, and the faders
    // take the height the rows leave.
    let lineup = parts.shown(MIXER_STRIP, theme);
    let rows: usize = (lineup.iter())
        .map(|shown| match shown.part.draw {
            Draw::Strip { rows, .. } => rows(&mixer),
            _ => 0,
        })
        .sum();
    let fixed = rows as f32 * (sizes.row + sizes.gap) + 2.0 * sizes.margin + sizes.scroll_bar;
    mixer.fader = (ui.available_height() - fixed).max(sizes.min_fader);
    let mixer = mixer;
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        let master = Channel::Master(levels.master);
        ui.push_id("master", |ui| mixer.strip(ui, &lineup, &master, actions));
        ui.separator();
        if project.tracks.is_empty() {
            placeholder(ui, NO_TRACKS);
            return;
        }
        ScrollArea::both()
            .auto_shrink(false)
            .show_viewport(ui, |ui, viewport| {
                ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = sizes.gap;
                    // Only the strips in sight are drawn, so a hundred tracks
                    // cost no more than a screenful. The rest take their
                    // room, and each strip's widgets keep their ids.
                    let left = ui.cursor().min.x;
                    for track in &project.tracks {
                        let x = ui.cursor().min.x - left;
                        if x + sizes.strip < viewport.min.x || x > viewport.max.x {
                            ui.allocate_space(vec2(sizes.strip, 0.0));
                            continue;
                        }
                        let channel = Channel::Track(track, levels.track(track.id));
                        ui.push_id(track.id, |ui| mixer.strip(ui, &lineup, &channel, actions));
                    }
                });
            });
    });
}

/// Registers the parts of the mixer's strips, in the order strips show
/// them at first.
pub(crate) fn register(registry: &mut Registry) {
    let one: fn(&Mixer<'_>) -> usize = |_| 1;
    registry.register(part(
        "name",
        "Name",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.name(ui, channel, actions);
        },
    ));
    registry.register(part(
        "input",
        "Input",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.row(ui, |ui| {
                if let Channel::Track(track, _) = channel {
                    mixer.input(ui, track, actions);
                }
            });
        },
    ));
    registry.register(part(
        "monitor",
        "Monitoring",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.row(ui, |ui| {
                if let Channel::Track(track, _) = channel {
                    mixer.monitor(ui, track, actions);
                }
            });
        },
    ));
    registry.register(part(
        "output",
        "Output",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.row(ui, |ui| {
                let Channel::Track(track, _) = channel else {
                    return;
                };
                // A MIDI track without an instrument plays others'.
                let instrument = track.devices.iter().any(|d| d.kind.is_instrument());
                if track.kind == TrackKind::Midi && !instrument {
                    mixer.midi_to(ui, track, actions);
                } else if let Some(bus) = mixer.output(ui, track) {
                    mix(actions, Change::Output(track.id, bus));
                }
            });
        },
    ));
    registry.register(part(
        "sends",
        "Sends",
        Size::Natural,
        |mixer| mixer.returns.len(),
        |ui, mixer, channel, actions| {
            for to in &mixer.returns {
                mixer.row(ui, |ui| {
                    // A return track doesn't send to the returns.
                    if let Channel::Track(track, _) = channel
                        && track.kind != TrackKind::Return
                    {
                        mixer.send(ui, track, to, actions);
                    }
                });
            }
        },
    ));
    registry.register(part(
        "pan",
        "Pan",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.row(ui, |ui| mixer.pan(ui, channel, actions));
        },
    ));
    registry.register(part(
        "fader",
        "Fader and Meter",
        Size::Rest,
        |_| 0,
        |ui, mixer, channel, actions| mixer.fader_and_meter(ui, channel, actions),
    ));
    registry.register(part(
        "readouts",
        "Gain and Peak",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.row(ui, |ui| mixer.readouts(ui, channel, actions));
        },
    ));
    registry.register(part(
        "buttons",
        "Mute, Solo and Arm",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.row(ui, |ui| {
                if let Channel::Track(track, _) = channel {
                    mixer.buttons(ui, track, actions);
                }
            });
        },
    ));
    registry.register(part(
        "clip_type",
        "Clip Type",
        Size::Natural,
        one,
        |ui, mixer, channel, actions| {
            mixer.row(ui, |ui| mixer.clip_type(ui, channel, actions));
        },
    ));
    registry.register(part(
        "cpu",
        "CPU",
        Size::Natural,
        one,
        |ui, mixer, channel, _| {
            mixer.row(ui, |ui| mixer.cpu(ui, channel));
        },
    ));
}

/// A part of the mixer's strips, shown at first, that takes `rows` of a
/// strip's rows: the fader takes the height the others leave.
fn part(
    id: &'static str,
    title: &'static str,
    size: Size,
    rows: fn(&Mixer<'_>) -> usize,
    draw: fn(&mut Ui, &Mixer<'_>, &Channel<'_>, &mut Vec<Action>),
) -> Part {
    Part {
        id,
        title,
        shown: true,
        size,
        fits: &[MIXER_STRIP],
        draw: Draw::Strip { rows, draw },
    }
}

/// What every strip needs to know.
pub(crate) struct Mixer<'a> {
    project: &'a Project,
    /// What's selected, for the selected tracks' strips to be lit.
    editor: &'a Editor,
    /// The return tracks. Each strip has a row of sends with one for each.
    returns: Vec<&'a Track>,
    /// The inputs the input menus offer.
    inputs: Choices<'a>,
    /// Where automated controls show their automation's value.
    playhead: Beats,
    /// Where modulators have the controls they move.
    levels: &'a Levels,
    /// What each channel costs the CPU.
    performance: &'a Performance,
    /// How tall the faders are.
    fader: f32,
    theme: &'a Theme,
    mouse: &'a MouseMap,
    /// The strips' parts, as the user has them.
    parts: &'a Parts,
}

/// What a strip is the channel of, with its meter's readings.
pub(crate) enum Channel<'a> {
    Track(&'a Track, Level),
    Master(Level),
}

impl Channel<'_> {
    fn chain(&self) -> Chain {
        match self {
            Channel::Track(track, _) => Chain::Track(track.id),
            Channel::Master(_) => Chain::Master,
        }
    }

    /// What the strip's controls are called after: "Lead Pan".
    fn name(&self) -> &str {
        match self {
            Channel::Track(track, _) => &track.name,
            Channel::Master(_) => "Master",
        }
    }

    fn level(&self) -> Level {
        match self {
            Channel::Track(_, level) | Channel::Master(level) => *level,
        }
    }

    /// The channel's own gain and pan, without automation.
    fn gain_and_pan(&self, project: &Project) -> (f32, f32) {
        match self {
            Channel::Track(track, _) => (track.gain, track.pan),
            Channel::Master(_) => (project.master.gain, project.master.pan),
        }
    }

    /// The change that sets the channel's gain to `gain`.
    fn gain(&self, gain: f32) -> Change {
        match self {
            Channel::Track(track, _) => Change::Gain(track.id, gain),
            Channel::Master(_) => Change::MasterGain(gain),
        }
    }
}

impl Mixer<'_> {
    fn sizes(&self) -> &MixerSizes {
        &self.theme.mixer
    }

    /// The strip of `channel`, with the parts in `lineup`, lit while its
    /// track is selected. Clicking it where it has no control selects the
    /// track. Right-clicking a part that has no menu of its own offers to
    /// hide it, or to customise the strips.
    fn strip(
        &self,
        ui: &mut Ui,
        lineup: &[Shown<'_>],
        channel: &Channel<'_>,
        actions: &mut Vec<Action>,
    ) {
        let selected = self.selected(channel);
        // Where each part is down the strip, by id.
        let mut spans = Vec::with_capacity(lineup.len());
        let strip = self.frame(ui, selected, |ui| {
            for shown in lineup {
                let Draw::Strip { rows, draw } = shown.part.draw else {
                    continue;
                };
                // A part without rows, such as the sends when there are no
                // return tracks, takes no room.
                if rows(self) == 0 && !matches!(shown.part.size, Size::Rest) {
                    continue;
                }
                let part = ui.scope(|ui| draw(ui, self, channel, actions)).response;
                spans.push((shown.part.id, part.rect.y_range()));
            }
        });
        let name = format!("{} Strip", channel.name());
        strip.widget_info(|| WidgetInfo::selected(Role::Group, true, selected, &name));
        self.select(ui, &strip, channel, actions);
        let clicked = strip.container_secondary_clicked().then(|| {
            let y = ui.input(|input| input.pointer.interact_pos())?.y;
            let (id, _) = spans.iter().find(|(_, span)| span.contains(y))?;
            Some(*id)
        });
        self.parts.context_menu(&strip, MIXER_STRIP, clicked);
        // Devices and presets dragged from the browser, and devices from the
        // devices panel, go at the end of its chain, as on its track.
        let rect = strip.rect.intersect(ui.clip_rect());
        let target = Destination::Chain(Some(channel.chain()));
        actions.extend(browser::drop_in(ui.ctx(), rect, &target, self.theme));
    }

    /// Whether `channel`'s strip is lit: its track is selected, or is the
    /// track the devices panel shows, which the master can be.
    fn selected(&self, channel: &Channel<'_>) -> bool {
        let editor = self.editor;
        let chain = channel.chain();
        editor.selected_track == Some(chain)
            || (chain.track()).is_some_and(|id| editor.selection.tracks.contains(&id))
    }

    /// Selects `channel`'s track, or the master, if `response` was clicked,
    /// as a click on its header in the arrangement does: see
    /// [`Target::TrackHeader`].
    fn select(
        &self,
        ui: &Ui,
        response: &Response,
        channel: &Channel<'_>,
        actions: &mut Vec<Action>,
    ) {
        let mods = modifiers(ui.input(|input| input.modifiers));
        let action = |gesture| self.mouse.action(Target::TrackHeader, gesture, mods);
        // The second click of a double click is a click too.
        let action = match action(Gesture::DoubleClick) {
            Some(action) if response.double_clicked() => action,
            _ if response.clicked() => match action(Gesture::Click) {
                Some(action) => action,
                None => return,
            },
            _ => return,
        };
        let (id, args) = match (action, channel) {
            (MouseAction::Select | MouseAction::ToggleSelect, Channel::Track(track, _)) => {
                let toggle = action == MouseAction::ToggleSelect;
                let args = Args::new().with("tracks", vec![track.id]);
                (edit::SELECT_TRACKS, args.with("toggle", toggle))
            }
            // The master is selected alone.
            (MouseAction::Select | MouseAction::ToggleSelect, Channel::Master(_)) => {
                (edit::SELECT_MASTER, Args::new())
            }
            (MouseAction::Rename, Channel::Track(track, _)) => {
                (edit::RENAME, Args::new().with("track", track.id))
            }
            _ => return,
        };
        actions.push(Action::RunWith(id, args));
    }

    /// The strip's name, which selects its track when clicked, and can be
    /// right-clicked: a track's has a menu.
    fn name(&self, ui: &mut Ui, channel: &Channel<'_>, actions: &mut Vec<Action>) {
        let Channel::Track(track, _) = channel else {
            let title = self.row(ui, |ui| self.title(ui, "Master")).on_hover_text(
                "The master: every track ends up here, and it plays out of the audio device.",
            );
            self.select(ui, &title, channel, actions);
            return;
        };
        let id = track.id;
        let title = self.row(ui, |ui| self.title(ui, &track.name));
        self.select(ui, &title, channel, actions);
        title.on_hover_text(about(track)).context_menu(|ui| {
            let mut safe = track.solo_safe;
            let toggle = ui
                .checkbox(&mut safe, "Solo-Safe")
                .on_hover_text("Soloing other tracks doesn't silence this one.");
            if toggle.changed() {
                mix(actions, Change::SoloSafe(id, safe));
            }
            let mut always_live = track.always_live;
            let toggle = ui.checkbox(&mut always_live, "Always Live").on_hover_text(
                "It renders live, as the audio device asks for each buffer, while the rest of \
                 the project renders ahead, so what you play into it and the knobs you turn on \
                 it are heard at once.",
            );
            if toggle.changed() {
                let args = Args::new().with("tracks", vec![id]);
                actions.push(Action::RunWith(track::TOGGLE_ALWAYS_LIVE, args));
            }
            ui.separator();
            if ui.button("Delete Track").clicked() {
                mix(actions, Change::Delete(id));
                ui.close();
            }
            ui.separator();
            self.parts.menu_items(ui, MIXER_STRIP, Some("name"));
        });
    }

    /// The pan, with its automation.
    fn pan(&self, ui: &mut Ui, channel: &Channel<'_>, actions: &mut Vec<Action>) {
        let param = ParamAddress::Pan(channel.chain());
        let (_, own) = channel.gain_and_pan(self.project);
        let shown = self.shown(param, own);
        let control = self.pan_control(ui, shown, &format!("{} Pan", channel.name()));
        let travel = foot(control.0.rect);
        self.modulated(ui, &control.0, param, travel, (shown + 1.0) / 2.0, actions);
        self.automated(ui, &control.0, param, "pan", actions);
        moved(control, actions, |pan| match channel {
            Channel::Track(track, _) => Change::Pan(track.id, pan),
            Channel::Master(_) => Change::MasterPan(pan),
        });
    }

    /// The fader beside the meter, with the fader's automation.
    fn fader_and_meter(&self, ui: &mut Ui, channel: &Channel<'_>, actions: &mut Vec<Action>) {
        let param = ParamAddress::Volume(channel.chain());
        let (own, _) = channel.gain_and_pan(self.project);
        let gain = self.shown(param, own);
        let fader = self.channel(ui, gain, channel.level(), channel.name());
        // Up its left edge, beside the groove.
        let rect = fader.0.rect;
        let (bottom, top) = self.travel(rect);
        let x = rect.left() + 1.5;
        let travel = Travel {
            low: pos2(x, bottom),
            high: pos2(x, top),
        };
        let at = height_of(gain, FADER_TOP);
        self.modulated(ui, &fader.0, param, travel, at, actions);
        self.automated(ui, &fader.0, param, "fader", actions);
        moved(fader, actions, |gain| channel.gain(gain));
    }

    /// The clip stage's type.
    fn clip_type(&self, ui: &mut Ui, channel: &Channel<'_>, actions: &mut Vec<Action>) {
        let label = format!("{} Clip Type", channel.name());
        match channel {
            Channel::Track(track, _) => {
                let id = ("clip", track.id);
                if let Some(clip_type) = self.clip_menu(ui, id, track.clip_type, label) {
                    mix(actions, Change::ClipType(track.id, clip_type));
                }
            }
            Channel::Master(_) => {
                let current = self.project.master.clip_type;
                if let Some(clip_type) = self.clip_menu(ui, "master_clip", current, label) {
                    mix(actions, Change::MasterClipType(clip_type));
                }
            }
        }
    }

    /// The share of a core the channel takes: its devices', its clips', its
    /// modulators' and its mixer's. Amber or red when it's busy.
    fn cpu(&self, ui: &mut Ui, channel: &Channel<'_>) {
        let share = self.performance.channel(channel.chain());
        let figure = share.map_or("--".to_owned(), performance::percent);
        let size = vec2(self.sizes().inner(), self.sizes().row);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        response.widget_info(|| WidgetInfo {
            value: share.map(f64::from),
            current_text_value: Some(figure.clone()),
            ..WidgetInfo::labeled(Role::Meter, true, format!("{} CPU", channel.name()))
        });
        if ui.is_rect_visible(rect) {
            let colour = share.and_then(|share| performance::colour(share, self.theme));
            let colour = colour.unwrap_or_else(|| ui.visuals().weak_text_color());
            let font = FontId::monospace(self.theme.small_text);
            let text = format!("CPU {figure}");
            ui.painter()
                .text(rect.center(), Align2::CENTER_CENTER, text, font, colour);
        }
        response.on_hover_ui(|ui| {
            ui.label(match share {
                Some(_) => format!(
                    "{} took {figure} of a core over the last second: its devices, clips, \
                     modulators and mixer.",
                    channel.name()
                ),
                None => "CPU figures show while a device plays.".to_owned(),
            });
        });
    }

    /// Where the track's output goes: a menu of the master, the groups it
    /// can feed, which a return track can't, and Sends Only. Returns the bus
    /// the user picked.
    fn output(&self, ui: &mut Ui, track: &Track) -> Option<Bus> {
        let current = self.project.output_of(track);
        let text = self.bus_name(current);
        let mut picked = None;
        let menu = ComboBox::from_id_salt(("output", track.id))
            .width(self.sizes().inner())
            .truncate()
            .selected_text(text)
            .show_ui(ui, |ui| picked = self.bus_items(ui, track));
        name_menu(&menu.response, format!("{} Output", track.name), text);
        menu.response.on_hover_text("Where the track's output goes");
        picked
    }

    /// The items of the track's output menu: the master, the groups it can
    /// feed and Sends Only, with the bus it feeds ticked. Returns the bus
    /// the user picked, if it's a change.
    fn bus_items(&self, ui: &mut Ui, track: &Track) -> Option<Bus> {
        let current = self.project.output_of(track);
        let mut picked = None;
        for bus in self.project.buses_for(track.id) {
            let mut item = ui.selectable_label(bus == current, self.bus_name(bus));
            if bus == Bus::SendsOnly {
                item = item.on_hover_text(
                    "Feeds nothing: the track is heard through its sends, and the tracks that \
                     take its sound, alone",
                );
            }
            if item.clicked() && bus != current {
                picked = Some(bus);
            }
        }
        picked
    }

    /// Where a MIDI track without an instrument plays its notes, its MIDI
    /// To: a menu of No Output, then the instruments on other tracks it can
    /// play, each opening to the MIDI channel to play it on, or none. Asks
    /// for the instrument and the channel the user picked.
    fn midi_to(&self, ui: &mut Ui, track: &Track, actions: &mut Vec<Action>) {
        let project = self.project;
        let playing: Vec<&Wire> = project.midi_to(track.id).collect();
        let text = match playing[..] {
            [] => "No Output".to_owned(),
            [wire] => self.midi_to_name(wire.to.device(), wire.channel),
            _ => format!("{} Instruments", playing.len()),
        };
        let mut picked = None;
        let menu = ComboBox::from_id_salt(("midi to", track.id))
            .width(self.sizes().inner())
            .truncate()
            .selected_text(&text)
            .show_ui(ui, |ui| {
                let none = playing.is_empty();
                if ui.selectable_label(none, "No Output").clicked() && !none {
                    picked = Some((None, None));
                }
                let instruments = project.midi_to_instruments(track.id);
                if !instruments.is_empty() {
                    ui.separator();
                }
                for device in instruments {
                    let wire = playing.iter().find(|w| w.to.device() == device);
                    let name = self.instrument_name(device);
                    let item = ui.menu_button(name, |ui| {
                        for channel in iter::once(None).chain((0..Wire::CHANNELS).map(Some)) {
                            let on = wire.is_some_and(|w| w.channel == channel);
                            if ui
                                .selectable_label(on, midi_channel_name(channel))
                                .clicked()
                            {
                                if !on {
                                    picked = Some((Some(device), channel));
                                }
                                ui.close();
                            }
                        }
                    });
                    item.response.on_hover_text(
                        "Plays it on the MIDI channel you pick, as a part of a multi-timbral \
                         instrument plays, or with Any Channel, each note on its own",
                    );
                }
            });
        name_menu(&menu.response, format!("{} MIDI To", track.name), &text);
        menu.response.on_hover_text(match playing[..] {
            [] => "Where the track plays its notes: an instrument on another track".to_owned(),
            [wire] => format!(
                "The track plays {}",
                self.midi_to_name(wire.to.device(), wire.channel)
            ),
            _ => "The track plays its notes on these instruments".to_owned(),
        });
        if let Some((to, channel)) = picked {
            let track = track.id;
            mix(actions, Change::MidiTo { track, to, channel });
        }
    }

    /// What the MIDI To menu calls `device` on another track: its track, or
    /// its track and its name, where the track has another instrument.
    fn instrument_name(&self, device: DeviceId) -> String {
        let project = self.project;
        let Some((chain, _, found)) = project.find_device(device) else {
            return "?".to_owned();
        };
        let devices = project.devices(chain).unwrap_or_default();
        let track = match chain {
            Chain::Track(track) => project.track(track).map_or("?", |t| t.name.as_str()),
            Chain::Master => "Master",
        };
        match devices.iter().filter(|d| d.kind.is_instrument()).count() {
            0 | 1 => track.to_owned(),
            _ => format!("{track}, {}", found.kind.name()),
        }
    }

    /// What the MIDI To menu shows for playing `device` on `channel`: the
    /// instrument, and the channel's number, from 1, if it has one.
    fn midi_to_name(&self, device: DeviceId, channel: Option<u8>) -> String {
        let name = self.instrument_name(device);
        match channel {
            Some(channel) => format!("{name} {}", channel + 1),
            None => name,
        }
    }

    fn bus_name(&self, bus: Bus) -> &str {
        match bus {
            Bus::Master => "Master",
            Bus::Group(id) => self.project.track(id).map_or("?", |t| t.name.as_str()),
            Bus::SendsOnly => "Sends Only",
        }
    }

    /// Where an audio track records from: a menu of the inputs, each a pair
    /// of channels or a channel at a time, with the default input first,
    /// then the other tracks, each opening to its taps, and Resampling. For
    /// a MIDI track, a menu of the MIDI inputs, then the other tracks with
    /// notes, each opening to its taps that carry them.
    fn input(&self, ui: &mut Ui, track: &Track, actions: &mut Vec<Action>) {
        if track.kind == TrackKind::Midi {
            self.midi_input(ui, track, actions);
        }
        if track.kind != TrackKind::Audio {
            return;
        }
        let current = match &track.input {
            Some(Input::Audio(input)) => Some(input),
            _ => None,
        };
        let tapped = track.input.as_ref().and_then(Input::tap);
        let text = match (current, tapped) {
            (Some(input), _) => self.input_name(input),
            (None, Some(tap)) => self.tap_input_name(tap),
            (None, None) => "No Input".to_owned(),
        };
        let missing = current.is_some_and(|input| {
            !input.device.is_empty() && !self.inputs.audio.iter().any(|c| c.id == input.device)
        });
        let mut picked = None;
        let shown = if missing {
            RichText::new(&text).color(self.theme.warning)
        } else {
            RichText::new(&text)
        };
        let menu = ComboBox::from_id_salt(("input", track.id))
            .width(self.sizes().inner())
            .truncate()
            .selected_text(shown)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(track.input.is_none(), "No Input")
                    .clicked()
                {
                    picked = Some(None);
                }
                let default = InputChoice {
                    id: String::new(),
                    name: DEFAULT_INPUT.to_owned(),
                    channels: 2,
                };
                for choice in std::iter::once(&default).chain(self.inputs.audio) {
                    ui.separator();
                    ui.label(RichText::new(&choice.name).weak());
                    for input in channel_choices(choice) {
                        let on = current.is_some_and(|c| same_channels(c, &input));
                        let label = channels_name(&choice.name, &input);
                        if ui.selectable_label(on, label).clicked() {
                            picked = Some(Some(Input::Audio(input)));
                        }
                    }
                }
                if let Some(input) = self.tap_items(ui, track, tapped) {
                    picked = Some(Some(input));
                }
            });
        if menu.response.clicked() {
            actions.push(Action::ListInputs);
        }
        name_menu(&menu.response, format!("{} Input", track.name), &text);
        let hint = match tapped {
            _ if missing => {
                "Where the track records from. This input isn't there: it's not plugged in."
            }
            Some(tap) if Input::Tap(tap).is_resampling() => {
                "Where the track records from: the master, without this track in it, which it \
                 never plays"
            }
            Some(_) => "Where the track records from: another track's sound, which it plays",
            None => "Where the track records from",
        };
        menu.response.on_hover_text(hint);
        let changed = match (&picked, &track.input) {
            (Some(None), None) => false,
            (Some(Some(Input::Audio(input))), Some(Input::Audio(current))) => {
                !same_channels(input, current)
            }
            (Some(Some(input)), Some(current)) => input != current,
            (picked, _) => picked.is_some(),
        };
        if let Some(input) = picked.filter(|_| changed) {
            mix(actions, Change::Input(track.id, input));
        }
    }

    /// The other tracks in a track's input menu, each opening to its taps
    /// that carry what the track takes, sound for an audio track and notes
    /// for a MIDI track, then for an audio track, Resampling, the master's
    /// Post Fader. A tap that would close a loop is there but can't be
    /// picked, and a track with none the track can take can't be opened.
    /// `current` is ticked. Returns the input the user picked, if it's a
    /// change.
    fn tap_items(&self, ui: &mut Ui, track: &Track, current: Option<Tap>) -> Option<Input> {
        let project = self.project;
        let mut picked = None;
        let refused = |tap: Tap| match current == Some(tap) {
            true => None,
            false => project.check_input(track.id, tap).err(),
        };
        let mut first = true;
        for other in project.tracks.iter().filter(|t| t.id != track.id) {
            let taps: Vec<Tap> = (project.taps(Chain::Track(other.id)).into_iter())
                .filter(|&tap| !matches!(refused(tap), Some(InputError::Notes | InputError::Sound)))
                .collect();
            if taps.is_empty() {
                continue;
            }
            if std::mem::take(&mut first) {
                ui.separator();
                ui.label(RichText::new("Tracks").weak());
            }
            let open = taps.iter().any(|&tap| refused(tap).is_none());
            ui.add_enabled_ui(open, |ui| {
                ui.menu_button(other.name.as_str(), |ui| {
                    for &tap in &taps {
                        let on = current == Some(tap);
                        let point = project.tap_point_name(tap).unwrap_or_default();
                        let refusal = refused(tap);
                        let item = ui.add_enabled(refusal.is_none(), Button::selectable(on, point));
                        let item = match refusal {
                            Some(why) => item.on_disabled_hover_text(why_not(why)),
                            None => item,
                        };
                        if item.clicked() {
                            if !on {
                                picked = Some(Input::Tap(tap));
                            }
                            ui.close();
                        }
                    }
                })
            });
        }
        if track.kind != TrackKind::Audio {
            return picked;
        }
        ui.separator();
        let resampling = current.is_some_and(|tap| Input::Tap(tap).is_resampling());
        let item = ui.selectable_label(resampling, RESAMPLING).on_hover_text(
            "Records the master, without this track in it. The track never plays it, and \
             plays nothing while it records.",
        );
        if item.clicked() && !resampling {
            picked = Some(Input::RESAMPLING);
        }
        picked
    }

    /// What a track's input menu calls its input from `tap`: "from Kick,
    /// Post FX", or Resampling for the master's Post Fader.
    fn tap_input_name(&self, tap: Tap) -> String {
        if Input::Tap(tap).is_resampling() {
            return RESAMPLING.to_owned();
        }
        let project = self.project;
        let point = project.tap_point_name(tap).unwrap_or_default();
        let channel = match project.tap_chain(tap) {
            Some(Chain::Track(track)) => project.track(track).map_or("?", |t| t.name.as_str()),
            Some(Chain::Master) => "Master",
            None => "?",
        };
        format!("from {channel}, {point}")
    }

    /// What an input menu calls `input`.
    fn input_name(&self, input: &AudioInput) -> String {
        let name = match input.device.as_str() {
            "" => DEFAULT_INPUT,
            id => self
                .inputs
                .audio
                .iter()
                .find(|choice| choice.id == id)
                .map_or(input.name.as_str(), |choice| choice.name.as_str()),
        };
        channels_name(name, input)
    }

    /// Where a MIDI track records from: a menu of the MIDI inputs, every
    /// one first, each with all its channels or one of them, then the other
    /// MIDI tracks' notes.
    fn midi_input(&self, ui: &mut Ui, track: &Track, actions: &mut Vec<Action>) {
        let current = match &track.input {
            Some(Input::Midi(input)) => Some(input),
            _ => None,
        };
        let tapped = track.input.as_ref().and_then(Input::tap);
        let text = match (current, tapped) {
            (Some(input), _) => self.midi_input_name(input),
            (None, Some(tap)) => self.tap_input_name(tap),
            (None, None) => "No Input".to_owned(),
        };
        let missing = current.is_some_and(|input| {
            !input.device.is_empty() && !self.inputs.midi.iter().any(|c| c.id == input.device)
        });
        let mut picked = None;
        let shown = if missing {
            RichText::new(&text).color(self.theme.warning)
        } else {
            RichText::new(&text)
        };
        let menu = ComboBox::from_id_salt(("midi input", track.id))
            .width(self.sizes().inner())
            .truncate()
            .selected_text(shown)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(track.input.is_none(), "No Input")
                    .clicked()
                {
                    picked = Some(None);
                }
                let every = MidiChoice {
                    id: String::new(),
                    name: ALL_MIDI_INPUTS.to_owned(),
                };
                for choice in std::iter::once(&every).chain(self.inputs.midi) {
                    ui.separator();
                    ui.label(RichText::new(&choice.name).weak());
                    ui.horizontal_wrapped(|ui| {
                        for channel in std::iter::once(None).chain((0..16).map(Some)) {
                            let input = MidiInput {
                                device: choice.id.clone(),
                                name: match choice.id.as_str() {
                                    "" => String::new(),
                                    _ => choice.name.clone(),
                                },
                                channel,
                            };
                            let on = current.is_some_and(|c| same_source(c, &input));
                            let face = channel.map_or("All".to_owned(), |c| (c + 1).to_string());
                            let label = match channel {
                                Some(_) => midi_name(&choice.name, &input),
                                None => format!("{}, All Channels", choice.name),
                            };
                            let button = ui.selectable_label(on, face);
                            button.widget_info(|| {
                                WidgetInfo::selected(Role::Button, true, on, &label)
                            });
                            if button.on_hover_text(&label).clicked() {
                                picked = Some(Some(Input::Midi(input)));
                            }
                        }
                    });
                }
                if let Some(input) = self.tap_items(ui, track, tapped) {
                    picked = Some(Some(input));
                }
            });
        if menu.response.clicked() {
            actions.push(Action::ListInputs);
        }
        name_menu(&menu.response, format!("{} Input", track.name), &text);
        // A plugin's notes come when its device's Notes' Timing says.
        let plugin = tapped.and_then(|tap| match tap {
            Tap::Device(device) => self.project.find_device(device),
            _ => None,
        });
        let plugin = plugin.and_then(|(_, _, device)| Some((device, device.kind.plugin()?)));
        let hint = match (tapped, plugin) {
            _ if missing => {
                "Where the track records from. This MIDI input isn't there: it's not plugged in."
                    .to_owned()
            }
            (Some(_), Some((device, plugin))) => {
                // Its latency matters only where its menu has Notes' Timing.
                let status = self
                    .inputs
                    .plugins
                    .and_then(|status| status.get(&device.id));
                let name = device.kind.name();
                match status.filter(|status| status.note_timing.is_some()) {
                    Some(status) => {
                        let timing = match plugin.note_timing {
                            NoteTiming::WithSound => "with its sound",
                            NoteTiming::AsPlayed => "as it plays them",
                        };
                        let latency = status.latency_ms;
                        format!(
                            "Where the track records from: the notes {name} plays, which come \
                             {timing}, against its latency of {latency:.1} ms. If they're early \
                             or late, its Notes' Timing, in its menu in the devices panel, \
                             changes that"
                        )
                    }
                    None => format!("Where the track records from: the notes {name} plays"),
                }
            }
            (Some(_), None) => {
                "Where the track records from: another track's notes, which it plays".to_owned()
            }
            (None, _) => "Where the track records from: a MIDI input, or all of them, and one \
                          channel or all of them, as MPE controllers need"
                .to_owned(),
        };
        menu.response.on_hover_text(hint);
        let changed = match (&picked, &track.input) {
            (Some(None), None) => false,
            (Some(Some(Input::Midi(input))), Some(Input::Midi(current))) => {
                !same_source(input, current)
            }
            (Some(Some(input)), Some(current)) => input != current,
            (picked, _) => picked.is_some(),
        };
        if let Some(input) = picked.filter(|_| changed) {
            mix(actions, Change::Input(track.id, input));
        }
    }

    /// What a MIDI input menu calls `input`.
    fn midi_input_name(&self, input: &MidiInput) -> String {
        let name = match input.device.as_str() {
            "" => ALL_MIDI_INPUTS,
            id => (self.inputs.midi.iter())
                .find(|choice| choice.id == id)
                .map_or(input.name.as_str(), |choice| choice.name.as_str()),
        };
        midi_name(name, input)
    }

    /// When an audio or MIDI track plays its input instead of its clips:
    /// In, Auto, or Off.
    fn monitor(&self, ui: &mut Ui, track: &Track, actions: &mut Vec<Action>) {
        if !matches!(track.kind, TrackKind::Audio | TrackKind::Midi) {
            return;
        }
        let sizes = self.sizes();
        let size = vec2((sizes.inner() - 2.0 * sizes.gap) / 3.0, sizes.row - 2.0);
        let modes = [
            (Monitor::In, "In", "Monitor: always plays the input"),
            (
                Monitor::Auto,
                "Auto",
                "Monitor: plays the input while the track is armed, and the transport is \
                 stopped or recording",
            ),
            (
                Monitor::Off,
                "Off",
                "Monitor: never plays the input, only records it",
            ),
        ];
        for (monitor, face, hint) in modes {
            let on = track.monitor == monitor;
            let label = format!("{} Monitor {face}", track.name);
            let button = toggle(
                ui,
                Face::Text(face),
                on,
                self.theme.monitor,
                &label,
                size,
                self.theme,
            )
            .on_hover_text(hint);
            if button.clicked() && !on {
                mix(actions, Change::Monitor(track.id, monitor));
            }
        }
    }
}

/// What the input menus call the default input.
const DEFAULT_INPUT: &str = "Default";

/// What an audio track's input menu calls the master's Post Fader.
const RESAMPLING: &str = "Resampling";

/// Why an audio track can't take a tap, for its item's hover text.
fn why_not(why: InputError) -> &'static str {
    match why {
        InputError::Loop => "That would make a loop: what comes out of this track reaches it.",
        InputError::NoTrack
        | InputError::NoTap
        | InputError::Bus
        | InputError::Notes
        | InputError::Sound
        | InputError::Master => "This track can't take it.",
    }
}

/// What the MIDI input menus call every MIDI input.
const ALL_MIDI_INPUTS: &str = "All Inputs";

/// Whether `a` and `b` take the same channels of the same MIDI input,
/// whatever names they have for it.
fn same_source(a: &MidiInput, b: &MidiInput) -> bool {
    (&a.device, a.channel) == (&b.device, b.channel)
}

/// `name` with the MIDI channel `input` takes, counting from 1: "Keys,
/// Ch. 10", or just "Keys" for all of them.
fn midi_name(name: &str, input: &MidiInput) -> String {
    match input.channel {
        Some(channel) => format!("{name}, Ch. {}", channel + 1),
        None => name.to_owned(),
    }
}

/// The ways a track can take `choice`'s channels: each pair, then each
/// channel on its own. An input whose channels aren't known yet offers two.
fn channel_choices(choice: &InputChoice) -> Vec<AudioInput> {
    let channels = match choice.channels {
        0 => 2,
        channels => channels,
    };
    let input = |channel, stereo| AudioInput {
        device: choice.id.clone(),
        name: match choice.id.as_str() {
            "" => String::new(),
            _ => choice.name.clone(),
        },
        channel,
        stereo,
    };
    let pairs = (0..channels / 2).map(|pair| input(pair * 2, true));
    let singles = (0..channels).map(|channel| input(channel, false));
    pairs.chain(singles).collect()
}

/// Whether `a` and `b` take the same channels of the same input, whatever
/// names they have for it.
fn same_channels(a: &AudioInput, b: &AudioInput) -> bool {
    (&a.device, a.channel, a.stereo) == (&b.device, b.channel, b.stereo)
}

/// `name` with the channels `input` takes, counting from 1: "Default 1/2".
fn channels_name(name: &str, input: &AudioInput) -> String {
    let first = u32::from(input.channel) + 1;
    if input.stereo {
        format!("{name} {first}/{}", first + 1)
    } else {
        format!("{name} {first}")
    }
}

/// How a level that moves sideways goes: see [`Mixer::level_bar`].
struct Bar<'a> {
    /// The top of its scale, in dB.
    top: f32,
    /// The level a double-click gives it.
    reset: f32,
    /// What it says on it beside its value: a send's return track.
    name: Option<&'a str>,
    /// What screen readers call it.
    label: &'a str,
    width: f32,
}

/// Asks the app to make `change`.
fn mix(actions: &mut Vec<Action>, change: Change) {
    actions.push(Action::Mixer(change));
}

/// Asks the app for the `change` a control makes, if it was moved, from the
/// control's response and the value it was moved to. Dragging a control is
/// one gesture, so one undo takes the whole drag back.
fn moved<T>(
    (response, value): (Response, Option<T>),
    actions: &mut Vec<Action>,
    change: impl FnOnce(T) -> Change,
) {
    if response.drag_started() {
        actions.push(Action::BeginGesture);
    }
    if let Some(value) = value {
        mix(actions, change(value));
    }
    if response.drag_stopped() {
        actions.push(Action::EndGesture);
    }
}

/// What the strip's name says when hovered.
fn about(track: &Track) -> String {
    let kind = match track.kind {
        TrackKind::Audio => "Audio track",
        TrackKind::Midi => "MIDI track",
        TrackKind::Return => "Return track",
        TrackKind::Group => "Group track",
    };
    let mut text = format!("{}\n{kind}", track.name);
    if let Some(tone) = track.tone {
        text.push_str(&format!(
            "\nTest tone: {:.2} Hz at {}",
            tone.frequency,
            decibels(tone.level)
        ));
    }
    text.push_str("\nRight-click for more.");
    text
}

impl Mixer<'_> {
    /// Lays out a strip: a column a strip wide, in a frame, lit if it's
    /// `selected`, where the layout puts the next widget. Returns the
    /// frame's response, which has the clicks its controls don't take.
    fn frame(&self, ui: &mut Ui, selected: bool, add: impl FnOnce(&mut Ui)) -> Response {
        let sizes = self.sizes();
        let size = vec2(sizes.strip, ui.available_height());
        let fill = match selected {
            true => self.theme.header_selected,
            false => ui.visuals().faint_bg_color,
        };
        ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
            // Under the controls, so they keep their clicks.
            let clicks = UiBuilder::new().sense(Sense::click());
            ui.scope_builder(clicks, |ui| {
                Frame::new()
                    .fill(fill)
                    .corner_radius(self.theme.corner)
                    .inner_margin(sizes.margin)
                    .show(ui, |ui| {
                        ui.set_width(sizes.inner());
                        ui.spacing_mut().item_spacing = vec2(sizes.gap, sizes.gap);
                        add(ui);
                    });
            })
            .response
        })
        .inner
    }

    /// Lays out one of a strip's rows: a row high and as wide as the strip,
    /// left to right.
    fn row<R>(&self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        let size = vec2(self.sizes().inner(), self.sizes().row);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(size);
            add(ui)
        })
        .inner
    }

    /// A strip's name, which can be right-clicked.
    fn title(&self, ui: &mut Ui, name: &str) -> Response {
        let label = Label::new(RichText::new(name).strong())
            .truncate()
            .sense(Sense::click());
        ui.add_sized([self.sizes().inner(), self.sizes().row], label)
    }

    /// The track's send to the return track `to`: its level, and whether it
    /// takes the signal before the fader.
    fn send(&self, ui: &mut Ui, track: &Track, to: &Track, actions: &mut Vec<Action>) {
        self.send_in(ui, track, to, self.sizes().inner(), true, actions);
    }

    /// The same, `width` wide, in a strip or, without the return track's
    /// name on its level or the strips' parts in its menu, in a row of the
    /// Routing panel.
    fn send_in(
        &self,
        ui: &mut Ui,
        track: &Track,
        to: &Track,
        width: f32,
        strip: bool,
        actions: &mut Vec<Action>,
    ) {
        let sizes = self.sizes();
        let send = track.sends.iter().find(|send| send.to == to.id);
        let (level, pre_fader) = send.map_or((0.0, false), |send| {
            let level = self.shown(ParamAddress::Send(send.id), send.level);
            (level, send.pre_fader)
        });
        let label = format!("{} Send to {}", track.name, to.name);
        let width = width - sizes.gap - sizes.pre_button;
        let name = strip.then_some(to.name.as_str());
        let control = self.send_control(ui, level, &to.name, name, &label, width);
        if let Some(send) = send {
            let param = ParamAddress::Send(send.id);
            let at = height_of(level, SEND_TOP);
            let travel = foot(control.0.rect);
            self.modulated(ui, &control.0, param, travel, at, actions);
            self.mark(ui, &control.0, param);
            control.0.context_menu(|ui| {
                if ui.button("Remove Send").clicked() {
                    let remove = Change::RemoveSend {
                        track: track.id,
                        to: to.id,
                    };
                    mix(actions, remove);
                    ui.close();
                }
                ui.separator();
                self.automation_items(ui, param, actions);
                modulators::routing_items(ui, self.project, param, actions);
                if strip {
                    ui.separator();
                    self.parts.menu_items(ui, MIXER_STRIP, Some("sends"));
                }
            });
        }
        moved(control, actions, |level| Change::SendLevel {
            track: track.id,
            to: to.id,
            level,
        });
        let label = format!("Pre-Fader {} Send to {}", track.name, to.name);
        let size = vec2(sizes.pre_button, sizes.row - 2.0);
        let pre = toggle(
            ui,
            Face::Text("Pre"),
            pre_fader,
            self.theme.solo,
            &label,
            size,
            self.theme,
        )
        .on_hover_text("Pre-fader: the send takes the signal before the fader and the pan.");
        if pre.clicked() {
            let pre = Change::SendPreFader {
                track: track.id,
                to: to.id,
                pre_fader: !pre_fader,
            };
            mix(actions, pre);
        }
    }

    /// What a control for `param` shows: the value its automation gives it
    /// at the playhead, unless someone overrode that, or else `own`.
    fn shown(&self, param: ParamAddress, own: f32) -> f32 {
        automated_value(self.project, param, self.playhead, f64::from(own)) as f32
    }

    /// Marks the control `response` if `param` has automation, and gives it
    /// a menu of the automation's commands, and the menu of the strips'
    /// `part` it's in.
    fn automated(
        &self,
        ui: &Ui,
        response: &Response,
        param: ParamAddress,
        part: &'static str,
        actions: &mut Vec<Action>,
    ) {
        self.mark(ui, response, param);
        response.context_menu(|ui| {
            self.automation_items(ui, param, actions);
            modulators::routing_items(ui, self.project, param, actions);
            ui.separator();
            self.parts.menu_items(ui, MIXER_STRIP, Some(part));
        });
    }

    /// Shows along `travel` how far modulators can move the control
    /// `response` of `param` from `at`, where its value is, and where they
    /// have it, and takes a modulator's output dragged onto it.
    fn modulated(
        &self,
        ui: &Ui,
        response: &Response,
        param: ParamAddress,
        travel: Travel,
        at: f32,
        actions: &mut Vec<Action>,
    ) {
        let shown = modulators::Shown {
            project: self.project,
            levels: self.levels,
            param,
        };
        actions.extend(shown.on(ui, response, travel, at, self.theme));
    }

    fn mark(&self, ui: &Ui, response: &Response, param: ParamAddress) {
        automation_mark(ui, self.theme, self.project, response, param);
    }

    fn automation_items(&self, ui: &mut Ui, param: ParamAddress, actions: &mut Vec<Action>) {
        automation_items(ui, self.project, param, actions);
    }

    /// Mute, solo, and arm for audio and MIDI tracks.
    fn buttons(&self, ui: &mut Ui, track: &Track, actions: &mut Vec<Action>) {
        let id = track.id;
        let name = &track.name;
        let theme = self.theme;
        let sizes = self.sizes();
        let size = vec2((sizes.inner() - 2.0 * sizes.gap) / 3.0, sizes.row - 2.0);
        let mute = toggle(
            ui,
            Face::Text("M"),
            track.muted,
            theme.mute,
            &format!("Mute {name}"),
            size,
            theme,
        )
        .on_hover_text("Mute");
        if mute.clicked() {
            mix(actions, Change::Muted(id, !track.muted));
        }
        let hint = if track.solo_safe {
            "Solo. Ctrl-click to solo more than one track.\nSolo-safe: soloing other tracks \
             doesn't silence this one."
        } else {
            "Solo. Ctrl-click to solo more than one track."
        };
        let solo = toggle(
            ui,
            Face::Text("S"),
            track.solo,
            theme.solo,
            &format!("Solo {name}"),
            size,
            theme,
        )
        .on_hover_text(hint);
        if solo.clicked() {
            let solo = !track.solo;
            // As in Live, a click solos the track alone, and Ctrl-click
            // (Cmd-click on a Mac) solos it along with the others.
            let mods = modifiers(ui.input(|input| input.modifiers));
            let action = self.mouse.action(Target::SoloButton, Gesture::Click, mods);
            let solo = Change::Solo {
                track: id,
                solo,
                exclusive: solo && action != Some(MouseAction::AddSolo),
            };
            mix(actions, solo);
        }
        if matches!(track.kind, TrackKind::Audio | TrackKind::Midi) {
            let arm = toggle(
                ui,
                Face::Dot,
                track.armed,
                theme.arm,
                &format!("Arm {name}"),
                size,
                theme,
            )
            .on_hover_text("Arm for recording");
            if arm.clicked() {
                mix(actions, Change::Armed(id, !track.armed));
            }
        }
    }

    /// The fader beside the meter and its scale. Returns the fader, and the
    /// gain it was moved to.
    fn channel(&self, ui: &mut Ui, gain: f32, level: Level, name: &str) -> (Response, Option<f32>) {
        let sizes = self.sizes();
        let size = vec2(sizes.inner(), self.fader);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Min), |ui| {
            ui.set_min_size(size);
            let gap = sizes.gap - 1.0;
            ui.spacing_mut().item_spacing.x = gap;
            let width = sizes.cap.x + sizes.meter + sizes.scale + 2.0 * gap;
            ui.add_space(((sizes.inner() - width) / 2.0).max(0.0));
            let moved = self.fader(ui, gain, &format!("{name} Volume"));
            self.meter(ui, level, &format!("{name} Level"));
            self.scale(ui);
            moved
        })
        .inner
    }

    /// The gain field and the highest peak under the fader. A gain typed or
    /// dragged into the field sets the channel's.
    fn readouts(&self, ui: &mut Ui, channel: &Channel<'_>, actions: &mut Vec<Action>) {
        let sizes = self.sizes();
        let size = vec2((sizes.inner() - sizes.gap) / 2.0, sizes.row - 2.0);
        let (own, _) = channel.gain_and_pan(self.project);
        let gain = self.shown(ParamAddress::Volume(channel.chain()), own);
        moved(gain_field(ui, gain, size), actions, |gain| {
            channel.gain(gain)
        });
        let label = format!("{} Highest Peak", channel.name());
        let peak = self.peak_readout(ui, channel.level().highest, &label, size);
        if peak.clicked() {
            actions.push(Action::ClearPeaks);
        }
    }

    /// A vertical fader for `gain`, from silence at the bottom to +6 dB at
    /// the top. Returns it, and the gain the user moved it to.
    fn fader(&self, ui: &mut Ui, gain: f32, label: &str) -> (Response, Option<f32>) {
        let cap = self.sizes().cap;
        let size = vec2(cap.x, self.fader);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let db = decibels_of(gain).max(FLOOR);
        let range = f64::from(FLOOR)..=f64::from(FADER_TOP);
        let mut moved =
            slider_info(ui, &response, label, f64::from(db), range).map(|db| gain_of(db as f32));
        let at = height_of(gain, FADER_TOP);
        let (bottom, top) = self.travel(rect);
        if let Some(to) = self.dragged(ui, &response, at, bottom - top, true) {
            moved = Some(gain_at(to, FADER_TOP));
        }
        if response.double_clicked() {
            moved = Some(1.0);
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let visuals = ui.style().interact(&response);
            let x = rect.center().x;
            let groove = Rect::from_x_y_ranges(x - 2.0..=x + 2.0, top..=bottom);
            painter.rect_filled(groove, 2.0, ui.visuals().extreme_bg_color);
            let unity = self.y_at(rect, height_of(1.0, FADER_TOP));
            let mark = ui.visuals().widgets.noninteractive.fg_stroke;
            painter.hline(rect.x_range(), unity, Stroke::new(1.0, mark.color));
            let cap = Rect::from_center_size(pos2(x, self.y_at(rect, at)), cap);
            painter.rect(
                cap,
                self.theme.corner,
                visuals.bg_fill,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
            painter.hline(
                cap.shrink2(vec2(5.0, 0.0)).x_range(),
                cap.center().y,
                visuals.fg_stroke,
            );
        }
        let response = response.on_hover_text(format!(
            "Volume: {}\nDrag, or Ctrl-drag to move finely. Double-click for 0 dB.",
            decibels(gain)
        ));
        (response, moved.filter(|&new| new != gain))
    }

    /// A stereo meter: the peak in light colours, the RMS in solid ones.
    fn meter(&self, ui: &mut Ui, level: Level, label: &str) {
        let width = self.sizes().meter;
        let (rect, response) = ui.allocate_exact_size(vec2(width, self.fader), Sense::hover());
        let loudest = level.peak[0].max(level.peak[1]);
        response.widget_info(|| WidgetInfo {
            value: Some(f64::from(decibels_of(loudest).max(FLOOR))),
            ..WidgetInfo::labeled(Role::Meter, true, label)
        });
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter();
        let (bottom, top) = self.travel(rect);
        let width = (width - 1.0) / 2.0;
        for c in 0..2 {
            let left = rect.left() + c as f32 * (width + 1.0);
            let x = Rangef::new(left, left + width);
            let bar = Rect::from_x_y_ranges(x, top..=bottom);
            painter.rect_filled(bar, 1.0, ui.visuals().extreme_bg_color);
            let (bottom, top) = (self.y_at(rect, 0.0), self.y_at(rect, 1.0));
            let column = Rect::from_x_y_ranges(x, top..=bottom);
            paint_level(painter, column, level.peak[c], 0.4, self.theme);
            paint_level(painter, column, level.rms[c], 1.0, self.theme);
            let peak = meter_height(level.peak[c]);
            if peak > 0.0 {
                let colour = meter_colour(peak, self.theme);
                painter.hline(x, self.y_at(rect, peak), Stroke::new(1.5, colour));
            }
        }
    }

    /// The meters' scale in dB, beside a meter.
    fn scale(&self, ui: &mut Ui) {
        let size = vec2(self.sizes().scale, self.fader);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter();
        let color = ui.visuals().weak_text_color();
        for db in MARKS {
            let y = self.y_at(rect, height_of(gain_of(db), FADER_TOP));
            let text = if db > 0.0 {
                format!("+{db}")
            } else {
                format!("{db}")
            };
            painter.text(
                pos2(rect.left(), y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(self.sizes().scale_text),
                color,
            );
        }
    }

    /// The y coordinates of the bottom and the top of a fader's travel in
    /// `rect`. A meter beside it reads on the same scale.
    fn travel(&self, rect: Rect) -> (f32, f32) {
        let cap = self.sizes().cap;
        (rect.bottom() - cap.y / 2.0, rect.top() + cap.y / 2.0)
    }

    /// The y coordinate `height` of the way up the travel in `rect`.
    fn y_at(&self, rect: Rect, height: f32) -> f32 {
        let (bottom, top) = self.travel(rect);
        bottom + (top - bottom) * height
    }

    /// A send's level, from silence at the left to 0 dB at the right, with
    /// `name` on it, the return track's, if it has one: the send to `to`.
    /// Returns it, and the level the user moved it to.
    fn send_control(
        &self,
        ui: &mut Ui,
        level: f32,
        to: &str,
        name: Option<&str>,
        label: &str,
        width: f32,
    ) -> (Response, Option<f32>) {
        let bar = Bar {
            top: SEND_TOP,
            reset: 0.0,
            name,
            label,
            width,
        };
        let control = self.level_bar(ui, level, bar);
        let response = control.0.on_hover_text(format!(
            "Send to {to}: {}\nDrag, or Ctrl-drag to move finely. Double-click to silence it.",
            decibels(level)
        ));
        (response, control.1)
    }

    /// A channel's volume on its side, as the Routing panel shows a track's
    /// output: from silence at the left to +6 dB at the right, as its fader
    /// goes, with a mark at 0 dB.
    fn volume_bar(
        &self,
        ui: &mut Ui,
        gain: f32,
        label: &str,
        width: f32,
    ) -> (Response, Option<f32>) {
        let bar = Bar {
            top: FADER_TOP,
            reset: 1.0,
            name: None,
            label,
            width,
        };
        let control = self.level_bar(ui, gain, bar);
        let response = control.0.on_hover_text(format!(
            "Volume: {}\nDrag, or Ctrl-drag to move finely. Double-click for 0 dB.",
            decibels(gain)
        ));
        (response, control.1)
    }

    /// A level that moves sideways, `bar` says how, filled as far as
    /// `level`, with its name, if it has one, and its value on it. A
    /// double-click sets it to the bar's reset. Returns it, and the level
    /// the user moved it to.
    fn level_bar(&self, ui: &mut Ui, level: f32, bar: Bar<'_>) -> (Response, Option<f32>) {
        let top = bar.top;
        let size = vec2(bar.width, self.sizes().row - 4.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let db = decibels_of(level).max(FLOOR);
        let range = f64::from(FLOOR)..=f64::from(top);
        let mut moved = slider_info(ui, &response, bar.label, f64::from(db), range)
            .map(|db| gain_of(db as f32));
        let at = height_of(level, top);
        if let Some(to) = self.dragged(ui, &response, at, rect.width(), false) {
            moved = Some(gain_at(to, top));
        }
        if response.double_clicked() {
            moved = Some(bar.reset);
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            let visuals = ui.style().interact(&response);
            painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
            let mut filled = rect;
            filled.set_width(rect.width() * at);
            painter.rect_filled(filled, 2.0, ui.visuals().selection.bg_fill);
            if top > 0.0 {
                let unity = rect.left() + rect.width() * height_of(1.0, top);
                let mark = ui.visuals().widgets.noninteractive.fg_stroke;
                painter.vline(unity, rect.y_range(), Stroke::new(1.0, mark.color));
            }
            let font = FontId::proportional(self.sizes().small_text);
            let color = visuals.text_color();
            let value = painter.text(
                rect.right_center() - vec2(3.0, 0.0),
                Align2::RIGHT_CENTER,
                db_text(decibels_of(level)),
                font.clone(),
                color,
            );
            if let Some(name) = bar.name {
                let room = Rect::from_x_y_ranges(rect.left()..=value.left() - 2.0, rect.y_range());
                painter.with_clip_rect(room).text(
                    rect.left_center() + vec2(3.0, 0.0),
                    Align2::LEFT_CENTER,
                    name,
                    font,
                    color,
                );
            }
            painter.rect_stroke(rect, 2.0, visuals.bg_stroke, StrokeKind::Inside);
        }
        (response, moved.filter(|&new| new != level))
    }

    /// The pan, from hard left to hard right. Returns it, and the pan the
    /// user moved it to.
    fn pan_control(&self, ui: &mut Ui, pan: f32, label: &str) -> (Response, Option<f32>) {
        let size = vec2(self.sizes().inner(), self.sizes().row - 4.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        // Screen readers set it from 50 left to 50 right, as it reads.
        let range = -50.0..=50.0;
        let mut moved = slider_info(ui, &response, label, f64::from(pan * 50.0), range)
            .map(|pan| pan as f32 / 50.0);
        let at = (pan + 1.0) / 2.0;
        if let Some(to) = self.dragged(ui, &response, at, rect.width(), false) {
            moved = Some(to * 2.0 - 1.0);
        }
        if response.double_clicked() {
            moved = Some(0.0);
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let visuals = ui.style().interact(&response);
            painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
            let centre = rect.center().x;
            let x = rect.left() + rect.width() * at.clamp(0.0, 1.0);
            let filled =
                Rect::from_x_y_ranges(Rangef::new(centre.min(x), centre.max(x)), rect.y_range());
            painter.rect_filled(filled, 0.0, ui.visuals().selection.bg_fill);
            painter.vline(
                centre,
                rect.y_range(),
                Stroke::new(1.0, visuals.fg_stroke.color),
            );
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                pan_text(pan),
                FontId::proportional(self.sizes().small_text),
                visuals.text_color(),
            );
            painter.rect_stroke(rect, 2.0, visuals.bg_stroke, StrokeKind::Inside);
        }
        let response = response.on_hover_text(format!(
            "Pan: {}\nDrag, or Ctrl-drag to move finely. Double-click to centre it.",
            pan_text(pan)
        ));
        (response, moved.filter(|&new| new != pan))
    }

    /// The highest peak since the peaks were cleared, red once it's over
    /// full scale. Clicking it clears them.
    fn peak_readout(&self, ui: &mut Ui, highest: f32, label: &str, size: Vec2) -> Response {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let text = db_text(decibels_of(highest));
        response.widget_info(|| WidgetInfo {
            current_text_value: Some(text.clone()),
            ..WidgetInfo::labeled(Role::Button, true, label)
        });
        if ui.is_rect_visible(rect) {
            let over = highest > 1.0;
            let visuals = ui.style().interact(&response);
            let fill = if over {
                self.theme.meter_over
            } else {
                ui.visuals().extreme_bg_color
            };
            let ink = if over {
                self.theme.ink
            } else {
                visuals.text_color()
            };
            let painter = ui.painter();
            painter.rect(rect, 2.0, fill, visuals.bg_stroke, StrokeKind::Inside);
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                &text,
                FontId::monospace(self.sizes().readout_text),
                ink,
            );
        }
        response.on_hover_text(
            "The highest peak since the peaks were cleared. Red: the channel went over full \
             scale.\nClick to clear every channel's peak.",
        )
    }

    /// The clip stage's type: a menu of them. Returns the type the user
    /// picked.
    fn clip_menu(
        &self,
        ui: &mut Ui,
        id: impl AsIdSalt,
        clip_type: ClipType,
        label: String,
    ) -> Option<ClipType> {
        let text = format!("Clip: {}", clip_name(clip_type));
        let mut picked = None;
        let menu = ComboBox::from_id_salt(id)
            .width(self.sizes().inner())
            .selected_text(text.as_str())
            .show_ui(ui, |ui| {
                for option in [ClipType::Off, ClipType::Hard, ClipType::Soft] {
                    let item = ui
                        .selectable_label(option == clip_type, clip_name(option))
                        .on_hover_text(clip_hint(option));
                    if item.clicked() && option != clip_type {
                        picked = Some(option);
                    }
                }
            });
        name_menu(&menu.response, label, &text);
        menu.response.on_hover_text(
            "The clip stage after the fader, for a channel that goes over full scale",
        );
        picked
    }

    /// Where the user dragged a control to, from `at` of the way along its
    /// travel, which is `length` points long: up for `vertical` controls,
    /// right for the others. The mouse map's fine drag, Ctrl-drag (Cmd on a
    /// Mac) in Live's, moves it a tenth as far, for small changes.
    fn dragged(
        &self,
        ui: &Ui,
        response: &Response,
        at: f32,
        length: f32,
        vertical: bool,
    ) -> Option<f32> {
        if !response.dragged() || length <= 0.0 {
            return None;
        }
        let delta = response.drag_delta();
        let moved = if vertical { -delta.y } else { delta.x };
        if moved == 0.0 {
            return None;
        }
        let mods = modifiers(ui.input(|input| input.modifiers));
        let action = self.mouse.action(Target::MixerControl, Gesture::Drag, mods);
        let scale = if action == Some(MouseAction::Fine) {
            0.1
        } else {
            1.0
        };
        Some((at + moved / length * scale).clamp(0.0, 1.0))
    }
}

/// Fills the meter bar `column` up to `gain`, in the colours of the scale's
/// zones, `alpha` opaque.
fn paint_level(painter: &Painter, column: Rect, gain: f32, alpha: f32, theme: &Theme) {
    let at = meter_height(gain);
    let zones = [
        (height_of(gain_of(LOUD), FADER_TOP), theme.meter_quiet),
        (height_of(1.0, FADER_TOP), theme.meter_loud),
        (1.0, theme.meter_over),
    ];
    let y_at = |height: f32| column.bottom() - column.height() * height;
    let mut from = 0.0;
    for (to, color) in zones {
        let end = at.min(to);
        if end > from {
            let y = Rangef::new(y_at(end), y_at(from));
            painter.rect_filled(
                Rect::from_x_y_ranges(column.x_range(), y),
                0.0,
                color.gamma_multiply(alpha),
            );
        }
        from = to;
    }
}

/// What a control for `param` shows: the value its automation gives it at
/// `playhead`, unless someone overrode that, or else `own`, its own value.
pub(crate) fn automated_value(
    project: &Project,
    param: ParamAddress,
    playhead: Beats,
    own: f64,
) -> f64 {
    match project.lane(param) {
        Some(lane) if !lane.overridden => lane
            .value_at(playhead, project.param_scale(param))
            .unwrap_or(own),
        _ => own,
    }
}

/// A dot at the top left of the control `response` if `param` has
/// automation: in the automation's colour while it follows it, and grey
/// once someone overrode it.
pub(crate) fn automation_mark(
    ui: &Ui,
    theme: &Theme,
    project: &Project,
    response: &Response,
    param: ParamAddress,
) {
    let Some(lane) = project.lane(param) else {
        return;
    };
    let colour = match lane.overridden {
        true => theme.automation_off,
        false => theme.automation,
    };
    let radius = theme.point;
    let at = response.rect.left_top() + vec2(radius, radius);
    ui.painter().circle_filled(at, radius, colour);
}

/// The commands for `param`'s automation, for a control's menu: showing its
/// lane, re-enabling it once someone overrode it, and clearing it.
pub(crate) fn automation_items(
    ui: &mut Ui,
    project: &Project,
    param: ParamAddress,
    actions: &mut Vec<Action>,
) {
    let lane = project.lane(param);
    let items = [
        (automation::SHOW_LANE, "Show Automation", true),
        (
            automation::RE_ENABLE,
            "Re-Enable Automation",
            lane.is_some_and(|lane| lane.overridden),
        ),
        (automation::CLEAR, "Clear Automation", lane.is_some()),
    ];
    for (command, title, enabled) in items {
        if ui.add_enabled(enabled, egui::Button::new(title)).clicked() {
            let args = Args::new().with("param", param);
            actions.push(Action::RunWith(command, args));
            ui.close();
        }
    }
}

/// The colour of a meter at `height` of the way up: the colour of the
/// scale's zone there.
pub(crate) fn meter_colour(height: f32, theme: &Theme) -> Color32 {
    if height < height_of(gain_of(LOUD), FADER_TOP) {
        theme.meter_quiet
    } else if height <= height_of(1.0, FADER_TOP) {
        theme.meter_loud
    } else {
        theme.meter_over
    }
}

/// How far up a meter shows `gain`, from 0.0 to 1.0: nothing below the
/// floor.
pub(crate) fn meter_height(gain: f32) -> f32 {
    if decibels_of(gain) > FLOOR {
        height_of(gain, FADER_TOP)
    } else {
        0.0
    }
}

/// The gain in dB, to drag or type. Returns it, and the gain the user set.
fn gain_field(ui: &mut Ui, gain: f32, size: Vec2) -> (Response, Option<f32>) {
    let before = decibels_of(gain).max(FLOOR);
    let mut db = before;
    let field = ui
        .add_sized(
            size,
            DragValue::new(&mut db)
                .speed(0.1)
                .range(FLOOR..=FADER_TOP)
                .clamp_existing_to_range(false)
                .custom_formatter(|db, _| db_text(db as f32))
                .custom_parser(parse_decibels)
                // A typed gain applies on Enter, not at each digit.
                .update_while_editing(false),
        )
        .on_hover_text("Volume in dB: drag, or click and type");
    let typed = (field.changed() && db != before).then(|| gain_of(db));
    (field, typed)
}

fn clip_name(clip_type: ClipType) -> &'static str {
    match clip_type {
        ClipType::Off => "Off",
        ClipType::Hard => "Hard",
        ClipType::Soft => "Soft",
    }
}

fn clip_hint(clip_type: ClipType) -> &'static str {
    match clip_type {
        ClipType::Off => "No clipping: the signal passes as it is.",
        ClipType::Hard => "Cuts the signal off at full scale.",
        ClipType::Soft => {
            "Rounds off the signal above -6 dB, so it never quite reaches full scale."
        }
    }
}

/// Names the slider `response` for screen readers, and tells them its
/// value in `range`. Returns the value one asked it to take.
pub(crate) fn slider_info(
    ui: &Ui,
    response: &Response,
    label: &str,
    value: f64,
    range: RangeInclusive<f64>,
) -> Option<f64> {
    response.widget_info(|| WidgetInfo {
        value: Some(value),
        ..WidgetInfo::labeled(Role::Slider, true, label)
    });
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(*range.start());
        node.set_max_numeric_value(*range.end());
        node.add_action(Request::SetValue);
    });
    ui.input(|input| {
        input
            .accesskit_action_requests(response.id, Request::SetValue)
            .filter_map(|request| match request.data {
                Some(ActionData::NumericValue(value)) => Some(value),
                _ => None,
            })
            .last()
    })
    .filter(|value| value.is_finite())
    .map(|value| value.clamp(*range.start(), *range.end()))
}

/// What a menu calls a note wire's MIDI channel: "Channel 1" to "Channel
/// 16", or "Any Channel" for none of its own, which plays each note as the
/// instrument's own track's play.
pub(crate) fn midi_channel_name(channel: Option<u8>) -> String {
    match channel {
        Some(channel) => format!("Channel {}", channel + 1),
        None => "Any Channel".to_owned(),
    }
}

/// Names the combo box `response` for screen readers, showing `value`.
pub(crate) fn name_menu(response: &Response, label: String, value: &str) {
    response.widget_info(|| WidgetInfo {
        current_text_value: Some(value.to_owned()),
        ..WidgetInfo::labeled(Role::ComboBox, true, &label)
    });
}

/// The travel along the foot of a control at `rect` that moves sideways,
/// for what modulators do to it.
fn foot(rect: Rect) -> Travel {
    let y = rect.bottom() - 2.0;
    Travel {
        low: pos2(rect.left(), y),
        high: pos2(rect.right(), y),
    }
}

/// Where `gain` sits on a scale whose top is `top` dB: 0.0 for silence, 1.0
/// at the top. The fourth root spreads out the loud end as a console's
/// faders do: on a fader, 0 dB sits 84% of the way up, -20 dB at 47% and
/// -60 dB at 15%.
fn height_of(gain: f32, top: f32) -> f32 {
    (gain.max(0.0) / gain_of(top)).sqrt().sqrt().min(1.0)
}

/// The gain at `height` on a scale whose top is `top` dB.
fn gain_at(height: f32, top: f32) -> f32 {
    gain_of(top) * height.clamp(0.0, 1.0).powi(4)
}

/// The linear gain of `db` decibels: silence at or below the floor.
fn gain_of(db: f32) -> f32 {
    if db <= FLOOR {
        0.0
    } else {
        10_f32.powf(db / 20.0)
    }
}

/// A linear gain in decibels: negative infinity for silence.
fn decibels_of(gain: f32) -> f32 {
    20.0 * gain.log10()
}

/// A linear gain in decibels: "-6.0 dB".
pub(crate) fn decibels(gain: f32) -> String {
    if gain > 0.0 {
        let db = 20.0 * gain.log10();
        // No "-0.0 dB" for gains a hair under unity.
        format!("{:.1} dB", if db.abs() < 0.05 { 0.0 } else { db })
    } else {
        "-inf dB".to_owned()
    }
}

/// Decibels for the narrow fields of a strip: "-6.0", "+1.5", "0.0", or
/// "-inf" at the floor and below.
fn db_text(db: f32) -> String {
    if db.is_nan() || db <= FLOOR {
        return "-inf".to_owned();
    }
    let rounded = (db * 10.0).round() / 10.0;
    if rounded == 0.0 {
        "0.0".to_owned()
    } else {
        format!("{rounded:+.1}")
    }
}

/// Decibels as typed into a gain field: "-6", "+1.5 dB" or "-inf".
fn parse_decibels(text: &str) -> Option<f64> {
    let text = text.trim().to_ascii_lowercase();
    let text = text.strip_suffix("db").unwrap_or(&text).trim();
    match text {
        "-inf" | "inf" => Some(f64::from(FLOOR)),
        _ => text.parse().ok().filter(|db: &f64| db.is_finite()),
    }
}

/// A pan as Live shows it: "C", or from "50L" to "50R".
pub(crate) fn pan_text(pan: f32) -> String {
    let amount = (pan.abs() * 50.0).round();
    if amount == 0.0 {
        "C".to_owned()
    } else if pan < 0.0 {
        format!("{amount}L")
    } else {
        format!("{amount}R")
    }
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Toggled;
    use egui::{CursorIcon, Modifiers};
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{
        CommandError, CommandId, Context, Document, Named, NoLibrary, Request, mixer,
    };
    use musicable_model::automation::{Lane, Point};
    use musicable_model::modulation::ModulatorKind;
    use musicable_model::param::Scale;
    use musicable_model::plugin::{PluginDevice, PluginKind, PluginSettings};
    use musicable_model::routing::Port;
    use musicable_model::{DeviceKind, PluginFormat};

    use super::*;
    use crate::testing::{Player, cursor_at};

    /// The mixer, on a document its changes go to and its commands run on,
    /// as the app would make and run them, and the actions it handed back.
    struct State {
        document: Document,
        commands: musicable_commands::Registry,
        player: Player,
        levels: Levels,
        performance: Performance,
        inputs: Vec<InputChoice>,
        midi: Vec<MidiChoice>,
        playhead: Beats,
        parts: Parts,
        actions: Vec<Action>,
        requests: Vec<Request>,
        errors: Vec<CommandError>,
    }

    fn harness(project: Project) -> Harness<'static, State> {
        harness_with(project, Levels::default())
    }

    fn harness_with(project: Project, levels: Levels) -> Harness<'static, State> {
        // A four-channel interface.
        let interface = InputChoice {
            id: "interface".to_owned(),
            name: "Interface".to_owned(),
            channels: 4,
        };
        let keystep = MidiChoice {
            id: "keystep".to_owned(),
            name: "Keystep".to_owned(),
        };
        let mut commands = musicable_commands::Registry::new();
        musicable_commands::register_all(&mut commands);
        let state = State {
            document: Document::new(project),
            commands,
            player: Player::default(),
            levels,
            performance: Performance::default(),
            inputs: vec![interface],
            midi: vec![keystep],
            playhead: Beats::ZERO,
            parts: Parts::default(),
            actions: Vec::new(),
            requests: Vec::new(),
            errors: Vec::new(),
        };
        // Frames as quick as a screen's, so two clicks make a double click,
        // and enough of them for a tooltip to settle.
        let builder = Harness::builder().with_step_dt(1.0 / 60.0);
        let builder = builder.with_max_steps(40).with_size([900.0, 600.0]);
        builder.build_ui_state(
            |ui, state: &mut State| {
                let mut actions = Vec::new();
                let (theme, mouse) = (Theme::default(), MouseMap::live());
                let project = state.document.project();
                let inputs = Choices {
                    audio: &state.inputs,
                    midi: &state.midi,
                    plugins: None,
                };
                show(
                    ui,
                    project,
                    state.document.editor(),
                    &state.levels,
                    &state.performance,
                    inputs,
                    state.playhead,
                    &theme,
                    &mouse,
                    &state.parts,
                    &mut actions,
                );
                state.parts.carry_out();
                for action in &actions {
                    match action {
                        Action::Mixer(change) => {
                            if let Err(error) = mixer::change(&mut state.document, change.clone()) {
                                state.errors.push(error);
                            }
                        }
                        Action::BeginGesture => state.document.begin_gesture(),
                        Action::EndGesture => state.document.end_gesture(),
                        Action::RunWith(id, args) => {
                            let mut ctx = Context {
                                document: &mut state.document,
                                transport: &mut state.player,
                                requests: &mut state.requests,
                                library: &mut NoLibrary,
                            };
                            if let Err(error) = state.commands.run_with(id, args, &mut ctx) {
                                state.errors.push(error);
                            }
                        }
                        _ => {}
                    }
                }
                state.actions.extend(actions);
            },
            state,
        )
    }

    /// Lead, an audio track at -6 dB with a send to the reverb; Keys, a MIDI
    /// track feeding the drums group; and a reverb return.
    fn project() -> (Project, [TrackId; 4]) {
        let mut project = Project::new();
        let lead = project.add_track("Lead", TrackKind::Audio);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let drums = project.add_track("Drums", TrackKind::Group);
        let reverb = project.add_track("Reverb", TrackKind::Return);
        let send = project.new_send(reverb);
        let track = project.track_mut(lead).unwrap();
        track.gain = 0.5;
        track.sends.push(send);
        project.track_mut(keys).unwrap().output = Bus::Group(drums);
        (project, [lead, keys, drums, reverb])
    }

    fn mixed(harness: &Harness<'_, State>) -> Vec<Change> {
        let actions = harness.state().actions.iter();
        actions
            .filter_map(|action| match action {
                Action::Mixer(change) => Some(change.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_track_has_a_strip() {
        let (project, _) = project();
        let harness = harness(project);
        for name in ["Lead", "Keys", "Drums", "Reverb", "Master"] {
            let label = format!("{name} Volume");
            let volume = harness.get_by_role_and_label(Role::Slider, &label);
            let db = volume.accesskit_node().numeric_value().unwrap();
            let expected = if name == "Lead" { -6.02 } else { 0.0 };
            assert!((db - expected).abs() < 0.01, "{name}: {db}");
            let label = format!("{name} Level");
            let meter = harness.get_by_role_and_label(Role::Meter, &label);
            assert_eq!(
                meter.accesskit_node().numeric_value(),
                Some(f64::from(FLOOR))
            );
            let label = format!("{name} Pan");
            let pan = harness.get_by_role_and_label(Role::Slider, &label);
            assert_eq!(pan.accesskit_node().numeric_value(), Some(0.0));
            let label = format!("{name} Clip Type");
            let clip = harness.get_by_role_and_label(Role::ComboBox, &label);
            assert_eq!(clip.value().as_deref(), Some("Clip: Off"));
        }
        // Every track but the return can send to it.
        for name in ["Lead", "Keys", "Drums"] {
            harness.get_by_role_and_label(Role::Slider, &format!("{name} Send to Reverb"));
        }
        assert!(harness.query_by_label("Reverb Send to Reverb").is_none());
        // Audio and MIDI tracks can be armed.
        for (name, armable) in [("Lead", true), ("Keys", true), ("Drums", false)] {
            let label = format!("Arm {name}");
            assert_eq!(harness.query_by_label(&label).is_some(), armable, "{name}");
        }
        assert!(harness.query_by_label("Arm Reverb").is_none());
        // Every track chooses where its output goes, and a MIDI track
        // without an instrument where it plays its notes.
        let output = |name: &str| {
            let label = format!("{name} Output");
            harness
                .get_by_role_and_label(Role::ComboBox, &label)
                .value()
        };
        assert_eq!(output("Lead").as_deref(), Some("Master"));
        assert_eq!(output("Reverb").as_deref(), Some("Master"));
        assert!(harness.query_by_label("Keys Output").is_none());
        let midi_to = harness.get_by_role_and_label(Role::ComboBox, "Keys MIDI To");
        assert_eq!(midi_to.value().as_deref(), Some("No Output"));
        assert!(harness.query_by_label_contains("No tracks yet").is_none());
    }

    #[test]
    fn without_tracks_it_says_how_to_add_one() {
        let mut harness = harness(Project::new());
        let words = harness.get_by_label_contains("No tracks yet").rect();
        harness.get_by_role_and_label(Role::Slider, "Master Volume");
        // The pointer turns to the text cursor over the words, and not over
        // the room around them.
        assert_eq!(cursor_at(&mut harness, words.center()), CursorIcon::Text);
        let corner = pos2(20.0, 20.0);
        assert_eq!(cursor_at(&mut harness, corner), CursorIcon::Default);
    }

    #[test]
    fn hidden_parts_leave_the_faders_their_room() {
        let (project, _) = project();
        let mut harness = harness(project);
        let fader = |harness: &Harness<'_, State>| {
            let fader = harness.get_by_role_and_label(Role::Slider, "Lead Volume");
            fader.rect().height()
        };
        let before = fader(&harness);
        harness
            .get_by_role_and_label(Role::ComboBox, "Lead Output")
            .click_secondary();
        harness.run();
        harness.get_by_label("Hide Output").click();
        harness.run();
        // Every strip loses the row, and its fader grows into it.
        for name in ["Lead", "Keys", "Drums", "Reverb"] {
            let label = format!("{name} Output");
            assert!(harness.query_by_label(&label).is_none(), "{name}");
        }
        let sizes = Theme::default().mixer;
        let grown = fader(&harness) - before;
        assert!((grown - sizes.row - sizes.gap).abs() < 0.5, "{grown}");
        // The faders still line up.
        let top = |label: &str| {
            harness
                .get_by_role_and_label(Role::Slider, label)
                .rect()
                .top()
        };
        assert_eq!(top("Lead Volume"), top("Master Volume"));
    }

    #[test]
    fn the_controls_change_the_mix() {
        let (project, [lead, keys, drums, reverb]) = project();
        let mut harness = harness(project);
        let slider = |harness: &Harness<'_, State>, label: &str, value: f64| {
            let node = harness.get_by_role_and_label(Role::Slider, label);
            node.set_value_accesskit(value);
        };
        slider(&harness, "Lead Volume", -12.0);
        harness.run();
        slider(&harness, "Keys Pan", -25.0);
        harness.run();
        slider(&harness, "Keys Send to Reverb", 0.0);
        harness.run();
        slider(&harness, "Master Volume", 6.0);
        harness.run();
        for label in ["Mute Lead", "Arm Keys", "Pre-Fader Lead Send to Reverb"] {
            harness.get_by_label(label).click();
            harness.run();
        }
        harness
            .get_by_role_and_label(Role::ComboBox, "Lead Output")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Drums").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::ComboBox, "Reverb Clip Type")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Soft").click();
        harness.run();

        let changes = mixed(&harness);
        let [
            Change::Gain(id, gain),
            Change::Pan(pan_id, pan),
            ref level,
            Change::MasterGain(master),
        ] = changes[..4]
        else {
            panic!("{changes:?}");
        };
        assert_eq!(id, lead);
        assert!((gain - 0.251).abs() < 0.001, "{gain}");
        assert_eq!((pan_id, pan), (keys, -0.5));
        let full = Change::SendLevel {
            track: keys,
            to: reverb,
            level: 1.0,
        };
        assert_eq!(*level, full);
        assert!((master - 1.995).abs() < 0.001, "{master}");
        let pre = Change::SendPreFader {
            track: lead,
            to: reverb,
            pre_fader: true,
        };
        let rest = [
            Change::Muted(lead, true),
            Change::Armed(keys, true),
            pre,
            Change::Output(lead, Bus::Group(drums)),
            Change::ClipType(reverb, ClipType::Soft),
        ];
        assert_eq!(changes[4..], rest);
        // The mixer shows what changed.
        let mute = harness.get_by_label("Mute Lead").accesskit_node().toggled();
        assert_eq!(mute, Some(Toggled::True));
        let keys = harness.get_by_role_and_label(Role::Slider, "Keys Pan");
        assert_eq!(keys.accesskit_node().numeric_value(), Some(-25.0));
    }

    #[test]
    fn dragging_a_fader_is_one_gesture() {
        let (project, [lead, ..]) = project();
        let mut harness = harness(project);
        let fader = harness.get_by_role_and_label(Role::Slider, "Lead Volume");
        let at = fader.rect().center();
        harness.hover_at(at);
        harness.run();
        harness.drag_at(at);
        harness.run();
        for y in 1..=3 {
            harness.hover_at(at - vec2(0.0, 10.0 * y as f32));
            harness.run();
        }
        harness.drop_at(at - vec2(0.0, 30.0));
        harness.run();

        let actions = &harness.state().actions;
        assert_eq!(actions.first(), Some(&Action::BeginGesture));
        assert_eq!(actions.last(), Some(&Action::EndGesture));
        let gains: Vec<f32> = actions[1..actions.len() - 1]
            .iter()
            .map(|action| match action {
                Action::Mixer(Change::Gain(id, gain)) if *id == lead => *gain,
                other => panic!("{other:?} in the middle of a drag"),
            })
            .collect();
        assert!(gains.len() > 1 && gains.is_sorted(), "{gains:?}");
        assert!(gains[0] > 0.5);
        // One undo takes the whole drag back.
        let document = &mut harness.state_mut().document;
        assert_eq!(document.undo_name(), Some("Volume"));
        assert!(document.undo());
        assert_eq!(document.project().track(lead).unwrap().gain, 0.5);
        assert_eq!(document.undo_name(), None);
    }

    #[test]
    fn a_click_solos_one_track_and_ctrl_click_another() {
        let (project, [lead, keys, ..]) = project();
        let mut harness = harness(project);
        harness.get_by_label("Solo Lead").click();
        harness.run();
        harness
            .get_by_label("Solo Keys")
            .click_modifiers(Modifiers::COMMAND);
        harness.run();
        let solo = |track, exclusive| Change::Solo {
            track,
            solo: true,
            exclusive,
        };
        assert_eq!(mixed(&harness), [solo(lead, true), solo(keys, false)]);
        let soloed = |harness: &Harness<'_, State>, track| {
            let project = harness.state().document.project();
            project.track(track).unwrap().solo
        };
        assert!(soloed(&harness, lead) && soloed(&harness, keys));
        // Clicking a soloed track unsolos just that one.
        harness.get_by_label("Solo Lead").click();
        harness.run();
        assert!(!soloed(&harness, lead) && soloed(&harness, keys));
    }

    #[test]
    fn clicking_a_strip_selects_its_track_and_lights_it() {
        let (project, [lead, keys, ..]) = project();
        let mut harness = harness(project);
        let lit = |harness: &Harness<'_, State>, name: &str| {
            let label = format!("{name} Strip");
            let strip = harness.get_by_label(&label);
            strip.accesskit_node().toggled() == Some(Toggled::True)
        };
        let selected = |harness: &Harness<'_, State>| {
            let editor = harness.state().document.editor();
            let tracks: Vec<_> = editor.selection.tracks.iter().copied().collect();
            (editor.selected_track, tracks)
        };
        // Its name selects its track, for the devices panel to show.
        harness.get_by_label("Lead").click();
        harness.run();
        assert_eq!(selected(&harness), (Some(Chain::Track(lead)), vec![lead]));
        assert!(lit(&harness, "Lead") && !lit(&harness, "Keys"));
        // Ctrl-click adds another, as in the arrangement's headers.
        harness
            .get_by_label("Keys")
            .click_modifiers(Modifiers::COMMAND);
        harness.run();
        assert_eq!(
            selected(&harness),
            (Some(Chain::Track(keys)), vec![lead, keys])
        );
        assert!(lit(&harness, "Lead") && lit(&harness, "Keys"));
        // So does a click where it has no control, and the master's selects
        // the master, alone.
        let strip = harness.get_by_label("Master Strip").rect();
        let at = pos2(strip.center().x, strip.bottom() - 1.0);
        harness.hover_at(at);
        harness.run();
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        harness.run();
        assert_eq!(selected(&harness), (Some(Chain::Master), vec![]));
        assert!(lit(&harness, "Master") && !lit(&harness, "Lead"));
        // The controls keep their clicks.
        harness.get_by_label("Solo Keys").click();
        harness.run();
        assert_eq!(selected(&harness).0, Some(Chain::Master));
        // A double click on a track's name renames it, once the clicks before
        // are long enough ago not to make it a triple click.
        let at = harness.get_by_label("Keys").rect().center();
        harness.hover_at(at);
        harness.run_steps(60);
        for pressed in [true, false, true, false] {
            harness.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        harness.run();
        let renamed = Request::Rename(Named::Track(keys));
        assert!(harness.state().requests.contains(&renamed));
        assert!(harness.state().errors.is_empty());
    }

    #[test]
    fn the_strips_menus_delete_tracks_and_sends() {
        let (project, [lead, keys, ..]) = project();
        let mut harness = harness(project);
        harness.get_by_label("Keys").click_secondary();
        harness.run();
        harness.get_by_label("Solo-Safe").click();
        harness.run();
        harness
            .get_by_label("Lead Send to Reverb")
            .click_secondary();
        harness.run();
        harness.get_by_label("Remove Send").click();
        harness.run();
        harness.get_by_label("Lead").click_secondary();
        harness.run();
        harness.get_by_label("Delete Track").click();
        harness.run();
        let reverb = harness.state().document.project().tracks[2].id;
        let expected = [
            Change::SoloSafe(keys, true),
            Change::RemoveSend {
                track: lead,
                to: reverb,
            },
            Change::Delete(lead),
        ];
        assert_eq!(mixed(&harness), expected);
        assert!(harness.query_by_label("Lead").is_none());
    }

    #[test]
    fn a_strips_menu_sets_its_track_always_live() {
        let (project, [_, keys, ..]) = project();
        let mut harness = harness(project);
        let always_live = |harness: &Harness<'_, State>| {
            let project = harness.state().document.project();
            project.track(keys).unwrap().always_live
        };
        for on in [true, false] {
            harness.get_by_label("Keys").click_secondary();
            harness.run();
            let item = harness.get_by_label("Always Live");
            let toggled = if on { Toggled::False } else { Toggled::True };
            assert_eq!(item.accesskit_node().toggled(), Some(toggled));
            item.click();
            harness.run();
            assert_eq!(always_live(&harness), on);
        }
        assert!(harness.state().errors.is_empty());
    }

    #[test]
    fn automated_controls_show_their_automation_and_its_commands() {
        let (mut project, [lead, ..]) = project();
        let volume = ParamAddress::Volume(Chain::Track(lead));
        let send = ParamAddress::Send(project.track(lead).unwrap().sends[0].id);
        let pan = ParamAddress::Pan(Chain::Master);
        let lane = |param, points: &[(i64, f64)]| {
            let points = points.iter().map(|&(at, v)| Point::new(Beats::new(at), v));
            Lane::new(param, points.collect())
        };
        project.automation = vec![
            lane(volume, &[(0, 1.0), (8, 0.25)]),
            lane(send, &[(0, 0.25)]),
            lane(pan, &[(0, -1.0), (8, 0.0)]),
        ];
        let mut harness = harness(project);
        let value = |harness: &Harness<'_, State>, label: &str| {
            let node = harness.get_by_role_and_label(Role::Slider, label);
            node.accesskit_node().numeric_value().unwrap()
        };
        // The controls show their automation's values at the playhead.
        harness.state_mut().playhead = Beats::new(4);
        harness.run();
        let project = harness.state().document.project();
        let at = project
            .lane(volume)
            .unwrap()
            .value_at(Beats::new(4), Scale::Fader);
        let db = 20.0 * at.unwrap().log10();
        assert!((value(&harness, "Lead Volume") - db).abs() < 0.01);
        assert!((value(&harness, "Lead Send to Reverb") + 12.04).abs() < 0.01);
        assert_eq!(value(&harness, "Master Pan"), -25.0);

        // Their menus show the lane, and clear the automation.
        let ran = |harness: &Harness<'_, State>, command: CommandId, param: ParamAddress| {
            let run = Action::RunWith(command, Args::new().with("param", param));
            harness.state().actions.contains(&run)
        };
        harness
            .get_by_role_and_label(Role::Slider, "Lead Volume")
            .click_secondary();
        harness.run();
        let re_enable = harness.get_by_label("Re-Enable Automation");
        assert!(re_enable.accesskit_node().is_disabled());
        harness.get_by_label("Show Automation").click();
        harness.run();
        assert!(ran(&harness, automation::SHOW_LANE, volume));
        harness
            .get_by_role_and_label(Role::Slider, "Master Pan")
            .click_secondary();
        harness.run();
        harness.get_by_label("Clear Automation").click();
        harness.run();
        assert!(ran(&harness, automation::CLEAR, pan));

        // Moved by hand, a control overrides its automation, and holds where
        // it was moved to until the automation is re-enabled.
        let node = harness.get_by_role_and_label(Role::Slider, "Lead Volume");
        node.set_value_accesskit(-12.0);
        harness.run();
        let project = harness.state().document.project();
        assert!(project.lane(volume).unwrap().overridden);
        assert!((value(&harness, "Lead Volume") + 12.0).abs() < 0.01);
        harness
            .get_by_role_and_label(Role::Slider, "Lead Volume")
            .click_secondary();
        harness.run();
        harness.get_by_label("Re-Enable Automation").click();
        harness.run();
        assert!(ran(&harness, automation::RE_ENABLE, volume));
    }

    #[test]
    fn the_meters_read_the_levels() {
        let (project, [lead, ..]) = project();
        let mut levels = Levels::default();
        let loud = Level {
            peak: [0.5, 0.25],
            rms: [0.25, 0.125],
            highest: 1.5,
        };
        levels.tracks.insert(lead, loud);
        levels.master.highest = 0.5;
        let mut harness = harness_with(project, levels);
        let meter = harness.get_by_role_and_label(Role::Meter, "Lead Level");
        let db = meter.accesskit_node().numeric_value().unwrap();
        assert!((db + 6.02).abs() < 0.01, "{db}");
        let peak = |harness: &Harness<'_, State>, name: &str| {
            let label = format!("{name} Highest Peak");
            harness.get_by_role_and_label(Role::Button, &label).value()
        };
        assert_eq!(peak(&harness, "Lead").as_deref(), Some("+3.5"));
        assert_eq!(peak(&harness, "Keys").as_deref(), Some("-inf"));
        assert_eq!(peak(&harness, "Master").as_deref(), Some("-6.0"));
        harness.get_by_label("Master Highest Peak").click();
        harness.run();
        assert_eq!(harness.state().actions, [Action::ClearPeaks]);
    }

    #[test]
    fn the_cpu_rows_read_each_channels_share_of_a_core() {
        let (project, [lead, keys, ..]) = project();
        let mut harness = harness(project);
        let cpu = |harness: &Harness<'_, State>, name: &str| {
            let label = format!("{name} CPU");
            harness.get_by_role_and_label(Role::Meter, &label).value()
        };
        // Without a device playing, there are no figures.
        assert_eq!(cpu(&harness, "Lead").as_deref(), Some("--"));
        let channels = &mut harness.state_mut().performance.channels;
        channels.insert(Chain::Track(lead), 0.3);
        channels.insert(Chain::Track(keys), 0.002);
        channels.insert(Chain::Master, 0.05);
        harness.run();
        assert_eq!(cpu(&harness, "Lead").as_deref(), Some("30%"));
        assert_eq!(cpu(&harness, "Keys").as_deref(), Some("<1%"));
        assert_eq!(cpu(&harness, "Master").as_deref(), Some("5%"));
        // The part menu hides the row, as it does the others.
        (harness.get_by_role_and_label(Role::Meter, "Lead CPU")).click_secondary();
        harness.run();
        harness.get_by_label("Hide CPU").click();
        harness.run();
        assert!(harness.query_by_label("Master CPU").is_none());
    }

    #[test]
    fn meters_move_until_every_reading_is_down() {
        let mut levels = Levels::default();
        assert!(!levels.moving());
        levels.master.rms[1] = 0.001;
        assert!(levels.moving());
        levels.master.rms[1] = gain_of(FLOOR - 1.0);
        levels.tracks.insert(TrackId(3), Level::default());
        assert!(!levels.moving());
        // The highest peak holds, so it doesn't count.
        levels.master.highest = 1.0;
        assert!(!levels.moving());
        levels.tracks.get_mut(&TrackId(3)).unwrap().peak[0] = 0.5;
        assert!(levels.moving());
    }

    #[test]
    fn faders_spread_out_the_loud_end() {
        let at = |db: f32| height_of(gain_of(db), FADER_TOP);
        assert!((at(0.0) - 0.841).abs() < 0.001, "{}", at(0.0));
        assert!((at(-20.0) - 0.473).abs() < 0.001, "{}", at(-20.0));
        assert!((at(-60.0) - 0.150).abs() < 0.001, "{}", at(-60.0));
        assert_eq!(at(FADER_TOP), 1.0);
        assert_eq!(at(12.0), 1.0);
        assert_eq!(height_of(0.0, FADER_TOP), 0.0);
        for height in [0.0, 0.1, 0.5, 0.84, 1.0] {
            let back = height_of(gain_at(height, SEND_TOP), SEND_TOP);
            assert!((back - height).abs() < 1e-6, "{height}: {back}");
        }
        assert_eq!(gain_at(1.0, SEND_TOP), 1.0);
        assert_eq!(gain_of(FLOOR), 0.0);
    }

    #[test]
    fn audio_tracks_choose_their_input_and_monitoring() {
        let (project, [lead, ..]) = project();
        let mut harness = harness(project);
        harness.run();
        let input = |harness: &Harness<'_, State>| {
            harness
                .get_by_role_and_label(Role::ComboBox, "Lead Input")
                .value()
        };
        // A new audio track takes the default input in stereo. Only audio
        // and MIDI tracks record.
        assert_eq!(input(&harness).as_deref(), Some("Default 1/2"));
        assert!(harness.query_by_label("Drums Input").is_none());
        assert!(harness.query_by_label("Drums Monitor In").is_none());

        harness
            .get_by_role_and_label(Role::ComboBox, "Lead Input")
            .click();
        harness.run();
        // Opening the menu asks for the inputs again, in case one was
        // plugged in.
        assert!(harness.state().actions.contains(&Action::ListInputs));
        // The interface's pairs, then its channels on their own.
        for label in [
            "Interface 1/2",
            "Interface 3/4",
            "Interface 1",
            "Interface 4",
        ] {
            assert!(harness.query_by_label(label).is_some(), "{label}");
        }
        assert!(harness.query_by_label("Interface 5").is_none());
        // The menu scrolls, as an interface's channels don't all fit.
        harness.get_by_label("Interface 3").scroll_to_me();
        harness.run();
        harness.get_by_label("Interface 3").click();
        harness.run();
        assert_eq!(input(&harness).as_deref(), Some("Interface 3"));
        let mono = AudioInput {
            device: "interface".to_owned(),
            name: "Interface".to_owned(),
            channel: 2,
            stereo: false,
        };

        // Auto, the default, is on: clicking it changes nothing.
        harness.get_by_label("Lead Monitor Auto").click();
        harness.run();
        harness.get_by_label("Lead Monitor In").click();
        harness.run();
        assert_eq!(
            mixed(&harness),
            [
                Change::Input(lead, Some(Input::Audio(mono))),
                Change::Monitor(lead, Monitor::In),
            ]
        );

        // An input that isn't plugged in keeps its name.
        harness.state_mut().inputs.clear();
        harness.run();
        assert_eq!(input(&harness).as_deref(), Some("Interface 3"));
        harness
            .get_by_role_and_label(Role::ComboBox, "Lead Input")
            .click();
        harness.run();
        // The menu is still scrolled down to the interface's channels.
        harness.get_by_label("No Input").scroll_to_me();
        harness.run();
        harness.get_by_label("No Input").click();
        harness.run();
        assert_eq!(input(&harness).as_deref(), Some("No Input"));
    }

    #[test]
    fn audio_tracks_take_another_tracks_sound_or_resample() {
        let (project, [lead, keys, ..]) = project();
        let mut harness = harness(project);
        harness.run();
        let open = |harness: &mut Harness<'_, State>| {
            harness
                .get_by_role_and_label(Role::ComboBox, "Lead Input")
                .click();
            harness.run();
        };
        let input = |harness: &Harness<'_, State>| {
            harness
                .get_by_role_and_label(Role::ComboBox, "Lead Input")
                .value()
        };
        open(&mut harness);
        // Lead sends to the reverb, so taking the reverb's sound would make
        // a loop.
        let reverb = harness.get_by_role_and_label(Role::Button, "Reverb");
        assert!(reverb.accesskit_node().is_disabled());
        // Keys is a MIDI track, whose Pre FX is notes, not sound.
        harness.get_by_role_and_label(Role::Button, "Keys").hover();
        harness.run();
        assert!(harness.query_by_label("Pre FX").is_none());
        harness.get_by_label("Post Fader");
        harness.get_by_label("Post FX").click();
        harness.run();
        assert_eq!(input(&harness).as_deref(), Some("from Keys, Post FX"));
        // It plays what it takes, which on Auto it would only while armed.
        let project = harness.state().document.project();
        assert_eq!(project.track(lead).unwrap().monitor, Monitor::In);

        // Resampling is at the bottom, after the tracks.
        open(&mut harness);
        harness.get_by_label("Resampling").scroll_to_me();
        harness.run();
        harness.get_by_label("Resampling").click();
        harness.run();
        assert_eq!(input(&harness).as_deref(), Some("Resampling"));
        let keys = Input::Tap(Tap::PostFx(Chain::Track(keys)));
        assert_eq!(
            mixed(&harness),
            [
                Change::Input(lead, Some(keys)),
                Change::Input(lead, Some(Input::RESAMPLING)),
            ]
        );
    }

    #[test]
    fn tracks_and_returns_can_feed_sends_only() {
        let (project, [lead, .., reverb]) = project();
        let mut harness = harness(project);
        harness.run();
        for name in ["Reverb", "Lead"] {
            let label = format!("{name} Output");
            harness
                .get_by_role_and_label(Role::ComboBox, &label)
                .click();
            harness.run();
            harness.get_by_label("Sends Only").click();
            harness.run();
            let output = harness.get_by_role_and_label(Role::ComboBox, &label);
            assert_eq!(output.value().as_deref(), Some("Sends Only"));
        }
        assert_eq!(
            mixed(&harness),
            [
                Change::Output(reverb, Bus::SendsOnly),
                Change::Output(lead, Bus::SendsOnly),
            ]
        );
    }

    #[test]
    fn midi_tracks_choose_their_midi_input_and_monitoring() {
        let (project, [_, keys, ..]) = project();
        let mut harness = harness(project);
        harness.run();
        let input = |harness: &Harness<'_, State>| {
            harness
                .get_by_role_and_label(Role::ComboBox, "Keys Input")
                .value()
        };
        // A new MIDI track takes every channel of every MIDI input.
        assert_eq!(input(&harness).as_deref(), Some("All Inputs"));
        harness
            .get_by_role_and_label(Role::ComboBox, "Keys Input")
            .click();
        harness.run();
        assert!(harness.state().actions.contains(&Action::ListInputs));
        for label in [
            "All Inputs, Ch. 1",
            "Keystep, All Channels",
            "Keystep, Ch. 16",
        ] {
            assert!(harness.query_by_label(label).is_some(), "{label}");
        }
        harness.get_by_label("Keystep, Ch. 10").scroll_to_me();
        harness.run();
        harness.get_by_label("Keystep, Ch. 10").click();
        harness.run();
        assert_eq!(input(&harness).as_deref(), Some("Keystep, Ch. 10"));
        harness.get_by_label("Keys Monitor Off").click();
        harness.run();
        let tenth = MidiInput {
            device: "keystep".to_owned(),
            name: "Keystep".to_owned(),
            channel: Some(9),
        };
        assert_eq!(
            mixed(&harness),
            [
                Change::Input(keys, Some(Input::Midi(tenth))),
                Change::Monitor(keys, Monitor::Off),
            ]
        );
        // A MIDI input that isn't plugged in keeps its name.
        harness.state_mut().midi.clear();
        harness.run();
        assert_eq!(input(&harness).as_deref(), Some("Keystep, Ch. 10"));
    }

    #[test]
    fn midi_tracks_take_another_midi_tracks_notes() {
        let (mut project, [_, keys, ..]) = project();
        let pad = project.add_track("Pad", TrackKind::Midi);
        let arp = project.new_device(DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.example.arp".into(),
            name: "Arp".into(),
            kind: PluginKind::MidiEffect,
            inputs: Vec::new(),
            note_output: false,
            note_timing: musicable_model::plugin::NoteTiming::WithSound,
            note_timing_chosen: false,
            settings: PluginSettings::default(),
        }));
        let arp_id = arp.id;
        project.track_mut(pad).unwrap().devices.push(arp);
        let mut harness = harness(project);
        harness.run();
        harness
            .get_by_role_and_label(Role::ComboBox, "Keys Input")
            .click();
        harness.run();
        // The other MIDI tracks come after the MIDI inputs, with their
        // notes: the Lead, an audio track, has sound, and nothing the
        // master has is notes.
        harness.get_by_label("Tracks").scroll_to_me();
        harness.run();
        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Lead")
                .is_none()
        );
        assert!(harness.query_by_label("Resampling").is_none());
        // Its points with notes: what comes into its devices, what the
        // arpeggiator plays, and what reaches an instrument, at Post FX,
        // but not its sound after the fader.
        harness.get_by_role_and_label(Role::Button, "Pad").hover();
        harness.run();
        for point in ["Pre FX", "after Arp", "Post FX"] {
            assert!(harness.query_by_label(point).is_some(), "{point}");
        }
        assert!(harness.query_by_label("Post Fader").is_none());
        harness.get_by_label("after Arp").click();
        harness.run();
        let input = harness.get_by_role_and_label(Role::ComboBox, "Keys Input");
        assert_eq!(input.value().as_deref(), Some("from Pad, after Arp"));
        // It plays the notes it takes, as it would a keyboard's.
        let project = harness.state().document.project();
        assert_eq!(project.track(keys).unwrap().monitor, Monitor::In);
        let notes = Input::Tap(Tap::Device(arp_id));
        assert_eq!(mixed(&harness), [Change::Input(keys, Some(notes))]);
    }

    #[test]
    fn midi_tracks_take_the_notes_a_plugin_plays_out_on_any_track() {
        let (mut project, [lead, keys, ..]) = project();
        // A plugin that hears the notes in the Lead's sound, and plays
        // them out as it renders.
        let detector = project.new_device(DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.example.detector".into(),
            name: "Detector".into(),
            kind: PluginKind::AudioEffect,
            inputs: Vec::new(),
            note_output: true,
            note_timing: musicable_model::plugin::NoteTiming::WithSound,
            note_timing_chosen: false,
            settings: PluginSettings::default(),
        }));
        let detector_id = detector.id;
        project.track_mut(lead).unwrap().devices.push(detector);
        let mut harness = harness(project);
        harness.run();
        harness
            .get_by_role_and_label(Role::ComboBox, "Keys Input")
            .click();
        harness.run();
        harness.get_by_label("Tracks").scroll_to_me();
        harness.run();
        // The Lead, an audio track, has notes after the detector, and
        // nowhere else.
        harness.get_by_role_and_label(Role::Button, "Lead").hover();
        harness.run();
        for point in ["Pre FX", "Post FX", "Post Fader"] {
            assert!(harness.query_by_label(point).is_none(), "{point}");
        }
        harness.get_by_label("after Detector").click();
        harness.run();
        let input = harness.get_by_role_and_label(Role::ComboBox, "Keys Input");
        assert_eq!(input.value().as_deref(), Some("from Lead, after Detector"));
        let notes = Input::Tap(Tap::Device(detector_id));
        assert_eq!(mixed(&harness), [Change::Input(keys, Some(notes))]);
    }

    #[test]
    fn a_midi_track_without_an_instrument_plays_anothers_on_a_channel() {
        let (mut project, [_, keys, ..]) = project();
        let strings = project.add_track("Strings", TrackKind::Midi);
        let synth = project.new_device(DeviceKind::TestInstrument { level: 0.5 });
        let synth_id = synth.id;
        project.track_mut(strings).unwrap().devices.push(synth);
        let mut harness = harness(project);
        let midi_to = |harness: &Harness<'_, State>| {
            harness
                .get_by_role_and_label(Role::ComboBox, "Keys MIDI To")
                .value()
        };
        assert_eq!(midi_to(&harness).as_deref(), Some("No Output"));
        // Strings has an instrument, so it plays its sound out.
        harness.get_by_role_and_label(Role::ComboBox, "Strings Output");
        harness
            .get_by_role_and_label(Role::ComboBox, "Keys MIDI To")
            .click();
        harness.run();
        // The instruments on other tracks, each opening to the channels.
        harness
            .get_by_role_and_label(Role::Button, "Strings")
            .hover();
        harness.run();
        harness.get_by_label("Any Channel");
        harness.get_by_label("Channel 16");
        harness.get_by_label("Channel 3").click();
        harness.run();
        let to = Some(synth_id);
        let three = Change::MidiTo {
            track: keys,
            to,
            channel: Some(2),
        };
        assert_eq!(mixed(&harness), [three]);
        assert_eq!(midi_to(&harness).as_deref(), Some("Strings 3"));
        let project = harness.state().document.project();
        let wires: Vec<_> = project.midi_to(keys).map(|w| (w.to, w.channel)).collect();
        assert_eq!(wires, [(Port::Notes(synth_id), Some(2))]);

        // No Output plays it nowhere.
        harness
            .get_by_role_and_label(Role::ComboBox, "Keys MIDI To")
            .click();
        harness.run();
        harness.get_by_label("No Output").click();
        harness.run();
        assert_eq!(midi_to(&harness).as_deref(), Some("No Output"));
        assert_eq!(harness.state().document.project().midi_to(keys).count(), 0);
    }

    #[test]
    fn a_midi_track_shows_the_notes_it_takes_from_another_and_lets_them_go() {
        let (mut project, [_, keys, ..]) = project();
        let pad = project.add_track("Pad", TrackKind::Midi);
        project.track_mut(keys).unwrap().input = Some(Input::Tap(Tap::PreFx(pad)));
        let mut harness = harness(project);
        harness.run();
        let input = |harness: &Harness<'_, State>| {
            harness
                .get_by_role_and_label(Role::ComboBox, "Keys Input")
                .value()
        };
        assert_eq!(input(&harness).as_deref(), Some("from Pad, Pre FX"));
        harness
            .get_by_role_and_label(Role::ComboBox, "Keys Input")
            .click();
        harness.run();
        harness.get_by_label("No Input").scroll_to_me();
        harness.run();
        harness.get_by_label("No Input").click();
        harness.run();
        assert_eq!(mixed(&harness), [Change::Input(keys, None)]);
    }

    #[test]
    fn decibels_read_and_parse() {
        assert_eq!(decibels(1.0), "0.0 dB");
        assert_eq!(decibels(0.5), "-6.0 dB");
        assert_eq!(decibels(0.08), "-21.9 dB");
        assert_eq!(decibels(2.0), "6.0 dB");
        assert_eq!(decibels(0.0), "-inf dB");
        assert_eq!(db_text(-6.02), "-6.0");
        assert_eq!(db_text(1.54), "+1.5");
        assert_eq!(db_text(-0.04), "0.0");
        assert_eq!(db_text(FLOOR), "-inf");
        assert_eq!(db_text(f32::NEG_INFINITY), "-inf");
        assert_eq!(db_text(f32::NAN), "-inf");
        assert_eq!(parse_decibels("-6"), Some(-6.0));
        assert_eq!(parse_decibels(" +1.5 dB "), Some(1.5));
        assert_eq!(parse_decibels("-3db"), Some(-3.0));
        assert_eq!(parse_decibels("-INF"), Some(f64::from(FLOOR)));
        assert_eq!(parse_decibels("loud"), None);
        assert_eq!(parse_decibels("NaN"), None);
    }

    #[test]
    fn modulators_move_faders_pans_and_sends_that_they_reach() {
        let (mut project, [lead, keys, drums, _]) = project();
        // An LFO on the drums group, which Keys feeds, and a Macro on the
        // master, which reaches every track.
        let lfo = project.new_modulator(ModulatorKind::Lfo, "LFO 1");
        let knobs = project.new_modulator(ModulatorKind::Macro, "Macro 1");
        let (lfo_out, knob) = (Output::new(lfo.id, 0), Output::new(knobs.id, 0));
        project
            .track_mut(drums)
            .unwrap()
            .modulation
            .modulators
            .push(lfo);
        project.master.modulation.modulators.push(knobs);
        let send = ParamAddress::Send(project.track(lead).unwrap().sends[0].id);
        let mut harness = harness(project);
        let menu = |harness: &mut Harness<'_, State>, control: &str| {
            harness
                .get_by_role_and_label(Role::Slider, control)
                .click_secondary();
            harness.run();
        };
        let add = |harness: &mut Harness<'_, State>, control: &str, output: &str| {
            menu(harness, control);
            harness.get_by_label("Add Modulation").hover();
            harness.run();
            harness.get_by_role_and_label(Role::Button, output).click();
            harness.run();
        };
        add(&mut harness, "Keys Volume", "LFO 1 on Drums");
        add(&mut harness, "Lead Pan", "Macro 1: Knob 1 on Master");
        add(
            &mut harness,
            "Lead Send to Reverb",
            "Macro 1: Knob 1 on Master",
        );
        let routed = |harness: &Harness<'_, State>, param| {
            let project = harness.state().document.project();
            let routings = project.routings_to(param);
            routings
                .map(|(_, r)| (r.from, r.amount))
                .collect::<Vec<_>>()
        };
        let volume = |track| ParamAddress::Volume(Chain::Track(track));
        let pan = ParamAddress::Pan(Chain::Track(lead));
        let new = modulators::NEW_AMOUNT;
        assert_eq!(routed(&harness, volume(keys)), [(lfo_out, new)]);
        assert_eq!(routed(&harness, pan), [(knob, new)]);
        assert_eq!(routed(&harness, send), [(knob, new)]);

        // Lead doesn't feed the group, so only the master's Macro reaches
        // it.
        menu(&mut harness, "Lead Volume");
        harness.get_by_label("Add Modulation").hover();
        harness.run();
        assert!(harness.query_by_label("LFO 1 on Drums").is_none());
        harness.get_by_role_and_label(Role::Button, "Macro 1: Knob 1 on Master");
        harness.key_press(egui::Key::Escape);
        harness.run();

        // A fader lists what moves it, with its settings.
        menu(&mut harness, "Keys Volume");
        harness.get_by_label("LFO 1 on Drums +25%").hover();
        harness.run();
        harness.get_by_label("Bypass").click();
        harness.run();
        let project = harness.state().document.project();
        let (_, routing) = project.routings_to(volume(keys)).next().unwrap();
        assert!(routing.bypassed);

        // An output let go on a fader moves it.
        let dragged = browser::Dragged::Output {
            output: knob,
            owner: Chain::Master,
            name: "Macro 1: Knob 1".to_owned(),
        };
        let fader = harness
            .get_by_role_and_label(Role::Slider, "Lead Volume")
            .rect()
            .center();
        egui::DragAndDrop::set_payload(&harness.ctx, dragged);
        harness.hover_at(fader);
        harness.run();
        harness.event(egui::Event::PointerButton {
            pos: fader,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run();
        assert_eq!(routed(&harness, volume(lead)), [(knob, new)]);
        assert!(harness.state().errors.is_empty());
    }

    #[test]
    fn pans_read_as_live_shows_them() {
        assert_eq!(pan_text(0.0), "C");
        assert_eq!(pan_text(0.005), "C");
        assert_eq!(pan_text(-0.5), "25L");
        assert_eq!(pan_text(1.0), "50R");
    }
}
