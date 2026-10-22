//! The piano roll's context menus, as the arrangement's: commands from the
//! registry, with their arguments taken from what the menu was opened on.
//!
//! Each place has its list of commands. A parameter that names a clip gets
//! the clip the piano roll shows, one that names notes the note the menu was
//! opened on, or the selected notes if it's one of them, and a position the
//! place the menu was opened. A command that needs an argument the menu can't
//! give is left out. The edit commands take no arguments: the menu gives the
//! piano roll the focus, so they act on its notes.

use egui::Ui;
use musicable_commands::{Args, Command, CommandId, Type, Value, edit, notes, transport, view};
use musicable_model::clip::NoteId;

use super::{Hit, Scene, Spot};
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
    Some(notes::QUANTIZE),
    None,
    Some(view::ZOOM_TO_SELECTION),
    Some(edit::SELECT_ALL),
];

const ROWS: Items = &[
    Some(edit::PASTE),
    Some(edit::SELECT_ALL),
    None,
    Some(notes::TOGGLE_DRAW),
    Some(notes::QUANTIZE),
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

/// The commands of the context menu for `hit`.
fn items(hit: Hit) -> Vec<Option<CommandId>> {
    let lists: &[Items] = match hit {
        Hit::Note { .. } | Hit::Velocity(Some(_)) => &[NOTE, GRID],
        Hit::Row(_) | Hit::Key(_) | Hit::Velocity(None) => &[ROWS, GRID],
        Hit::Ruler => &[RULER],
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
