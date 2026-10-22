//! The arrangement's context menus: commands from the registry, with their
//! arguments taken from what the menu was opened on.
//!
//! Each place has its list of commands. A command's title, keys and
//! condition come from the registry, and each of its parameters that names
//! a clip, a track, a locator or a position is filled in from where the
//! menu was opened. A command that needs an argument the menu can't give is
//! left out.

use egui::{Button, Ui};
use musicable_commands::{
    Args, Command, CommandId, Type, Value, arrange, edit, file, locator, track, transport, view,
};
use musicable_model::TrackKind;

use super::{Hit, Scene, Spot};
use crate::shell::{Action, toggled, with_keys};

/// A context menu's commands, in order. `None` is a separator.
type Items = &'static [Option<CommandId>];

const CLIP: Items = &[
    Some(edit::CUT),
    Some(edit::COPY),
    Some(edit::PASTE),
    Some(edit::DUPLICATE),
    Some(edit::DELETE),
    None,
    Some(edit::SPLIT),
    Some(edit::RENAME),
    None,
    Some(view::ZOOM_TO_SELECTION),
];

const LANE: Items = &[
    Some(edit::PASTE),
    None,
    Some(arrange::INSERT_MIDI_CLIP),
    Some(file::IMPORT_AUDIO),
    Some(locator::ADD),
    None,
    Some(edit::SELECT_ALL),
];

/// Under the clips and the lanes' own commands.
const GRID: Items = &[
    None,
    Some(view::NARROW_GRID),
    Some(view::WIDEN_GRID),
    Some(view::TRIPLET_GRID),
    Some(view::FIXED_GRID),
    Some(view::TOGGLE_SNAP),
];

const HEADER: Items = &[
    Some(edit::RENAME),
    Some(track::DELETE),
    None,
    Some(track::NEW_AUDIO_TRACK),
    Some(track::NEW_MIDI_TRACK),
    Some(track::NEW_RETURN_TRACK),
    Some(track::NEW_GROUP_TRACK),
];

const EMPTY: Items = &[
    Some(edit::PASTE),
    None,
    Some(track::NEW_AUDIO_TRACK),
    Some(track::NEW_MIDI_TRACK),
    Some(track::NEW_RETURN_TRACK),
    Some(track::NEW_GROUP_TRACK),
    None,
    Some(file::IMPORT_AUDIO),
];

const RULER: Items = &[
    Some(transport::LOCATE),
    Some(locator::ADD),
    None,
    Some(transport::TOGGLE_LOOP),
    Some(view::TOGGLE_FOLLOW),
];

const LOCATOR: Items = &[
    Some(transport::LOCATE),
    Some(edit::RENAME),
    Some(locator::DELETE),
];

/// The commands of the context menu for `hit`.
fn items(hit: Hit) -> Vec<Option<CommandId>> {
    let lists: &[Items] = match hit {
        Hit::Clip { .. } => &[CLIP, GRID],
        Hit::Lane(_) => &[LANE, GRID],
        Hit::Empty => &[EMPTY],
        Hit::Header(_) | Hit::TrackEdge(_) => &[HEADER],
        Hit::Ruler | Hit::Strip | Hit::LoopBrace | Hit::LoopEdge(_) => &[RULER],
        Hit::Locator(_) => &[LOCATOR],
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
        if let Some(args) = args(command, scene, spot).filter(|_| fits(&id, scene, spot)) {
            entries.push(Some((command, args)));
        }
    }
    // No separator first, last, or after another.
    let mut previous_was_command = false;
    let shown: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(i, entry)| {
            let keep = match entry {
                Some(_) => true,
                None => previous_was_command && entries[i + 1..].iter().any(Option::is_some),
            };
            if keep {
                previous_was_command = entry.is_some();
            }
            keep
        })
        .map(|(_, entry)| entry)
        .collect();
    for entry in shown {
        let Some((command, args)) = entry else {
            ui.separator();
            continue;
        };
        let id = &command.id;
        let enabled = !args.is_empty() || command.when.applies(view.project, view.editor);
        let mut button = with_keys(Button::new(command.title.as_str()), view.keymap, id);
        if let Some(on) = toggled(view, id) {
            button = button.selected(on);
        }
        if ui.add_enabled(enabled, button).clicked() {
            actions.push(if args.is_empty() {
                Action::Run(id.clone())
            } else {
                Action::RunWith(id.clone(), args.clone())
            });
            ui.close();
        }
    }
}

/// The arguments `command` takes from `spot`, or `None` if it needs one
/// that can't be taken from there.
fn args(command: &Command, scene: &Scene<'_>, spot: Spot) -> Option<Args> {
    let selection = &scene.editor().selection;
    let mut args = Args::new();
    for param in &command.params {
        let value: Option<Value> = match (param.ty, spot.hit) {
            (Type::Clip, Hit::Clip { clip, .. }) => Some(clip.into()),
            // The selected clips, if the menu was opened on one of them.
            (Type::List(Type::Clip), Hit::Clip { clip, .. }) => {
                Some(if selection.clips.contains(&clip) {
                    selection.clips.iter().copied().collect::<Vec<_>>().into()
                } else {
                    vec![clip].into()
                })
            }
            (Type::Track, hit) => hit.track().map(Value::from),
            (Type::List(Type::Track), hit) => hit.track().map(|track| {
                if selection.tracks.contains(&track) {
                    selection.tracks.iter().copied().collect::<Vec<_>>().into()
                } else {
                    vec![track].into()
                }
            }),
            (Type::Locator, Hit::Locator(id)) => Some(id.into()),
            (Type::Beats, _) if matches!(param.name.as_ref(), "at" | "start") => {
                Some(spot.at.into())
            }
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

/// Whether `id` suits the track the menu was opened on: MIDI clips go on
/// MIDI tracks, and audio files on audio tracks.
fn fits(id: &CommandId, scene: &Scene<'_>, spot: Spot) -> bool {
    let kind = spot
        .hit
        .track()
        .and_then(|track| scene.project().track(track))
        .map(|track| track.kind);
    match id {
        id if *id == arrange::INSERT_MIDI_CLIP => kind == Some(TrackKind::Midi),
        id if *id == file::IMPORT_AUDIO => kind.is_none_or(|kind| kind == TrackKind::Audio),
        _ => true,
    }
}
