//! Drawing controllers: the piano roll's lane.
//!
//! The lane under the piano roll's notes shows their velocities, or how
//! the clip moves a controller, such as the mod wheel or the sustain pedal:
//! a curve of steps. The commands here choose what it shows, and draw,
//! erase and clear a controller's curve in a MIDI clip: the clip their
//! `clip` argument names, or else the one the piano roll shows. They place
//! points in the clip's own time, as notes are, and a controller's values go
//! from 0 to 1. The lane's mouse gestures run them, and so can scripts and
//! agents.

use musicable_model::Project;
use musicable_model::clip::{self, ClipId, Controller, Point};
use musicable_model::time::{BeatRange, Beats};

use crate::editor::{Editor, Lane};
use crate::notes::{CLIP, clip_arg, midi};
use crate::{Args, Command, CommandError, CommandId, Context, Param, Registry, Type, Value, When};

/// Shows a controller in the piano roll's lane.
pub const SHOW: CommandId = CommandId::new("controllers.show");
/// Shows the notes' velocities in the piano roll's lane.
pub const SHOW_VELOCITIES: CommandId = CommandId::new("controllers.show_velocities");
/// Draws over a stretch of a controller's curve.
pub const DRAW: CommandId = CommandId::new("controllers.draw");
/// Erases a stretch of a controller's curve.
pub const ERASE: CommandId = CommandId::new("controllers.erase");
/// Takes away a controller's whole curve, so the clip leaves it alone.
pub const CLEAR: CommandId = CommandId::new("controllers.clear");

const CONTROLLER: Param = Param::optional(
    "controller",
    Type::Int,
    "The controller's MIDI CC number, from 0 to 119: the one the piano roll's lane shows, \
     without it.",
);
const START: Param = Param::required(
    "start",
    Type::Beats,
    "Where the stretch starts, in the clip's own time.",
);
const END: Param = Param::required(
    "end",
    Type::Beats,
    "Where it ends. From there on, the curve holds what it did.",
);

/// Registers the controller commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(SHOW, "Show Controller").with_params(&[Param::required(
            "controller",
            Type::Int,
            "The controller's MIDI CC number, from 0 to 119, such as 1 for the mod wheel or \
             64 for the sustain pedal.",
        )]),
        |ctx, args| {
            let controller = controller(args.get("controller")?)?;
            ctx.document.editor_mut().piano_roll.lane = Lane::Controller(controller);
            Ok(Value::None)
        },
    );
    registry.register(SHOW_VELOCITIES, "Show Velocities", |ctx| {
        ctx.document.editor_mut().piano_roll.lane = Lane::Velocity;
    });
    registry.add(
        Command::new(DRAW, "Draw Controller").with_params(&[
            START,
            END,
            Param::required(
                "at",
                Type::List(&Type::Beats),
                "Where each point is, in the clip's own time: from the start, up to the end.",
            ),
            Param::required(
                "values",
                Type::List(&Type::Number),
                "Each point's value, from 0 to 1, in the same order. The controller holds it \
                 until the next point.",
            ),
            CONTROLLER,
            CLIP,
        ]),
        |ctx, args| {
            let (clip, controller, range) = stretch(ctx, args)?;
            let at: Vec<Beats> = args.get("at")?;
            let values: Vec<f64> = args.get("values")?;
            if at.len() != values.len() {
                return Err(CommandError::invalid(
                    "each point needs a position and a value",
                ));
            }
            if !values.iter().all(|value| (0.0..=1.0).contains(value)) {
                return Err(CommandError::invalid(
                    "a controller's value goes from 0 to 1",
                ));
            }
            if !at.iter().all(|&at| range.contains(at)) {
                return Err(CommandError::invalid(
                    "each point goes from the start, up to the end",
                ));
            }
            let mut drawn: Vec<Point> = at
                .into_iter()
                .zip(values)
                .map(|(at, value)| Point::new(at, value as f32))
                .collect();
            drawn.sort_by_key(|point| point.at);
            redraw(ctx, "Draw Controller", clip, controller, range, &drawn)?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(ERASE, "Erase Controller").with_params(&[START, END, CONTROLLER, CLIP]),
        |ctx, args| {
            let (clip, controller, range) = stretch(ctx, args)?;
            redraw(ctx, "Erase Controller", clip, controller, range, &[])?;
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(CLEAR, "Clear Controller")
            .when(When::ControllerCurve)
            .with_params(&[CONTROLLER, CLIP]),
        |ctx, args| {
            let clip = clip_arg(ctx, args)?;
            let controller = controller_arg(ctx, args)?;
            ctx.document.transact("Clear Controller", |tx| {
                tx.set_controller(clip, controller, Vec::new());
            });
            Ok(Value::None)
        },
    );
}

/// The clip and the controller the piano roll's lane shows, if it shows a
/// controller its clip moves.
pub(crate) fn shown(project: &Project, editor: &Editor) -> Option<(ClipId, Controller)> {
    let roll = &editor.piano_roll;
    let (Some(clip), Lane::Controller(controller)) = (roll.clip, roll.lane) else {
        return None;
    };
    let (_, m) = midi(project, clip).ok()?;
    (!m.controller(controller).is_empty()).then_some((clip, controller))
}

/// Controller `number`, if it is one.
fn controller(number: i64) -> Result<Controller, CommandError> {
    u8::try_from(number)
        .ok()
        .and_then(Controller::new)
        .ok_or_else(|| CommandError::invalid("a controller goes from 0 to 119"))
}

/// The controller the `controller` argument names, or else the one the
/// piano roll's lane shows.
fn controller_arg(ctx: &Context<'_>, args: &Args) -> Result<Controller, CommandError> {
    match args.opt("controller")? {
        Some(number) => controller(number),
        None => match ctx.document.editor().piano_roll.lane {
            Lane::Controller(controller) => Ok(controller),
            Lane::Velocity => Err(CommandError::invalid(
                "the piano roll's lane shows no controller",
            )),
        },
    }
}

/// The clip, the controller, and the stretch of the clip's own time, from
/// `start` to `end`, that `args` name.
fn stretch(
    ctx: &Context<'_>,
    args: &Args,
) -> Result<(ClipId, Controller, BeatRange), CommandError> {
    let clip = clip_arg(ctx, args)?;
    let controller = controller_arg(ctx, args)?;
    let (start, end): (Beats, Beats) = (args.get("start")?, args.get("end")?);
    Ok((
        clip,
        controller,
        BeatRange::new(start.min(end), start.max(end)),
    ))
}

/// Draws `drawn` over `range` of `controller`'s curve in the clip with id
/// `clip`, as a transaction called `name` (see [`clip::redraw`]), and
/// shows the curve in the piano roll's lane.
fn redraw(
    ctx: &mut Context<'_>,
    name: &str,
    clip: ClipId,
    controller: Controller,
    range: BeatRange,
    drawn: &[Point],
) -> Result<(), CommandError> {
    let (project, editor) = ctx.document.parts();
    let (c, m) = midi(project, clip)?;
    let points = clip::redraw(m.controller(controller), range, drawn);
    editor.open_in_piano_roll(c);
    editor.piano_roll.lane = Lane::Controller(controller);
    ctx.document.transact(name, |tx| {
        tx.set_controller(clip, controller, points);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use musicable_model::TrackKind;
    use musicable_model::clip::{AudioClip, ClipContent, MidiClip};

    use super::*;
    use crate::edit;
    use crate::editor::Focus;
    use crate::testing::Setup;

    /// Keys, a MIDI track with a clip from beat 4 to 12 that holds the
    /// sustain pedal down from a beat into its own time to beat 3, and Vox,
    /// an audio track with a clip.
    fn lane() -> (Setup, [ClipId; 2]) {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let vox = project.add_track("Vox", TrackKind::Audio);
        let mut midi = MidiClip::default();
        let pedal = [(1, 1.0), (3, 0.0)].map(|(at, value)| Point::new(Beats::new(at), value));
        midi.set_controller(Controller::SUSTAIN, pedal.to_vec());
        let range = BeatRange::new(Beats::new(4), Beats::new(12));
        let k = project.new_clip("K", range, ClipContent::Midi(midi));
        let audio = ClipContent::Audio(AudioClip::new("v.wav"));
        let v = project.new_clip("V", range, audio);
        let ids = [k.id, v.id];
        project.track_mut(keys).unwrap().clips.push(k);
        project.track_mut(vox).unwrap().clips.push(v);
        (Setup::new(project), ids)
    }

    /// `controller`'s curve in `clip`, as (beat, value).
    fn curve(setup: &Setup, clip: ClipId, controller: Controller) -> Vec<(f64, f32)> {
        let (_, m) = midi(setup.document.project(), clip).unwrap();
        let points = m.controller(controller).iter();
        points.map(|p| (p.at.to_f64(), p.value)).collect()
    }

    fn show(setup: &mut Setup, controller: i64) {
        let args = Args::new().with("controller", controller);
        setup.run_with(&SHOW, args).unwrap();
    }

    fn draw(start: f64, end: f64, points: &[(f64, f64)]) -> Args {
        let at: Vec<Beats> = points.iter().map(|&(at, _)| Beats::from_f64(at)).collect();
        let values: Vec<f64> = points.iter().map(|&(_, value)| value).collect();
        Args::new()
            .with("start", Beats::from_f64(start))
            .with("end", Beats::from_f64(end))
            .with("at", at)
            .with("values", values)
    }

    #[test]
    fn curves_are_drawn_erased_and_cleared_as_steps_in_the_undo_history() {
        let (mut setup, [k, _]) = lane();
        let roll = &setup.document.editor().piano_roll;
        assert_eq!((roll.clip, roll.lane), (None, Lane::Velocity));
        let wheel = Controller::MOD_WHEEL;
        // Drawing shows the clip and the controller, and gives the piano
        // roll the focus.
        let args = draw(0.0, 2.0, &[(0.0, 0.25), (1.0, 0.5)]).with("controller", 1_i64);
        setup.run_with(&DRAW, args.with("clip", k)).unwrap();
        let editor = setup.document.editor();
        assert_eq!(editor.piano_roll.clip, Some(k));
        assert_eq!(editor.piano_roll.lane, Lane::Controller(wheel));
        assert_eq!(editor.focus, Focus::PIANO_ROLL);
        // Before, the clip left the wheel alone: nothing to pick up after.
        assert_eq!(curve(&setup, k, wheel), [(0.0, 0.25), (1.0, 0.5)]);
        // Over the end, given out of order: the wheel comes back to 0.5 at
        // the end, a point that changes nothing is left out, and the start
        // and end can come either way round.
        let args = draw(3.0, 0.5, &[(2.0, 1.0), (0.5, 0.25), (1.5, 1.0)]);
        setup.run_with(&DRAW, args).unwrap();
        assert_eq!(
            curve(&setup, k, wheel),
            [(0.0, 0.25), (1.5, 1.0), (3.0, 0.5)]
        );
        // Erasing: what came before holds through the stretch, and the
        // curve picks up again at its end.
        let args = Args::new()
            .with("start", Beats::new(1))
            .with("end", Beats::from_f64(2.5));
        setup.run_with(&ERASE, args).unwrap();
        assert_eq!(
            curve(&setup, k, wheel),
            [(0.0, 0.25), (2.5, 1.0), (3.0, 0.5)]
        );
        // Clearing leaves the controller alone, and the sustain pedal as it
        // was.
        assert!(setup.applies(&CLEAR));
        setup.run(&CLEAR);
        assert_eq!(curve(&setup, k, wheel), []);
        assert!(!setup.applies(&CLEAR));
        assert_eq!(
            curve(&setup, k, Controller::SUSTAIN),
            [(1.0, 1.0), (3.0, 0.0)]
        );
        assert_eq!(
            setup.committed_names(),
            [
                "Draw Controller",
                "Draw Controller",
                "Erase Controller",
                "Clear Controller"
            ]
        );
        for _ in 0..3 {
            setup.run(&edit::UNDO);
        }
        assert_eq!(curve(&setup, k, wheel), [(0.0, 0.25), (1.0, 0.5)]);
        setup.run(&edit::UNDO);
        assert_eq!(curve(&setup, k, wheel), []);
    }

    #[test]
    fn the_lane_shows_velocities_or_a_controller() {
        let (mut setup, [k, _]) = lane();
        show(&mut setup, 64);
        let lane = setup.document.editor().piano_roll.lane;
        assert_eq!(lane, Lane::Controller(Controller::SUSTAIN));
        // The piano roll shows no clip yet: nothing to clear.
        assert!(!setup.applies(&CLEAR));
        setup.select_clips(&[k]);
        assert!(setup.applies(&CLEAR));
        // The lane keeps what it shows from clip to clip, so the mod wheel
        // shows, and this clip doesn't move it.
        show(&mut setup, 1);
        assert!(!setup.applies(&CLEAR));
        setup.run(&SHOW_VELOCITIES);
        assert_eq!(setup.document.editor().piano_roll.lane, Lane::Velocity);
        assert!(!setup.applies(&CLEAR));
        // Showing changes nothing in the project.
        assert_eq!(setup.document.take_committed(), []);
    }

    #[test]
    fn controller_commands_check_their_arguments() {
        let (mut setup, [k, v]) = lane();
        let one = || draw(0.0, 1.0, &[(0.0, 0.5)]);
        assert_eq!(
            setup.run_with(&DRAW, one().with("controller", 1_i64)),
            Err(CommandError::invalid("the piano roll shows no clip"))
        );
        assert_eq!(
            setup.run_with(&DRAW, one().with("clip", k)),
            Err(CommandError::invalid(
                "the piano roll's lane shows no controller"
            ))
        );
        let in_k = || one().with("clip", k);
        assert_eq!(
            setup.run_with(&DRAW, in_k().with("controller", 120_i64)),
            Err(CommandError::invalid("a controller goes from 0 to 119"))
        );
        assert_eq!(
            setup.run_with(&SHOW, Args::new().with("controller", -1_i64)),
            Err(CommandError::invalid("a controller goes from 0 to 119"))
        );
        show(&mut setup, 1);
        let mismatched = in_k().with("values", vec![0.5, 1.0]);
        assert_eq!(
            setup.run_with(&DRAW, mismatched),
            Err(CommandError::invalid(
                "each point needs a position and a value"
            ))
        );
        assert_eq!(
            setup.run_with(&DRAW, in_k().with("values", vec![1.5])),
            Err(CommandError::invalid(
                "a controller's value goes from 0 to 1"
            ))
        );
        assert_eq!(
            setup.run_with(&DRAW, in_k().with("at", vec![Beats::new(1)])),
            Err(CommandError::invalid(
                "each point goes from the start, up to the end"
            ))
        );
        assert_eq!(
            setup.run_with(&DRAW, one().with("clip", v)),
            Err(CommandError::invalid(format!(
                "clip {} isn't a MIDI clip",
                v.0
            )))
        );
        assert_eq!(setup.document.take_committed(), []);
    }
}
