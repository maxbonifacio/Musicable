//! A side input's sources, as the Compressor's and the Gate's Sidechain
//! column starts with them: a picker for each wire into it, with the wire's
//! gain under it, and a + by the column's name that adds another source,
//! summed with the others. A picker's menu has No Input, which takes its
//! wire away, then the other tracks, each opening to its taps, then This
//! track, opening to the points before the device on its own chain. Taps
//! that would close a loop, or that the side input takes already, are
//! there but can't be picked.

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Button, FontId, Popup, Rect, Response, Role, Sense, Shape, Stroke, StrokeKind, Ui,
    WidgetInfo, pos2, vec2,
};
use musicable_commands::{Args, wire};
use musicable_model::native::{ParamInfo, Scale, Unit};
use musicable_model::routing::{Port, Tap, Wire, WireError};
use musicable_model::{Chain, Project, TrackKind};

use super::{Control, SIDECHAIN, Setting};
use crate::mixer::{automated_value, name_menu};
use crate::mouse::MouseMap;
use crate::shell::{Action, View};
use crate::theme::Theme;

/// A wire's gain, as the bar under its picker shows it: from silence to
/// +24 dB, unity by default.
const GAIN: ParamInfo = ParamInfo {
    key: "gain",
    name: "Gain",
    group: SIDECHAIN,
    min: 0.0,
    max: Wire::GAIN_MAX as f64,
    default: 1.0,
    unit: Unit::Gain,
    scale: Scale::Fader,
    stepped: false,
    modulated: false,
};

/// What the user picked from a source menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    NoInput,
    Tap(Tap),
}

/// Shows the sources of `port`, the side input of the device the panel
/// calls `name`: a picker for each wire into it, with the wire's gain, or
/// one that says No Input. Asks for the wires the user makes, changes and
/// takes away.
pub(super) fn sources(
    ui: &mut Ui,
    port: Port,
    name: &str,
    view: &View<'_>,
    theme: &Theme,
    mouse: &MouseMap,
    actions: &mut Vec<Action>,
) {
    let project = view.project;
    let Some(chain) = project.port_chain(port) else {
        return;
    };
    let width = theme.devices.control;
    let wires: Vec<Wire> = project.wires_into(port).copied().collect();
    if wires.is_empty() {
        let label = format!("{name} {SIDECHAIN} Source");
        if let Some(Pick::Tap(tap)) = picker(ui, project, port, chain, None, &label, theme) {
            actions.push(connect(port, tap));
        }
        return;
    }
    for (n, wire) in wires.iter().enumerate() {
        // After the first, numbered, so screen readers tell them apart.
        let number = match n {
            0 => String::new(),
            n => format!(" {}", n + 1),
        };
        ui.push_id(wire.id, |ui| {
            let label = format!("{name} {SIDECHAIN} Source{number}");
            let on = |args: Args| args.with("wire", wire.id);
            match picker(ui, project, port, chain, Some(wire.from), &label, theme) {
                Some(Pick::NoInput) => {
                    actions.push(Action::RunWith(wire::DISCONNECT, on(Args::new())));
                }
                Some(Pick::Tap(tap)) => {
                    let args = wire::with_tap(on(Args::new()), tap);
                    actions.push(Action::RunWith(wire::SET_SOURCE, args));
                }
                None => {}
            }
            let setting = Setting::WireGain(wire.id);
            let playhead = view.playback.playhead;
            let own = f64::from(wire.gain);
            let value = automated_value(project, setting.address(), playhead, own);
            let control = Control {
                setting,
                info: &GAIN,
                name: GAIN.name,
                label: format!("{name} {}{number}", GAIN.full_name()),
                project,
                levels: view.levels,
            };
            control.show(ui, GAIN.clamp(value), width, theme, mouse, actions);
        });
    }
}

/// The + at the right of the Sidechain column's name, `heading`, which adds
/// another source to `port`, the side input of the device the panel calls
/// `name`, once it has one.
pub(super) fn add(
    ui: &mut Ui,
    heading: Rect,
    port: Port,
    name: &str,
    view: &View<'_>,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let project = view.project;
    let Some(chain) = project.port_chain(port) else {
        return;
    };
    if project.wires_into(port).next().is_none() {
        return;
    }
    let side = heading.height();
    let right = ui.max_rect().right();
    let rect = Rect::from_min_max(
        pos2(right - side, heading.top()),
        pos2(right, heading.bottom()),
    );
    let response = ui.interact(rect, ui.scope_id().with("add source"), Sense::click());
    let label = format!("Add a Source to {name} {SIDECHAIN}");
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();
        if response.hovered() {
            painter.rect_filled(rect, theme.corner, visuals.weak_bg_fill);
        }
        let font = FontId::proportional(theme.small_text);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "+",
            font,
            visuals.text_color(),
        );
    }
    let response = response.on_hover_text("Adds another source, summed with the others");
    let picked = Popup::menu(&response)
        .show(|ui| menu(ui, project, port, chain, None, false))
        .and_then(|shown| shown.inner);
    if let Some(Pick::Tap(tap)) = picked {
        actions.push(connect(port, tap));
    }
}

/// A source's picker, named `label`, showing `current`, the tap of the wire
/// it's for, or No Input. Returns what the user picked from its menu.
fn picker(
    ui: &mut Ui,
    project: &Project,
    port: Port,
    chain: Chain,
    current: Option<Tap>,
    label: &str,
    theme: &Theme,
) -> Option<Pick> {
    let text = current.map_or("No Input".to_owned(), |tap| {
        source_name(project, tap, chain)
    });
    let size = vec2(theme.devices.control, ui.spacing().interact_size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    name_menu(&response, label.to_owned(), &text);
    if ui.is_rect_visible(rect) {
        paint_picker(ui, &response, rect, &text, theme);
    }
    let hint = match current.and_then(|tap| project.tap_name(tap)) {
        Some(tap) => format!("The key comes from {tap}."),
        None => {
            "Where the key comes from. With No Input, it's what comes into the device.".to_owned()
        }
    };
    let response = response.on_hover_text(hint);
    Popup::menu(&response)
        .show(|ui| menu(ui, project, port, chain, current, true))
        .and_then(|shown| shown.inner)
}

/// Paints a picker at `rect`, which `response` is for, showing `text`, cut
/// short to fit, and the triangle a menu of choices has.
fn paint_picker(ui: &Ui, response: &Response, rect: Rect, text: &str, theme: &Theme) {
    let visuals = ui.style().interact(response);
    let painter = ui.painter();
    painter.rect(
        rect,
        theme.corner,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    let padding = ui.spacing().button_padding.x;
    let icon = ui.spacing().icon_width;
    let icon = Rect::from_center_size(
        pos2(rect.right() - padding - icon / 2.0, rect.center().y),
        vec2(icon * 0.7, icon * 0.45),
    );
    let triangle = vec![icon.left_top(), icon.right_top(), icon.center_bottom()];
    painter.add(Shape::convex_polygon(
        triangle,
        visuals.fg_stroke.color,
        Stroke::NONE,
    ));
    let ink = visuals.text_color();
    let room = (icon.left() - padding) - (rect.left() + padding);
    let font = FontId::proportional(theme.small_text);
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, ink);
    job.wrap = TextWrapping::truncate_at_width(room.max(0.0));
    let galley = painter.layout_job(job);
    let at = pos2(
        rect.left() + padding,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(at, galley, ink);
}

/// The menu of what `port`, the side input of a device on `chain`, can take
/// from: No Input, if `none`, then the other tracks, each opening to its
/// taps, then This track, opening to the points before the device. The
/// `current` tap is ticked. Returns what the user picked, if it's a change.
fn menu(
    ui: &mut Ui,
    project: &Project,
    port: Port,
    chain: Chain,
    current: Option<Tap>,
    none: bool,
) -> Option<Pick> {
    let mut picked = None;
    if none
        && ui
            .add(Button::selectable(current.is_none(), "No Input"))
            .clicked()
    {
        if current.is_some() {
            picked = Some(Pick::NoInput);
        }
        ui.close();
    }
    // A tap carries sound, unless it's a MIDI track's notes.
    let sound = |tap: &Tap| match *tap {
        Tap::PreFx(track) => project
            .track(track)
            .is_some_and(|t| t.kind != TrackKind::Midi),
        _ => true,
    };
    let refused = |tap: Tap| match current == Some(tap) {
        true => None,
        false => project.check_wire(tap, port).err(),
    };
    let mut item = |ui: &mut Ui, tap: Tap| {
        let on = current == Some(tap);
        let point = project.tap_point_name(tap).unwrap_or_default();
        let refusal = refused(tap);
        let button = ui.add_enabled(refusal.is_none(), Button::selectable(on, point));
        let button = match refusal {
            Some(why) => button.on_disabled_hover_text(why_not(why)),
            None => button,
        };
        if button.clicked() {
            if !on {
                picked = Some(Pick::Tap(tap));
            }
            ui.close();
        }
    };
    let others: Vec<_> = (project.tracks.iter())
        .filter(|track| Chain::Track(track.id) != chain)
        .collect();
    if none && !others.is_empty() {
        ui.separator();
    }
    for track in others {
        let taps: Vec<Tap> = project.taps(Chain::Track(track.id));
        let taps: Vec<Tap> = taps.into_iter().filter(sound).collect();
        let open = taps.iter().any(|&tap| refused(tap).is_none());
        ui.add_enabled_ui(open, |ui| {
            ui.menu_button(track.name.as_str(), |ui| {
                for &tap in &taps {
                    item(ui, tap);
                }
            })
        });
    }
    // The points before the device, on its own chain.
    let before: Vec<Tap> = (project.taps(chain).into_iter())
        .take_while(|&tap| tap != Tap::Device(port.device()))
        .filter(sound)
        .collect();
    if !before.is_empty() {
        ui.separator();
        ui.menu_button("This track", |ui| {
            for &tap in &before {
                item(ui, tap);
            }
        });
    }
    picked
}

/// Why a tap can't be picked, for its item's hover text.
fn why_not(why: WireError) -> &'static str {
    match why {
        WireError::Wired(_) => "This side input takes it already.",
        WireError::Loop => "That would make a loop: what the device puts out reaches it.",
        WireError::Notes => "It's notes, which no wire takes yet.",
        WireError::NoTap | WireError::NoPort => "It isn't there.",
    }
}

/// What a picker shows for `tap`, a source of a device on `chain`: "Kick,
/// Post FX" for another track's, or "after EQ" for a point earlier on the
/// device's own.
fn source_name(project: &Project, tap: Tap, chain: Chain) -> String {
    let point = project.tap_point_name(tap).unwrap_or_default();
    match project.tap_chain(tap) {
        Some(Chain::Track(track)) if Chain::Track(track) != chain => {
            let track = project.track(track).map_or("", |t| t.name.as_str());
            format!("{track}, {point}")
        }
        Some(Chain::Master) if chain != Chain::Master => format!("Master, {point}"),
        _ => point,
    }
}

/// The command that wires `tap` into `port`.
fn connect(port: Port, tap: Tap) -> Action {
    let args = Args::new().with("device", port.device());
    Action::RunWith(wire::CONNECT, wire::with_tap(args, tap))
}
