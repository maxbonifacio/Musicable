//! The automation commands: showing lanes under tracks, editing their
//! points, clearing them, and re-enabling automation that someone
//! overrode by moving a control by hand.
//!
//! Each command names its parameter by [`ParamAddress`], which scripts
//! write as text: "track 3 volume", "master pan", "send 2" or "device 4
//! param 17" (see [`param_text`](crate::param_text)).

use musicable_model::automation::Point;
use musicable_model::param::ParamAddress;
use musicable_model::time::Beats;
use musicable_model::{Chain, Project, TrackId};

use crate::{Command, CommandError, CommandId, Context, Param, Registry, Type, Value, When};

/// Shows a track's automation lanes, or hides them.
pub const SHOW: CommandId = CommandId::new("automation.show");
/// Shows a lane for a parameter under its track.
pub const SHOW_LANE: CommandId = CommandId::new("automation.show_lane");
/// Hides a parameter's lane.
pub const HIDE_LANE: CommandId = CommandId::new("automation.hide_lane");
/// Sets the points a parameter's automation moves through.
pub const SET_POINTS: CommandId = CommandId::new("automation.set_points");
/// Adds a point to a parameter's automation.
pub const ADD_POINT: CommandId = CommandId::new("automation.add_point");
/// Deletes the points in a stretch of a parameter's automation.
pub const DELETE_POINTS: CommandId = CommandId::new("automation.delete_points");
/// Deletes a parameter's automation, which keeps the value it had at the
/// playhead.
pub const CLEAR: CommandId = CommandId::new("automation.clear");
/// Has parameters follow their automation again after someone moved them
/// by hand.
pub const RE_ENABLE: CommandId = CommandId::new("automation.re_enable");

const PARAM: Param = Param::required("param", Type::Param, "The parameter.");
const BEATS: Type = Type::List(&Type::Beats);
const NUMBERS: Type = Type::List(&Type::Number);

/// Registers the automation commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(SHOW, "Show/Hide Automation").with_params(&[
            Param::optional(
                "track",
                Type::Track,
                "The track: the one the panels show without it.",
            ),
            Param::optional("master", Type::Bool, "The master's instead."),
        ]),
        |ctx, args| {
            let chain = match (args.opt::<TrackId>("track")?, args.opt("master")?) {
                (_, Some(true)) => Chain::Master,
                (Some(track), _) => Chain::Track(track),
                (None, _) => match ctx.document.editor().selected_track {
                    Some(chain) => chain,
                    None => return Err(CommandError::invalid("no track is selected")),
                },
            };
            let (project, editor) = ctx.document.parts();
            if let Chain::Track(track) = chain
                && project.track(track).is_none()
            {
                return Err(no_track(track));
            }
            if editor.automation.remove(&chain).is_none() {
                editor
                    .automation
                    .insert(chain, lanes_to_show(project, chain));
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SHOW_LANE, "Show Automation Lane").with_params(&[
            PARAM,
            Param::optional(
                "instead_of",
                Type::Param,
                "The lane it takes the place of: it goes at the bottom without it.",
            ),
        ]),
        |ctx, args| {
            let param: ParamAddress = args.get("param")?;
            let instead_of: Option<ParamAddress> = args.opt("instead_of")?;
            let (project, editor) = ctx.document.parts();
            let chain = project.param_chain(param).ok_or_else(|| no_param(param))?;
            let shown = editor.automation.entry(chain).or_default();
            match instead_of.and_then(|old| shown.iter().position(|&p| p == old)) {
                _ if shown.contains(&param) => {
                    if let Some(old) = instead_of {
                        shown.retain(|&p| p != old || p == param);
                    }
                }
                Some(at) => shown[at] = param,
                None => shown.push(param),
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(HIDE_LANE, "Hide Automation Lane").with_params(&[PARAM]),
        |ctx, args| {
            let param: ParamAddress = args.get("param")?;
            let (project, editor) = ctx.document.parts();
            if let Some(chain) = project.param_chain(param)
                && let Some(shown) = editor.automation.get_mut(&chain)
            {
                shown.retain(|&p| p != param);
                if shown.is_empty() {
                    editor.automation.remove(&chain);
                }
            }
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(SET_POINTS, "Edit Automation").with_params(&[
            PARAM,
            Param::required("at", BEATS, "Where each point is."),
            Param::required(
                "values",
                NUMBERS,
                "Each point's value, in the parameter's own units.",
            ),
        ]),
        |ctx, args| {
            let param = automatable(ctx, args.get("param")?)?;
            let at: Vec<Beats> = args.get("at")?;
            let values: Vec<f64> = args.get("values")?;
            if at.len() != values.len() {
                return Err(CommandError::invalid(
                    "there must be as many values as places",
                ));
            }
            let points = at.into_iter().zip(values).map(|(at, v)| Point::new(at, v));
            let points = clamped(ctx.document.project(), param, points.collect());
            ctx.document
                .transact("Edit Automation", |tx| tx.set_lane_points(param, points));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(ADD_POINT, "Add Automation Point").with_params(&[
            PARAM,
            Param::required("at", Type::Beats, "Where it goes."),
            Param::required(
                "value",
                Type::Number,
                "Its value, in the parameter's own units.",
            ),
        ]),
        |ctx, args| {
            let param = automatable(ctx, args.get("param")?)?;
            let point = Point::new(args.get("at")?, args.get("value")?);
            let project = ctx.document.project();
            let mut points = project.lane(param).map(|l| l.points.clone());
            let mut points = points.take().unwrap_or_default();
            // After any at the same place, so it ends a step there.
            let index = points.partition_point(|p| p.at <= point.at);
            points.insert(index, point);
            let points = clamped(project, param, points);
            ctx.document.transact("Add Automation Point", |tx| {
                tx.set_lane_points(param, points)
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(DELETE_POINTS, "Delete Automation Points").with_params(&[
            PARAM,
            Param::required("start", Type::Beats, "The first place to delete points at."),
            Param::required("end", Type::Beats, "The last place to delete points at."),
        ]),
        |ctx, args| {
            let param: ParamAddress = args.get("param")?;
            let (start, end): (Beats, Beats) = (args.get("start")?, args.get("end")?);
            let Some(lane) = ctx.document.project().lane(param) else {
                return Ok(Value::None);
            };
            let mut points = lane.points.clone();
            points.retain(|p| p.at < start || p.at > end);
            let playhead = ctx.transport.playhead();
            ctx.document.transact("Delete Automation Points", |tx| {
                if points.is_empty() {
                    clear(tx, param, playhead);
                } else {
                    tx.set_lane_points(param, points);
                }
            });
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(CLEAR, "Clear Automation").with_params(&[PARAM]),
        |ctx, args| {
            let param: ParamAddress = args.get("param")?;
            let playhead = ctx.transport.playhead();
            ctx.document
                .transact("Clear Automation", |tx| clear(tx, param, playhead));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(RE_ENABLE, "Re-Enable Automation")
            .with_params(&[Param::optional(
                "param",
                Type::Param,
                "The parameter: every one someone overrode without it.",
            )])
            .when(When::AutomationOverridden),
        |ctx, args| {
            let param: Option<ParamAddress> = args.opt("param")?;
            let overridden: Vec<ParamAddress> = (ctx.document.project().automation.iter())
                .filter(|l| l.overridden && param.is_none_or(|p| p == l.param))
                .map(|l| l.param)
                .collect();
            ctx.document.transact("Re-Enable Automation", |tx| {
                for param in overridden {
                    tx.set_lane_overridden(param, false);
                }
            });
            Ok(Value::None)
        },
    );
}

/// Removes the lane of `param`, leaving the parameter with the value its
/// automation gave it at `playhead`, as Live does, so nothing jumps.
fn clear(tx: &mut crate::Transaction<'_>, param: ParamAddress, playhead: Beats) {
    let project = tx.project();
    let scale = project.param_scale(param);
    let Some(lane) = project.lane(param) else {
        return;
    };
    let overridden = lane.overridden;
    let value = lane.value_at(playhead, scale);
    tx.remove_lane(param);
    // An overridden lane wasn't playing: the parameter has its own value.
    if let Some(value) = value.filter(|_| !overridden) {
        tx.set_param_value(param, value);
    }
}

/// The lanes [`SHOW`] shows for `chain`: one for each of its automated
/// parameters, or else its volume's.
fn lanes_to_show(project: &Project, chain: Chain) -> Vec<ParamAddress> {
    let automated: Vec<ParamAddress> = (project.automation.iter())
        .map(|l| l.param)
        .filter(|&param| project.param_chain(param) == Some(chain))
        .collect();
    match automated.is_empty() {
        true => vec![ParamAddress::Volume(chain)],
        false => automated,
    }
}

/// `param`, if the project has it.
fn automatable(ctx: &Context<'_>, param: ParamAddress) -> Result<ParamAddress, CommandError> {
    match ctx.document.project().has_param(param) {
        true => Ok(param),
        false => Err(no_param(param)),
    }
}

/// `points` with their values within the range of `param`, where the
/// project knows it, and whole numbers for a built-in device's switches,
/// choices and counts. A plugin keeps its own parameters in range.
fn clamped(project: &Project, param: ParamAddress, mut points: Vec<Point>) -> Vec<Point> {
    if let Some(info) = project.native_param(param) {
        for point in &mut points {
            point.value = info.clamp(point.value);
        }
    } else if let Some(spec) = project.param_spec(param) {
        for point in &mut points {
            point.value = spec.clamp(point.value);
        }
    }
    points
}

fn no_param(param: ParamAddress) -> CommandError {
    CommandError::invalid(format!("the project has no {}", crate::param_text(param)))
}

fn no_track(track: TrackId) -> CommandError {
    CommandError::invalid(format!("there's no track {}", track.0))
}

#[cfg(test)]
mod tests {
    use musicable_model::TrackKind;
    use musicable_model::automation::Lane;

    use super::*;
    use crate::Args;
    use crate::testing::Setup;

    fn point(at: i64, value: f64) -> Point {
        Point::new(Beats::new(at), value)
    }

    fn add_bass(setup: &mut Setup) -> TrackId {
        let bass = |tx: &mut crate::Transaction<'_>| {
            crate::track::add_track(tx, "Bass".into(), TrackKind::Audio)
        };
        setup.document.transact("New Track", bass)
    }

    fn points(setup: &Setup, param: ParamAddress) -> Vec<(Beats, f64)> {
        let lane = setup.document.project().lane(param);
        lane.map_or(Vec::new(), |l| {
            l.points.iter().map(|p| (p.at, p.value)).collect()
        })
    }

    #[test]
    fn points_go_in_one_at_a_time_or_all_at_once_within_range() {
        let mut setup = Setup::new(Project::new());
        let track = add_bass(&mut setup);
        let volume = ParamAddress::Volume(Chain::Track(track));
        let add = |at: i64, value: f64| {
            Args::new()
                .with("param", volume)
                .with("at", Beats::new(at))
                .with("value", value)
        };
        setup.run_with(&ADD_POINT, add(4, 0.5)).unwrap();
        setup.run_with(&ADD_POINT, add(0, 9.0)).unwrap();
        let top = musicable_model::param::ParamSpec::VOLUME_MAX;
        assert_eq!(
            points(&setup, volume),
            [(Beats::ZERO, top), (Beats::new(4), 0.5)]
        );
        assert_eq!(setup.document.undo_name(), Some("Add Automation Point"));

        let set = Args::new()
            .with("param", "track 0 pan")
            .with("at", vec![Beats::new(1), Beats::new(2)])
            .with("values", vec![-0.5, 0.5]);
        setup.run_with(&SET_POINTS, set).unwrap();
        let pan = ParamAddress::Pan(Chain::Track(track));
        assert_eq!(
            points(&setup, pan),
            [(Beats::new(1), -0.5), (Beats::new(2), 0.5)]
        );

        let uneven = Args::new()
            .with("param", pan)
            .with("at", vec![Beats::new(1)])
            .with("values", Vec::<f64>::new());
        assert!(setup.run_with(&SET_POINTS, uneven).is_err());
        let missing = add(0, 1.0).with("param", "track 7 volume");
        assert!(setup.run_with(&ADD_POINT, missing).is_err());
    }

    #[test]
    fn clearing_keeps_the_value_at_the_playhead() {
        let mut setup = Setup::new(Project::new());
        let track = add_bass(&mut setup);
        let pan = ParamAddress::Pan(Chain::Track(track));
        setup.document.transact("Draw", |tx| {
            tx.set_lane_points(pan, vec![point(0, -1.0), point(8, 1.0)])
        });
        setup.transport.playhead = Beats::new(6);
        setup
            .run_with(&CLEAR, Args::new().with("param", pan))
            .unwrap();
        assert!(setup.document.project().lane(pan).is_none());
        assert_eq!(setup.document.project().track(track).unwrap().pan, 0.5);

        // Deleting the last points clears the lane the same way.
        setup.document.transact("Draw", |tx| {
            tx.set_lane_points(pan, vec![point(0, -1.0), point(8, 1.0)])
        });
        setup.transport.playhead = Beats::new(2);
        let delete = Args::new()
            .with("param", pan)
            .with("start", Beats::ZERO)
            .with("end", Beats::new(4));
        setup.run_with(&DELETE_POINTS, delete.clone()).unwrap();
        assert_eq!(points(&setup, pan), [(Beats::new(8), 1.0)]);
        setup
            .run_with(&DELETE_POINTS, delete.with("end", Beats::new(8)))
            .unwrap();
        assert!(setup.document.project().lane(pan).is_none());
        assert_eq!(setup.document.project().track(track).unwrap().pan, 1.0);
    }

    #[test]
    fn moving_a_control_overrides_until_automation_is_re_enabled() {
        let mut setup = Setup::new(Project::new());
        let track = add_bass(&mut setup);
        let volume = ParamAddress::Volume(Chain::Track(track));
        let master = ParamAddress::Pan(Chain::Master);
        setup.document.transact("Draw", |tx| {
            tx.set_lane_points(volume, vec![point(0, 0.5)]);
            tx.set_lane_points(master, vec![point(0, 0.5)]);
        });
        assert!(!setup.applies(&RE_ENABLE));

        crate::mixer::change(&mut setup.document, crate::mixer::Change::Gain(track, 0.25));
        crate::mixer::change(&mut setup.document, crate::mixer::Change::MasterPan(-0.25));
        let project = setup.document.project();
        assert!(!project.is_automated(volume) && !project.is_automated(master));
        assert_eq!(project.track(track).unwrap().gain, 0.25);
        assert!(setup.applies(&RE_ENABLE));
        // One undo takes back the move and the override.
        assert!(setup.document.undo());
        assert!(setup.document.project().is_automated(master));
        assert!(setup.document.redo());

        let one = Args::new().with("param", volume);
        setup.run_with(&RE_ENABLE, one).unwrap();
        assert!(setup.document.project().is_automated(volume));
        assert!(!setup.document.project().is_automated(master));
        setup.run(&RE_ENABLE);
        assert!(setup.document.project().is_automated(master));
    }

    #[test]
    fn tracks_show_their_automation_in_lanes_until_hidden() {
        let mut setup = Setup::new(Project::new());
        let track = add_bass(&mut setup);
        let chain = Chain::Track(track);
        let (volume, pan) = (ParamAddress::Volume(chain), ParamAddress::Pan(chain));
        let shown = |setup: &Setup| setup.document.editor().automation.get(&chain).cloned();
        let on = Args::new().with("track", track);
        // A track without automation shows its volume's lane.
        setup.run_with(&SHOW, on.clone()).unwrap();
        assert_eq!(shown(&setup), Some(vec![volume]));
        setup.run_with(&SHOW, on.clone()).unwrap();
        assert_eq!(shown(&setup), None);

        setup
            .document
            .transact("Draw", |tx| tx.set_lane_points(pan, vec![point(0, 0.5)]));
        setup.run_with(&SHOW, on).unwrap();
        assert_eq!(shown(&setup), Some(vec![pan]));
        let lane = |param: ParamAddress| Args::new().with("param", param);
        setup.run_with(&SHOW_LANE, lane(volume)).unwrap();
        assert_eq!(shown(&setup), Some(vec![pan, volume]));
        // A chooser swaps one lane for another.
        let swap = lane(volume).with("instead_of", pan);
        setup.run_with(&SHOW_LANE, swap).unwrap();
        assert_eq!(shown(&setup), Some(vec![volume]));
        setup.run_with(&HIDE_LANE, lane(volume)).unwrap();
        assert_eq!(shown(&setup), None);

        // Lanes go with their tracks.
        setup.run_with(&SHOW_LANE, lane(pan)).unwrap();
        crate::track::delete(&mut setup.document, &[track]);
        assert!(setup.document.editor().automation.is_empty());
        assert!(setup.document.project().automation.is_empty());
        assert!(setup.document.undo());
        assert_eq!(
            setup.document.project().automation,
            [Lane::new(pan, vec![point(0, 0.5)])]
        );
    }
}
