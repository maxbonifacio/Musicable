//! The parts of the tracks' headers: the track's colour, its name, the
//! button that shows its automation, mute, solo and arm, and its meter.
//! Headers are painted, so a hundred tracks stay quick: each part paints
//! into its column of a header, and the columns are the same in every
//! header, so the buttons line up. The master track's header has the same
//! parts, but for mute, solo and arm.

use egui::{Align2, FontId, Id, Painter, Rangef, Rect, Role, Sense, Ui, WidgetInfo, pos2};
use musicable_commands::mixer::Change;
use musicable_commands::{Args, automation};
use musicable_model::{Chain, Track, TrackKind};

use super::paint::{master_painter, tracks_painter};
use super::{Row, Scene, run};
use crate::mixer::{meter_colour, meter_height};
use crate::mouse::{Gesture, MouseAction, Target, modifiers};
use crate::parts::{self, Draw, Registry, Shown, Size, TRACK_HEADER};
use crate::shell::Action;
use crate::widgets::{Face, paint_toggle};

/// What a track header's part paints: where it goes in the header of a
/// track, or of the master, and with what.
pub(crate) struct HeaderPart<'a> {
    scene: &'a Scene<'a>,
    /// Paints only in the part of the headers the header is in: the
    /// tracks' or the master's.
    painter: &'a Painter,
    owner: Owner<'a>,
    /// The part's column of the header, from its top to its bottom.
    rect: Rect,
}

/// Whose header it is.
#[derive(Clone, Copy)]
enum Owner<'a> {
    /// The track in the row.
    Track(&'a Row, &'a Track),
    Master,
}

/// Registers the parts of the tracks' headers, in the order headers show
/// them at first.
pub(crate) fn register(registry: &mut Registry) {
    registry.register(part(
        "colour",
        "Colour",
        Size::Resizable(|theme| theme.colour_strip),
        |_, part, _| {
            let theme = part.scene.theme;
            let colour = match part.owner {
                Owner::Track(row, _) => theme.clip(row.index),
                Owner::Master => theme.master,
            };
            part.painter.rect_filled(part.rect, 0.0, colour);
        },
    ));
    registry.register(part("name", "Name", Size::Rest, |_, part, _| {
        let theme = part.scene.theme;
        let inset = theme.padding / 2.0;
        let room = Rect::from_x_y_ranges(
            part.rect.left() + inset..=part.rect.right() - inset,
            part.rect.top() + theme.padding..=part.rect.bottom(),
        );
        let name = match part.owner {
            Owner::Track(_, track) => track.name.as_str(),
            Owner::Master => "Master",
        };
        let painter = part.painter;
        painter
            .with_clip_rect(room.intersect(painter.clip_rect()))
            .text(
                room.left_top(),
                Align2::LEFT_TOP,
                name,
                FontId::proportional(theme.text),
                theme.header_text,
            );
    }));
    registry.register(part(
        "automation",
        "Automation Button",
        Size::Fixed(|theme| theme.button.x),
        |ui, part, actions| paint_buttons(ui, part, &[Button::Automation], actions),
    ));
    registry.register(part(
        "buttons",
        "Mute, Solo and Arm",
        Size::Fixed(|theme| 3.0 * theme.button.x + theme.padding),
        |ui, part, actions| {
            let buttons = [Button::Mute, Button::Solo, Button::Arm];
            paint_buttons(ui, part, &buttons, actions);
        },
    ));
    registry.register(part(
        "meter",
        "Meter",
        Size::Resizable(|theme| theme.header_meter + theme.padding / 2.0),
        |_, part, _| {
            let scene = part.scene;
            let theme = scene.theme;
            // Down the column's right, set in from the parts on its left.
            let width = (part.rect.width() - theme.padding / 2.0).max(1.0);
            let top = part.rect.top() + theme.padding;
            let meter = Rect::from_x_y_ranges(
                part.rect.right() - width..=part.rect.right(),
                top..=(part.rect.bottom() - theme.padding).max(top),
            );
            let level = match part.owner {
                Owner::Track(_, track) => scene.view.levels.track(track.id),
                Owner::Master => scene.view.levels.master,
            };
            let height = meter_height(level.peak[0].max(level.peak[1]));
            part.painter.rect_filled(meter, 0.0, theme.divider);
            if height > 0.0 {
                let mut filled = meter;
                filled.set_top(meter.bottom() - meter.height() * height);
                part.painter
                    .rect_filled(filled, 0.0, meter_colour(height, theme));
            }
        },
    ));
}

/// A part of the tracks' headers, shown at first.
fn part(
    id: &'static str,
    title: &'static str,
    size: Size,
    draw: fn(&mut Ui, &HeaderPart<'_>, &mut Vec<Action>),
) -> parts::Part {
    parts::Part {
        id,
        title,
        shown: true,
        size,
        fits: &[TRACK_HEADER],
        draw: Draw::Header(draw),
    }
}

/// Where the parts in `lineup` go across the headers: left to right, half
/// a padding apart, with a padding at the right. The parts that take what
/// the others leave share it.
pub(super) fn columns<'p>(
    scene: &Scene<'_>,
    lineup: &[Shown<'p>],
) -> Vec<(&'p parts::Part, Rangef)> {
    let theme = scene.theme;
    let headers = scene.layout.headers;
    let gap = theme.padding / 2.0;
    let gaps = gap * lineup.len().saturating_sub(1) as f32;
    let sized: f32 = lineup.iter().filter_map(|shown| shown.size).sum();
    let sharing = lineup.iter().filter(|shown| shown.size.is_none()).count();
    let left = headers.width() - sized - gaps - theme.padding;
    let share = (left / sharing.max(1) as f32).max(0.0);
    let mut x = headers.left();
    let columns = lineup.iter().map(|shown| {
        let width = shown.size.unwrap_or(share);
        let column = Rangef::new(x, x + width);
        x += width + gap;
        (shown.part, column)
    });
    columns.collect()
}

/// Paints the parts of the header of each track in sight, and of the
/// master's, in `columns`.
pub(super) fn paint(
    ui: &mut Ui,
    painter: &Painter,
    scene: &Scene<'_>,
    columns: &[(&parts::Part, Rangef)],
    actions: &mut Vec<Action>,
) {
    let mut header = |ui: &mut Ui, painter: &Painter, owner: Owner<'_>, rows: Rangef| {
        for &(part, column) in columns {
            let Draw::Header(draw) = part.draw else {
                continue;
            };
            let rect = Rect::from_x_y_ranges(column, rows);
            let header = HeaderPart {
                scene,
                painter,
                owner,
                rect,
            };
            draw(ui, &header, actions);
        }
    };
    let tracks = tracks_painter(painter, scene);
    for row in scene.layout.visible_rows() {
        let track = &scene.project().tracks[row.index];
        let rows = Rangef::new(row.top, row.bottom());
        header(ui, &tracks, Owner::Track(row, track), rows);
    }
    let master = scene.layout.master;
    let rows = Rangef::new(master.top, master.bottom());
    header(ui, &master_painter(painter, scene), Owner::Master, rows);
}

/// The header's buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Button {
    /// Shows the track's automation lanes, or hides them.
    Automation,
    Mute,
    Solo,
    Arm,
}

/// `buttons` along the top of the part's column, half a padding apart,
/// each in its place even when the track doesn't have it: return and
/// group tracks have no arm, and the master has only its automation's.
/// A row partly scrolled away has none.
fn paint_buttons(
    ui: &mut Ui,
    part: &HeaderPart<'_>,
    buttons: &[Button],
    actions: &mut Vec<Action>,
) {
    let scene = part.scene;
    let track = match part.owner {
        Owner::Track(_, track) => track,
        Owner::Master => {
            if buttons.contains(&Button::Automation) {
                master_automation_button(ui, part, actions);
            }
            return;
        }
    };
    let theme = scene.theme;
    let size = theme.button;
    let mods = modifiers(ui.input(|i| i.modifiers));
    for (i, &button) in buttons.iter().enumerate() {
        let x = part.rect.left() + i as f32 * (size.x + theme.padding / 2.0);
        let rect = Rect::from_min_size(pos2(x, part.rect.top() + theme.padding), size);
        let fits = fits(part, rect);
        let records = matches!(track.kind, TrackKind::Audio | TrackKind::Midi);
        if !fits || (button == Button::Arm && !records) {
            continue;
        }
        let name = &track.name;
        let (face, on, colour, label) = match button {
            Button::Mute => (
                Face::Text("M"),
                track.muted,
                theme.mute,
                format!("Mute track {name}"),
            ),
            Button::Solo => (
                Face::Text("S"),
                track.solo,
                theme.solo,
                format!("Solo track {name}"),
            ),
            Button::Arm => (
                Face::Dot,
                track.armed,
                theme.arm,
                format!("Arm track {name}"),
            ),
            Button::Automation => (
                Face::Text("A"),
                (scene.editor().automation).contains_key(&Chain::Track(track.id)),
                theme.automation,
                format!("Show automation of track {name}"),
            ),
        };
        let id = Id::unique(("header button", track.id, button));
        let response = ui.interact(rect, id, Sense::click());
        response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, &label));
        paint_toggle(ui, rect, &response, face, on, colour, theme);
        if !response.clicked() {
            continue;
        }
        let change = match button {
            Button::Automation => {
                let args = Args::new().with("track", track.id);
                run(actions, automation::SHOW, args);
                continue;
            }
            Button::Mute => Change::Muted(track.id, !on),
            Button::Arm => Change::Armed(track.id, !on),
            Button::Solo => {
                let action = scene.mouse.action(Target::SoloButton, Gesture::Click, mods);
                Change::Solo {
                    track: track.id,
                    solo: !on,
                    exclusive: !on && action != Some(MouseAction::AddSolo),
                }
            }
        };
        actions.push(Action::Mixer(change));
    }
}

/// The master's A button, which shows its automation lanes or hides them.
fn master_automation_button(ui: &mut Ui, part: &HeaderPart<'_>, actions: &mut Vec<Action>) {
    let theme = part.scene.theme;
    let rect = Rect::from_min_size(
        pos2(part.rect.left(), part.rect.top() + theme.padding),
        theme.button,
    );
    if !fits(part, rect) {
        return;
    }
    let on = (part.scene.editor().automation).contains_key(&Chain::Master);
    let response = ui.interact(rect, Id::unique("master automation button"), Sense::click());
    let label = "Show automation of the master track";
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, on, label));
    paint_toggle(
        ui,
        rect,
        &response,
        Face::Text("A"),
        on,
        theme.automation,
        theme,
    );
    if response.clicked() {
        run(actions, automation::SHOW, Args::new().with("master", true));
    }
}

/// Whether a button at `rect` fits in the header `part` is in, in sight.
fn fits(part: &HeaderPart<'_>, rect: Rect) -> bool {
    let headers = part.scene.layout.headers;
    part.painter.clip_rect().contains_rect(rect)
        && rect.bottom() <= part.rect.bottom()
        && rect.left() > headers.left()
}

/// The header part in `columns` at `x`, if one is there.
pub(super) fn part_at(columns: &[(&parts::Part, Rangef)], x: f32) -> Option<&'static str> {
    let (part, _) = columns.iter().find(|(_, column)| column.contains(x))?;
    Some(part.id)
}
