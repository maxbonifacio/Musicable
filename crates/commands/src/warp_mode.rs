//! A warped clip's mode, and the settings its modes play it with: the
//! Sample Editor's controls for them.
//!
//! The commands here set the warp mode of an audio clip, the transpose and
//! detune every mode but Re-Pitch moves its pitch by, and Slice's,
//! Granular's and Spectral's own settings: the clip their `clip` argument names, or else
//! the one the Sample Editor shows. A clip keeps each mode's settings while
//! another plays, as Live does, so setting one in another mode is kept for
//! when it plays. Each setting is one of the clip's parameters too (see
//! [`ClipParam`]), with a curve in the clip that the Sample Editor's lane
//! shows: [`SHOW_LANE`] picks which.

use musicable_model::clip::ClipId;
use musicable_model::param::ClipParam;
use musicable_model::warp::{
    GranularSettings, Preserve, SliceSettings, SpectralSettings, TransientLoop, Warp, WarpMode,
};

use crate::warp::{CLIP, audio, change_warp, clip_arg, warped};
use crate::{Command, CommandError, CommandId, Context, Param, Registry, Type, Value};

/// Sets a warped clip's mode.
pub const SET_MODE: CommandId = CommandId::new("warp.set_mode");
/// Sets how far a warped clip's audio is transposed, in semitones.
pub const SET_TRANSPOSE: CommandId = CommandId::new("warp.set_transpose");
/// Sets how far a warped clip's audio is detuned, in cents.
pub const SET_DETUNE: CommandId = CommandId::new("warp.set_detune");
/// Sets where Slice cuts a warped clip's audio.
pub const SET_PRESERVE: CommandId = CommandId::new("warp.set_preserve");
/// Sets what Slice plays where a slice's audio runs out.
pub const SET_TRANSIENT_LOOP: CommandId = CommandId::new("warp.set_transient_loop");
/// Sets how much each of Slice's slices fades as it plays.
pub const SET_TRANSIENT_ENVELOPE: CommandId = CommandId::new("warp.set_transient_envelope");
/// Sets whether Granular fits a warped clip's grains to its audio's pitch.
pub const SET_FOLLOW_PITCH: CommandId = CommandId::new("warp.set_follow_pitch");
/// Sets how long Granular's grains are.
pub const SET_GRAIN_SIZE: CommandId = CommandId::new("warp.set_grain_size");
/// Sets how far Granular's grains stray, while they don't follow the pitch.
pub const SET_FLUCTUATION: CommandId = CommandId::new("warp.set_fluctuation");
/// Sets how far Spectral keeps a warped clip's formants in place as it
/// transposes.
pub const SET_FORMANTS: CommandId = CommandId::new("warp.set_formants");
/// Sets whether Spectral renders a warped clip's audio with Rubber Band, the
/// best quality, rather than Signalsmith Stretch, the quicker to render.
pub const SET_HIGH_QUALITY: CommandId = CommandId::new("warp.set_high_quality");
/// Sets how fine the outline of the spectrum is that Spectral keeps a
/// warped clip's formants by, while High quality is on.
pub const SET_ENVELOPE: CommandId = CommandId::new("warp.set_envelope");
/// Shows the curve of one of the clip's parameters in the Sample Editor's
/// lane.
pub const SHOW_LANE: CommandId = CommandId::new("clip.show_lane");

/// The keys of the modes, as [`SET_MODE`] takes them.
const MODES: &str = "re_pitch, slice, granular or spectral";
/// The keys of the places Slice cuts at, as [`SET_PRESERVE`] takes them.
const PRESERVES: &str = "transients, bar, 1/2, 1/4, 1/8, 1/16 or 1/32";
/// The keys of the transient loop modes.
const LOOPS: &str = "off, forward or back_and_forth";
/// The keys of the clip's parameters, as [`SHOW_LANE`] takes them.
const PARAMS: &str = "gain, transpose, detune, transient_envelope, preserve, transient_loop, \
                      grain_size, fluctuation, formants or envelope";

/// Registers the commands for the warp modes' settings.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(SET_MODE, "Set Warp Mode").with_params(&[
            Param::required(
                "mode",
                Type::Text,
                "How the audio between the markers plays: re_pitch, resampled so its pitch \
                 follows its speed, slice, cut into slices at its transients, each played \
                 at the file's own speed, granular, played as short overlapping grains, \
                 each at the file's own speed, or spectral, stretched in the frequency \
                 domain, keeping its pitch.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let key: String = args.get("mode")?;
            let mode = WarpMode::from_key(&key).ok_or_else(|| {
                CommandError::invalid(format!("there's no warp mode {key:?}: it's {MODES}"))
            })?;
            set(ctx, args, "Set Warp Mode", |warp| warp.mode = mode)
        },
    );
    registry.add(
        Command::new(SET_TRANSPOSE, "Set Clip Transpose").with_params(&[
            Param::required(
                "semitones",
                Type::Int,
                "How far up, or down if negative, from -48 to 48. Every mode but Re-Pitch \
                 plays it.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let semitones = in_range(args.get("semitones")?, Warp::TRANSPOSE_MAX, "a transpose")?;
            set(ctx, args, "Set Clip Transpose", |warp| {
                warp.transpose = semitones
            })
        },
    );
    registry.add(
        Command::new(SET_DETUNE, "Set Clip Detune").with_params(&[
            Param::required(
                "cents",
                Type::Int,
                "How far up, or down if negative, from -50 to 50. Every mode but Re-Pitch \
                 plays it.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let cents = in_range(args.get("cents")?, Warp::DETUNE_MAX, "a detune")?;
            set(ctx, args, "Set Clip Detune", |warp| warp.detune = cents)
        },
    );
    registry.add(
        Command::new(SET_PRESERVE, "Set Slice Preserve").with_params(&[
            Param::required(
                "preserve",
                Type::Text,
                "Where Slice cuts the audio: transients, at each transient in sight, or on a \
                 grid of notes from the clip's 1.1.1: bar, 1/2, 1/4, 1/8, 1/16 or 1/32.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let key: String = args.get("preserve")?;
            let preserve = Preserve::from_key(&key).ok_or_else(|| {
                CommandError::invalid(format!("Slice can't cut at {key:?}: it's {PRESERVES}"))
            })?;
            set(ctx, args, "Set Slice Preserve", |warp| {
                warp.slice.preserve = preserve
            })
        },
    );
    registry.add(
        Command::new(SET_TRANSIENT_LOOP, "Set Transient Loop Mode").with_params(&[
            Param::required(
                "mode",
                Type::Text,
                "What a slice plays where its audio runs out before the next starts, as \
                 where the song plays the file slower than its own speed: off, silence, \
                 forward, the second half of the slice over and over, or back_and_forth, \
                 that half backwards and forwards.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let key: String = args.get("mode")?;
            let transient_loop = TransientLoop::from_key(&key).ok_or_else(|| {
                CommandError::invalid(format!(
                    "there's no transient loop mode {key:?}: it's {LOOPS}"
                ))
            })?;
            set(ctx, args, "Set Transient Loop Mode", |warp| {
                warp.slice.transient_loop = transient_loop
            })
        },
    );
    registry.add(
        Command::new(SET_TRANSIENT_ENVELOPE, "Set Transient Envelope").with_params(&[
            Param::required(
                "envelope",
                Type::Number,
                "How much each slice fades as it plays, from 0, a very quick decay, to 100, \
                 no fade.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let envelope: f64 = args.get("envelope")?;
            if !(0.0..=SliceSettings::ENVELOPE_MAX).contains(&envelope) {
                return Err(CommandError::invalid(
                    "a Transient Envelope goes from 0 to 100",
                ));
            }
            set(ctx, args, "Set Transient Envelope", |warp| {
                warp.slice.envelope = envelope
            })
        },
    );
    registry.add(
        Command::new(SET_FOLLOW_PITCH, "Set Follow Pitch").with_params(&[
            Param::required(
                "on",
                Type::Bool,
                "Whether Granular fits the grains to the audio's pitch, where analysis found \
                 one, so a line with one note at a time keeps its sound, as Live's Tones \
                 does, rather than cutting them on a steady grid, spread by Fluctuation, as \
                 Live's Texture cuts pads and noise.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let on: bool = args.get("on")?;
            set(ctx, args, "Set Follow Pitch", |warp| {
                warp.granular.follow_pitch = on
            })
        },
    );
    registry.add(
        Command::new(SET_GRAIN_SIZE, "Set Grain Size").with_params(&[
            Param::required(
                "milliseconds",
                Type::Number,
                "How long each of Granular's grains is, from 5 to 500 ms: with Follow pitch \
                 on, a whole number of the pitch's periods, within a period of this, and two \
                 at least.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let size: f64 = args.get("milliseconds")?;
            let range = GranularSettings::GRAIN_SIZE_MIN..=GranularSettings::GRAIN_SIZE_MAX;
            if !range.contains(&size) {
                return Err(CommandError::invalid("a Grain Size goes from 5 to 500 ms"));
            }
            set(ctx, args, "Set Grain Size", |warp| {
                warp.granular.grain_size = size
            })
        },
    );
    registry.add(
        Command::new(SET_FLUCTUATION, "Set Fluctuation").with_params(&[
            Param::required(
                "fluctuation",
                Type::Number,
                "While Follow pitch is off, how far each grain strays from where and when \
                 it would play, as a share of its length, from 0, a steady grid, to 100. \
                 It's the same every time the clip plays.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let fluctuation: f64 = args.get("fluctuation")?;
            if !(0.0..=GranularSettings::FLUCTUATION_MAX).contains(&fluctuation) {
                return Err(CommandError::invalid("Fluctuation goes from 0 to 100"));
            }
            set(ctx, args, "Set Fluctuation", |warp| {
                warp.granular.fluctuation = fluctuation
            })
        },
    );
    registry.add(
        Command::new(SET_FORMANTS, "Set Formants").with_params(&[
            Param::required(
                "formants",
                Type::Number,
                "How far Spectral keeps the formants, which give a voice its character, in \
                 place as it transposes, from 0, moving with the pitch, to 100, staying \
                 where they were.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let formants: f64 = args.get("formants")?;
            if !(0.0..=SpectralSettings::FORMANTS_MAX).contains(&formants) {
                return Err(CommandError::invalid("Formants go from 0 to 100"));
            }
            set(ctx, args, "Set Formants", |warp| {
                warp.spectral.formants = formants
            })
        },
    );
    registry.add(
        Command::new(SET_HIGH_QUALITY, "Set High Quality").with_params(&[
            Param::required(
                "on",
                Type::Bool,
                "Whether Spectral renders with Rubber Band, the best quality and slower to \
                 render, as Live's Complex Pro, rather than Signalsmith Stretch, as Live's \
                 Complex.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let on: bool = args.get("on")?;
            set(ctx, args, "Set High Quality", |warp| {
                warp.spectral.high_quality = on
            })
        },
    );
    registry.add(
        Command::new(SET_ENVELOPE, "Set Spectral Envelope").with_params(&[
            Param::required(
                "envelope",
                Type::Number,
                "While High quality is on, how fine the outline of the spectrum is that \
                 Spectral keeps the formants by, from 8 to 512: 128 suits most audio, lower \
                 suits high voices and higher low ones.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let envelope: f64 = args.get("envelope")?;
            let range = SpectralSettings::ENVELOPE_MIN..=SpectralSettings::ENVELOPE_MAX;
            if !range.contains(&envelope) {
                return Err(CommandError::invalid("an Envelope goes from 8 to 512"));
            }
            set(ctx, args, "Set Spectral Envelope", |warp| {
                warp.spectral.envelope = envelope
            })
        },
    );
    registry.add(
        Command::new(SHOW_LANE, "Show Clip Lane").with_params(&[
            Param::required(
                "param",
                Type::Text,
                "The parameter whose curve the Sample Editor's lane shows: gain, transpose, \
                 detune, transient_envelope, preserve, transient_loop, grain_size, \
                 fluctuation, formants or envelope.",
            ),
            CLIP,
        ]),
        |ctx, args| {
            let key: String = args.get("param")?;
            let param = ClipParam::from_key(&key).ok_or_else(|| {
                CommandError::invalid(format!(
                    "an audio clip has no parameter {key:?}: it's {PARAMS}"
                ))
            })?;
            let clip = clip_arg(ctx, args)?;
            let (project, editor) = ctx.document.parts();
            let (c, _) = audio(project, clip)?;
            editor.show_in_sample_editor(c);
            editor.sample_editor.lane = param;
            Ok(Value::None)
        },
    );
}

/// Changes the warp of the clip the arguments name as `change` does, as a
/// transaction called `name`, if that changes it.
fn set(
    ctx: &mut Context<'_>,
    args: &crate::Args,
    name: &str,
    change: impl FnOnce(&mut Warp),
) -> Result<Value, CommandError> {
    let clip: ClipId = clip_arg(ctx, args)?;
    let (_, _, warp) = warped(ctx.document.project(), clip)?;
    let mut new = warp.clone();
    change(&mut new);
    if new != *warp {
        ctx.document.transact(name, |tx| change_warp(tx, clip, new));
    }
    Ok(Value::None)
}

/// `value`, as an `i8`, if it's from -`max` to `max`, or else an error
/// saying what goes from where to where.
fn in_range(value: i64, max: i8, what: &str) -> Result<i8, CommandError> {
    match i8::try_from(value) {
        Ok(value) if value.unsigned_abs() <= max.unsigned_abs() => Ok(value),
        _ => Err(CommandError::invalid(format!(
            "{what} goes from -{max} to {max}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::automation::Point;
    use musicable_model::param::ParamAddress;
    use musicable_model::time::Beats;

    use super::*;
    use crate::testing::Setup;
    use crate::warp::tests::analysed_song;
    use crate::{Args, automation, edit, warp};

    fn warp_of(setup: &Setup, clip: ClipId) -> Warp {
        warped(setup.document.project(), clip).unwrap().2.clone()
    }

    #[test]
    fn each_setting_is_a_step_and_undo_takes_it_back() {
        let (mut setup, clip) = analysed_song();
        let runs: [(CommandId, &str, Value); 12] = [
            (SET_MODE, "mode", "slice".into()),
            (SET_TRANSPOSE, "semitones", (-7_i64).into()),
            (SET_DETUNE, "cents", 12_i64.into()),
            (SET_PRESERVE, "preserve", "1/8".into()),
            (SET_TRANSIENT_LOOP, "mode", "forward".into()),
            (SET_TRANSIENT_ENVELOPE, "envelope", 40.0.into()),
            (SET_FOLLOW_PITCH, "on", false.into()),
            (SET_GRAIN_SIZE, "milliseconds", 65.0.into()),
            (SET_FLUCTUATION, "fluctuation", 50.0.into()),
            (SET_FORMANTS, "formants", 25.0.into()),
            (SET_HIGH_QUALITY, "on", true.into()),
            (SET_ENVELOPE, "envelope", 64.0.into()),
        ];
        for (command, name, value) in runs.clone() {
            let args = Args::new().with(name, value).with("clip", clip);
            setup.run_with(&command, args).unwrap();
        }
        let warp = warp_of(&setup, clip);
        assert_eq!(warp.mode, WarpMode::Slice);
        assert_eq!((warp.transpose, warp.detune), (-7, 12));
        assert_eq!(
            warp.slice,
            SliceSettings {
                preserve: Preserve::Eighth,
                transient_loop: TransientLoop::Forward,
                envelope: 40.0,
            }
        );
        let granular = GranularSettings {
            follow_pitch: false,
            grain_size: 65.0,
            fluctuation: 50.0,
        };
        assert_eq!(warp.granular, granular);
        let spectral = SpectralSettings {
            high_quality: true,
            formants: 25.0,
            envelope: 64.0,
        };
        assert_eq!(warp.spectral, spectral);
        assert_eq!(
            setup.committed_names(),
            [
                "Set Warp Mode",
                "Set Clip Transpose",
                "Set Clip Detune",
                "Set Slice Preserve",
                "Set Transient Loop Mode",
                "Set Transient Envelope",
                "Set Follow Pitch",
                "Set Grain Size",
                "Set Fluctuation",
                "Set Formants",
                "Set High Quality",
                "Set Spectral Envelope",
            ]
        );
        // The same again changes nothing, and leaves no step.
        for (command, name, value) in runs {
            setup
                .run_with(&command, Args::new().with(name, value))
                .unwrap();
        }
        assert!(setup.committed_names().is_empty());
        setup.run(&edit::UNDO);
        assert_eq!(warp_of(&setup, clip).spectral.envelope, 128.0);
        setup.run(&edit::UNDO);
        assert!(!warp_of(&setup, clip).spectral.high_quality);
        setup.run(&edit::UNDO);
        assert_eq!(warp_of(&setup, clip).spectral.formants, 100.0);
        setup.run(&edit::UNDO);
        assert_eq!(warp_of(&setup, clip).granular.fluctuation, 25.0);
        setup.run(&edit::UNDO);
        assert_eq!(warp_of(&setup, clip).granular.grain_size, 30.0);
        setup.run(&edit::UNDO);
        assert!(warp_of(&setup, clip).granular.follow_pitch);
        setup.run(&edit::UNDO);
        assert_eq!(warp_of(&setup, clip).slice.envelope, 100.0);
        let markers = warp_of(&setup, clip).markers;
        assert_eq!(markers, warp.markers, "the markers stay as they were");
    }

    #[test]
    fn settings_out_of_range_or_unknown_are_refused() {
        let (mut setup, clip) = analysed_song();
        let wrong: [(CommandId, &str, Value); 18] = [
            (SET_MODE, "mode", "complex".into()),
            (SET_FOLLOW_PITCH, "on", 1_i64.into()),
            (SET_GRAIN_SIZE, "milliseconds", 4.9.into()),
            (SET_GRAIN_SIZE, "milliseconds", 501.0.into()),
            (SET_FLUCTUATION, "fluctuation", (-0.5).into()),
            (SET_FLUCTUATION, "fluctuation", 100.5.into()),
            (SET_TRANSPOSE, "semitones", 49_i64.into()),
            (SET_TRANSPOSE, "semitones", (-300_i64).into()),
            (SET_DETUNE, "cents", 51_i64.into()),
            (SET_PRESERVE, "preserve", "1/3".into()),
            (SET_TRANSIENT_LOOP, "mode", "backwards".into()),
            (SET_TRANSIENT_ENVELOPE, "envelope", 100.5.into()),
            (SET_FORMANTS, "formants", (-1.0).into()),
            (SET_FORMANTS, "formants", 101.0.into()),
            (SET_HIGH_QUALITY, "on", "yes".into()),
            (SET_ENVELOPE, "envelope", 7.9.into()),
            (SET_ENVELOPE, "envelope", 513.0.into()),
            (SHOW_LANE, "param", "volume".into()),
        ];
        for (command, name, value) in wrong {
            let args = Args::new().with(name, value).with("clip", clip);
            let result = setup.run_with(&command, args);
            assert!(result.is_err(), "{command:?} {name}");
        }
        assert!(setup.committed_names().is_empty());
        // An unwarped clip has no warp to set.
        setup.run(&warp::SET_WARP);
        setup.committed_names();
        let args = Args::new().with("semitones", 3_i64);
        let error = setup.run_with(&SET_TRANSPOSE, args).unwrap_err();
        assert!(error.to_string().contains("isn't warped"), "{error}");
    }

    #[test]
    fn the_lane_shows_the_curve_of_the_parameter_picked() {
        let (mut setup, clip) = analysed_song();
        assert_eq!(setup.document.editor().sample_editor.lane, ClipParam::Gain);
        let args = Args::new().with("param", "preserve");
        setup.run_with(&SHOW_LANE, args).unwrap();
        assert_eq!(
            setup.document.editor().sample_editor.lane,
            ClipParam::Preserve
        );
        assert!(setup.committed_names().is_empty());
        // Its curve holds a step at a time, in the clip's own time.
        let param = ParamAddress::Clip(clip, ClipParam::Preserve);
        let args = Args::new()
            .with("param", param)
            .with("at", vec![Beats::ZERO, Beats::new(2)])
            .with("values", vec![5.0, 1.4]);
        setup.run_with(&automation::SET_POINTS, args).unwrap();
        let (_, audio) = audio(setup.document.project(), clip).unwrap();
        let lane = audio.lane(ClipParam::Preserve).unwrap();
        let points = [Point::new(Beats::ZERO, 5.0), Point::new(Beats::new(2), 1.0)];
        assert_eq!(lane.points, points, "rounded to whole steps");
        assert_eq!(lane.value_at(Beats::new(1)), 5.0);
    }

    #[test]
    fn a_parameter_set_without_its_curve_sets_the_warp_s_setting() {
        let (mut setup, clip) = analysed_song();
        let param = |param| ParamAddress::Clip(clip, param);
        setup.document.transact("Set", |tx| {
            tx.set_param_value(param(ClipParam::Transpose), 4.6);
            tx.set_param_value(param(ClipParam::Preserve), 2.2);
            tx.set_param_value(param(ClipParam::TransientLoop), -1.0);
            tx.set_param_value(param(ClipParam::TransientEnvelope), 250.0);
            tx.set_param_value(param(ClipParam::GrainSize), 1000.0);
            tx.set_param_value(param(ClipParam::Fluctuation), -5.0);
            tx.set_param_value(param(ClipParam::Formants), -3.0);
            tx.set_param_value(param(ClipParam::Envelope), 2.0);
        });
        let warp = warp_of(&setup, clip);
        assert_eq!(warp.transpose, 5);
        assert_eq!(warp.slice.preserve, Preserve::Half);
        assert_eq!(warp.slice.transient_loop, TransientLoop::Off);
        assert_eq!(warp.slice.envelope, 100.0);
        assert_eq!(warp.granular.grain_size, 500.0);
        assert_eq!(warp.granular.fluctuation, 0.0);
        assert_eq!(warp.spectral.formants, 0.0);
        assert_eq!(warp.spectral.envelope, 8.0);
        // Clearing a curve leaves the setting at the value it had at the
        // playhead.
        let args = Args::new()
            .with("param", param(ClipParam::Detune))
            .with("at", vec![Beats::ZERO, Beats::new(4)])
            .with("values", vec![-20.0, 20.0]);
        setup.run_with(&automation::SET_POINTS, args).unwrap();
        let args = Args::new().with("param", param(ClipParam::Detune));
        setup.run_with(&automation::CLEAR, args).unwrap();
        assert_eq!(warp_of(&setup, clip).detune, -20);
    }
}
