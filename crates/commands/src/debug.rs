//! Commands for checking Musicable itself, which agents run in the PC
//! checks: recording what the audio device plays, with a timeline of what
//! the engine applied as it played, measuring a device's response, and
//! blind tests of pairs of sounds.

use std::path::PathBuf;

use musicable_model::DeviceId;

use crate::{
    BlindRound, Command, CommandError, CommandId, Param, Probing, Registry, Request, Type, Value,
};

/// Records every frame Musicable hands the audio device to a WAV file, with
/// a timeline of what the engine applied, on the same clock, beside it.
pub const RECORD_OUTPUT: CommandId = CommandId::new("debug.record_output");
/// Stops recording the output, and writes its timeline.
pub const STOP_RECORDING_OUTPUT: CommandId = CommandId::new("debug.stop_recording_output");
/// Plays a known signal through a copy of a device, offline, and measures
/// its frequency, phase and dynamics response.
pub const MEASURE_RESPONSE: CommandId = CommandId::new("debug.measure_response");
/// Makes a blind test of pairs of files, in a folder of its own.
pub const MAKE_BLIND_TEST: CommandId = CommandId::new("debug.make_blind_test");
/// Opens a blind test in the Blind Test window, to be judged.
pub const OPEN_BLIND_TEST: CommandId = CommandId::new("debug.open_blind_test");

/// Registers the debug commands.
pub fn register(registry: &mut Registry) {
    registry.add(
        Command::new(RECORD_OUTPUT, "Record Output")
            .with_params(&[Param::required(
                "file",
                Type::Text,
                "The WAV file to record to, a full path. The timeline goes beside it, with \
                 .timeline.json in place of .wav.",
            )])
            .alone(),
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
    registry.add(
        Command::new(STOP_RECORDING_OUTPUT, "Stop Recording Output").alone(),
        |ctx, _| {
            ctx.requests.push(Request::StopRecordingOutput);
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(MEASURE_RESPONSE, "Measure Device Response")
            .alone()
            .with_params(&[
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
    registry.add(
        Command::new(MAKE_BLIND_TEST, "Make Blind Test")
            .alone()
            .with_params(&[
                Param::required(
                    "folder",
                    Type::Text,
                    "A new or empty folder for the round, a full path.",
                ),
                Param::required(
                    "first",
                    Type::List(&Type::Text),
                    "Files, full paths: each is paired with the file at the same place in second.",
                ),
                Param::required(
                    "second",
                    Type::List(&Type::Text),
                    "Their partners, as many, full paths.",
                ),
                Param::optional(
                    "first_name",
                    Type::Text,
                    "What the first files are, such as Live, for the results. Without it, first.",
                ),
                Param::optional(
                    "second_name",
                    Type::Text,
                    "What the second files are, such as Musicable. Without it, second.",
                ),
                Param::optional(
                    "name",
                    Type::Text,
                    "The round's name, which the window shows.",
                ),
                Param::optional(
                    "abx",
                    Type::Bool,
                    "Whether each trial has an X, a copy of A or of B, to name. Without it, true.",
                ),
                Param::optional(
                    "seed",
                    Type::Int,
                    "The seed the order and sides are shuffled with. Without it, the time.",
                ),
            ]),
        |ctx, args| {
            let paths = |name: &str| -> Result<Vec<PathBuf>, CommandError> {
                let list: Vec<String> = args.get(name)?;
                Ok(list.into_iter().map(PathBuf::from).collect())
            };
            let name = |key: &str, or: &str| -> Result<String, CommandError> {
                Ok(args.opt::<String>(key)?.unwrap_or_else(|| or.to_owned()))
            };
            ctx.requests.push(Request::MakeBlindTest(BlindRound {
                folder: PathBuf::from(args.get::<String>("folder")?),
                first: paths("first")?,
                second: paths("second")?,
                first_name: name("first_name", "first")?,
                second_name: name("second_name", "second")?,
                name: name("name", "")?,
                abx: args.opt("abx")?.unwrap_or(true),
                seed: args.opt::<i64>("seed")?.map(|seed| seed as u64),
            }));
            Ok(Value::None)
        },
    );
    registry.add(
        Command::new(OPEN_BLIND_TEST, "Open Blind Test...")
            .alone()
            .with_params(&[Param::asked(
                "folder",
                Type::Text,
                "The round's folder, a full path: the app asks which without it.",
            )]),
        |ctx, args| {
            let folder = args.opt::<String>("folder")?.map(PathBuf::from);
            ctx.requests.push(Request::OpenBlindTest(folder));
            Ok(Value::None)
        },
    );
}
