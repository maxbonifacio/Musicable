//! The Settings window's Mouse page: what each gesture does, from where it
//! starts and the modifiers held, and the user's mouse map file.

use eframe::egui::accesskit::Role;
use eframe::egui::{
    Align, Button, ComboBox, Label, Layout, RichText, ScrollArea, Ui, WidgetInfo, vec2,
};
use musicable_commands::Modifiers;
use musicable_ui::{Gesture, MouseAction, MouseMap, Target, modifier_sets};

use crate::mouse_file::UserMouse;

/// How wide the column of places is.
const PLACE_WIDTH: f32 = 210.0;
/// How wide the column of gestures is.
const GESTURE_WIDTH: f32 = 140.0;
/// How wide the menus of actions are.
const ACTION_WIDTH: f32 = 250.0;

/// The places gestures start, in groups, in the order the page lists them.
const GROUPS: [(&str, &[Target]); 7] = [
    (
        "Arrangement",
        &[
            Target::ClipTitle,
            Target::ClipBody,
            Target::ClipEdge,
            Target::FadeHandle,
            Target::Lane,
            Target::Empty,
            Target::TrackHeader,
            Target::TrackEdge,
        ],
    ),
    (
        "Ruler and loop",
        &[
            Target::Ruler,
            Target::Strip,
            Target::LoopBrace,
            Target::LoopEdge,
            Target::Locator,
        ],
    ),
    (
        "Takes and automation",
        &[
            Target::TakeLane,
            Target::TakeHeader,
            Target::AutomationLane,
            Target::AutomationLine,
            Target::AutomationPoint,
            Target::AutomationHeader,
        ],
    ),
    (
        "Piano roll",
        &[
            Target::Note,
            Target::NoteEdge,
            Target::NoteGrid,
            Target::DrawNote,
            Target::DrawGrid,
            Target::PianoKey,
            Target::VelocityLane,
            Target::ControllerLane,
        ],
    ),
    (
        "Sample Editor",
        &[Target::WarpMarker, Target::WaveformTop, Target::Waveform],
    ),
    (
        "Mixer and devices",
        &[
            Target::SoloButton,
            Target::MixerControl,
            Target::DeviceControl,
        ],
    ),
    ("Scrolling and zooming", &[Target::Timeline]),
];

/// What the user asked for on the page.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked {
    /// Use this mouse map from now on, and save it.
    Map(MouseMap),
    /// Make every gesture do what it does at first again.
    Reset,
    /// Read the mouse map file again, after editing it.
    Reload,
    /// Create the file, to edit.
    Create,
}

/// Draws the page for `map`, the mouse map in use, and the user's file.
/// Returns what the user asked for.
pub fn show(ui: &mut Ui, map: &MouseMap, file: &UserMouse) -> Option<Asked> {
    let live = MouseMap::live();
    let sets = offered();
    let mut asked = None;
    ui.label(
        RichText::new(
            "What each gesture does, from where it starts. With modifiers held that do nothing \
             of their own there, a gesture does what it does without them.",
        )
        .weak(),
    );
    ui.add_space(6.0);
    ScrollArea::vertical()
        .max_height(380.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, (title, targets)) in GROUPS.into_iter().enumerate() {
                if i > 0 {
                    ui.add_space(8.0);
                }
                ui.label(RichText::new(title).strong());
                for &target in targets {
                    if let Some(changed) = place(ui, map, &live, &sets, target) {
                        asked = Some(Asked::Map(changed));
                    }
                }
            }
        });
    ui.separator();
    file_section(ui, map, &live, file).or(asked)
}

/// The sets of modifiers the page offers: those with Cmd only on a Mac,
/// which has the key.
fn offered() -> Vec<Modifiers> {
    let mac = cfg!(target_os = "macos");
    (modifier_sets().into_iter())
        .filter(|mods| mac || !mods.cmd)
        .collect()
}

/// A place's rows: one for each gesture that can do something there, and
/// one for each set of modifiers that gesture has an action with, now or at
/// first. Then a menu that gives it an action with other modifiers. Returns
/// the mouse map with the user's change.
fn place(
    ui: &mut Ui,
    map: &MouseMap,
    live: &MouseMap,
    sets: &[Modifiers],
    target: Target,
) -> Option<MouseMap> {
    let mut changed = None;
    let mut first = true;
    // The gestures with modifiers that have no row, which the menu offers.
    let mut more = Vec::new();
    for gesture in target.gestures() {
        for &mods in sets {
            let has = |map: &MouseMap| map.bound(target, gesture, mods).is_some();
            if mods != Modifiers::NONE && !has(map) && !has(live) {
                more.push((gesture, mods));
                continue;
            }
            if let Some(map) = row(ui, map, live, target, gesture, mods, first) {
                changed = Some(map);
            }
            first = false;
        }
    }
    if more.is_empty() {
        return changed;
    }
    ui.horizontal(|ui| {
        ui.add_space(PLACE_WIDTH + ui.spacing().item_spacing.x);
        let menu = ComboBox::from_id_salt(("more modifiers", target))
            .selected_text("Add Modifiers...")
            .width(GESTURE_WIDTH)
            .show_ui(ui, |ui| {
                for &(gesture, mods) in &more {
                    if ui.selectable_label(false, gesture.with(mods)).clicked() {
                        // It does what it did, or the first thing it can
                        // if that was nothing, until another's picked.
                        let action = (map.action(target, gesture, mods))
                            .or_else(|| target.actions(gesture).first().copied());
                        let mut map = map.clone();
                        if let Some(action) = action {
                            map.bind(target, gesture, mods, action);
                        }
                        changed = Some(map);
                    }
                }
            });
        let name = format!("{}: Add Modifiers", target.title());
        let menu = menu.response;
        menu.widget_info(|| WidgetInfo::labeled(Role::ComboBox, true, &name));
        menu.on_hover_text("Gives a gesture here an action with other modifiers held");
    });
    changed
}

/// The row of `gesture` on `target` with `mods` held: the place's title on
/// its first row, the gesture, a menu of what it can do, and a reset button
/// if it doesn't do what it does at first. Returns the mouse map with the
/// user's change.
fn row(
    ui: &mut Ui,
    map: &MouseMap,
    live: &MouseMap,
    target: Target,
    gesture: Gesture,
    mods: Modifiers,
    first: bool,
) -> Option<MouseMap> {
    let mut changed = None;
    let height = ui.spacing().interact_size.y;
    let column = Layout::left_to_right(Align::Center);
    let written = gesture.with(mods);
    let name = format!("{}: {written}", target.title());
    // What's left with no action of its own.
    let none = match mods {
        Modifiers::NONE => "Nothing".to_owned(),
        _ => format!("Same as {}", gesture.name()),
    };
    let title = |action: Option<MouseAction>| match action {
        Some(action) => action.title().to_owned(),
        None => none.clone(),
    };
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(PLACE_WIDTH, height), column, |ui| {
            ui.set_width(PLACE_WIDTH);
            if first {
                ui.add(Label::new(target.title()).truncate());
            }
        });
        ui.allocate_ui_with_layout(vec2(GESTURE_WIDTH, height), column, |ui| {
            ui.set_width(GESTURE_WIDTH);
            ui.add(Label::new(RichText::new(&written).monospace()).truncate());
        });
        let now = map.bound(target, gesture, mods);
        let shown = title(now);
        let mut picked = now;
        let menu = ComboBox::from_id_salt((target, gesture, mods))
            .selected_text(&shown)
            .width(ACTION_WIDTH)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut picked, None, title(None));
                for &action in target.actions(gesture) {
                    ui.selectable_value(&mut picked, Some(action), action.title());
                }
            });
        menu.response.widget_info(|| WidgetInfo {
            current_text_value: Some(shown.clone()),
            ..WidgetInfo::labeled(Role::ComboBox, true, &name)
        });
        if picked != now {
            changed = Some(with(map, target, gesture, mods, picked));
        }
        let at_first = live.bound(target, gesture, mods);
        if now != at_first {
            let reset = ui.add(Button::new("Reset").small());
            let reset_name = format!("Reset {name}");
            reset.widget_info(|| WidgetInfo::labeled(Role::Button, true, &reset_name));
            let hint = format!("Back to \"{}\", as at first", title(at_first));
            if reset.on_hover_text(hint).clicked() {
                changed = Some(with(map, target, gesture, mods, at_first));
            }
        }
    });
    changed
}

/// `map`, with `gesture` on `target` with `mods` held doing `action`, or
/// with no action of its own.
fn with(
    map: &MouseMap,
    target: Target,
    gesture: Gesture,
    mods: Modifiers,
    action: Option<MouseAction>,
) -> MouseMap {
    let mut map = map.clone();
    match action {
        Some(action) => map.bind(target, gesture, mods, action),
        None => map.unbind(target, gesture, mods),
    };
    map
}

/// The user's mouse map file, what was wrong with it, and what can be done
/// with it.
fn file_section(ui: &mut Ui, map: &MouseMap, live: &MouseMap, file: &UserMouse) -> Option<Asked> {
    let mut asked = None;
    let warn = ui.visuals().warn_fg_color;
    if let Some(error) = file.error() {
        ui.label(RichText::new(error).color(warn));
    }
    if !file.problems().is_empty() {
        let text = "Your mouse map file has mistakes, which Musicable leaves out:";
        ui.label(RichText::new(text).color(warn));
        for problem in file.problems() {
            ui.label(RichText::new(format!("\u{2022} {problem}.")).color(warn));
        }
    }
    if let Some(notice) = file.notice() {
        ui.label(notice);
    }
    match file.path() {
        Some(path) => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Your mouse map file:");
                ui.add(
                    Label::new(RichText::new(path.display().to_string()).monospace())
                        .selectable(true),
                );
            });
        }
        None => {
            ui.label(
                "Musicable found no folder for your mouse map file, so changes last until it \
                 closes.",
            );
        }
    }
    ui.horizontal(|ui| {
        let changed = map != live || !file.problems().is_empty() || file.error().is_some();
        let reset = ui
            .add_enabled(changed, Button::new("Reset All"))
            .on_hover_text("Makes every gesture do what it does at first again");
        if reset.clicked() {
            asked = Some(Asked::Reset);
        }
        if let Some(path) = file.path() {
            if path.exists() {
                let reload = ui
                    .button("Reload")
                    .on_hover_text("Reads the file again, after editing it");
                if reload.clicked() {
                    asked = Some(Asked::Reload);
                }
            } else if ui
                .button("Create It")
                .on_hover_text("Creates the file, which lists every gesture, to edit")
                .clicked()
            {
                asked = Some(Asked::Create);
            }
        }
    });
    asked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_groups_list_every_place_once_in_order() {
        let listed: Vec<Target> = GROUPS
            .iter()
            .flat_map(|(_, targets)| targets.iter().copied())
            .collect();
        assert_eq!(listed, Target::ALL);
    }
}
