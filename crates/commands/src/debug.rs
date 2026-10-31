//! Commands for checking Musicable itself, which agents run in the PC
//! checks: recording what the audio device plays, with a timeline of what
//! the engine applied as it played.

use std::path::PathBuf;

use crate::{Command, CommandError, CommandId, Param, Registry, Request, Type, Value};

/// Records every frame Musicable hands the audio device to a WAV file, with
/// a timeline of what the engine applied, on the same clock, beside it.
pub const RECORD_OUTPUT: CommandId = CommandId::new("debug.record_output");
/// Stops recording the output, and writes its timeline.
pub const STOP_RECORDING_OUTPUT: CommandId = CommandId::new("debug.stop_recording_output");

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
}
