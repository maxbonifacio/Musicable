//! Commands for checking Musicable itself, which agents run in the PC
//! checks: recording what the audio device plays, with a timeline of what
//! the engine applied as it played, and measuring a device's response.

use std::path::PathBuf;

use musicable_model::DeviceId;

use crate::{Command, CommandError, CommandId, Param, Probing, Registry, Request, Type, Value};

/// Records every frame Musicable hands the audio device to a WAV file, with
/// a timeline of what the engine applied, on the same clock, beside it.
pub const RECORD_OUTPUT: CommandId = CommandId::new("debug.record_output");
/// Stops recording the output, and writes its timeline.
pub const STOP_RECORDING_OUTPUT: CommandId = CommandId::new("debug.stop_recording_output");
/// Plays a known signal through a copy of a device, offline, and measures
/// its frequency, phase and dynamics response.
pub const MEASURE_RESPONSE: CommandId = CommandId::new("debug.measure_response");

/// Registers the debug commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(RECORD_OUTPUT, "Record Output").with_params(&[Param::required(
            "file",
            Type::Text,
            "The WAV file to record to, a full path. The timeline goes beside it, with \
             .timeline.json in place of .wav.",
        )]),
        |ctx, args| {
            let file = PathBuf::from(args.get::<String>("file")?);
            if !(file.extension()).is_some_and(|extension| extension.eq_ignore_ascii_case("wav")) {
                return Err(CommandError::Invalid(format!(
                    "{} isn't a .wav file.",
                    file.display()
                )));
            }
            ctx.requests.push(Request::RecordOutput(file));
            Ok(Value::None)
        },
    );
    registry.register(STOP_RECORDING_OUTPUT, "Stop Recording Output", |ctx| {
        ctx.requests.push(Request::StopRecordingOutput);
    });
    registry.add(
        Command::new(MEASURE_RESPONSE, "Measure Device Response").with_params(&[
            Param::required(
                "device",
                Type::Device,
                "The device: one that takes audio, not an instrument or a MIDI effect.",
            ),
            Param::optional(
                "level_dbfs",
                Type::Number,
                "The noise's RMS level, in dBFS, from -60 to -6. Without it, -18.",
            ),
            Param::optional(
                "at",
                Type::List(&Type::Number),
                "Frequencies, in hertz, to give the gain and phase at, as well as the third \
                 octaves.",
            ),
        ]),
        |ctx, args| {
            let device: DeviceId = args.get("device")?;
            if ctx.document.project().find_device(device).is_none() {
                return Err(CommandError::invalid(format!(
                    "there's no device {}",
                    device.0
                )));
            }
            let level_dbfs: Option<f64> = args.opt("level_dbfs")?;
            let at: Vec<f64> = args.opt("at")?.unwrap_or_default();
            if !level_dbfs.iter().chain(&at).all(|value| value.is_finite()) {
                return Err(CommandError::invalid("the numbers have to be finite"));
            }
            ctx.requests.push(Request::MeasureResponse(Probing {
                device,
                level_dbfs,
                at,
            }));
            Ok(Value::None)
        },
    );
}
