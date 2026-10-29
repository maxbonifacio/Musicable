//! The transport bar, along the top of the window: going to the start,
//! playing, stopping and recording, Capture, the loop and punch, the
//! metronome, the tempo and the time signature, where the playhead is, and
//! the CPU meter on the right. Each is a part the user can hide, move or
//! resize, and the bar draws those shown left to right, with a line
//! between sections.

use egui::{Button, DragValue, Id, Rect, Response, RichText, Sense, Ui, vec2};
use musicable_commands::{CommandId, transport};
use musicable_model::time::TempoMap;

use crate::parts::{Draw, Part, Parts, Registry, Size, TRANSPORT_BAR};
use crate::performance::percent;
use crate::shell::{Action, View, chords};
use crate::theme::Theme;
use crate::widgets::{Icon, clock, icon_button, meter, position};

/// What the transport bar's parts draw from.
pub(crate) struct Bar<'a, 'v> {
    pub view: &'a View<'v>,
    pub theme: &'a Theme,
}

impl Bar<'_, '_> {
    /// Runs `id` when `response` was clicked, with its keys in the tooltip.
    fn run(&self, response: Response, id: CommandId, actions: &mut Vec<Action>) {
        if response.on_hover_text(hint(self.view, &id)).clicked() {
            actions.push(Action::Run(id));
        }
    }
}

/// Registers the transport bar's parts, in the order the bar shows them at
/// first.
pub(crate) fn register(registry: &mut Registry) {
    registry.register(part(
        "go_to_start",
        "Go to Start",
        "transport",
        Size::Natural,
        |ui, bar, _, actions| {
            let start = icon_button(ui, Icon::ToStart, false, bar.theme);
            bar.run(start, transport::GO_TO_START, actions);
        },
    ));
    registry.register(part(
        "play_stop",
        "Play and Stop",
        "transport",
        Size::Natural,
        |ui, bar, _, actions| {
            let playing = bar.view.playback.playing;
            let play = icon_button(ui, Icon::Play, playing, bar.theme);
            bar.run(play, transport::PLAY, actions);
            let stop = icon_button(ui, Icon::Stop, false, bar.theme);
            bar.run(stop, transport::STOP, actions);
        },
    ));
    registry.register(part(
        "record",
        "Record",
        "transport",
        Size::Natural,
        |ui, bar, _, actions| {
            let on = bar.view.playback.record;
            let record = icon_button(ui, Icon::Record, on, bar.theme);
            bar.run(record, transport::RECORD, actions);
        },
    ));
    registry.register(part(
        "capture",
        "Capture",
        "transport",
        Size::Natural,
        |ui, bar, _, actions| {
            let capture = ui.add(Button::new("Capture"));
            bar.run(capture, transport::CAPTURE, actions);
        },
    ));
    registry.register(part(
        "loop_punch",
        "Loop and Punch",
        "loop",
        Size::Natural,
        loop_punch,
    ));
    registry.register(part(
        "metronome",
        "Metronome",
        "metronome",
        Size::Natural,
        |ui, bar, _, actions| {
            let on = bar.view.project.metronome;
            let metronome = ui.add(Button::selectable(on, "Metronome"));
            bar.run(metronome, transport::TOGGLE_METRONOME, actions);
        },
    ));
    registry.register(part("tempo", "Tempo", "tempo", Size::Natural, tempo));
    registry.register(part(
        "signature",
        "Time Signature",
        "tempo",
        Size::Natural,
        |ui, bar, _, _| {
            let tempo = &bar.view.project.tempo;
            let signature = tempo.signature_at(bar.view.playback.playhead);
            ui.label(format!(
                "{}/{}",
                signature.numerator(),
                signature.denominator()
            ));
        },
    ));
    registry.register(part(
        "position",
        "Position",
        "position",
        Size::Natural,
        |ui, bar, _, _| {
            let playback = &bar.view.playback;
            let at = position(&bar.view.project.tempo, playback.playhead);
            let text = RichText::new(at)
                .monospace()
                .size(bar.theme.position_text)
                .strong();
            ui.label(text).on_hover_text("Bars, beats and sixteenths");
            ui.label(RichText::new(clock(playback.seconds)).monospace().weak());
        },
    ));
    // It pushes the parts after it to the bar's right end.
    registry.register(part(
        "space",
        "Flexible Space",
        "",
        Size::Rest,
        |ui, _, size, _| {
            ui.allocate_exact_size(vec2(size.unwrap_or(0.0), 0.0), Sense::hover());
        },
    ));
    registry.register(part(
        "cpu",
        "CPU Meter",
        "cpu",
        Size::Resizable(|theme| theme.cpu_meter.x),
        cpu,
    ));
}

/// A part of the transport bar, shown at first. Parts of the same
/// `section` go together, and a part in the section "" has no line beside
/// it.
fn part(
    id: &'static str,
    title: &'static str,
    section: &'static str,
    size: Size,
    draw: fn(&mut Ui, &Bar<'_, '_>, Option<f32>, &mut Vec<Action>),
) -> Part {
    Part {
        id,
        title,
        shown: true,
        size,
        fits: &[TRANSPORT_BAR],
        draw: Draw::Bar { section, draw },
    }
}

/// Loop, Punch In and Punch Out, which punch in and out at the loop's ends.
fn loop_punch(ui: &mut Ui, bar: &Bar<'_, '_>, _: Option<f32>, actions: &mut Vec<Action>) {
    let (view, project) = (bar.view, bar.view.project);
    let (tempo, region) = (&project.tempo, project.loop_region);
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
}

/// The tempo at the playhead, to drag or type, and the buttons that nudge
/// it.
fn tempo(ui: &mut Ui, bar: &Bar<'_, '_>, _: Option<f32>, actions: &mut Vec<Action>) {
    let mut bpm = bar.view.project.tempo.bpm_at(bar.view.playback.playhead);
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
    bar.run(ui.small_button("\u{2212}"), transport::TEMPO_DOWN, actions);
    bar.run(ui.small_button("+"), transport::TEMPO_UP, actions);
}

/// The CPU meter, `width` wide, with the load and the dropout counter.
fn cpu(ui: &mut Ui, bar: &Bar<'_, '_>, width: Option<f32>, actions: &mut Vec<Action>) {
    let (audio, theme) = (&bar.view.audio, bar.theme);
    let load = audio.load.map_or("CPU --".to_owned(), |load| {
        format!("CPU {:.0}%", load * 100.0)
    });
    // What the project's devices and mixers took, which the Performance
    // panel lists.
    let performance = bar.view.performance;
    let details = match performance.rows.is_empty() {
        true => audio.details.to_owned(),
        false => format!(
            "{}\n\nThe project took {} of a core over the last second, adding up what View > \
             Performance lists.",
            audio.details,
            percent(performance.total)
        ),
    };
    ui.label(RichText::new(load).monospace())
        .on_hover_text(&details);
    let size = vec2(width.unwrap_or(theme.cpu_meter.x), theme.cpu_meter.y);
    meter(ui, audio.load, size, theme).on_hover_text(&details);
    ui.separator();
    let dropouts = if audio.dropouts == 0 {
        RichText::new("No dropouts")
    } else {
        let plural = if audio.dropouts == 1 { "" } else { "s" };
        RichText::new(format!("{} dropout{plural}", audio.dropouts)).color(theme.warning)
    };
    let counter = ui
        .add(Button::new(dropouts).frame(false))
        .on_hover_text(format!("{details}\n\nClick to count from zero again."));
    if counter.clicked() {
        actions.push(Action::ResetDropouts);
    }
}

/// Shows the transport bar's parts left to right in `ui`, a panel along
/// the top of the window, and adds what the user asked for to `actions`.
/// Right-clicking the bar offers to hide the part there, or to customise
/// the bar.
pub(crate) fn show(
    ui: &mut Ui,
    view: &View<'_>,
    theme: &Theme,
    parts: &Parts,
    actions: &mut Vec<Action>,
) {
    let bar = Bar { view, theme };
    let lineup = parts.shown(TRANSPORT_BAR, theme);
    // How far the parts after the flexible space reached past it last
    // frame, so it leaves them that much room at the bar's right end.
    let tail_id = Id::unique("transport bar tail");
    let tail = ui.data(|data| data.get_temp::<f32>(tail_id));
    // Where each part is across the bar, by id.
    let mut spans = Vec::with_capacity(lineup.len());
    let mut rest = None;
    ui.add_space(4.0);
    let row = ui.horizontal(|ui| {
        let mut section = None;
        for shown in &lineup {
            let Draw::Bar { section: own, draw } = shown.part.draw else {
                continue;
            };
            if let Some(last) = section.replace(own)
                && last != own
                && !last.is_empty()
                && !own.is_empty()
            {
                ui.separator();
            }
            let size = match shown.part.size {
                Size::Rest => Some((ui.available_width() - tail.unwrap_or(0.0)).max(0.0)),
                _ => shown.size,
            };
            let part = ui.scope(|ui| draw(ui, &bar, size, actions)).response;
            if matches!(shown.part.size, Size::Rest) {
                rest = Some(part.rect.right());
            }
            spans.push((shown.part.id, part.rect.x_range()));
        }
    });
    ui.add_space(4.0);
    if let Some(right) = rest {
        let end = spans.last().map_or(right, |(_, span)| span.max);
        let measured = end - right;
        if tail.is_none_or(|tail| (tail - measured).abs() > 0.5) {
            ui.data_mut(|data| data.insert_temp(tail_id, measured));
            ui.ctx()
                .request_discard("the transport bar measured the parts after its space");
        }
    }
    let rect = Rect::from_x_y_ranges(
        ui.max_rect().x_range(),
        row.response.rect.expand(4.0).y_range(),
    );
    let anchor = ui.interact(rect, Id::unique("transport bar parts"), Sense::hover());
    let clicked = anchor.container_secondary_clicked().then(|| {
        let x = ui.input(|input| input.pointer.interact_pos())?.x;
        let (id, _) = spans.iter().find(|(_, span)| span.contains(x))?;
        Some(*id)
    });
    parts.context_menu(&anchor, TRANSPORT_BAR, clicked);
}

/// A command's title and keys, for a tooltip: "Play (Shift+Space)".
fn hint(view: &View<'_>, id: &CommandId) -> String {
    let title = view
        .commands
        .get(id)
        .map_or(id.as_str(), |c| c.title.as_str());
    match chords(view, id).as_str() {
        "" => title.to_owned(),
        chords => format!("{title} ({chords})"),
    }
}
