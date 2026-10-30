//! How much of the CPU the project takes: each device's share of a core,
//! and its longest block, in its title bar, each track's in the mixer's
//! CPU row, and everything's in the Performance panel, busiest first, as
//! the app last worked them out from how long the engine's nodes take.
//! Clicking a row of the panel selects its device, which the devices panel
//! brings into sight, or its track.

use std::collections::BTreeMap;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Align2, Color32, FontId, Label, Pos2, Rect, Response, RichText, Role, ScrollArea, Sense,
    Ui, WidgetInfo, pos2, vec2,
};
use musicable_commands::{Args, device, edit};
use musicable_model::{Chain, DeviceId, Monitor, Project, TrackId};

use crate::devices::describe;
use crate::shell::{Action, View};
use crate::theme::Theme;

/// What the panel shows without figures.
const NO_FIGURES: &str = "Each device's share of the CPU is listed here, busiest first, while a \
                          device plays.";

/// What the Average column's heading says in its tooltip.
const AVERAGE: &str = "The share of one core it took over the last second: 50% is half a core. \
                       Shares add up.";

/// What the Longest column's heading says in its tooltip.
const LONGEST: &str = "Its longest block over the last second, as a share of the buffer: over \
                       100%, it alone took longer than a buffer lasts. Only for what renders \
                       live.";

/// What the Renders column's heading says in its tooltip.
const RENDERS: &str = "Ahead: rendered ahead of the audio device, on all the cores, as Render \
                       Ahead in Settings > Audio has it. Live: rendered as the device asks for \
                       each buffer, as a track monitoring its input or set to Always Live is, \
                       and a plugin that must, or is set to Render Ahead Never, with what it \
                       plays into, and everything is while Render Ahead is off.";

/// A share of a core over this turns a figure amber.
pub const BUSY: f32 = 0.25;
/// A share of a core over this turns a figure red.
pub const VERY_BUSY: f32 = 0.5;

/// What something in the project costs the CPU, over the last second.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cpu {
    /// The share of one core it takes to keep up: 0.5 is half a core, and
    /// shares add up.
    pub average: f32,
    /// Its longest block, as a share of the audio device's period: over
    /// 1.0, it alone took longer than a callback lasts. It matters only
    /// while it renders live.
    pub longest: f32,
    /// Whether it renders ahead of the audio device, rather than live, as
    /// the device asks for each block. A block rendered ahead has only to
    /// be ready before the lead runs out.
    pub ahead: bool,
}

impl Cpu {
    /// Its longest block, while it renders live.
    pub fn live_longest(&self) -> Option<f32> {
        (!self.ahead).then_some(self.longest)
    }

    /// How it renders, as the Performance panel says: "Ahead" or "Live".
    pub fn renders(&self) -> &'static str {
        match self.ahead {
            true => "Ahead",
            false => "Live",
        }
    }
}

/// What has a channel render live while the rest of the project renders
/// ahead, for the Live mark in its track's header: the track or the plugin
/// device that renders live of itself, and whose sound, notes or modulation
/// reach the channel. That's on the channel itself if it can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Live {
    /// A track that monitors its input, or is set to Always Live.
    Track(TrackId),
    /// A plugin device whose plugin says it must render live, as a plugin
    /// that talks to hardware does.
    Plugin(DeviceId),
    /// A plugin device whose plugin is set to Render Ahead Never.
    Never(DeviceId),
}

impl Live {
    /// What has `chain` render live, as its Live mark says, such as "Live:
    /// fed by the armed track Vocal".
    pub(crate) fn reason(&self, project: &Project, chain: Chain) -> String {
        let (device, why) = match *self {
            Self::Track(track) => return track_reason(project, track, chain),
            Self::Plugin(device) => (device, "says it must render live"),
            Self::Never(device) => (device, "is set to Render Ahead Never"),
        };
        let Some((on, _, device)) = project.find_device(device) else {
            return "Live".to_owned();
        };
        let name = device.kind.name();
        match on {
            _ if on == chain => format!("Live: {name} {why}"),
            Chain::Track(track) => match project.track(track) {
                Some(track) => format!("Live: fed by {name} on {}, which {why}", track.name),
                None => format!("Live: fed by {name}, which {why}"),
            },
            Chain::Master => format!("Live: fed by {name} on the master, which {why}"),
        }
    }
}

/// What has `chain` render live, `track` monitoring its input or set to
/// Always Live.
fn track_reason(project: &Project, track: TrackId, chain: Chain) -> String {
    let Some(source) = project.track(track) else {
        return "Live".to_owned();
    };
    let name = &source.name;
    let own = chain == Chain::Track(source.id);
    match (own, source.always_live, source.monitor) {
        (true, true, _) => "Live: set to Always Live".to_owned(),
        (true, false, Monitor::In) => "Live: monitoring its input".to_owned(),
        (true, false, _) => "Live: armed, monitoring its input".to_owned(),
        (false, true, _) => format!("Live: fed by {name}, which is set to Always Live"),
        (false, false, Monitor::In) => format!("Live: fed by {name}, which monitors its input"),
        (false, false, _) => format!("Live: fed by the armed track {name}"),
    }
}

/// Something in the project that takes CPU time, as the Performance panel
/// lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Row {
    Device(DeviceId),
    /// What a track plays: its clips, and the input it monitors.
    Clips(TrackId),
    /// A channel's modulators.
    Modulators(Chain),
    /// A channel's strip, but for its devices: its fader and pan, its
    /// sends, its bus, its built-in slots, and the delays that line its
    /// paths up.
    Mixer(Chain),
    /// The metronome.
    Metronome,
}

/// The CPU figures, as the app last worked them out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Performance {
    /// Each row's figures.
    pub rows: BTreeMap<Row, Cpu>,
    /// Each channel's share of a core: its devices', clips', modulators'
    /// and mixer's.
    pub channels: BTreeMap<Chain, f32>,
    /// The whole project's share of a core: 1.5 is a core and a half.
    pub total: f32,
    /// The share of a core of what renders ahead of the device.
    pub ahead: f32,
}

impl Performance {
    /// The figures of the device with `id`, once it has some.
    pub fn device(&self, id: DeviceId) -> Option<Cpu> {
        self.rows.get(&Row::Device(id)).copied()
    }

    /// The share of a core `chain`'s channel takes, once it has a figure.
    pub fn channel(&self, chain: Chain) -> Option<f32> {
        self.channels.get(&chain).copied()
    }

    /// The rows, busiest first.
    pub fn busiest(&self) -> Vec<(Row, Cpu)> {
        let mut rows: Vec<(Row, Cpu)> = self.rows.iter().map(|(&row, &cpu)| (row, cpu)).collect();
        rows.sort_by(|a, b| b.1.average.total_cmp(&a.1.average).then(a.0.cmp(&b.0)));
        rows
    }
}

/// The Performance panel's own state, kept between frames.
#[derive(Debug, Default)]
pub(crate) struct PerformancePanel {
    /// The rows in the order the list last showed them.
    order: Vec<Row>,
    /// The device clicked last, for the devices panel to bring into sight.
    revealed: Option<DeviceId>,
}

impl PerformancePanel {
    pub fn show(&mut self, ui: &mut Ui, view: &View<'_>, theme: &Theme, actions: &mut Vec<Action>) {
        let (project, performance) = (view.project, view.performance);
        let mut rows: Vec<(Row, Cpu)> = (performance.busiest().into_iter())
            .filter(|&(row, _)| names(project, row).is_some())
            .collect();
        if rows.is_empty() {
            self.order.clear();
            ui.add(Label::new(RichText::new(NO_FIGURES).weak()).wrap());
            return;
        }
        // While the pointer is over the list, the rows keep their places,
        // so a click lands on the row it was aimed at. Rows new since go
        // at the foot, busiest first.
        if ui.rect_contains_pointer(ui.max_rect()) {
            let places: BTreeMap<Row, usize> = (self.order.iter().enumerate())
                .map(|(place, &row)| (row, place))
                .collect();
            rows.sort_by_key(|(row, _)| places.get(row).copied().unwrap_or(usize::MAX));
        }
        self.order = rows.iter().map(|&(row, _)| row).collect();
        let lines = Lines {
            columns: Columns::across(ui.available_width(), theme),
            height: ui.spacing().interact_size.y,
            theme,
        };
        lines.heading(ui);
        // The total stays in sight under the list.
        let footer = lines.height + ui.spacing().item_spacing.y;
        let shown = ScrollArea::vertical()
            .id_salt("performance")
            .auto_shrink(false)
            .max_height((ui.available_height() - footer).max(lines.height))
            .show_rows(ui, lines.height, rows.len(), |ui, range| {
                let mut revealed = None;
                for &(row, cpu) in &rows[range] {
                    let Some((channel, part)) = names(project, row) else {
                        continue;
                    };
                    let line = Line {
                        channel,
                        part: &part,
                        renders: cpu.renders(),
                        average: cpu.average,
                        longest: cpu.live_longest(),
                        clicks: row != Row::Metronome,
                    };
                    if lines.line(ui, &line).clicked() {
                        revealed = select(row, actions).or(revealed);
                    }
                }
                revealed
            });
        self.revealed = shown.inner.or(self.revealed);
        let total = Line {
            channel: "Total",
            part: "",
            renders: "",
            average: performance.total,
            longest: None,
            clicks: false,
        };
        let _ = lines.line(ui, &total);
    }

    /// The device clicked since this was last asked, which the devices
    /// panel shows with it selected, for it to bring into sight.
    pub fn take_revealed(&mut self) -> Option<DeviceId> {
        self.revealed.take()
    }
}

/// One of the panel's lines: a row's names and figures, or the total's.
struct Line<'a> {
    /// The track or the master.
    channel: &'a str,
    /// The device, or the part of the channel.
    part: &'a str,
    /// How it renders: "Ahead", "Live", or nothing for the total.
    renders: &'a str,
    average: f32,
    longest: Option<f32>,
    /// Whether it senses clicks.
    clicks: bool,
}

/// How the panel's lines are laid out.
struct Lines<'a> {
    columns: Columns,
    height: f32,
    theme: &'a Theme,
}

impl Lines<'_> {
    /// The columns' headings, with what the figures mean in their tooltips.
    fn heading(&self, ui: &mut Ui) {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), self.height), Sense::hover());
        let cells = self.cells(rect);
        let headings = [
            ("Track", None),
            ("Device", None),
            ("Renders", Some(RENDERS)),
            ("Average", Some(AVERAGE)),
            ("Longest", Some(LONGEST)),
        ];
        for ((cell, align), (text, tooltip)) in cells.into_iter().zip(headings) {
            let response = ui.interact(cell, ui.scope_id().with(text), Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(Role::ColumnHeader, true, text));
            if let Some(tooltip) = tooltip {
                response.on_hover_text(tooltip);
            }
            if ui.is_rect_visible(cell) {
                let font = FontId::proportional(self.theme.small_text);
                let color = ui.visuals().weak_text_color();
                ui.painter()
                    .text(self.anchor(cell, align), align, text, font, color);
            }
        }
    }

    /// A line of the list, which senses clicks if it says so. The average
    /// is amber or red when it's busy.
    fn line(&self, ui: &mut Ui, line: &Line<'_>) -> Response {
        let size = vec2(ui.available_width(), self.height);
        let sense = if line.clicks {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(size, sense);
        let label = [line.channel, line.part]
            .into_iter()
            .filter(|name| !name.is_empty());
        let label = label.collect::<Vec<_>>().join(", ");
        let average = percent(line.average);
        let value = match line.renders {
            "" => average,
            renders => format!("{average}, {}", renders.to_lowercase()),
        };
        response.widget_info(|| WidgetInfo {
            current_text_value: Some(value.clone()),
            ..WidgetInfo::labeled(Role::ListItem, true, &label)
        });
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let theme = self.theme;
        let visuals = ui.style().interact(&response);
        if line.clicks && response.hovered() {
            ui.painter()
                .rect_filled(rect, theme.corner, visuals.weak_bg_fill);
        }
        let text = visuals.text_color();
        let [track, part, renders, average_cell, longest_cell] = self.cells(rect);
        let names = [
            (track, line.channel),
            (part, line.part),
            (renders, line.renders),
        ];
        for ((cell, align), name) in names {
            let font = FontId::proportional(theme.text);
            let mut job = LayoutJob::simple_singleline(name.to_owned(), font, text);
            job.wrap =
                TextWrapping::truncate_at_width((cell.width() - 2.0 * theme.padding).max(0.0));
            let galley = ui.painter().layout_job(job);
            let at = self.anchor(cell, align) - vec2(0.0, galley.size().y / 2.0);
            ui.painter().with_clip_rect(cell).galley(at, galley, text);
        }
        let figures = [
            (
                average_cell,
                Some(line.average),
                colour(line.average, theme),
            ),
            (longest_cell, line.longest, None),
        ];
        for ((cell, align), share, colour) in figures {
            let Some(share) = share else {
                continue;
            };
            let font = FontId::monospace(theme.text);
            let colour = colour.unwrap_or(text);
            ui.painter().text(
                self.anchor(cell, align),
                align,
                percent(share),
                font,
                colour,
            );
        }
        response
    }

    /// The cells of a line in `rect`, left to right, with how their text
    /// goes in them: the track's, the device's, how it renders, and the
    /// two figures'.
    fn cells(&self, rect: Rect) -> [(Rect, Align2); 5] {
        let Columns {
            track,
            part,
            renders,
            figure,
        } = self.columns;
        let mut left = rect.left();
        let mut cell = |width: f32, align| {
            let cell = Rect::from_min_size(pos2(left, rect.top()), vec2(width, rect.height()));
            left += width;
            (cell, align)
        };
        [
            cell(track, Align2::LEFT_CENTER),
            cell(part, Align2::LEFT_CENTER),
            cell(renders, Align2::LEFT_CENTER),
            cell(figure, Align2::RIGHT_CENTER),
            cell(figure, Align2::RIGHT_CENTER),
        ]
    }

    /// Where text aligned `align` in `cell` goes, inside its padding.
    fn anchor(&self, cell: Rect, align: Align2) -> Pos2 {
        let padding = self.theme.padding;
        match align.x() {
            Align::Max => pos2(cell.right() - padding, cell.center().y),
            _ => pos2(cell.left() + padding, cell.center().y),
        }
    }
}

/// Where a line's parts go across it.
#[derive(Debug, Clone, Copy)]
struct Columns {
    track: f32,
    part: f32,
    renders: f32,
    /// Each figure's.
    figure: f32,
}

impl Columns {
    /// The columns of a line `width` wide: how it renders and the figures
    /// as wide as they need, and the names sharing the rest.
    fn across(width: f32, theme: &Theme) -> Self {
        let figure = (theme.text * 5.0).min(width / 5.0);
        let renders = (theme.text * 5.0).min(width / 5.0);
        let names = (width - 2.0 * figure - renders).max(0.0);
        Self {
            track: names * 0.4,
            part: names * 0.6,
            renders,
            figure,
        }
    }
}

/// What `row` is called in the list: its track or the master, and its
/// device or its part of the channel. `None` for a device or a track the
/// project doesn't have, as one just deleted.
fn names(project: &Project, row: Row) -> Option<(&str, String)> {
    let channel = |chain| match chain {
        Chain::Track(track) => project.track(track).map(|track| track.name.as_str()),
        Chain::Master => Some("Master"),
    };
    Some(match row {
        Row::Device(id) => {
            let (chain, _, device) = project.find_device(id)?;
            (channel(chain)?, describe(device).0.to_owned())
        }
        Row::Clips(track) => (channel(Chain::Track(track))?, "Clips".to_owned()),
        Row::Modulators(chain) => (channel(chain)?, "Modulators".to_owned()),
        Row::Mixer(chain) => (channel(chain)?, "Mixer".to_owned()),
        Row::Metronome => ("", "Metronome".to_owned()),
    })
}

/// Selects what `row` is: its device, which it returns, for the devices
/// panel to bring into sight, or its track or the master.
fn select(row: Row, actions: &mut Vec<Action>) -> Option<DeviceId> {
    let chain = match row {
        Row::Device(id) => {
            let args = Args::new().with("devices", vec![id]);
            actions.push(Action::RunWith(device::SELECT, args));
            return Some(id);
        }
        Row::Clips(track) => Chain::Track(track),
        Row::Modulators(chain) | Row::Mixer(chain) => chain,
        Row::Metronome => return None,
    };
    actions.push(match chain {
        Chain::Track(track) => {
            let args = Args::new().with("tracks", vec![track]);
            Action::RunWith(edit::SELECT_TRACKS, args)
        }
        Chain::Master => Action::RunWith(edit::SELECT_MASTER, Args::new()),
    });
    None
}

/// A share of a core as a figure: "12%", or "<1%" for a sliver.
pub(crate) fn percent(share: f32) -> String {
    let percent = share * 100.0;
    if percent > 0.0 && percent < 0.5 {
        "<1%".to_owned()
    } else {
        format!("{percent:.0}%")
    }
}

/// The colour of a figure for `share` of a core: amber over [`BUSY`], red
/// over [`VERY_BUSY`], and the text's own below.
pub(crate) fn colour(share: f32, theme: &Theme) -> Option<Color32> {
    if share > VERY_BUSY {
        Some(theme.meter_over)
    } else if share > BUSY {
        Some(theme.meter_loud)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_model::{DeviceKind, TrackKind};

    use super::*;
    use crate::panels::{DEVICES, PERFORMANCE};
    use crate::testing::{State, click_at, harness};

    type Harness = egui_kittest::Harness<'static, State>;

    #[test]
    fn figures_read_as_percentages() {
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.002), "<1%");
        assert_eq!(percent(0.123), "12%");
        assert_eq!(percent(1.5), "150%");
    }

    #[test]
    fn busy_figures_turn_amber_then_red() {
        let theme = Theme::default();
        assert_eq!(colour(0.25, &theme), None);
        assert_eq!(colour(0.3, &theme), Some(theme.meter_loud));
        assert_eq!(colour(0.51, &theme), Some(theme.meter_over));
    }

    #[test]
    fn rows_come_busiest_first() {
        let cpu = |average| Cpu {
            average,
            ..Cpu::default()
        };
        let performance = Performance {
            rows: BTreeMap::from([
                (Row::Device(DeviceId(1)), cpu(0.1)),
                (Row::Mixer(Chain::Master), cpu(0.02)),
                (Row::Device(DeviceId(2)), cpu(0.4)),
            ]),
            ..Performance::default()
        };
        let rows: Vec<Row> = performance
            .busiest()
            .into_iter()
            .map(|(row, _)| row)
            .collect();
        assert_eq!(
            rows,
            [
                Row::Device(DeviceId(2)),
                Row::Device(DeviceId(1)),
                Row::Mixer(Chain::Master)
            ]
        );
        assert_eq!(performance.device(DeviceId(1)), Some(cpu(0.1)));
        assert_eq!(performance.device(DeviceId(3)), None);
    }

    fn cpu(average: f32, longest: f32) -> Cpu {
        Cpu {
            average,
            longest,
            ahead: false,
        }
    }

    /// What the project in [`project`] has.
    struct Ids {
        bass: TrackId,
        drive: DeviceId,
        limiter: DeviceId,
    }

    /// A Bass track with a device on it, a Lead track, and a device on the
    /// master.
    fn project() -> (Project, Ids) {
        let mut project = Project::new();
        let bass = project.add_track("Bass", TrackKind::Audio);
        project.add_track("Lead", TrackKind::Audio);
        let drive = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        let limiter = project.new_device(DeviceKind::TestGain { gain: 0.5 });
        let ids = Ids {
            bass,
            drive: drive.id,
            limiter: limiter.id,
        };
        project.track_mut(bass).unwrap().devices.push(drive);
        project.master.devices.push(limiter);
        (project, ids)
    }

    /// The panel's rows, top to bottom, with their averages.
    fn rows(harness: &Harness) -> Vec<(String, String)> {
        let row = |node: egui_kittest::Node<'_>| {
            let node = node.accesskit_node();
            let label = node.label().unwrap_or_default();
            (label, node.value().unwrap_or_default().to_owned())
        };
        let mut rows: Vec<_> = harness.get_all_by_role(Role::ListItem).collect();
        rows.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        rows.into_iter().map(row).collect()
    }

    fn row_names(harness: &Harness) -> Vec<String> {
        rows(harness).into_iter().map(|(name, _)| name).collect()
    }

    #[test]
    fn the_performance_panel_lists_the_busiest_first_and_selects_what_is_clicked() {
        let (project, ids) = project();
        let mut harness = harness(project);
        harness.state_mut().shell.toggle_panel(PERFORMANCE);
        harness.run();
        harness.get_by_label(NO_FIGURES);
        let gone = DeviceId(999);
        harness.state_mut().performance = Performance {
            rows: BTreeMap::from([
                (Row::Device(ids.drive), cpu(0.3, 0.6)),
                (Row::Mixer(Chain::Track(ids.bass)), cpu(0.02, 0.05)),
                (Row::Device(ids.limiter), cpu(0.1, 0.2)),
                (Row::Metronome, cpu(0.001, 0.0)),
                // A device just deleted.
                (Row::Device(gone), cpu(0.9, 1.0)),
            ]),
            channels: BTreeMap::new(),
            total: 1.321,
            ahead: 0.0,
        };
        harness.run();
        let listed = [
            ("Bass, Test Gain", "30%, live"),
            ("Master, Test Gain", "10%, live"),
            ("Bass, Mixer", "2%, live"),
            ("Metronome", "<1%, live"),
            ("Total", "132%"),
        ];
        let listed = listed.map(|(name, average)| (name.to_owned(), average.to_owned()));
        assert_eq!(rows(&harness), listed);
        for heading in ["Track", "Device", "Renders", "Average", "Longest"] {
            harness.get_by_role_and_label(Role::ColumnHeader, heading);
        }
        // A device's row selects it, and the devices panel shows it.
        let click = |harness: &mut Harness, name| {
            let at = harness
                .get_by_role_and_label(Role::ListItem, name)
                .rect()
                .center();
            click_at(harness, at);
        };
        click(&mut harness, "Bass, Test Gain");
        let select = Action::RunWith(device::SELECT, Args::new().with("devices", vec![ids.drive]));
        assert!(harness.state().actions.contains(&select));
        assert!(harness.state().shell.panel_shown(DEVICES));
        // A track's own part selects the track.
        click(&mut harness, "Bass, Mixer");
        let args = Args::new().with("tracks", vec![ids.bass]);
        let select = Action::RunWith(edit::SELECT_TRACKS, args);
        assert!(harness.state().actions.contains(&select));
        // The metronome and the total have nothing to select.
        harness.state_mut().actions.clear();
        click(&mut harness, "Metronome");
        click(&mut harness, "Total");
        let selects = |action: &Action| match action {
            Action::RunWith(id, _) => [device::SELECT, edit::SELECT_TRACKS].contains(id),
            _ => false,
        };
        assert!(!harness.state().actions.iter().any(selects));
    }

    #[test]
    fn rows_say_they_render_ahead_without_a_longest_block() {
        let ahead = Cpu {
            average: 0.4,
            longest: 3.0,
            ahead: true,
        };
        assert_eq!((ahead.renders(), ahead.live_longest()), ("Ahead", None));
        let live = cpu(0.1, 0.5);
        assert_eq!((live.renders(), live.live_longest()), ("Live", Some(0.5)));
        let (project, ids) = project();
        let mut harness = harness(project);
        harness.state_mut().shell.toggle_panel(PERFORMANCE);
        harness.state_mut().performance = Performance {
            rows: BTreeMap::from([(Row::Device(ids.drive), ahead)]),
            channels: BTreeMap::new(),
            total: 0.4,
            ahead: 0.4,
        };
        harness.run();
        let listed = [("Bass, Test Gain", "40%, ahead"), ("Total", "40%")];
        let listed = listed.map(|(name, average)| (name.to_owned(), average.to_owned()));
        assert_eq!(rows(&harness), listed);
    }

    #[test]
    fn the_rows_keep_their_places_while_the_pointer_is_over_them() {
        let (project, ids) = project();
        let mut harness = harness(project);
        harness.state_mut().shell.toggle_panel(PERFORMANCE);
        let figures = |drive, limiter| Performance {
            rows: BTreeMap::from([
                (Row::Device(ids.drive), cpu(drive, 0.0)),
                (Row::Device(ids.limiter), cpu(limiter, 0.0)),
            ]),
            channels: BTreeMap::new(),
            total: drive + limiter,
            ahead: 0.0,
        };
        harness.state_mut().performance = figures(0.3, 0.1);
        harness.run();
        let order = ["Bass, Test Gain", "Master, Test Gain", "Total"].map(String::from);
        assert_eq!(row_names(&harness), order);
        // The limiter gets busier while the pointer is over the list.
        let at = harness
            .get_by_role_and_label(Role::ListItem, "Total")
            .rect()
            .center();
        harness.hover_at(at);
        harness.state_mut().performance = figures(0.3, 0.5);
        harness.run();
        assert_eq!(row_names(&harness), order);
        assert_eq!(rows(&harness)[1].1, "50%, live");
        // Once it's gone, the busiest goes first again.
        harness.hover_at(pos2(1.0, 1.0));
        harness.run();
        let order = ["Master, Test Gain", "Bass, Test Gain", "Total"].map(String::from);
        assert_eq!(row_names(&harness), order);
    }
}
