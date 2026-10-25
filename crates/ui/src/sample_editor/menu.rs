//! The Sample Editor's context menu, on its waveform, as Live's: commands
//! from the registry for the clip's warp markers and transients, with their
//! arguments taken from where the menu was opened. On a marker, they act on
//! the selected markers if it's one of them; on a transient's pseudo
//! marker, on the transient; in the time selected, on the transients in it;
//! and elsewhere, where the menu was opened. Set 1.1.1 Here and Warp From
//! Here act at the marker, the transient, or the audio where the menu was
//! opened, and Warp Selection as a Loop on the time selected. Quantize
//! moves the transient, or else those in the time selected, or all of
//! them. The menu gives the Sample Editor the focus, so the commands
//! without arguments act on what it has selected.
//!
//! And the menu of what the lane under the waveform can show.

use egui::{Popup, Response, Role, Ui, WidgetInfo};
use musicable_commands::{Args, Command, CommandId, edit, rewarp, transient, warp, warp_mode};
use musicable_model::param::ClipParam;

use super::{Hit, Scene, Spot, of, run};
use crate::menus::context_menu;
use crate::shell::Action;

/// Shows the context menu for `spot`.
pub(super) fn show(ui: &mut Ui, scene: &Scene<'_>, spot: Spot, actions: &mut Vec<Action>) {
    let view = scene.view;
    let entries: Vec<Option<(&Command, Args)>> = (entries(scene, spot).into_iter())
        .filter_map(|entry| match entry {
            None => Some(None),
            Some((id, args)) => view.commands.get(&id).map(|command| Some((command, args))),
        })
        .collect();
    context_menu(ui, view, &entries, actions);
}

/// Selects the marker the menu was opened on, unless it's selected, so the
/// menu's commands act on it, as the arrangement's do on a clip.
pub(super) fn select_for(scene: &Scene<'_>, spot: Spot, actions: &mut Vec<Action>) {
    if let Hit::Marker(i) = spot.hit
        && !scene.markers().get(i).is_some_and(|m| scene.selected(m))
    {
        run(actions, warp::SELECT_MARKERS, of(scene, &[i]));
    }
}

/// The commands for `spot`, with their arguments, in order: `None` is a
/// separator. Those without arguments act on what's selected. Warp comes
/// last, and alone while the clip isn't warped.
fn entries(scene: &Scene<'_>, spot: Spot) -> Vec<Option<(CommandId, Args)>> {
    let warp_switch = Some((warp::SET_WARP, Args::new()));
    if scene.warp().is_none() {
        return vec![warp_switch];
    }
    let mut entries = Vec::new();
    let clip = scene.clip.id;
    let frames = |frame: u64| {
        Args::new()
            .with("frames", vec![frame as i64])
            .with("clip", clip)
    };
    // The point the warp commands act at.
    let mut point = spot.frame;
    match spot.hit {
        Hit::Marker(i) => {
            point = scene.markers().get(i).map(|marker| marker.frame);
            let markers = selected_with(scene, i);
            entries.push(Some((warp::DELETE_MARKERS, of(scene, &markers))));
        }
        Hit::Transient(frame) => {
            point = Some(frame);
            entries.push(Some((warp::INSERT_MARKERS, frames(frame))));
            entries.push(Some((transient::DELETE, frames(frame))));
        }
        _ if in_time_selected(scene, spot) => {
            entries.push(Some((warp::INSERT_MARKERS, Args::new())));
            entries.push(Some((warp::DELETE_MARKERS, Args::new())));
            entries.push(Some((transient::DELETE, Args::new())));
        }
        _ => {
            let at = Args::new()
                .with("at", vec![scene.own_of(spot.at)])
                .with("clip", clip);
            entries.push(Some((warp::INSERT_MARKERS, at)));
            entries.push(Some((warp::DELETE_MARKERS, Args::new())));
            if let Some(frame) = spot.frame {
                entries.push(Some((transient::INSERT, frames(frame))));
            }
        }
    }
    entries.push(None);
    if let Some(frame) = point {
        let at = Args::new().with("frame", frame as i64).with("clip", clip);
        entries.push(Some((rewarp::SET_DOWNBEAT, at.clone())));
        entries.push(None);
        for id in [
            rewarp::FROM_HERE,
            rewarp::FROM_HERE_AT_SONG_TEMPO,
            rewarp::FROM_HERE_STRAIGHT,
            rewarp::SONG_TEMPO_FROM_HERE,
        ] {
            entries.push(Some((id, at.clone())));
        }
    }
    let time = scene.editor().sample_editor.time;
    if time.is_some_and(|time| time.start <= spot.own && spot.own <= time.end) {
        entries.push(Some((rewarp::SELECTION_AS_LOOP, Args::new())));
    }
    entries.push(None);
    if scene
        .warp()
        .is_some_and(|w| w.transients().next().is_some())
    {
        let quantize = match spot.hit {
            Hit::Transient(frame) => frames(frame),
            _ => Args::new().with("clip", clip),
        };
        entries.push(Some((edit::QUANTIZE, quantize)));
    }
    entries.push(Some((transient::RESET, Args::new())));
    entries.push(None);
    entries.push(warp_switch);
    entries
}

/// The places of the markers that a command on the marker at place `i`
/// acts on: the selected ones, if it's one of them.
fn selected_with(scene: &Scene<'_>, i: usize) -> Vec<usize> {
    let all = scene.markers();
    match all.get(i).is_some_and(|m| scene.selected(m)) {
        true => (0..all.len())
            .filter(|&j| scene.selected(&all[j]))
            .collect(),
        false => vec![i],
    }
}

/// Whether the menu was opened in the time selected, which has transients
/// in it.
fn in_time_selected(scene: &Scene<'_>, spot: Spot) -> bool {
    let time = scene.editor().sample_editor.time;
    let inside = time.is_some_and(|time| time.start <= spot.own && spot.own <= time.end);
    let selected = warp::selected_transients(scene.project(), scene.editor());
    inside && selected.is_some_and(|(_, frames)| !frames.is_empty())
}

/// The menu of the clip's parameters whose curves the lane can show, which
/// a click on its name, `chooser`, opens: those the clip's mode plays, then
/// the rest of its warp's, which the clip keeps for the modes that play
/// them.
pub(super) fn lanes(chooser: &Response, scene: &Scene<'_>, actions: &mut Vec<Action>) {
    let shown = scene.lane();
    chooser.widget_info(|| WidgetInfo {
        current_text_value: Some(shown.spec().name),
        ..WidgetInfo::labeled(Role::ComboBox, true, "Lane")
    });
    let mode = scene.warp().map(|warp| warp.mode);
    let (playing, rest): (Vec<ClipParam>, Vec<ClipParam>) =
        (ClipParam::ALL.into_iter()).partition(|param| param.plays_in(mode));
    Popup::menu(chooser).show(|ui| {
        let mut item = |ui: &mut Ui, param: ClipParam| {
            let name = param.spec().name;
            let item = ui
                .selectable_label(param == shown, name)
                .on_hover_text(plays_in(param));
            if item.clicked() && param != shown {
                let args = Args::new()
                    .with("param", param.key())
                    .with("clip", scene.clip.id);
                run(actions, warp_mode::SHOW_LANE, args);
            }
        };
        for &param in &playing {
            item(ui, param);
        }
        if !rest.is_empty() {
            ui.separator();
        }
        for &param in &rest {
            item(ui, param);
        }
    });
}

/// Which modes play `param`, as its entry in the menu of the lanes says.
fn plays_in(param: ClipParam) -> &'static str {
    match param {
        ClipParam::Gain => "Plays in every mode, and while the clip isn't warped",
        ClipParam::Transpose | ClipParam::Detune => "Plays in every mode but Re-Pitch",
        ClipParam::TransientEnvelope | ClipParam::Preserve | ClipParam::TransientLoop => {
            "Plays in Slice"
        }
        ClipParam::Formants => "Plays in Spectral",
    }
}
