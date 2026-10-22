//! The arrangement's context menus: commands from the registry, with their
//! arguments taken from what the menu was opened on.
//!
//! Each place has its list of commands. A command's title, keys and
//! condition come from the registry, and each of its parameters that names
//! a clip, a track, a locator, a take, a parameter or a position is filled
//! in from where the menu was opened. A command that needs an argument the
//! menu can't give is left out.

use egui::Ui;
use musicable_commands::{
    Args, Command, CommandId, Type, Value, arrange, automation, edit, file, locator, take, track,
    transport, view,
};
use musicable_model::time::Beats;
use musicable_model::{Chain, TrackKind};

use super::{Hit, Scene, Spot};
use crate::menus::context_menu;
use crate::shell::Action;

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
    Some(view::TOGGLE_TAKE_LANES),
    Some(automation::SHOW),
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

/// On a take's lane, Comp copies the clip the menu was opened on, and on
/// its header, all of the take.
const TAKE: Items = &[
    Some(take::COMP),
    None,
    Some(edit::RENAME),
    Some(take::DELETE),
    None,
    Some(view::TOGGLE_TAKE_LANES),
];

/// On an automation lane or its header, for its parameter.
const AUTOMATION: Items = &[
    Some(automation::RE_ENABLE),
    Some(automation::CLEAR),
    None,
    Some(automation::HIDE_LANE),
    Some(automation::SHOW),
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
        Hit::Take { .. } | Hit::TakeHeader { .. } => &[TAKE],
        Hit::Automation { .. } => &[AUTOMATION, GRID],
        Hit::AutomationHeader { .. } => &[AUTOMATION],
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
    context_menu(ui, view, &entries, actions);
}

/// The arguments `command` takes from `spot`, or `None` if it needs one
/// that can't be taken from there.
fn args(command: &Command, scene: &Scene<'_>, spot: Spot) -> Option<Args> {
    let selection = &scene.editor().selection;
    let mut args = Args::new();
    for param in &command.params {
        let value: Option<Value> = match (param.ty, spot.hit) {
            (Type::Take, Hit::Take { take, .. } | Hit::TakeHeader { take, .. }) => {
                Some(take.into())
            }
            // The stretch Comp copies.
            (Type::Beats, Hit::Take { .. } | Hit::TakeHeader { .. }) => {
                let (start, end) = comped(scene, spot.hit)?;
                match param.name.as_ref() {
                    "start" => Some(start.into()),
                    "end" => Some(end.into()),
                    _ => None,
                }
            }
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
            (Type::Param, hit) if param.name == "param" => hit.lane().map(Value::from),
            // The master's lanes are its, not the selected track's.
            (
                Type::Bool,
                Hit::Automation {
                    chain: Chain::Master,
                    ..
                }
                | Hit::AutomationHeader {
                    chain: Chain::Master,
                    ..
                },
            ) if param.name == "master" => Some(true.into()),
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

/// What Comp copies from the take lane `hit` is on: the clip it's on, or
/// on the take's header, all of the take. `None` between its clips.
fn comped(scene: &Scene<'_>, hit: Hit) -> Option<(Beats, Beats)> {
    let (take, clip) = match hit {
        Hit::Take { take, clip, .. } => (take, Some(clip?)),
        Hit::TakeHeader { take, .. } => (take, None),
        _ => return None,
    };
    let (_, take) = scene.project().find_take(take)?;
    let clips = take
        .clips
        .iter()
        .filter(|c| clip.is_none_or(|clip| c.id == clip));
    let start = clips.clone().map(|c| c.start).min()?;
    let end = clips.map(|c| c.end()).max()?;
    Some((start, end))
}

/// Whether `id` suits the track the menu was opened on: MIDI clips go on
/// MIDI tracks, audio files on audio tracks, and return and group tracks
/// record nothing, so they have no takes. On an automation lane, there has
/// to be automation to clear, and to have overridden to re-enable.
fn fits(id: &CommandId, scene: &Scene<'_>, spot: Spot) -> bool {
    let kind = spot
        .hit
        .track()
        .and_then(|track| scene.project().track(track))
        .map(|track| track.kind);
    let lane = spot
        .hit
        .lane()
        .and_then(|param| scene.project().lane(param));
    match id {
        id if *id == arrange::INSERT_MIDI_CLIP => kind == Some(TrackKind::Midi),
        id if *id == file::IMPORT_AUDIO => kind.is_none_or(|kind| kind == TrackKind::Audio),
        id if *id == view::TOGGLE_TAKE_LANES => kind.is_some_and(|kind| !kind.is_bus()),
        id if *id == automation::RE_ENABLE => lane.is_some_and(|lane| lane.overridden),
        id if *id == automation::CLEAR => lane.is_some(),
        _ => true,
    }
}
