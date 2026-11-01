//! The audio commands, run as people and agents run them, on files the
//! tests make, whose answers are known.

use std::cell::{Cell, RefCell};
use std::f64::consts::{FRAC_PI_4, TAU};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use musicable_commands::json as args_json;
use musicable_io::audio::{Decoder, SampleFormat, Writer};
use musicable_mcp::local::LocalTools;
use serde_json::{Map, Value as Json, json};

use super::cli::{self, Parsed};
use super::{AudioCommand, CANCELLED, Job, Share, Source, Tools, commands, format_of, same_frames};

/// A folder of its own for a test, removed when it ends.
pub(super) struct Folder(pub(super) PathBuf);

impl Folder {
    pub(super) fn new(name: &str) -> Self {
        let name = format!("musicable-audio-{name}-{}", std::process::id());
        let dir: PathBuf = std::env::temp_dir().join(name).components().collect();
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes `samples`, interleaved, to `path` in the format its extension
/// says.
pub(super) fn write(
    path: &Path,
    sample_format: SampleFormat,
    rate: u32,
    channels: u16,
    samples: &[f64],
) {
    let format = format_of(path).unwrap();
    let file = BufWriter::new(File::create(path).unwrap());
    let mut writer = Writer::new(file, format, sample_format, rate, channels).unwrap();
    writer.write_f64(samples).unwrap();
    writer.finish().unwrap();
}

/// Random numbers from -1 to 1, the same on every system.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        // xorshift64*.
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let bits = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        bits as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// A triangle wave of `period` frames at `frame`, from -1 to 1. Made with
/// arithmetic alone, it's the same on every system, so the files the
/// examples make are too.
fn triangle(frame: usize, period: usize) -> f64 {
    let phase = (frame % period) as f64 / period as f64;
    4.0 * (phase - (phase + 0.25).floor() + 0.25 - 0.5).abs() - 1.0
}

/// The files the examples work on, in `folder`: `tone.wav`, 2 s of stereo
/// 24-bit 44.1 kHz, silent for 0.5 s, then a 441 Hz tone at -12 dBFS for
/// a second and at -6 dBFS for half a second; `take.wav`, the same 10 ms
/// later and 1 dB quieter, with a click at 1.2 s; `loop.flac`, a second of
/// stereo 16-bit 44.1 kHz noise hits, four of them; and `voice.aif`, 1.5 s
/// of mono 32-bit float 48 kHz, a 240 Hz tone that swells and fades.
pub(super) fn fixtures(folder: &Path) {
    let tone: Vec<f64> = (0..88_200)
        .flat_map(|i| {
            let level = match i {
                ..22_050 => 0.0,
                22_050..66_150 => 0.25,
                _ => 0.5,
            };
            let sample = level * triangle(i, 100);
            [sample, sample]
        })
        .collect();
    write(
        &folder.join("tone.wav"),
        SampleFormat::Int24,
        44_100,
        2,
        &tone,
    );
    // The tone 10 ms later and 1 dB quieter, with a click at 1.2 s.
    let quieter = 10f64.powf(-1.0 / 20.0);
    let take: Vec<f64> = (0..88_200)
        .flat_map(|i| {
            let sample = match i {
                52_920 => 0.3,
                ..441 => 0.0,
                i => quieter * tone[2 * (i - 441)],
            };
            [sample, sample]
        })
        .collect();
    write(
        &folder.join("take.wav"),
        SampleFormat::Int24,
        44_100,
        2,
        &take,
    );
    let mut noise = Noise(0x5EED);
    let hits: Vec<f64> = (0..44_100)
        .flat_map(|i| {
            let since = (i % 11_025) as f64 / 4410.0;
            let level = 0.7 * (1.0 - since).max(0.0);
            [level * noise.next(), level * noise.next()]
        })
        .collect();
    write(
        &folder.join("loop.flac"),
        SampleFormat::Int16,
        44_100,
        2,
        &hits,
    );
    let voice: Vec<f64> = (0..72_000)
        .map(|i| {
            let along = i as f64 / 72_000.0;
            let swell = 0.6 * (1.0 - (2.0 * along - 1.0).abs());
            swell * triangle(i, 200)
        })
        .collect();
    write(
        &folder.join("voice.aif"),
        SampleFormat::Float32,
        48_000,
        1,
        &voice,
    );
}

/// Every sample of the file at `path`, interleaved.
fn samples(path: &Path) -> Vec<f64> {
    let mut decoder = Decoder::open(path).unwrap();
    let info = decoder.info();
    let mut samples = vec![0.0; info.frames as usize * info.channels];
    let mut read = 0;
    while read < info.frames as usize {
        read += decoder
            .read_f64(read as u64, &mut samples[read * info.channels..])
            .unwrap();
    }
    samples
}

pub(super) fn texts(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// Runs `command` with `words` on the command line, in `folder`.
fn typed(command: &AudioCommand, words: &[&str], folder: &Path) -> Result<Json, String> {
    let args = match cli::parse(command, &texts(words))? {
        Parsed::Args(args) => args,
        Parsed::Help => panic!("{words:?} asks for help"),
    };
    command.run(&args, &Job::quiet(Some(folder)))
}

fn command(id: &str) -> AudioCommand {
    (commands().into_iter())
        .find(|command| command.id == id)
        .unwrap_or_else(|| panic!("there's no {id}"))
}

/// Runs the command `id` with `words` on the command line, in `folder`.
fn run(id: &str, words: &[&str], folder: &Path) -> Result<Json, String> {
    typed(&command(id), words, folder)
}

/// The arguments the paths in which are file names in `folder`.
const PATH_ARGUMENTS: [&str; 8] = [
    "file",
    "files",
    "out",
    "like",
    "a",
    "b",
    "reference",
    "out_dir",
];

/// `arguments` with the file names in them made whole paths in `folder`.
fn in_folder(mut arguments: Map<String, Json>, folder: &Path) -> Map<String, Json> {
    let whole = |name: &Json| Json::from(folder.join(name.as_str().unwrap()).display().to_string());
    for name in PATH_ARGUMENTS {
        match arguments.get_mut(name) {
            Some(Json::Array(names)) => names.iter_mut().for_each(|name| *name = whole(name)),
            Some(name) => *name = whole(name),
            None => {}
        }
    }
    arguments
}

/// `result` with `folder` written as `<folder>` in its paths, and `/`
/// between names, as on every system.
pub(super) fn without_folder(result: &Json, folder: &Path, to: &str) -> Json {
    match result {
        Json::String(text) => {
            let folder = folder.display().to_string();
            Json::from(text.replace(&folder, to).replace('\\', "/"))
        }
        Json::Array(items) => Json::Array(
            (items.iter())
                .map(|item| without_folder(item, folder, to))
                .collect(),
        ),
        Json::Object(object) => Json::Object(
            (object.iter())
                .map(|(name, value)| (name.clone(), without_folder(value, folder, to)))
                .collect(),
        ),
        value => value.clone(),
    }
}

/// Checks `value` against the JSON Schema `schema`, as far as the audio
/// commands' schemas go: types, choices, minimums, objects' properties,
/// lists' items and length, and `anyOf`.
fn check(value: &Json, schema: &Json, at: &str) -> Result<(), String> {
    if let Some(variants) = schema.get("anyOf").and_then(Json::as_array) {
        return match variants
            .iter()
            .any(|variant| check(value, variant, at).is_ok())
        {
            true => Ok(()),
            false => Err(format!("{at}: {value} fits none of {schema}")),
        };
    }
    if let Some(ty) = schema.get("type") {
        let types: Vec<&str> = match ty {
            Json::Array(types) => types.iter().filter_map(Json::as_str).collect(),
            ty => ty.as_str().into_iter().collect(),
        };
        let fits = |ty: &str| match ty {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            "number" => value.is_number(),
            "integer" => value.is_i64() || value.is_u64(),
            _ => false,
        };
        if !types.into_iter().any(fits) {
            return Err(format!("{at}: {value} isn't of type {ty}"));
        }
    }
    if let Some(choices) = schema.get("enum").and_then(Json::as_array)
        && !choices.contains(value)
    {
        return Err(format!("{at}: {value} isn't one of {choices:?}"));
    }
    if let (Some(minimum), Some(number)) = (schema["minimum"].as_f64(), value.as_f64())
        && number < minimum
    {
        return Err(format!("{at}: {number} is less than {minimum}"));
    }
    if let Json::Object(object) = value {
        for name in schema["required"].as_array().into_iter().flatten() {
            let name = name.as_str().unwrap();
            if !object.contains_key(name) {
                return Err(format!("{at}: there's no {name}"));
            }
        }
        for (name, field) in object {
            match schema["properties"].get(name) {
                Some(property) => check(field, property, &format!("{at}.{name}"))?,
                None if schema["additionalProperties"] == false => {
                    return Err(format!("{at}: {name} isn't in the schema"));
                }
                None => {}
            }
        }
    }
    if let Json::Array(items) = value {
        if let Some(most) = schema["maxItems"].as_u64() {
            assert!(items.len() as u64 <= most, "{at}: more than {most} items");
        }
        for (i, item) in items.iter().enumerate() {
            check(item, &schema["items"], &format!("{at}[{i}]"))?;
        }
    }
    Ok(())
}

/// The contents of every file in `folder`, by name.
fn contents(folder: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = (fs::read_dir(folder).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read(&path).unwrap())
        })
        .collect();
    files.sort();
    files
}

#[test]
fn every_command_runs_its_example_from_the_command_line_and_as_a_tool() {
    for command in commands() {
        let id = command.id;
        assert!(!command.example.is_empty(), "{id} has no example");
        let typed_in = Folder::new(&format!("typed-{}", command.name()));
        let called_in = Folder::new(&format!("called-{}", command.name()));
        fixtures(&typed_in.0);
        fixtures(&called_in.0);
        let before = contents(&typed_in.0);

        let args = match cli::parse(&command, &texts(command.example)) {
            Ok(Parsed::Args(args)) => args,
            other => panic!("{id}'s example reads as {other:?}"),
        };
        let typed_result = (command.run(&args, &Job::quiet(Some(&typed_in.0))))
            .unwrap_or_else(|why| panic!("{id} from the command line: {why}"));
        let arguments = in_folder(args_json::args_to_json(&args), &called_in.0);
        let called_result = (Tools.call(&command.tool(), &arguments, &|| false, &|_| {}))
            .unwrap_or_else(|why| panic!("{id} as a tool: {why}"));
        assert_eq!(
            without_folder(&typed_result, &typed_in.0, "<folder>"),
            without_folder(&called_result, &called_in.0, "<folder>"),
            "{id} gives back the same both ways",
        );
        let schema = Json::Object(command.output_schema());
        check(&called_result, &schema, id).unwrap();
        assert!(!cli::text(&typed_result, &schema).is_empty());

        // What it wrote reads back as it says, and what it read is as it
        // was.
        if let Some(out) = called_result.get("out") {
            let path = Path::new(out["file"].as_str().unwrap());
            assert!(
                path.starts_with(&called_in.0),
                "{id} wrote {}",
                path.display()
            );
            let info = Decoder::open(path).unwrap().info();
            assert_eq!(Json::from(info.frames), out["frames"], "{id}");
            assert_eq!(Json::from(info.channels), out["channels"], "{id}");
            assert_eq!(Json::from(info.sample_rate), out["sample_rate"], "{id}");
            assert!(
                !Path::new(&format!("{}.part", path.display())).exists(),
                "{id}"
            );
        }
        let after = contents(&typed_in.0);
        for (name, bytes) in &before {
            let now = after.iter().find(|(file, _)| file == name);
            assert_eq!(
                now.map(|(_, bytes)| bytes),
                Some(bytes),
                "{id} changed {name}"
            );
        }
        assert!(
            after.iter().all(|(name, _)| !name.ends_with(".part")),
            "{id}"
        );
    }
}

#[test]
fn commands_have_ids_tools_and_schemas_of_their_own() {
    let commands = commands();
    let mut ids: Vec<&str> = commands.iter().map(|command| command.id).collect();
    ids.dedup();
    assert_eq!(ids.len(), commands.len(), "ids are in order, each once");
    assert_eq!(
        ids,
        [
            "audio.compare",
            "audio.convert",
            "audio.cut",
            "audio.find",
            "audio.gain",
            "audio.generate",
            "audio.info",
            "audio.join",
            "audio.loudness",
            "audio.match",
            "audio.spectrum"
        ]
    );
    for command in &commands {
        assert!(command.tool().starts_with("audio_"));
        for name in command.positional {
            let param = command.params.iter().find(|param| param.name == *name);
            assert!(param.is_some_and(|param| !param.optional), "{}", command.id);
        }
        let input = command.input_schema();
        assert_eq!(input["type"], "object");
        assert_eq!(input["additionalProperties"], false);
        let output = command.output_schema();
        assert_eq!(output["type"], "object", "{}", command.id);
        let writes =
            (command.params.iter()).any(|param| matches!(param.name.as_ref(), "out" | "out_dir"));
        assert_eq!(command.read_only, !writes, "{}", command.id);
    }
    let catalogue = super::catalogue();
    assert_eq!(catalogue.as_array().unwrap().len(), commands.len());
    assert_eq!(catalogue[2]["tool"], "audio_cut");
    let tools = Tools.tools();
    assert_eq!(tools.len(), commands.len());
    assert!(Tools.instructions().contains("audio_info"));
}

#[test]
fn info_describes_each_file_and_says_which_it_couldnt_read() {
    let folder = Folder::new("info");
    fixtures(&folder.0);
    fs::write(folder.path("notes.txt"), "not audio").unwrap();
    let result = run(
        "audio.info",
        &["tone.wav", "voice.aif", "missing.wav", "notes.txt"],
        &folder.0,
    )
    .unwrap();
    let files = result["files"].as_array().unwrap();
    assert_eq!(files[0]["encoding"], "int24");
    assert_eq!(files[0]["frames"], 88_200);
    assert_eq!(files[0]["seconds"], 2.0);
    assert_eq!(files[0]["peak_dbfs"], -6.02);
    assert_eq!(files[0]["acid_bpm"], Json::Null);
    assert_eq!(files[1]["format"], "aiff");
    assert_eq!(files[1]["encoding"], "float32");
    assert_eq!(files[1]["sample_rate"], 48_000);
    let missing = folder.path("missing.wav").display().to_string();
    assert_eq!(
        files[2],
        json!({"file": missing, "error": format!("There's no file at {missing}.")})
    );
    let error = files[3]["error"].as_str().unwrap();
    assert!(
        error.ends_with("isn't a WAV, AIFF or FLAC file, which are the files Musicable reads.")
    );

    // Agents give whole paths.
    let arguments = json!({"files": ["tone.wav"]});
    let Json::Object(arguments) = arguments else {
        unreachable!()
    };
    let refused = Tools.call("audio_info", &arguments, &|| false, &|_| {});
    assert_eq!(
        refused,
        Err(
            "Give the whole path of tone.wav, from the top of the disk: the tools don't know \
             which folder it's in."
                .to_owned()
        )
    );
}

/// The exact samples of `sample_format`: every value it holds is as
/// likely, in `frames` frames of 2 channels.
fn exact_samples(sample_format: SampleFormat, frames: usize, seed: u64) -> Vec<f64> {
    let mut noise = Noise(seed);
    (0..frames * 2)
        .map(|_| {
            let sample = noise.next();
            match sample_format {
                SampleFormat::Float32 => f64::from(sample as f32 * 1.5),
                SampleFormat::Float64 => sample * 1.5,
                int => {
                    let scale = f64::from(1u32 << (int.bits() - 1));
                    (sample * scale).floor().clamp(-scale, scale - 1.0) / scale
                }
            }
        })
        .collect()
}

/// Each format and the sample formats files of it hold, and the extension
/// of each.
fn every_format() -> Vec<(&'static str, SampleFormat)> {
    use SampleFormat::{Float32, Float64, Int8, Int16, Int24, Int32};
    let mut formats = Vec::new();
    for extension in ["wav", "aif", "flac"] {
        for sample_format in [Int8, Int16, Int24, Int32, Float32, Float64] {
            let flac = extension == "flac";
            if !flac || (!sample_format.is_float() && sample_format.bits() <= 24) {
                formats.push((extension, sample_format));
            }
        }
    }
    formats
}

#[test]
fn cuts_joins_and_conversions_are_exact_in_every_format() {
    let folder = Folder::new("exact");
    for (extension, sample_format) in every_format() {
        let name = format!("{}.{extension}", sample_format.name());
        let source = folder.path(&name);
        let all = exact_samples(sample_format, 3000, 7);
        write(&source, sample_format, 44_100, 2, &all);
        let what = name.clone();

        // A cut is the frames it cut.
        let out = format!("cut-{name}");
        let cut = run(
            "audio.cut",
            &[
                &name,
                "--from-frame",
                "1000",
                "--length-frames",
                "1500",
                "--out",
                &out,
            ],
            &folder.0,
        )
        .unwrap();
        assert_eq!(cut["exact"], true, "{what}");
        assert_eq!(cut["from"], json!({"seconds": 0.022676, "frame": 1000}));
        assert_eq!(
            cut["out"]["encoding"],
            sample_format.encoding().name(),
            "{what}"
        );
        let copy = samples(&folder.path(&out));
        assert_eq!(copy.len(), 3000);
        assert!(
            (copy.iter().zip(&all[2000..5000])).all(|(a, b)| a.to_bits() == b.to_bits()),
            "{what}"
        );

        // A join is its files end to end.
        let other = format!("other-{name}");
        let more = exact_samples(sample_format, 500, 11);
        write(&folder.path(&other), sample_format, 44_100, 2, &more);
        let out = format!("join-{name}");
        let joined = run(
            "audio.join",
            &[&name, &other, "--repeat", "2", "--out", &out],
            &folder.0,
        )
        .unwrap();
        assert_eq!(joined["exact"], true, "{what}");
        assert_eq!(joined["count"], 4);
        assert_eq!(joined["parts"][3]["from"]["frame"], 6500);
        let both = [all.clone(), more.clone(), all.clone(), more.clone()].concat();
        let copy = samples(&folder.path(&out));
        assert!(
            copy.len() == both.len()
                && (copy.iter().zip(&both)).all(|(a, b)| a.to_bits() == b.to_bits()),
            "{what}"
        );

        // So is a conversion to another format, which hashes the same.
        for to in ["wav", "aif", "flac"] {
            if to == extension
                || (to == "flac" && (sample_format.is_float() || sample_format.bits() > 24))
            {
                continue;
            }
            let out = format!("convert-{}-{extension}.{to}", sample_format.name());
            let converted = run("audio.convert", &[&name, "--out", &out], &folder.0).unwrap();
            assert_eq!(converted["exact"], true, "{what} to {to}");
            assert_eq!(converted["dithered"], false);
            let info = run("audio.info", &[&name], &folder.0).unwrap();
            assert_eq!(
                converted["out"]["audio_md5"], info["files"][0]["audio_md5"],
                "{what} to {to}"
            );
        }
    }
}

#[test]
fn a_cut_s_fades_change_its_ends_alone() {
    let folder = Folder::new("fades");
    let all = exact_samples(SampleFormat::Float32, 4410, 3);
    write(
        &folder.path("a.wav"),
        SampleFormat::Float32,
        44_100,
        2,
        &all,
    );
    let cut = run(
        "audio.cut",
        &[
            "a.wav",
            "--from",
            "0.01",
            "--to",
            "0.09",
            "--fade-ms",
            "10",
            "--out",
            "b.wav",
        ],
        &folder.0,
    )
    .unwrap();
    assert_eq!(cut["exact"], true);
    assert_eq!(cut["to"]["frame"], 3969);
    let copy = samples(&folder.path("b.wav"));
    let source = &all[441 * 2..3969 * 2];
    // Silent at the ends, and the source between the fades.
    assert_eq!((copy[0], copy[1]), (0.0, 0.0));
    assert_eq!(copy[copy.len() - 2..], [0.0, 0.0]);
    assert_eq!(
        copy[441 * 2..copy.len() - 441 * 2],
        source[441 * 2..source.len() - 441 * 2]
    );
    assert!(copy[220 * 2].abs() < source[220 * 2].abs());
}

#[test]
fn joins_repeat_leave_gaps_and_crossfade() {
    let folder = Folder::new("joins");
    let one = vec![0.5; 2000];
    write(&folder.path("one.wav"), SampleFormat::Int16, 1000, 2, &one);
    let other = vec![-0.25; 2000];
    write(
        &folder.path("other.wav"),
        SampleFormat::Int24,
        1000,
        2,
        &other,
    );

    let gapped = run(
        "audio.join",
        &[
            "one.wav",
            "other.wav",
            "--gap",
            "0.5",
            "--out",
            "gapped.wav",
        ],
        &folder.0,
    )
    .unwrap();
    // The narrowest encoding that holds both.
    assert_eq!(gapped["out"]["encoding"], "int24");
    assert_eq!(gapped["out"]["frames"], 2500);
    assert_eq!(
        gapped["parts"][1]["from"],
        json!({"seconds": 1.5, "frame": 1500})
    );
    let copy = samples(&folder.path("gapped.wav"));
    assert_eq!(copy[1999], 0.5);
    assert_eq!(copy[2000..3000], [0.0; 1000]);
    assert_eq!(copy[3000], -0.25);

    let faded = run(
        "audio.join",
        &[
            "one.wav",
            "one.wav",
            "--crossfade-ms",
            "100",
            "--out",
            "faded.wav",
        ],
        &folder.0,
    )
    .unwrap();
    assert_eq!(faded["exact"], true);
    assert_eq!(faded["out"]["frames"], 1900);
    assert_eq!(faded["parts"][1]["from"]["frame"], 900);
    // Equal power: a signal crossfaded into itself swells by up to 3 dB.
    let copy = samples(&folder.path("faded.wav"));
    let middle = copy[950 * 2];
    assert!((middle - 0.5 * 2f64.sqrt()).abs() < 0.01, "{middle}");

    let refused = |words: &[&str]| run("audio.join", words, &folder.0).unwrap_err();
    assert_eq!(
        refused(&[
            "one.wav",
            "one.wav",
            "--gap",
            "1",
            "--crossfade-ms",
            "10",
            "--out",
            "x.wav"
        ]),
        "Give gap or crossfade_ms, not both."
    );
    write(&folder.path("fast.wav"), SampleFormat::Int16, 2000, 2, &one);
    let rates = refused(&["one.wav", "fast.wav", "--out", "x.wav"]);
    assert!(rates.contains("is at 2000 Hz"), "{rates}");
    let long = refused(&[
        "one.wav",
        "one.wav",
        "--crossfade-ms",
        "1500",
        "--out",
        "x.wav",
    ]);
    assert!(long.contains("is shorter than its crossfades"), "{long}");
    let floats = vec![0.5; 200];
    write(
        &folder.path("float.wav"),
        SampleFormat::Float32,
        1000,
        2,
        &floats,
    );
    let flac = refused(&["one.wav", "float.wav", "--out", "x.flac"]);
    assert!(
        flac.starts_with("FLAC files hold integers of up to 24 bits"),
        "{flac}"
    );
}

#[test]
fn stretches_are_given_in_seconds_or_frames() {
    let folder = Folder::new("stretches");
    fixtures(&folder.0);
    let cut = |words: &[&str]| {
        let mut all = vec!["tone.wav", "--out", "cut.wav", "--overwrite"];
        all.extend(words);
        run("audio.cut", &all, &folder.0)
    };
    let frames = |result: Result<Json, String>| {
        let result = result.unwrap();
        (
            result["from"]["frame"].clone(),
            result["to"]["frame"].clone(),
        )
    };
    assert_eq!(
        frames(cut(&["--from", "1", "--to", "1.5"])),
        (json!(44_100), json!(66_150))
    );
    assert_eq!(
        frames(cut(&["--from", "1", "--length", "0.5"])),
        (json!(44_100), json!(66_150))
    );
    assert_eq!(
        frames(cut(&["--from-frame", "7", "--length-frames", "3"])),
        (json!(7), json!(10))
    );
    assert_eq!(frames(cut(&["--to-frame", "10"])), (json!(0), json!(10)));
    assert_eq!(
        cut(&["--from", "1", "--from-frame", "3"]),
        Err("Give one of from, from_frame, not more.".to_owned())
    );
    let path = folder.path("tone.wav").display().to_string();
    assert_eq!(
        cut(&["--from", "1", "--length", "5"]),
        Err(format!(
            "{path} is 2 s long (88200 frames), so it ends before frame 264600."
        ))
    );
    assert_eq!(
        cut(&["--from", "1.5", "--to", "1"]),
        Err(
            "The stretch from frame 66150 to frame 44100 is empty: it must end after it starts."
                .to_owned()
        )
    );
    assert_eq!(
        cut(&["--from", "-1"]),
        Err("from is a number of seconds, 0 or more.".to_owned())
    );
}

#[test]
fn commands_never_write_over_a_file_or_to_one_they_read() {
    let folder = Folder::new("safety");
    fixtures(&folder.0);
    fs::write(folder.path("taken.wav"), "mine").unwrap();
    let cut = |out: &str, more: &[&str]| {
        let mut words = vec!["tone.wav", "--length", "0.1", "--out", out];
        words.extend(more);
        run("audio.cut", &words, &folder.0)
    };
    let taken = folder.path("taken.wav").display().to_string();
    assert_eq!(
        cut("taken.wav", &[]),
        Err(format!(
            "There's a file at {taken} already: set overwrite to write over it."
        ))
    );
    assert_eq!(fs::read(folder.path("taken.wav")).unwrap(), b"mine");
    let tone = folder.path("tone.wav").display().to_string();
    let reads = format!(
        "{tone} is a file the command reads, which it never writes to: write somewhere else."
    );
    assert_eq!(cut("tone.wav", &["--overwrite"]), Err(reads.clone()));
    assert_eq!(cut("./tone.wav", &["--overwrite"]), Err(reads));
    let gain = run(
        "audio.gain",
        &[
            "tone.wav",
            "--like",
            "loop.flac",
            "--out",
            "loop.flac",
            "--overwrite",
        ],
        &folder.0,
    );
    assert!(gain.unwrap_err().contains("is a file the command reads"));
    assert_eq!(
        cut("cut.mp3", &[]),
        Err(format!(
            "{} needs the extension .wav, .aif or .flac, which says what to write.",
            folder.path("cut.mp3").display()
        ))
    );
    // With overwrite, it writes over it, whole.
    let written = cut("taken.wav", &["--overwrite"]).unwrap();
    assert_eq!(written["out"]["frames"], 4410);
    assert!(!folder.path("taken.wav.part").exists());
    // And into folders that aren't there yet.
    let nested = cut("new/folder/cut.flac", &[]).unwrap();
    assert_eq!(nested["exact"], true);
}

#[test]
fn a_cancelled_command_leaves_nothing_behind() {
    let folder = Folder::new("cancelled");
    // Long enough to read in more than one chunk.
    let long = exact_samples(SampleFormat::Int16, 200_000, 5);
    write(
        &folder.path("long.wav"),
        SampleFormat::Int16,
        44_100,
        2,
        &long,
    );
    let before = contents(&folder.0);
    for (tool, arguments) in [
        ("audio_cut", json!({"file": "long.wav", "out": "cut.wav"})),
        (
            "audio_convert",
            json!({"file": "long.wav", "out": "cut.flac", "rate": 48000}),
        ),
        (
            "audio_gain",
            json!({"file": "long.wav", "out": "cut.wav", "db": -3}),
        ),
        (
            "audio_join",
            json!({"files": ["long.wav"], "out": "cut.wav", "repeat": 2}),
        ),
    ] {
        let Json::Object(arguments) = arguments else {
            unreachable!()
        };
        let arguments = in_folder(arguments, &folder.0);
        // Cancelled once it's under way.
        let checks = Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() > 2
        };
        let result = Tools.call(tool, &arguments, &cancelled, &|_| {});
        assert_eq!(result, Err(CANCELLED.to_owned()), "{tool}");
        assert_eq!(contents(&folder.0), before, "{tool} left something behind");
    }
}

#[test]
fn a_command_that_fails_part_way_leaves_nothing_behind() {
    let folder = Folder::new("failed");
    let long = exact_samples(SampleFormat::Int16, 200_000, 5);
    write(
        &folder.path("long.flac"),
        SampleFormat::Int16,
        44_100,
        2,
        &long,
    );
    // Damaged two thirds of the way in, past the first chunk.
    let mut bytes = fs::read(folder.path("long.flac")).unwrap();
    let damaged = bytes.len() * 2 / 3;
    bytes[damaged..damaged + 64]
        .iter_mut()
        .for_each(|byte| *byte ^= 0xFF);
    fs::write(folder.path("long.flac"), bytes).unwrap();
    let before = contents(&folder.0);
    for (id, words) in [
        ("audio.cut", &["long.flac", "--out", "cut.wav"][..]),
        (
            "audio.convert",
            &["long.flac", "--rate", "48000", "--out", "cut.wav"],
        ),
        (
            "audio.gain",
            &["long.flac", "--db", "-3", "--out", "cut.wav"],
        ),
        ("audio.join", &["long.flac", "--out", "cut.wav"]),
    ] {
        let why = run(id, words, &folder.0).unwrap_err();
        assert!(why.starts_with("Couldn't read "), "{id}: {why}");
        assert_eq!(contents(&folder.0), before, "{id} left something behind");
    }
}

#[test]
fn progress_goes_from_0_to_1() {
    let folder = Folder::new("progress");
    let long = exact_samples(SampleFormat::Int16, 200_000, 5);
    write(
        &folder.path("long.wav"),
        SampleFormat::Int16,
        44_100,
        2,
        &long,
    );
    let arguments = json!({"file": "long.wav", "out": "cut.wav", "from": 1});
    let Json::Object(arguments) = arguments else {
        unreachable!()
    };
    let arguments = in_folder(arguments, &folder.0);
    let reported = RefCell::new(Vec::new());
    let progress = |done: f64| reported.borrow_mut().push(done);
    Tools
        .call("audio_cut", &arguments, &|| false, &progress)
        .unwrap();
    let reported = reported.into_inner();
    assert!(reported.len() > 3, "{reported:?}");
    assert!(
        reported.windows(2).all(|pair| pair[0] <= pair[1]),
        "{reported:?}"
    );
    assert_eq!(reported.last(), Some(&1.0));
}

/// A stereo 441 Hz triangle wave at `rate` in stretches of `seconds` at
/// `level` each, or silent at none.
fn bursts(rate: u32, stretches: &[(f64, Option<f64>)]) -> Vec<f64> {
    let mut samples = Vec::new();
    for &(seconds, level) in stretches {
        let frames = (seconds * f64::from(rate)).round() as usize;
        for i in 0..frames {
            // From a peak, so the first sample isn't silent.
            let sample = level.map_or(0.0, |level| level * triangle(i + 25, 100));
            samples.extend([sample, sample]);
        }
    }
    samples
}

#[test]
fn find_finds_sounds_silences_and_the_loudest_and_quietest_stretches() {
    let folder = Folder::new("find");
    let stretches = [
        (0.3, None),
        (0.4, Some(0.1)),
        (0.2, None),
        (0.3, Some(0.5)),
        (1.0, None),
        (0.5, Some(0.25)),
        (0.25, None),
    ];
    write(
        &folder.path("bursts.wav"),
        SampleFormat::Int24,
        44_100,
        2,
        &bursts(44_100, &stretches),
    );
    let find = |words: &[&str]| {
        let mut all = vec!["bursts.wav"];
        all.extend(words);
        run("audio.find", &all, &folder.0).unwrap()
    };
    let frames = |result: &Json| -> Vec<(u64, u64)> {
        (result["stretches"].as_array().unwrap().iter())
            .map(|stretch| {
                let frame = |end: &str| stretch[end]["frame"].as_u64().unwrap();
                (frame("from"), frame("to"))
            })
            .collect()
    };
    // The gap of 0.2 s is part of the sound around it.
    let sound = find(&["--what", "sound"]);
    assert_eq!(frames(&sound), [(13_230, 52_920), (97_020, 119_070)]);
    assert_eq!(sound["found"], 2);
    assert_eq!(
        sound["sound"],
        json!({
            "from": {"seconds": 0.3, "frame": 13_230},
            "to": {"seconds": 2.7, "frame": 119_070},
        })
    );
    let silence = find(&["--what", "silence"]);
    assert_eq!(frames(&silence), [(52_920, 97_020)]);
    assert_eq!(silence["stretches"][0]["peak_dbfs"], Json::Null);
    assert_eq!(silence["stretches"][0]["loudness_lufs"], Json::Null);
    // Shorter silences count with a shorter shortest.
    let silences = find(&["--what", "silence", "--min-silence", "0.1"]);
    assert_eq!(
        frames(&silences),
        [
            (0, 13_230),
            (30_870, 39_690),
            (52_920, 97_020),
            (119_070, 130_095)
        ]
    );
    assert_eq!(
        find(&["--what", "sound", "--min-silence", "0.1"])["found"],
        3
    );
    assert_eq!(
        find(&["--what", "sound", "--count", "1", "--min-silence", "0.1"])["found"],
        3
    );

    let loudest = find(&["--what", "loudest", "--length", "0.3"]);
    assert_eq!(frames(&loudest), [(39_690, 52_920)]);
    assert_eq!(loudest["stretches"][0]["peak_dbfs"], -6.02);
    let three = find(&["--what", "loudest", "--length", "0.3", "--count", "3"]);
    let three = frames(&three);
    assert_eq!(three.len(), 3);
    assert!(
        three
            .windows(2)
            .all(|pair| pair[0].1 <= pair[1].0 || pair[1].1 <= pair[0].0)
    );
    // The next loudest is in the -12 dBFS burst.
    assert!(three[1].0 >= 97_020 && three[1].1 <= 119_070, "{three:?}");
    // The quietest is in the long silence between the sounds, not before
    // or after them.
    let quietest = find(&["--what", "quietest", "--length", "0.5"]);
    let [(from, to)] = frames(&quietest)[..] else {
        panic!("{quietest}")
    };
    assert!(from >= 52_920 && to <= 97_020, "{from} to {to}");
    // In a stretch of the file.
    let later = find(&["--what", "sound", "--from", "1.5"]);
    assert_eq!(frames(&later), [(97_020, 119_070)]);
}

#[test]
fn loudness_is_measured_as_ebu_r_128_has_it() {
    let folder = Folder::new("loudness");
    let near = |value: &Json, expected: f64, tolerance: f64| {
        let value = value
            .as_f64()
            .unwrap_or_else(|| panic!("{value} isn't a number"));
        assert!(
            (value - expected).abs() <= tolerance,
            "{value} isn't {expected} ±{tolerance}"
        );
    };
    // EBU Tech 3341's first case: a stereo 1 kHz sine at -23 dBFS reads
    // -23 LUFS, within 0.1 LU.
    let level = 10f64.powf(-23.0 / 20.0);
    let sine: Vec<f64> = (0..12 * 48_000)
        .flat_map(|i| {
            let sample = level * (TAU * 1000.0 * f64::from(i) / 48_000.0).sin();
            [sample, sample]
        })
        .collect();
    write(
        &folder.path("sine.wav"),
        SampleFormat::Float32,
        48_000,
        2,
        &sine,
    );
    let result = run("audio.loudness", &["sine.wav", "--to", "10"], &folder.0).unwrap();
    let levels = &result["files"][0];
    assert_eq!(levels["seconds"], 10.0);
    near(&levels["integrated_lufs"], -23.0, 0.1);
    near(&levels["momentary_max_lufs"], -23.0, 0.1);
    near(&levels["short_term_max_lufs"], -23.0, 0.1);
    near(&levels["range_lu"], 0.0, 0.1);
    assert_eq!(levels["sample_peak_dbfs"], -23.0);
    near(&levels["true_peak_dbtp"], -23.0, 0.01);
    // A sine's RMS is 3 dB below its peak.
    assert_eq!(levels["rms_dbfs"], -26.01);
    assert_eq!(levels["dc_offset"], 0.0);
    assert_eq!(levels["clipped_samples"], 0);

    // Case 19: a sine at a quarter of the rate, at 45°, peaks 3 dB above
    // its samples, within +0.2 and -0.4 dB: over full scale, though no
    // sample is.
    let frames = 24_000;
    let peaks: Vec<f64> = (0..frames)
        .flat_map(|i| {
            let fade = (i.min(frames - 1 - i) as f64 / 480.0).min(1.0);
            let sample = 1.41 * fade * (TAU * i as f64 / 4.0 + FRAC_PI_4).sin();
            [sample, sample]
        })
        .collect();
    write(
        &folder.path("peaks.wav"),
        SampleFormat::Float32,
        48_000,
        2,
        &peaks,
    );
    let result = run("audio.loudness", &["peaks.wav"], &folder.0).unwrap();
    let levels = &result["files"][0];
    let true_peak = levels["true_peak_dbtp"].as_f64().unwrap();
    assert!((2.6..=3.2).contains(&true_peak), "{true_peak}");
    assert_eq!(levels["sample_peak_dbfs"], -0.03);
    assert_eq!(levels["clipped_samples"], 0);
}

#[test]
fn conversions_resample_mix_down_and_dither() {
    let folder = Folder::new("convert");
    fixtures(&folder.0);
    let convert = |words: &[&str]| run("audio.convert", words, &folder.0);
    let resampled = convert(&["voice.aif", "--rate", "44100", "--out", "voice.wav"]).unwrap();
    assert_eq!(resampled["out"]["frames"], 66_150);
    assert_eq!(resampled["out"]["encoding"], "float32");
    assert_eq!(
        (resampled["exact"].clone(), resampled["dithered"].clone()),
        (json!(false), json!(false))
    );
    let before = run("audio.loudness", &["voice.aif"], &folder.0).unwrap();
    let after = run("audio.loudness", &["voice.wav"], &folder.0).unwrap();
    let lufs = |result: &Json| result["files"][0]["integrated_lufs"].as_f64().unwrap();
    assert!(
        (lufs(&before) - lufs(&after)).abs() < 0.05,
        "{before} {after}"
    );

    let narrower = convert(&["tone.wav", "--encoding", "int16", "--out", "tone.flac"]).unwrap();
    assert_eq!(narrower["dithered"], true);
    assert_eq!(narrower["exact"], false);
    // Silence stays silent.
    let quiet = samples(&folder.path("tone.flac"));
    assert!(quiet[..22_050 * 2].iter().all(|sample| *sample == 0.0));

    let mono = convert(&["tone.wav", "--channels", "1", "--out", "mono.wav"]).unwrap();
    assert_eq!(mono["out"]["channels"], 1);
    assert_eq!(mono["out"]["peak_dbfs"], -6.02);
    assert_eq!(
        (mono["exact"].clone(), mono["dithered"].clone()),
        (json!(false), json!(true))
    );
    let stereo = convert(&["voice.aif", "--channels", "2", "--out", "stereo.aif"]).unwrap();
    let both = samples(&folder.path("stereo.aif"));
    let (frames, _) = both.as_chunks::<2>();
    assert!(frames.iter().all(|[left, right]| left == right));
    assert_eq!(stereo["out"]["audio_md5"].as_str().map(str::len), Some(32));
    assert_eq!(stereo["exact"], true);
    // Copying integers' one channel to two changes no sample either, so it
    // doesn't dither: 100,000 frames, more than a chunk.
    let one = exact_samples(SampleFormat::Int16, 50_000, 3);
    write(
        &folder.path("one.wav"),
        SampleFormat::Int16,
        44_100,
        1,
        &one,
    );
    let two = convert(&["one.wav", "--channels", "2", "--out", "two.wav"]).unwrap();
    assert_eq!(
        (two["exact"].clone(), two["dithered"].clone()),
        (json!(true), json!(false))
    );
    let doubled: Vec<f64> = one.iter().flat_map(|&sample| [sample, sample]).collect();
    assert!(samples(&folder.path("two.wav")) == doubled);
    assert!(
        convert(&["tone.wav", "--channels", "3", "--out", "three.wav"])
            .unwrap_err()
            .contains("can't make 3 channels")
    );

    // Floats beyond full scale clip as integers, and FLAC needs integers.
    let loud = vec![1.5, -1.5, 0.5, 0.25];
    write(
        &folder.path("loud.wav"),
        SampleFormat::Float32,
        44_100,
        2,
        &loud,
    );
    let clipped = convert(&["loud.wav", "--encoding", "int16", "--out", "clipped.wav"]).unwrap();
    assert_eq!(clipped["clipped_samples"], 2);
    let flac = convert(&["loud.wav", "--out", "loud.flac"]).unwrap_err();
    assert!(flac.contains("give encoding int24 or int16"), "{flac}");
    let flac = convert(&["loud.wav", "--encoding", "float32", "--out", "loud.flac"]).unwrap_err();
    assert!(
        flac.starts_with("FLAC files hold integers of up to 24 bits"),
        "{flac}"
    );
}

#[test]
fn a_mono_file_is_a_stereo_one_only_if_each_channel_is_a_copy() {
    let folder = Folder::new("copies");
    let job = Job::quiet(Some(&folder.0));
    let file = |name: &str, channels: u16, samples: &[f64]| {
        let path = folder.path(name);
        write(&path, SampleFormat::Int16, 44_100, channels, samples);
        Source::open(&path).unwrap()
    };
    let mut one = file("one.wav", 1, &[0.25, -0.5, 0.125]);
    let copies = [0.25, 0.25, -0.5, -0.5, 0.125, 0.125];
    let cases = [
        (copies, true),
        ([0.25, 0.25, -0.5, -0.5, 0.125, 0.0], false),
        ([0.25, 0.25, 0.5, -0.5, 0.125, 0.125], false),
    ];
    for (i, (samples, same)) in cases.into_iter().enumerate() {
        let mut two = file(&format!("two-{i}.wav"), 2, &samples);
        let found = same_frames(&mut one, 0, &mut two, 0, 3, &[], &job, Share::ALL).unwrap();
        assert_eq!(found, same, "{samples:?}");
    }
    // Nor is a stereo file a mono one.
    let mut two = file("two.wav", 2, &copies);
    assert!(!same_frames(&mut two, 0, &mut one, 0, 3, &[], &job, Share::ALL).unwrap());
}

#[test]
fn gain_reaches_the_loudness_or_peak_it_is_asked_for() {
    let folder = Folder::new("gain");
    fixtures(&folder.0);
    let gain = |words: &[&str]| run("audio.gain", words, &folder.0);
    let quieter = gain(&["loop.flac", "--to-lufs", "-23", "--out", "a.wav"]).unwrap();
    assert_eq!(quieter["integrated_lufs"], -23.0);
    let peak = gain(&["tone.wav", "--to-peak-dbfs", "-1", "--out", "b.wav"]).unwrap();
    assert_eq!(peak["gain_db"], 5.02);
    assert_eq!(peak["sample_peak_dbfs"], -1.0);
    let true_peak = gain(&["loop.flac", "--to-true-peak-dbtp", "-1", "--out", "c.aif"]).unwrap();
    assert_eq!(true_peak["true_peak_dbtp"], -1.0);
    let like = gain(&["voice.aif", "--like", "a.wav", "--out", "d.wav"]).unwrap();
    assert_eq!(like["integrated_lufs"], -23.0);
    let louder = gain(&["tone.wav", "--db", "12", "--out", "e.wav"]).unwrap();
    // Half the samples of the -6 dBFS tone, 12 dB louder, are beyond full
    // scale.
    assert_eq!(louder["clipped_samples"], 22_050);
    assert_eq!(louder["sample_peak_dbfs"], 0.0);
    assert_eq!(
        gain(&[
            "tone.wav",
            "--db",
            "1",
            "--to-lufs",
            "-14",
            "--out",
            "f.wav"
        ]),
        Err(
            "Give one of db, to_lufs, to_peak_dbfs, to_true_peak_dbtp or like, which says how \
             much gain."
                .to_owned()
        )
    );
    write(
        &folder.path("silent.wav"),
        SampleFormat::Int16,
        44_100,
        1,
        &[0.0; 100],
    );
    let silent = gain(&["silent.wav", "--to-lufs", "-14", "--out", "g.wav"]).unwrap_err();
    assert!(
        silent.ends_with("is silent, or too quiet to measure, so no gain brings it to -14 LUFS.")
    );
}

#[test]
fn the_command_line_reads_options_as_their_parameters_types() {
    let cut = command("audio.cut");
    let parsed = |words: &[&str]| cli::parse(&cut, &texts(words));
    let Ok(Parsed::Args(args)) = parsed(&[
        "a.wav",
        "--fade-ms=5",
        "--out",
        "b.wav",
        "--overwrite",
        "--json",
    ]) else {
        panic!("it reads");
    };
    assert_eq!(args.get::<String>("file"), Ok("a.wav".to_owned()));
    assert_eq!(args.get::<f64>("fade_ms"), Ok(5.0));
    assert_eq!(args.get::<bool>("overwrite"), Ok(true));
    let Ok(Parsed::Args(args)) = parsed(&["--no-overwrite", "--", "--a.wav"]) else {
        panic!("it reads");
    };
    assert_eq!(args.get::<bool>("overwrite"), Ok(false));
    assert_eq!(args.get::<String>("file"), Ok("--a.wav".to_owned()));
    assert_eq!(parsed(&["a.wav", "--help"]), Ok(Parsed::Help));
    assert_eq!(
        parsed(&["a.wav", "--from", "soon"]),
        Err("--from takes a number, not \"soon\".".to_owned())
    );
    assert_eq!(
        parsed(&["a.wav", "--out"]),
        Err("--out needs a path after it.".to_owned())
    );
    assert_eq!(
        parsed(&["a.wav", "b.wav"]),
        Err("musicable audio cut takes one file before its options.".to_owned())
    );
    assert_eq!(
        parsed(&["a.wav", "--louder"]),
        Err(
            "--louder isn't an option of musicable audio cut: musicable audio cut --help lists \
             them."
                .to_owned()
        )
    );
    let info = command("audio.info");
    let Ok(Parsed::Args(args)) = cli::parse(&info, &texts(&["a.wav", "b.wav", "--files", "c.wav"]))
    else {
        panic!("it reads");
    };
    assert_eq!(
        args.get::<Vec<String>>("files"),
        Ok(texts(&["c.wav", "a.wav", "b.wav"]))
    );
    let find = command("audio.find");
    let help = cli::help(&find);
    assert!(help.starts_with("usage: musicable audio find FILE [options] [--json]"));
    assert!(
        help.contains("--what <loudest|quietest|sound|silence>"),
        "{help}"
    );
    assert!(help.contains("Required."));
}

#[test]
fn text_for_people_has_each_field_on_a_line_in_the_schema_s_order() {
    let folder = Folder::new("text");
    fixtures(&folder.0);
    let cut = command("audio.cut");
    let result = typed(
        &cut,
        &[
            "tone.wav", "--from", "1", "--length", "0.5", "--out", "c.wav",
        ],
        &folder.0,
    )
    .unwrap();
    let text = cli::text(&result, &Json::Object(cut.output_schema()));
    let text = text
        .replace(&folder.0.display().to_string(), "<folder>")
        .replace('\\', "/");
    assert_eq!(
        text,
        "from: 1.0 s, frame 44100\n\
         to: 1.5 s, frame 66150\n\
         out:\n  \
           file: <folder>/c.wav\n  \
           format: wav\n  \
           encoding: int24\n  \
           channels: 2\n  \
           sample_rate: 44100\n  \
           frames: 22050\n  \
           seconds: 0.5\n  \
           peak_dbfs: -12.04\n  \
           acid_bpm: none\n  \
           audio_md5: 2b4b0cd6a1ee7a1e83c3ac8b2b8a2f62\n\
         exact: true\n"
            .replace(
                "2b4b0cd6a1ee7a1e83c3ac8b2b8a2f62",
                result["out"]["audio_md5"].as_str().unwrap()
            )
    );
}

#[test]
fn compare_lines_two_files_up_matches_their_levels_and_finds_a_click() {
    let folder = Folder::new("compare");
    fixtures(&folder.0);
    let take = run("audio.compare", &["tone.wav", "take.wav"], &folder.0).unwrap();
    assert_eq!(take["identical"], false);
    assert_eq!(take["offset_frames"], 441);
    assert_eq!(take["offset_ms"], 10.0);
    assert_eq!(take["compared"]["from"]["frame"], 0);
    assert_eq!(take["compared"]["to"]["frame"], 88_200 - 441);
    let gain = take["gain_db"].as_f64().unwrap();
    assert!((gain - 1.0).abs() < 0.05, "{gain}");
    let mid = take["bands"][1]["difference_db"].as_f64().unwrap();
    assert!(mid.abs() < 0.05, "{mid}");
    // The click is all that's left, so it's where what's left is loudest.
    assert_eq!(take["clicks"]["count"], 1, "{take:#}");
    let click = &take["clicks"]["found"][0];
    assert_eq!(click["at"]["frame"], 52_920 - 441);
    assert_eq!(click["in"], "b");
    assert!(click["spread"].as_f64().unwrap() > 0.9);
    let loudest = &take["loudest_residual"][0];
    assert!(loudest["from"]["frame"].as_u64().unwrap() <= 52_479);
    assert!(loudest["to"]["frame"].as_u64().unwrap() > 52_479);
    // The tone starts in both at the same time, once they're lined up.
    let onsets = &take["onsets"];
    assert!(onsets["matched"].as_u64().unwrap() >= 1, "{onsets:#}");
    assert!(onsets["median_lag_ms"].as_f64().unwrap().abs() < 0.5);

    let at = run(
        "audio.compare",
        &["tone.wav", "take.wav", "--at", "1.19", "--at", "0.7"],
        &folder.0,
    )
    .unwrap();
    let click = at["at"][0]["high_difference_db"].as_f64().unwrap();
    let tone = at["at"][1]["high_difference_db"].as_f64().unwrap();
    assert!(click > 15.0 && tone.abs() < 0.5, "{click} {tone}");

    // Unaligned and unmatched, B is 10 ms late and 1 dB down.
    let unaligned = run(
        "audio.compare",
        &["tone.wav", "take.wav", "--no-align", "--no-match-level"],
        &folder.0,
    )
    .unwrap();
    assert_eq!(unaligned["offset_frames"], 0);
    assert_eq!(unaligned["gain_db"], 0.0);
    let lag = unaligned["onsets"]["median_lag_ms"].as_f64().unwrap();
    assert!((lag - 10.0).abs() < 0.5, "{lag}");
    let loudness = unaligned["loudness_difference_lu"].as_f64().unwrap();
    assert!((loudness + 1.0).abs() < 0.1, "{loudness}");

    let same = run("audio.compare", &["tone.wav", "tone.wav"], &folder.0).unwrap();
    assert_eq!(same["identical"], true);
    assert_eq!(same["offset_frames"], 0);
    assert_eq!(same["correlation"], 1.0);
    assert_eq!(same["residual_dbfs"], Json::Null);
    assert_eq!(same["clicks"]["count"], 0);

    assert_eq!(
        run("audio.compare", &["tone.wav", "voice.aif"], &folder.0),
        Err(
            "A is at 44100 Hz and B at 48000 Hz: convert one to the other's rate with \
             audio.convert first."
                .to_owned()
        )
    );
    assert!(run("audio.compare", &["tone.wav"], &folder.0).is_err());
}

#[test]
fn compare_finds_onsets_that_come_early() {
    let folder = Folder::new("compare-early");
    // Hits every half second, and the same 3 ms early.
    let mut noise = Noise(0xC11C);
    let noise: Vec<f64> = (0..88_200 + 132).map(|_| noise.next()).collect();
    let hits = |early: usize| -> Vec<f64> {
        (0..88_200)
            .flat_map(|i| {
                let j = i + early;
                let level = match j < 22_050 {
                    true => 0.0,
                    false => 0.7 * (1.0 - (j % 22_050) as f64 / 4410.0).max(0.0),
                };
                let sample = level * noise[j];
                [sample, sample]
            })
            .collect()
    };
    write(
        &folder.path("a.wav"),
        SampleFormat::Float32,
        44_100,
        2,
        &hits(0),
    );
    write(
        &folder.path("b.wav"),
        SampleFormat::Float32,
        44_100,
        2,
        &hits(132),
    );
    let early = run(
        "audio.compare",
        &["a.wav", "b.wav", "--no-align"],
        &folder.0,
    )
    .unwrap();
    let onsets = &early["onsets"];
    assert_eq!(onsets["matched"], 3, "{onsets:#}");
    let lag = onsets["median_lag_ms"].as_f64().unwrap();
    assert!((lag + 3.0).abs() < 0.2, "{lag}");
    // Lined up, they're the same.
    let lined_up = run("audio.compare", &["a.wav", "b.wav"], &folder.0).unwrap();
    assert_eq!(lined_up["offset_frames"], -132);
    assert_eq!(lined_up["residual_dbfs"], Json::Null);
    assert_eq!(lined_up["onsets"]["median_lag_ms"], 0.0);
}

#[test]
fn generated_signals_measure_as_they_were_made() {
    let folder = Folder::new("generate");
    let generate = |words: &[&str]| run("audio.generate", words, &folder.0).unwrap();
    let spectrum = |words: &[&str]| run("audio.spectrum", words, &folder.0).unwrap();

    generate(&[
        "sine.wav",
        "--signal",
        "sine",
        "--level-dbfs",
        "-6",
        "--seconds",
        "2",
    ]);
    let sine = spectrum(&["sine.wav", "--peaks", "1"]);
    // A peak of -6.02 dBFS is an RMS level of -9.03.
    let rms = sine["rms_dbfs"].as_f64().unwrap();
    assert!((rms + 9.03).abs() < 0.02, "{rms}");
    let peak = &sine["peaks"][0];
    assert!(
        (peak["hz"].as_f64().unwrap() - 1000.0).abs() < 0.5,
        "{peak}"
    );
    assert!(
        (peak["dbfs"].as_f64().unwrap() + 9.03).abs() < 0.05,
        "{peak}"
    );
    assert_eq!(peak["note"], "B5");
    let band = (sine["bands"].as_array().unwrap().iter())
        .find(|band| band["centre_hz"] == 1000.0)
        .unwrap();
    assert!(
        (band["dbfs"].as_f64().unwrap() + 9.03).abs() < 0.05,
        "{band}"
    );
    assert!(sine["flatness"].as_f64().unwrap() < 0.01);

    // White noise rises 3 dB an octave, and pink is as loud in each.
    generate(&[
        "white.wav",
        "--signal",
        "noise",
        "--level-dbfs",
        "-20",
        "--seconds",
        "4",
    ]);
    generate(&[
        "pink.wav",
        "--signal",
        "pink",
        "--level-dbfs",
        "-20",
        "--seconds",
        "4",
    ]);
    let octave = |result: &Json, hz: f64| {
        (result["bands"].as_array().unwrap().iter())
            .find(|band| band["centre_hz"] == hz)
            .and_then(|band| band["dbfs"].as_f64())
            .unwrap()
    };
    let white = spectrum(&["white.wav", "--bands", "octaves"]);
    assert!((white["rms_dbfs"].as_f64().unwrap() + 20.0).abs() < 0.05);
    assert!(white["flatness"].as_f64().unwrap() > 0.5);
    let rise = octave(&white, 4000.0) - octave(&white, 250.0);
    assert!((rise - 12.0).abs() < 1.0, "{rise}");
    let pink = spectrum(&["pink.wav", "--bands", "octaves"]);
    let rise = octave(&pink, 4000.0) - octave(&pink, 250.0);
    assert!(rise.abs() < 1.0, "{rise}");

    // Clicks start on the beat, at their peak; an impulse is one sample.
    generate(&[
        "clicks.wav",
        "--signal",
        "clicks",
        "--bpm",
        "100",
        "--seconds",
        "2",
        "--channels",
        "1",
        "--level-dbfs",
        "0",
    ]);
    let clicks = samples(&folder.path("clicks.wav"));
    for beat in [0, 28_800, 57_600, 86_400] {
        assert_eq!(clicks[beat], 1.0, "{beat}");
        assert_eq!(clicks[beat + 480], 0.0, "{beat}: 10 ms on");
    }
    let impulse = generate(&[
        "impulse.flac",
        "--signal",
        "impulse",
        "--seconds",
        "0.1",
        "--level-dbfs",
        "-6",
    ]);
    assert_eq!(impulse["out"]["encoding"], "int24");
    let impulse = samples(&folder.path("impulse.flac"));
    assert!((impulse[0] - 0.5012).abs() < 0.0001 && impulse[1] == impulse[0]);
    assert!(impulse[2..].iter().all(|sample| *sample == 0.0));
    let sweep = generate(&["sweep.wav", "--signal", "sweep", "--from-hz", "100"]);
    assert!((sweep["out"]["peak_dbfs"].as_f64().unwrap() + 12.0).abs() < 0.01);

    assert!(generate_fails(
        &folder,
        &["x.wav", "--signal", "sine", "--hz", "30000"]
    ));
    assert!(generate_fails(
        &folder,
        &["x.flac", "--signal", "sine", "--encoding", "float32"]
    ));
}

fn generate_fails(folder: &Folder, words: &[&str]) -> bool {
    run("audio.generate", words, &folder.0).is_err()
}

#[test]
fn a_spectrum_follows_a_pitch_s_partials_and_hears_one_beat() {
    let folder = Folder::new("partials");
    // 220 Hz, a clean second partial, and a third split in two 2 Hz apart.
    let rate = 48_000.0;
    let note: Vec<f64> = (0..96_000)
        .map(|n| {
            let t = n as f64 / rate;
            0.4 * (TAU * 220.0 * t).sin()
                + 0.2 * (TAU * 440.0 * t).sin()
                + 0.1 * (TAU * 660.0 * t).sin()
                + 0.05 * (TAU * 662.0 * t).sin()
        })
        .collect();
    write(
        &folder.path("note.wav"),
        SampleFormat::Float32,
        48_000,
        1,
        &note,
    );
    let result = run(
        "audio.spectrum",
        &["note.wav", "--fundamental-hz", "220", "--partials", "32"],
        &folder.0,
    )
    .unwrap();
    let partials = result["partials"].as_array().unwrap();
    assert_eq!(partials.len(), 32);
    let field = |i: usize, name: &str| partials[i][name].as_f64().unwrap();
    assert!(field(0, "purity") > 0.99 && field(1, "purity") > 0.99);
    assert!(field(0, "beating_db") < 0.2 && field(1, "beating_db") < 0.2);
    let beating = field(2, "beating_db");
    assert!((7.0..10.0).contains(&beating), "{beating}");
    // The fourth is silent, so it has no purity or beating.
    assert_eq!(partials[3]["purity"], Json::Null);
    assert_eq!(partials[3]["beating_db"], Json::Null);
    let first = field(0, "dbfs");
    assert!((first + 10.97).abs() < 0.05, "{first}");
}

#[test]
fn match_lines_files_up_with_a_reference_at_one_loudness() {
    let folder = Folder::new("match");
    fixtures(&folder.0);
    let matched = run(
        "audio.match",
        &["tone.wav", "take.wav", "voice.aif", "--out-dir", "matched"],
        &folder.0,
    )
    .unwrap();
    let take = &matched["files"][0];
    assert_eq!(take["offset_frames"], 441);
    let target = matched["target_lufs"].as_f64().unwrap();
    assert_eq!(target, take["lufs"].as_f64().unwrap(), "the quieter");
    assert!(
        (matched["reference"]["gain_db"].as_f64().unwrap() + 1.0).abs() < 0.1,
        "{matched:#}"
    );
    let error = matched["files"][1]["error"].as_str().unwrap();
    assert!(
        error.contains("48000 Hz and the reference at 44100 Hz"),
        "{error}"
    );
    assert!(!folder.path("matched/voice.aif").exists());

    // Written again, they line up frame for frame, as loud as each other.
    let compared = run(
        "audio.compare",
        &["matched/tone.wav", "matched/take.wav", "--no-align"],
        &folder.0,
    )
    .unwrap();
    assert_eq!(compared["b_frames"], 88_200);
    let gain = compared["gain_db"].as_f64().unwrap();
    assert!(gain.abs() < 0.05, "{gain}");
    let lufs = compared["loudness_difference_lu"].as_f64().unwrap();
    assert!(lufs.abs() < 0.02, "{lufs}");
    let louder = run(
        "audio.match",
        &[
            "tone.wav",
            "take.wav",
            "--out-dir",
            "louder",
            "--loudness",
            "loudest",
        ],
        &folder.0,
    )
    .unwrap();
    assert_eq!(louder["reference"]["gain_db"], 0.0);
    let fixed = run(
        "audio.match",
        &[
            "tone.wav",
            "take.wav",
            "--out-dir",
            "fixed",
            "--to-lufs",
            "-20",
        ],
        &folder.0,
    )
    .unwrap();
    assert_eq!(fixed["target_lufs"], -20.0);
    assert_eq!(
        run(
            "audio.match",
            &["tone.wav", "tone.wav", "--out-dir", "same"],
            &folder.0
        ),
        Err(
            "Two files are called tone.wav, and out_dir would hold one: rename one first."
                .to_owned()
        )
    );
}
