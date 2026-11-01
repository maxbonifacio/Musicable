//! The Routing panel: every connection in the project, a row each, under
//! the kind it is: each track's output, the sends, the inputs tracks take
//! from other tracks, their sound or their notes, the keys
//! wired into devices' side inputs, and the notes wired into instruments
//! and MIDI effects. A row has where the connection comes from, where it
//! goes, and its gain: an output's is its track's fader, a send's its level
//! and a key's its wire's gain. A note wire has its MIDI channel in its
//! place. The panel shows them all, or one track's alone: those that come
//! from it, or go to it or to one of its devices.
//!
//! Clicking either end shows it: a track, selected as clicking its strip
//! selects it, or a device, in the devices panel. The end a connection's
//! own control picks has that control's menu beside it: where an output
//! goes, and where an input, a key or notes come from. Each gain edits as
//! its control does, with the same drags, double-clicks and right-click
//! menus, and shows its automation and its modulators as that does.

#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::iter;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Button, Color32, ComboBox, CursorIcon, FontId, Layout, Popup, Rect, Response, RichText,
    Role, ScrollArea, Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};
use musicable_commands::mixer::Change;
use musicable_commands::{Args, device, edit, wire};
use musicable_model::mixer::Bus;
use musicable_model::param::ParamAddress;
use musicable_model::routing::{Port, Tap, Wire, WireId};
use musicable_model::{Chain, DeviceId, Input, Project, TrackId, TrackKind};

use super::{Choices, FADER_TOP, Mixer, foot, height_of, midi_channel_name, mix, moved, name_menu};
use crate::modulators;
use crate::mouse::MouseMap;
use crate::native::sidechain;
use crate::parts::Parts;
use crate::shell::{Action, View};
use crate::theme::Theme;

/// What the panel lists without tracks.
const NO_TRACKS: &str = "Each track's output, its sends, the inputs tracks take from other \
                         tracks, the keys wired into devices and the notes wired into \
                         instruments are listed here, once there are tracks.";

/// What the panel's menu calls every track's connections.
const ALL_TRACKS: &str = "All Tracks";

/// The panel's own state, kept between frames.
#[derive(Debug, Default)]
pub(crate) struct RoutingPanel {
    /// The track, or the master, whose connections it shows alone, or
    /// `None` for all of them.
    only: Option<Chain>,
    /// The device clicked last, for the devices panel to bring into sight.
    revealed: Option<DeviceId>,
}

/// One of the project's connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Connection {
    /// A track's output, to the bus it feeds or to Sends Only.
    Output(TrackId),
    /// A track's send to a return track.
    Send { track: TrackId, to: TrackId },
    /// A track's input from another track's tap.
    Input(TrackId, Tap),
    /// A wire into a device's side input.
    Key(WireId),
    /// A note wire into an instrument's or a MIDI effect's note input.
    Note(WireId),
}

impl Connection {
    /// Which of its ends has its control's menu beside it: where an output
    /// goes, and where an input or a key comes from.
    fn menus(self) -> [bool; 2] {
        match self {
            Connection::Output(_) => [false, true],
            Connection::Send { .. } => [false, false],
            Connection::Input(..) | Connection::Key(_) | Connection::Note(_) => [true, false],
        }
    }
}

/// A line of the list: the heading of a kind of connection, or one of
/// them.
enum Line {
    Heading(&'static str),
    Row(Connection),
}

/// Where a connection comes from, or goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// A track, or the master.
    Channel(Chain),
    Device(DeviceId),
    /// Nowhere: a track's output to Sends Only.
    Nowhere,
}

impl RoutingPanel {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        theme: &Theme,
        mouse: &MouseMap,
        parts: &Parts,
        actions: &mut Vec<Action>,
    ) {
        let project = view.project;
        // A track that's gone takes its connections with it.
        self.only = self.only.filter(|&chain| project.devices(chain).is_some());
        if project.tracks.is_empty() {
            ui.add(egui::Label::new(RichText::new(NO_TRACKS).weak()).wrap());
            return;
        }
        self.only_menu(ui, project);
        let mixer = Mixer {
            project,
            editor: view.editor,
            returns: Vec::new(),
            inputs: Choices {
                audio: view.inputs,
                midi: view.midi_inputs,
                plugins: Some(view.plugins.status),
            },
            playhead: view.playback.playhead,
            levels: view.levels,
            performance: view.performance,
            fader: 0.0,
            theme,
            mouse,
            parts,
        };
        let lines = lines(project, self.only);
        if lines.is_empty() {
            let name = self.only.map_or("?", |chain| channel_name(project, chain));
            let text = format!("Nothing comes from {name}, or goes to it.");
            ui.add(egui::Label::new(RichText::new(text).weak()).wrap());
            return;
        }
        let height = theme.mixer.row.max(ui.spacing().interact_size.y);
        let button = ui.spacing().interact_size.y.min(height);
        let widest = widest(ui, &mixer, &lines, button);
        let shown = ScrollArea::vertical()
            .id_salt("routing")
            .auto_shrink(false)
            .show_rows(ui, height, lines.len(), |ui, range| {
                let rows = Rows {
                    mixer: &mixer,
                    view,
                    columns: Columns::across(ui.available_width(), theme, widest),
                    height,
                    button,
                    revealed: Cell::new(None),
                };
                for line in &lines[range] {
                    match line {
                        Line::Heading(text) => rows.heading(ui, text),
                        Line::Row(connection) => {
                            ui.push_id(connection, |ui| rows.row(ui, *connection, actions));
                        }
                    }
                }
                rows.revealed.get()
            });
        self.revealed = shown.inner.or(self.revealed);
    }

    /// The device clicked since this was last asked, which the devices
    /// panel shows with it selected, for it to bring into sight.
    pub fn take_revealed(&mut self) -> Option<DeviceId> {
        self.revealed.take()
    }

    /// The menu of whose connections the panel shows: every track's, or
    /// one track's or the master's alone.
    fn only_menu(&mut self, ui: &mut Ui, project: &Project) {
        let name = |chain: Option<Chain>| match chain {
            None => ALL_TRACKS,
            Some(Chain::Master) => "Master",
            Some(Chain::Track(track)) => project.track(track).map_or("?", |t| t.name.as_str()),
        };
        let current = name(self.only);
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Show").weak());
            let menu = ComboBox::from_id_salt("routing only")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    let tracks = (project.tracks.iter()).map(|track| Some(Chain::Track(track.id)));
                    let every = std::iter::once(None)
                        .chain(tracks)
                        .chain([Some(Chain::Master)]);
                    for (n, only) in every.enumerate() {
                        // Every track's, then each one's.
                        if n == 1 {
                            ui.separator();
                        }
                        if ui.selectable_label(only == self.only, name(only)).clicked() {
                            picked = Some(only);
                        }
                    }
                });
            name_menu(&menu.response, "Show".to_owned(), current);
            menu.response.on_hover_text(
                "Every track's connections, or one track's alone: those that come from it, and \
                 those that go to it or to its devices",
            );
        });
        if let Some(only) = picked {
            self.only = only;
        }
    }
}

/// The lines the panel lists for `project`: each kind of connection's
/// heading, then its connections, of the track or the master `only` names,
/// if it names one. A kind without any has no heading.
fn lines(project: &Project, only: Option<Chain>) -> Vec<Line> {
    let mut outputs = Vec::new();
    let mut sends = Vec::new();
    let mut inputs = Vec::new();
    for track in &project.tracks {
        outputs.push(Connection::Output(track.id));
        for send in &track.sends {
            sends.push(Connection::Send {
                track: track.id,
                to: send.to,
            });
        }
        if let Some(Input::Tap(tap)) = track.input {
            inputs.push(Connection::Input(track.id, tap));
        }
    }
    // By the devices they go into, in the order the tracks and their
    // devices have them, and the master's last.
    let chains: Vec<Chain> = project.chains().map(|(chain, _)| chain).collect();
    let mut wires: Vec<(usize, usize, WireId)> = (project.wires.iter())
        .filter_map(|wire| {
            let (chain, index, _) = project.find_device(wire.to.device())?;
            let place = chains.iter().position(|&c| c == chain)?;
            Some((place, index, wire.id))
        })
        .collect();
    wires.sort_by_key(|&(place, index, _)| (place, index));
    let (mut keys, mut notes) = (Vec::new(), Vec::new());
    for (_, _, id) in wires {
        match project.wire(id).is_some_and(Wire::takes_notes) {
            true => notes.push(Connection::Note(id)),
            false => keys.push(Connection::Key(id)),
        }
    }
    let kinds = [
        ("Outputs", outputs),
        ("Sends", sends),
        ("Inputs", inputs),
        ("Keys", keys),
        ("Notes", notes),
    ];
    let mut lines = Vec::new();
    for (heading, connections) in kinds {
        let shown: Vec<Connection> = (connections.into_iter())
            .filter(|&connection| only.is_none_or(|only| involves(project, connection, only)))
            .collect();
        if !shown.is_empty() {
            lines.push(Line::Heading(heading));
            lines.extend(shown.into_iter().map(Line::Row));
        }
    }
    lines
}

/// Whether `connection` comes from `chain`, or goes to it or one of its
/// devices.
fn involves(project: &Project, connection: Connection, chain: Chain) -> bool {
    let (from, to) = ends(project, connection);
    [from, to]
        .into_iter()
        .any(|end| channel(project, end) == Some(chain))
}

/// Where `connection` comes from, and where it goes.
fn ends(project: &Project, connection: Connection) -> (End, End) {
    match connection {
        Connection::Output(track) => {
            let to = match project.track(track).map(|t| project.output_of(t)) {
                Some(Bus::Master) => End::Channel(Chain::Master),
                Some(Bus::Group(group)) => End::Channel(Chain::Track(group)),
                Some(Bus::SendsOnly) | None => End::Nowhere,
            };
            (End::Channel(Chain::Track(track)), to)
        }
        Connection::Send { track, to } => (
            End::Channel(Chain::Track(track)),
            End::Channel(Chain::Track(to)),
        ),
        Connection::Input(track, tap) => (tap_end(tap), End::Channel(Chain::Track(track))),
        Connection::Key(wire) | Connection::Note(wire) => match project.wire(wire) {
            Some(wire) => (tap_end(wire.from), End::Device(wire.to.device())),
            None => (End::Nowhere, End::Nowhere),
        },
    }
}

/// What `connection`'s row calls where it comes from, and where it goes.
fn names(mixer: &Mixer<'_>, connection: Connection) -> [String; 2] {
    let project = mixer.project;
    let track = |id| {
        project
            .track(id)
            .map_or("?", |t| t.name.as_str())
            .to_owned()
    };
    match connection {
        Connection::Output(id) => {
            let bus = project
                .track(id)
                .map(|t| mixer.bus_name(project.output_of(t)));
            [track(id), bus.unwrap_or("?").to_owned()]
        }
        Connection::Send { track: id, to } => [track(id), track(to)],
        Connection::Input(id, tap) => [tap_text(project, tap), track(id)],
        Connection::Key(wire) | Connection::Note(wire) => match project.wire(wire) {
            Some(wire) => [tap_text(project, wire.from), port_text(project, wire.to)],
            None => [String::new(), String::new()],
        },
    }
}

/// How wide the rows of `lines` need each end to be: the widest of the
/// names there, and a gap after it, before the menu button, `button` wide,
/// where its row has one.
fn widest(ui: &Ui, mixer: &Mixer<'_>, lines: &[Line], button: f32) -> [f32; 2] {
    let (painter, font) = (ui.painter(), FontId::proportional(mixer.theme.text));
    let gap = mixer.theme.mixer.gap;
    let mut widest = [0.0_f32; 2];
    for line in lines {
        let Line::Row(connection) = *line else {
            continue;
        };
        let names = names(mixer, connection);
        for ((widest, name), menu) in widest.iter_mut().zip(names).zip(connection.menus()) {
            let name = painter.layout_no_wrap(name, font.clone(), Color32::PLACEHOLDER);
            let button = if menu { button } else { 0.0 };
            *widest = widest.max(name.size().x + gap + button);
        }
    }
    widest
}

/// The end a connection from `tap` comes from: the device it comes out of,
/// or its track, or the master.
fn tap_end(tap: Tap) -> End {
    match tap {
        Tap::PreFx(track) => End::Channel(Chain::Track(track)),
        Tap::Device(device) => End::Device(device),
        Tap::PostFx(chain) | Tap::PostFader(chain) => End::Channel(chain),
    }
}

/// The track or the master `end` is on, if the project has it.
fn channel(project: &Project, end: End) -> Option<Chain> {
    match end {
        End::Channel(chain) => Some(chain),
        End::Device(device) => project.find_device(device).map(|(chain, ..)| chain),
        End::Nowhere => None,
    }
}

/// The name of the track `chain` is, or "Master".
fn channel_name(project: &Project, chain: Chain) -> &str {
    match chain {
        Chain::Track(track) => project.track(track).map_or("?", |t| t.name.as_str()),
        Chain::Master => "Master",
    }
}

/// What a row calls `tap`: its track or the master, and its point on it,
/// as in "Kick, Post FX" or "Bass, after EQ".
fn tap_text(project: &Project, tap: Tap) -> String {
    let chain = project.tap_chain(tap);
    let chain = chain.map_or("?", |chain| channel_name(project, chain));
    let point = project.tap_point_name(tap).unwrap_or_default();
    format!("{chain}, {point}")
}

/// What a row calls the device `port` belongs to: its track or the master,
/// and its name, with the plugin's name for the side input if it's a
/// plugin's, as in "Bass, Compressor".
fn port_text(project: &Project, port: Port) -> String {
    let Some((chain, _, device)) = project.find_device(port.device()) else {
        return "?".to_owned();
    };
    let chain = channel_name(project, chain);
    let name = device.kind.name();
    match (port, project.input_name(port)) {
        (Port::PluginInput(..), Some(input)) => format!("{chain}, {name} {input}"),
        _ => format!("{chain}, {name}"),
    }
}

/// What screen readers call a key's source menu and its gain: what the
/// Sidechain column calls them, after the track or the master the device
/// is on, as in "Bass Compressor Sidechain Source", and numbered after the
/// first wire into the side input, as in "Bass Compressor Sidechain Gain 2".
/// A note wire's are its source and its channel, as in "Strings Test Synth
/// Notes Source" and "Strings Test Synth Notes Channel", numbered before
/// them, as in "Strings Test Synth Notes 2 Channel", so the number reads
/// as no channel's.
fn key_labels(project: &Project, wire: &Wire) -> (String, String) {
    let found = project.find_device(wire.to.device());
    let (chain, device) = found.map_or(("?", "?"), |(chain, _, device)| {
        (channel_name(project, chain), device.kind.name())
    });
    let input = project.input_name(wire.to).unwrap_or_default();
    let named = format!("{chain} {device} {input}");
    let number = match project.wires_into(wire.to).position(|w| w.id == wire.id) {
        Some(0) | None => String::new(),
        Some(n) => format!(" {}", n + 1),
    };
    match wire.takes_notes() {
        true => (
            format!("{named}{number} Source"),
            format!("{named}{number} Channel"),
        ),
        false => (
            format!("{named} Source{number}"),
            format!("{named} Gain{number}"),
        ),
    }
}

/// The command that shows `end`, when it's clicked: selecting its track or
/// the master, or showing its device in the devices panel.
fn show(end: End) -> Option<Action> {
    Some(match end {
        End::Channel(Chain::Track(track)) => {
            let args = Args::new().with("tracks", vec![track]);
            Action::RunWith(edit::SELECT_TRACKS, args)
        }
        End::Channel(Chain::Master) => Action::RunWith(edit::SELECT_MASTER, Args::new()),
        End::Device(device) => {
            let args = Args::new().with("devices", vec![device]);
            Action::RunWith(device::SELECT, args)
        }
        End::Nowhere => return None,
    })
}

/// Where a row's parts go across it, by their widths.
#[derive(Debug, Clone, Copy)]
struct Columns {
    from: f32,
    arrow: f32,
    to: f32,
    gain: f32,
}

impl Columns {
    /// The columns of a row `width` wide, for ends `widest` wide at most:
    /// the gain as wide as two of the mixer's strips, up to a third of the
    /// row, the arrow a row high, and each end as wide as it needs, from a
    /// strip's width up to half of the rest, so that the rows read across
    /// without gaps however wide the panel is.
    fn across(width: f32, theme: &Theme, widest: [f32; 2]) -> Self {
        let sizes = &theme.mixer;
        let gain = (2.0 * sizes.strip).min(width / 3.0);
        let arrow = sizes.row;
        let room = ((width - gain - arrow - 3.0 * sizes.gap) / 2.0).max(0.0);
        let [from, to] = widest.map(|widest| widest.max(sizes.strip).min(room));
        Self {
            from,
            arrow,
            to,
            gain,
        }
    }
}

/// A menu beside one end of a row: what screen readers call it, what it
/// shows, and its hover text.
struct Menu<'a> {
    label: String,
    value: &'a str,
    hint: &'a str,
}

/// What every row needs to know.
struct Rows<'a> {
    /// The mixer's controls, which the rows' are.
    mixer: &'a Mixer<'a>,
    view: &'a View<'a>,
    columns: Columns,
    /// How tall each line is, and how big the menu buttons are.
    height: f32,
    button: f32,
    /// The device clicked, if one was.
    revealed: Cell<Option<DeviceId>>,
}

impl Rows<'_> {
    fn theme(&self) -> &Theme {
        self.mixer.theme
    }

    /// A kind of connection's heading.
    fn heading(&self, ui: &mut Ui, text: &str) {
        let size = vec2(ui.available_width(), self.height);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(size);
            ui.label(RichText::new(text).size(self.theme().small_text).weak());
        });
    }

    /// The row of `connection`: where it comes from, an arrow, where it
    /// goes, and its gain.
    fn row(&self, ui: &mut Ui, connection: Connection, actions: &mut Vec<Action>) {
        let size = vec2(ui.available_width(), self.height);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(size);
            ui.spacing_mut().item_spacing.x = self.theme().mixer.gap;
            match connection {
                Connection::Output(track) => self.output(ui, track, actions),
                Connection::Send { track, to } => self.send(ui, track, to, actions),
                Connection::Input(track, tap) => self.input(ui, track, tap, actions),
                Connection::Key(wire) | Connection::Note(wire) => {
                    if let Some(wire) = self.mixer.project.wire(wire) {
                        self.key(ui, wire, actions);
                    }
                }
            }
        });
    }

    /// A track's output: the track, the bus it feeds, with the output
    /// menu's buses beside it, and its fader.
    fn output(&self, ui: &mut Ui, id: TrackId, actions: &mut Vec<Action>) {
        let mixer = self.mixer;
        let project = mixer.project;
        let Some(track) = project.track(id) else {
            return;
        };
        let connection = Connection::Output(id);
        let (from, to) = ends(project, connection);
        let [name, bus] = names(mixer, connection);
        let columns = self.columns;
        self.end(ui, from, &name, None, columns.from, actions);
        self.arrow(ui);
        let menu = Menu {
            label: format!("{name} Output"),
            value: &bus,
            hint: "Where the track's output goes",
        };
        if let Some(button) = self.end(ui, to, &bus, Some(menu), columns.to, actions) {
            let picked = Popup::menu(&button)
                .show(|ui| mixer.bus_items(ui, track))
                .and_then(|shown| shown.inner);
            if let Some(bus) = picked {
                mix(actions, Change::Output(id, bus));
            }
        }
        let param = ParamAddress::Volume(Chain::Track(id));
        let gain = mixer.shown(param, track.gain);
        let label = format!("{} Volume", track.name);
        let control = mixer.volume_bar(ui, gain, &label, columns.gain);
        let at = height_of(gain, FADER_TOP);
        mixer.modulated(ui, &control.0, param, foot(control.0.rect), at, actions);
        mixer.mark(ui, &control.0, param);
        control.0.context_menu(|ui| {
            mixer.automation_items(ui, param, actions);
            modulators::routing_items(ui, project, param, actions);
        });
        moved(control, actions, |gain| Change::Gain(id, gain));
    }

    /// A send: the track, the return track it sends to, and its level, with
    /// its Pre-Fader switch, as the strip's send has them.
    fn send(&self, ui: &mut Ui, id: TrackId, to: TrackId, actions: &mut Vec<Action>) {
        let project = self.mixer.project;
        let (Some(track), Some(to)) = (project.track(id), project.track(to)) else {
            return;
        };
        let connection = Connection::Send {
            track: id,
            to: to.id,
        };
        let (from, into) = ends(project, connection);
        let [name, return_name] = names(self.mixer, connection);
        let columns = self.columns;
        self.end(ui, from, &name, None, columns.from, actions);
        self.arrow(ui);
        self.end(ui, into, &return_name, None, columns.to, actions);
        self.mixer
            .send_in(ui, track, to, columns.gain, false, actions);
    }

    /// A track's input from another's tap: the tap, with the input menu's
    /// tracks beside it, and the track.
    fn input(&self, ui: &mut Ui, id: TrackId, tap: Tap, actions: &mut Vec<Action>) {
        let mixer = self.mixer;
        let project = mixer.project;
        let Some(track) = project.track(id) else {
            return;
        };
        let connection = Connection::Input(id, tap);
        let (from, to) = ends(project, connection);
        let [text, name] = names(mixer, connection);
        let columns = self.columns;
        let value = mixer.tap_input_name(tap);
        let hint = match track.kind {
            TrackKind::Midi => "Where the track records from: another track's notes",
            _ if Input::Tap(tap).is_resampling() => {
                "Where the track records from: the master, without this track in it"
            }
            _ => "Where the track records from: another track's sound",
        };
        let menu = Menu {
            label: format!("{name} Input"),
            value: &value,
            hint,
        };
        if let Some(button) = self.end(ui, from, &text, Some(menu), columns.from, actions) {
            let picked = Popup::menu(&button)
                .show(|ui| {
                    let mut picked = None;
                    if ui.add(Button::selectable(false, "No Input")).clicked() {
                        picked = Some(None);
                    }
                    if let Some(input) = mixer.tap_items(ui, track, Some(tap)) {
                        picked = Some(Some(input));
                    }
                    picked
                })
                .and_then(|shown| shown.inner);
            if let Some(input) = picked {
                mix(actions, Change::Input(id, input));
            }
        }
        self.arrow(ui);
        self.end(ui, to, &name, None, columns.to, actions);
    }

    /// A key: the tap it comes from, with the source menu of the Sidechain
    /// column beside it, the device it keys, and its gain. Or a note wire:
    /// the tap its notes come from, with the taps that carry notes in the
    /// menu, the instrument or MIDI effect it plays, and its channel.
    fn key(&self, ui: &mut Ui, wire: &Wire, actions: &mut Vec<Action>) {
        let project = self.mixer.project;
        let notes = wire.takes_notes();
        let connection = match notes {
            true => Connection::Note(wire.id),
            false => Connection::Key(wire.id),
        };
        let (from, to) = ends(project, connection);
        let [text, device] = names(self.mixer, connection);
        let columns = self.columns;
        let (source, setting) = key_labels(project, wire);
        let menu = Menu {
            label: source,
            value: &text,
            hint: match notes {
                true => "Where the notes come from",
                false => "Where the key comes from",
            },
        };
        if let Some(button) = self.end(ui, from, &text, Some(menu), columns.from, actions) {
            Popup::menu(&button).show(|ui| sidechain::source_menu(ui, project, wire, actions));
        }
        self.arrow(ui);
        self.end(ui, to, &device, None, columns.to, actions);
        if notes {
            self.channel(ui, wire, setting, actions);
            return;
        }
        let (view, theme, mouse) = (self.view, self.theme(), self.mixer.mouse);
        sidechain::gain(ui, wire, setting, columns.gain, view, theme, mouse, actions);
    }

    /// A note wire's MIDI channel, in the gain's place: a menu of Any Channel
    /// and Channel 1 to 16, called `label`. Asks for the channel picked.
    fn channel(&self, ui: &mut Ui, wire: &Wire, label: String, actions: &mut Vec<Action>) {
        let text = midi_channel_name(wire.channel);
        let mut picked = None;
        let menu = ComboBox::from_id_salt(("note channel", wire.id))
            .width(self.columns.gain)
            .truncate()
            .selected_text(&text)
            .show_ui(ui, |ui| {
                for channel in iter::once(None).chain((0..Wire::CHANNELS).map(Some)) {
                    let on = channel == wire.channel;
                    if ui
                        .selectable_label(on, midi_channel_name(channel))
                        .clicked()
                        && !on
                    {
                        picked = Some(channel);
                    }
                }
            });
        name_menu(&menu.response, label, &text);
        menu.response.on_hover_text(
            "The MIDI channel the notes play on, as a part of a multi-timbral instrument \
             plays, or with Any Channel, each note on its own, as the instrument's own track's \
             notes do",
        );
        if let Some(channel) = picked {
            let mut args = Args::new().with("wire", wire.id);
            if let Some(channel) = channel {
                args.set("channel", i64::from(channel));
            }
            actions.push(Action::RunWith(wire::SET_CHANNEL, args));
        }
    }

    /// One end of a row, `width` wide: `text`, its name, which shows `end`
    /// when clicked, and if it's the end the connection's control picks,
    /// `menu`'s button after it. Returns that button, for its menu.
    fn end(
        &self,
        ui: &mut Ui,
        end: End,
        text: &str,
        menu: Option<Menu<'_>>,
        width: f32,
        actions: &mut Vec<Action>,
    ) -> Option<Response> {
        let size = vec2(width, self.height);
        let (side, gap) = (self.button, self.theme().mixer.gap);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(size);
            ui.spacing_mut().item_spacing.x = 0.0;
            let room = match menu {
                Some(_) => (width - gap - side).max(0.0),
                None => width,
            };
            let name = self.name(ui, end, text, room);
            if name.clicked() {
                if let End::Device(device) = end {
                    self.revealed.set(Some(device));
                }
                actions.extend(show(end));
            }
            let menu = menu?;
            ui.add_space(gap);
            let (rect, button) = ui.allocate_exact_size(vec2(side, side), Sense::click());
            name_menu(&button, menu.label, menu.value);
            if ui.is_rect_visible(rect) {
                let visuals = ui.style().interact(&button);
                let painter = ui.painter();
                let open = Popup::is_id_open(ui.ctx(), Popup::default_response_id(&button));
                if button.hovered() || open {
                    painter.rect_filled(rect, self.theme().corner, visuals.weak_bg_fill);
                }
                let icon = ui.spacing().icon_width;
                let icon = Rect::from_center_size(rect.center(), vec2(icon * 0.7, icon * 0.45));
                let triangle = vec![icon.left_top(), icon.right_top(), icon.center_bottom()];
                painter.add(Shape::convex_polygon(
                    triangle,
                    visuals.fg_stroke.color,
                    Stroke::NONE,
                ));
            }
            Some(button.on_hover_text(menu.hint))
        })
        .inner
    }

    /// The name at one end of a row, `width` wide, cut short to fit: one
    /// that shows something is a link, underlined while it's hovered.
    fn name(&self, ui: &mut Ui, end: End, text: &str, width: f32) -> Response {
        let shows = end != End::Nowhere;
        let sense = match shows {
            true => Sense::click(),
            false => Sense::hover(),
        };
        let (rect, response) = ui.allocate_exact_size(vec2(width, self.height), sense);
        let role = if shows { Role::Link } else { Role::Label };
        response.widget_info(|| WidgetInfo::labeled(role, true, text));
        if ui.is_rect_visible(rect) {
            let hovered = shows && response.hovered();
            let color = match hovered {
                true => ui.visuals().strong_text_color(),
                false => ui.visuals().text_color(),
            };
            let font = FontId::proportional(self.theme().text);
            let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
            job.wrap = TextWrapping::truncate_at_width(width);
            if hovered {
                for section in &mut job.sections {
                    section.format.underline = Stroke::new(1.0, color);
                }
            }
            let galley = ui.painter().layout_job(job);
            let at = pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
            ui.painter().with_clip_rect(rect).galley(at, galley, color);
        }
        let project = self.mixer.project;
        match end {
            End::Channel(Chain::Track(track)) => {
                let name = project.track(track).map_or("?", |t| t.name.as_str());
                (response.on_hover_cursor(CursorIcon::PointingHand))
                    .on_hover_text(format!("Selects {name}"))
            }
            End::Channel(Chain::Master) => response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("Selects the master"),
            End::Device(_) => response
                .on_hover_cursor(CursorIcon::PointingHand)
                .on_hover_text("Shows the device in the devices panel"),
            End::Nowhere => response.on_hover_text(
                "Feeds nothing: the track is heard through its sends, and the tracks that take \
                 its sound, alone",
            ),
        }
    }

    /// The arrow from where a connection comes from to where it goes.
    fn arrow(&self, ui: &mut Ui) {
        let size = vec2(self.columns.arrow, self.height);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        if ui.is_rect_visible(rect) {
            let color = ui.visuals().weak_text_color();
            let from = rect.left_center() + vec2(3.0, 0.0);
            let along = vec2((rect.width() - 6.0).max(0.0), 0.0);
            ui.painter().arrow(from, along, Stroke::new(1.0, color));
        }
    }
}
