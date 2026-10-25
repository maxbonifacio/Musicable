//! The piano roll's context menus, as the arrangement's: commands from the
//! registry, with their arguments taken from what the menu was opened on.
//! And the menu of what the lane under the notes can show.
//!
//! Each place has its list of commands. A parameter that names a clip gets
//! the clip the piano roll shows, one that names notes the note the menu was
//! opened on, or the selected notes if it's one of them, and a position the
//! place the menu was opened. A command that needs an argument the menu can't
//! give is left out. The edit commands take no arguments: the menu gives the
//! piano roll the focus, so they act on its notes.

use egui::{Popup, Response, Role, ScrollArea, Ui, WidgetInfo};
use musicable_commands::editor::Lane;
use musicable_commands::{
    Args, Command, CommandId, Type, Value, controllers, edit, notes, transport, view,
};
use musicable_model::clip::{Controller, NoteId};

use super::{Hit, Scene, Spot, run};
use crate::menus::context_menu;
use crate::shell::Action;

/// A context menu's commands, in order. `None` is a separator.
type Items = &'static [Option<CommandId>];

const NOTE: Items = &[
    Some(edit::CUT),
    Some(edit::COPY),
    Some(edit::PASTE),
    Some(edit::DUPLICATE),
    Some(edit::DELETE),
    None,
    Some(edit::QUANTIZE),
    None,
    Some(view::ZOOM_TO_SELECTION),
    Some(edit::SELECT_ALL),
];

const ROWS: Items = &[
    Some(edit::PASTE),
    Some(edit::SELECT_ALL),
    None,
    Some(notes::TOGGLE_DRAW),
    Some(edit::QUANTIZE),
];

/// Under the notes' and the rows' own commands.
const GRID: Items = &[
    None,
    Some(view::NARROW_GRID),
    Some(view::WIDEN_GRID),
    Some(view::TRIPLET_GRID),
    Some(view::FIXED_GRID),
    Some(view::TOGGLE_SNAP),
];

const RULER: Items = &[Some(transport::LOCATE), None, Some(view::TOGGLE_FOLLOW)];

/// The lane's while it shows a controller.
const CONTROLLER: Items = &[Some(controllers::CLEAR)];

/// How tall the menu of what the lane can show gets before it scrolls, in
/// points.
const LANES_HEIGHT: f32 = 320.0;

/// The commands of the context menu for `hit`.
fn items(hit: Hit) -> Vec<Option<CommandId>> {
    let lists: &[Items] = match hit {
        Hit::Note { .. } | Hit::Velocity(Some(_)) => &[NOTE, GRID],
        Hit::Row(_) | Hit::Key(_) | Hit::Velocity(None) => &[ROWS, GRID],
        Hit::Ruler => &[RULER],
        Hit::Controller => &[CONTROLLER, GRID],
    };
    lists
        .iter()
        .flat_map(|items| items.iter().cloned())
        .collect()
}

/// Shows the context menu for `spot`.
pub(super) fn show(ui: &mut Ui, scene: &Scene<'_>, spot: Spot, actions: &mut Vec<Action>) {
    let view = scene.view;
    let mut entries: Vec<Option<(&Command, Args)>> = Vec::new();
    for item in items(spot.hit) {
        let Some(id) = item else {
            entries.push(None);
            continue;
        };
        let Some(command) = view.commands.get(&id) else {
            continue;
        };
        if let Some(args) = args(command, scene, spot) {
            entries.push(Some((command, args)));
        }
    }
    context_menu(ui, view, &entries, actions);
}

/// The arguments `command` takes from `spot`, or `None` if it needs one
/// that can't be taken from there.
fn args(command: &Command, scene: &Scene<'_>, spot: Spot) -> Option<Args> {
    let mut args = Args::new();
    for param in &command.params {
        let value: Option<Value> = match (param.ty, spot.hit) {
            (Type::Clip, _) => Some(scene.clip.id.into()),
            // The selected notes, if the menu was opened on one of them.
            (Type::List(Type::Note), hit) => hit.note().map(|note| {
                let notes: Vec<NoteId> = if scene.selected(note) {
                    let selected = &scene.editor().piano_roll.notes;
                    selected.iter().copied().collect()
                } else {
                    vec![note]
                };
                notes.into()
            }),
            (Type::Beats, _) if param.name == "at" => Some(spot.at.into()),
            _ => None,
        };
        match value {
            Some(value) => args.set(&param.name, value),
            None if param.optional => {}
            None => return None,
        }
    }
    Some(args)
}

/// The menu of what the lane can show, which a click on its name,
/// `chooser`, opens: the notes' velocities, the controllers the clip moves,
/// those keyboards and pedals have, then the rest.
pub(super) fn lanes(chooser: &Response, scene: &Scene<'_>, actions: &mut Vec<Action>) {
    let shown = scene.editor().piano_roll.lane;
    let name = |lane| match lane {
        Lane::Velocity => "Velocity".to_owned(),
        Lane::Controller(controller) => format!("{controller}"),
    };
    chooser.widget_info(|| WidgetInfo {
        current_text_value: Some(name(shown)),
        ..WidgetInfo::labeled(Role::ComboBox, true, "Lane")
    });
    let moved: Vec<Controller> = scene.midi.controllers.keys().copied().collect();
    let common: Vec<Controller> = Controller::COMMON
        .into_iter()
        .filter(|c| !moved.contains(c))
        .collect();
    let rest: Vec<Controller> = Controller::all()
        .filter(|c| !moved.contains(c) && !common.contains(c))
        .collect();
    Popup::menu(chooser).show(|ui| {
        ScrollArea::vertical()
            .max_height(LANES_HEIGHT)
            .show(ui, |ui| {
                let mut item = |ui: &mut Ui, lane: Lane| {
                    if ui.selectable_label(lane == shown, name(lane)).clicked() && lane != shown {
                        match lane {
                            Lane::Velocity => {
                                run(actions, controllers::SHOW_VELOCITIES, Args::new());
                            }
                            Lane::Controller(c) => {
                                let args = Args::new().with("controller", i64::from(c.number()));
                                run(actions, controllers::SHOW, args);
                            }
                        }
                    }
                };
                item(ui, Lane::Velocity);
                for group in [&moved, &common, &rest] {
                    if !group.is_empty() {
                        ui.separator();
                    }
                    for &controller in group {
                        item(ui, Lane::Controller(controller));
                    }
                }
            });
    });
}
