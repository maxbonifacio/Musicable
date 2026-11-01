//! `musicable check FILE...`: runs scenarios, the acceptance checks the
//! testing strategy describes. A scenario is a TOML file: the project it
//! starts from, the commands to run, as agents run them, and what to
//! expect, a value one of the tools reads, or a render that nulls against
//! a golden, or meets its measures.
//!
//! Each scenario runs in a headless Musicable of its own, with settings of
//! its own, started from this program, and every step goes through the
//! same JSON path an agent's call takes: the tools, over the pipe. With
//! `--app`, the scenario plays into the Musicable that's open instead, a
//! step at a time with `--step`, so you can watch it do what it says.
//! `docs/agents.md` describes the format.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{BufRead, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use musicable_commands::json::{self as command_json, Results};
use musicable_io::audio::Decoder;
use musicable_mcp::client::Client;
use musicable_mcp::local::LocalTools;
use musicable_mcp::pipe::Address;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use crate::fixtures;

const USAGE: &str = "\
usage: musicable check [--app] [--step] [--bless] FILE...

Runs each scenario FILE in a headless Musicable of its own, through the tools
agents use, and says whether it held. A scenario is a TOML file: the project
it starts from, the commands to run, and what to expect. See docs/agents.md.

  --app     plays the scenarios into the Musicable that's open instead, which
            must let agents in
  --step    with --app, waits for Enter before each command
  --bless   writes each render whose golden is missing, or differs, as its
            golden, to listen to before it's committed";

/// How long a headless Musicable may take to start, and to quit.
const STARTING: Duration = Duration::from_secs(60);
const QUITTING: Duration = Duration::from_secs(15);
/// How long a scenario's work may take to land before its expectations
/// are checked, in seconds.
const SETTLING: u64 = 120;
/// What a render must null below unless the scenario says, in dBFS.
const NULL_BELOW: f64 = -120.0;
/// A value's tolerance with `near`, unless the scenario says.
const NEAR: f64 = 1e-6;
/// The measures' tolerances, as the testing strategy has them.
const LOUDNESS_WITHIN: f64 = 0.05;
const BANDS_WITHIN: f64 = 0.1;

/// A scenario, as its file has it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// The manual's promises it holds, such as `sidechains.muted-key`.
    #[serde(default)]
    pub covers: Vec<String>,
    /// What it's for, in a sentence, if it says.
    pub about: Option<String>,
    /// The project it starts from: a fixture's name, such as
    /// `kick-and-bass`, or a project file, by its path from the
    /// scenario's folder. A new project without it.
    pub project: Option<String>,
    /// The commands to run, in order.
    #[serde(default)]
    pub run: Vec<Step>,
    /// What to expect once they've run, and what they started has landed.
    #[serde(default)]
    pub expect: Vec<Expect>,
}

/// A command to run, as `run_command` runs it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The command's id, such as `device.add`.
    pub command: String,
    /// Its arguments, which can use what an earlier step gave back, as
    /// `"$comp"`.
    #[serde(default)]
    pub args: Map<String, Json>,
    /// A name for what it gives back, for the steps and expectations after
    /// it.
    #[serde(rename = "as")]
    pub named: Option<String>,
    /// Text the error must hold: the step must fail.
    pub fails: Option<String>,
}

/// Something to expect.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// A read tool to call: `get_project` without it.
    pub tool: Option<String>,
    /// Its arguments, which can use the steps' results.
    #[serde(default)]
    pub args: Map<String, Json>,
    /// Where in its answer the value is, as a path of fields and items,
    /// such as `tracks.1.muted`: the whole answer without it.
    pub at: Option<String>,
    /// The value it must be, which can be a step's result, as `"$bass"`.
    pub equals: Option<Json>,
    /// A number it must be near, within `within`.
    pub near: Option<f64>,
    pub within: Option<f64>,
    /// A render of the mix, over beats `from` to `to`, or the song without
    /// them, to null against `golden`.
    pub render: Option<Span>,
    /// The golden's file, by its path from the scenario's folder: a FLAC,
    /// WAV or AIFF file.
    pub golden: Option<String>,
    /// How the render must match its golden: `null`, every sample below
    /// `null_below_dbfs`, or `measures`, loudness within
    /// `loudness_within_lu` and third octaves within `bands_within_db`.
    pub compare: Option<String>,
    pub null_below_dbfs: Option<f64>,
    pub loudness_within_lu: Option<f64>,
    pub bands_within_db: Option<f64>,
}

/// A stretch of the song, in beats.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub from: f64,
    pub to: f64,
}

/// How `musicable check` was asked to run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Options {
    app: bool,
    step: bool,
    bless: bool,
}

/// `musicable check [--app] [--step] [--bless] FILE...`.
pub fn command_line(args: impl Iterator<Item = OsString>) -> ExitCode {
    let mut options = Options::default();
    let mut files = Vec::new();
    for arg in args {
        match arg.to_str() {
            Some("--app") => options.app = true,
            Some("--step") => options.step = true,
            Some("--bless") => options.bless = true,
            Some("-h" | "--help") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            Some(option) if option.starts_with('-') => {
                eprintln!("unknown option \"{option}\"\n{USAGE}");
                return ExitCode::FAILURE;
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if files.is_empty() {
        eprintln!("check needs a scenario file\n{USAGE}");
        return ExitCode::FAILURE;
    }
    if options.step && !options.app {
        eprintln!("--step is for --app, where you can watch each step\n{USAGE}");
        return ExitCode::FAILURE;
    }
    let mut failed = 0;
    for file in &files {
        let started = Instant::now();
        let outcome = check(file, options);
        let seconds = started.elapsed().as_secs_f64();
        match outcome {
            Ok(report) if report.problems.is_empty() => {
                println!(
                    "ok    {} ({seconds:.1} s){}",
                    file.display(),
                    report.notes()
                );
            }
            Ok(report) => {
                failed += 1;
                println!(
                    "FAIL  {} ({seconds:.1} s){}",
                    file.display(),
                    report.notes()
                );
                for problem in &report.problems {
                    println!("      {problem}");
                }
            }
            Err(why) => {
                failed += 1;
                println!("FAIL  {}\n      {why}", file.display());
            }
        }
    }
    println!("{} of {} scenarios held", files.len() - failed, files.len());
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// How a scenario went: what didn't hold, and what was noted, such as a
/// golden written.
#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub problems: Vec<String>,
    pub notes: Vec<String>,
}

impl Report {
    fn notes(&self) -> String {
        self.notes
            .iter()
            .map(|note| format!("\n      {note}"))
            .collect()
    }
}

/// Runs the scenario in `file` as `options` say, and says how it went, or
/// why it couldn't run.
fn check(file: &Path, options: Options) -> Result<Report, String> {
    let text = std::fs::read_to_string(file)
        .map_err(|error| format!("Couldn't read {}: {error}.", file.display()))?;
    let scenario: Scenario =
        toml::from_str(&text).map_err(|error| format!("It isn't a scenario: {error}"))?;
    let folder = file.parent().unwrap_or(Path::new(".")).to_owned();
    let scratch = Scratch::new()?;
    let project = match scenario.project.as_deref() {
        None => None,
        Some(name) => Some(match fixtures::fixture(name) {
            Some(fixture) => {
                let path = scratch
                    .0
                    .join(format!("{name}.{}", musicable_io::project::EXTENSION));
                musicable_io::project::save(&path, &fixture)
                    .map_err(|error| format!("Couldn't write the fixture {name}: {error}."))?;
                path
            }
            None => {
                let path = folder.join(name);
                if !path.is_file() {
                    let names: Vec<&str> = fixtures::FIXTURES.iter().map(|(n, _)| *n).collect();
                    return Err(format!(
                        "There's no fixture called {name}, and no project file {}: the \
                         fixtures are {}.",
                        path.display(),
                        names.join(", ")
                    ));
                }
                std::path::absolute(&path).unwrap_or(path)
            }
        }),
    };
    let mut musicable = match options.app {
        true => Musicable::open_app(project.as_deref())?,
        false => Musicable::headless(project.as_deref(), &scratch)?,
    };
    let report = run(&scenario, &musicable.client, &folder, &scratch, options);
    musicable.quit();
    Ok(report)
}

/// Runs `scenario`'s steps through `client`, then checks what it expects,
/// with renders in `scratch`, and goldens from `folder`.
fn run(
    scenario: &Scenario,
    client: &Client,
    folder: &Path,
    scratch: &Scratch,
    options: Options,
) -> Report {
    let mut report = Report::default();
    let mut results = Results::new();
    let stdin = std::io::stdin();
    for (index, step) in scenario.run.iter().enumerate() {
        let what = format!("Step {} ({})", index + 1, step.command);
        if options.step {
            print!("      {what}: press Enter to run it ");
            let _ = std::io::stdout().flush();
            let _ = stdin.lock().read_line(&mut String::new());
        }
        let args = match command_json::link(&step.args, &results) {
            Ok(args) => args,
            Err(why) => {
                report.problems.push(format!("{what}: {why}."));
                return report;
            }
        };
        let ran = client.call("run_command", json!({ "id": step.command, "args": args }));
        match (ran, &step.fails) {
            (Ok(answer), None) => {
                if let Some(named) = &step.named {
                    results.insert(named.clone(), answer["result"].clone());
                }
            }
            (Err(why), Some(expected)) if why.contains(expected.as_str()) => {}
            (Err(why), Some(expected)) => {
                report
                    .problems
                    .push(format!("{what} failed, but not with \"{expected}\": {why}"));
                return report;
            }
            (Err(why), None) => {
                report.problems.push(format!("{what} failed: {why}"));
                return report;
            }
            (Ok(answer), Some(expected)) => {
                report.problems.push(format!(
                    "{what} should have failed with \"{expected}\", and gave {answer}"
                ));
                return report;
            }
        }
    }
    // What the steps started, such as analysing or rendering, lands first.
    if let Err(why) = client.call("wait_for", json!({ "until": "idle", "timeout": SETTLING })) {
        report
            .problems
            .push(format!("The work the steps started didn't finish: {why}"));
        return report;
    }
    for (index, expect) in scenario.expect.iter().enumerate() {
        let what = format!("Expectation {}", index + 1);
        let held = match (&expect.render, &expect.golden) {
            (_, Some(golden)) => {
                rendered(expect, client, &folder.join(golden), scratch, options.bless)
            }
            (Some(_), None) => Err("A render needs its golden.".to_owned()),
            (None, None) => valued(expect, client, &results),
        };
        match held {
            Ok(Some(note)) => report.notes.push(format!("{what}: {note}")),
            Ok(None) => {}
            Err(why) => report.problems.push(format!("{what}: {why}")),
        }
    }
    report
}

/// Checks a value `expect` reads with a tool. Returns why it didn't hold.
fn valued(expect: &Expect, client: &Client, results: &Results) -> Result<Option<String>, String> {
    let tool = expect.tool.as_deref().unwrap_or("get_project");
    let args = command_json::link(&expect.args, results).map_err(|why| format!("{why}."))?;
    let answer = client.call(tool, Json::Object(args))?;
    let value = match &expect.at {
        Some(path) => part(&answer, path).ok_or_else(|| {
            format!(
                "{tool}'s answer has nothing at {path}: {}",
                shortened(&answer)
            )
        })?,
        None => &answer,
    };
    let at = expect.at.as_deref().unwrap_or(tool);
    match (&expect.equals, expect.near) {
        (Some(equals), None) => {
            let mut wrapped = Map::new();
            wrapped.insert("equals".into(), equals.clone());
            let equals = command_json::link(&wrapped, results).map_err(|why| format!("{why}."))?;
            let equals = &equals["equals"];
            if same(value, equals) {
                Ok(None)
            } else {
                Err(format!("{at} is {}, not {equals}", shortened(value)))
            }
        }
        (None, Some(near)) => {
            let within = expect.within.unwrap_or(NEAR);
            match value.as_f64() {
                Some(number) if (number - near).abs() <= within => Ok(None),
                Some(number) => Err(format!("{at} is {number}, not {near} ± {within}")),
                None => Err(format!("{at} is {}, not a number", shortened(value))),
            }
        }
        _ => Err("Say what to expect: equals, or near with within.".to_owned()),
    }
}

/// The part of `json` at `path`, fields and items by their names and
/// places, such as `tracks.1.muted`.
fn part<'a>(json: &'a Json, path: &str) -> Option<&'a Json> {
    path.split('.').try_fold(json, |json, part| match json {
        Json::Array(items) => items.get(part.parse::<usize>().ok()?),
        Json::Object(fields) => fields.get(part),
        _ => None,
    })
}

/// Whether two values are the same, numbers to within float noise, as
/// one read as 1 and another as 1.0 are.
fn same(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(a), Json::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(a), Some(b)) => (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0),
            _ => a == b,
        },
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same(a, b))
        }
        (Json::Object(a), Json::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, a)| b.get(k).is_some_and(|b| same(a, b)))
        }
        (a, b) => a == b,
    }
}

/// `json` as text, cut short if it's long.
fn shortened(json: &Json) -> String {
    let text = json.to_string();
    match text.char_indices().nth(300) {
        Some((at, _)) => format!("{}...", &text[..at]),
        None => text,
    }
}

/// Renders the mix as `expect` says, and checks it against `golden`,
/// writing it as the golden instead if `bless` says to and it's missing or
/// differs. Returns a note on what was written, or why it didn't hold.
fn rendered(
    expect: &Expect,
    client: &Client,
    golden: &Path,
    scratch: &Scratch,
    bless: bool,
) -> Result<Option<String>, String> {
    let extension = (golden.extension().and_then(|e| e.to_str()))
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    // Exact to −144 dBFS, as goldens are kept, or floats in a WAV file.
    let bits = match extension.as_str() {
        "flac" | "aif" | "aiff" => 24,
        "wav" => 32,
        _ => {
            return Err(format!(
                "{} isn't a FLAC, WAV or AIFF file.",
                golden.display()
            ));
        }
    };
    let render = scratch
        .0
        .join(format!("render-{}.{extension}", scratch.next()));
    let mut args = json!({
        "file": render,
        "bits": bits,
        "dither": false,
        "normalize": false,
        "tail": false,
        "range": "song",
    });
    if let Some(span) = expect.render {
        let project = client.call("get_project", json!({}))?;
        let tracks: Vec<Json> = (project["tracks"].as_array().into_iter().flatten())
            .map(|track| track["id"].clone())
            .collect();
        let select =
            json!({ "start": span.from, "end": span.to, "tracks": tracks, "master": true });
        client.call(
            "run_command",
            json!({ "id": "edit.select_time", "args": select }),
        )?;
        args["range"] = "selection".into();
    }
    let exported = client.call(
        "run_command",
        json!({ "id": "file.export_audio", "args": args }),
    );
    exported.map_err(|why| format!("The render failed: {why}"))?;
    if !golden.is_file() {
        if !bless {
            return Err(format!(
                "There's no golden {}: --bless writes the render there, to listen to.",
                golden.display()
            ));
        }
        return bless_golden(&render, golden).map(|()| Some(format!("wrote {}", golden.display())));
    }
    let held = match expect.compare.as_deref().unwrap_or("null") {
        "null" => nulls(
            &render,
            golden,
            expect.null_below_dbfs.unwrap_or(NULL_BELOW),
        ),
        "measures" => measures(&render, golden, expect),
        other => Err(format!("compare is null or measures, not {other}.")),
    };
    match held {
        Err(why) if bless => bless_golden(&render, golden)
            .map(|()| Some(format!("wrote {}, which differed: {why}", golden.display()))),
        held => held.map(|()| None),
    }
}

/// Writes `render` as the golden `golden`.
fn bless_golden(render: &Path, golden: &Path) -> Result<(), String> {
    if let Some(folder) = golden.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("Couldn't make {}: {error}.", folder.display()))?;
    }
    std::fs::copy(render, golden)
        .map(|_| ())
        .map_err(|error| format!("Couldn't write {}: {error}.", golden.display()))
}

/// Whether `render` nulls against `golden`: what's left when one is taken
/// from the other stays below `below` dBFS at every sample.
fn nulls(render: &Path, golden: &Path, below: f64) -> Result<(), String> {
    let (a, a_info) = samples(golden)?;
    let (b, b_info) = samples(render)?;
    if (a_info.sample_rate, a_info.channels) != (b_info.sample_rate, b_info.channels) {
        return Err(format!(
            "The render is {} Hz with {} channels, and the golden {} Hz with {}.",
            b_info.sample_rate, b_info.channels, a_info.sample_rate, a_info.channels
        ));
    }
    if a_info.frames != b_info.frames {
        return Err(format!(
            "The render is {} frames long, and the golden {}.",
            b_info.frames, a_info.frames
        ));
    }
    let (at, peak) = (a.iter().zip(&b).map(|(a, b)| (b - a).abs()).enumerate()).fold(
        (0, 0.0_f64),
        |most, (at, left)| if left > most.1 { (at, left) } else { most },
    );
    let dbfs = 20.0 * peak.log10();
    if peak == 0.0 || dbfs < below {
        return Ok(());
    }
    let seconds = (at / a_info.channels) as f64 / f64::from(a_info.sample_rate);
    Err(format!(
        "The render doesn't null against the golden: what's left peaks at {dbfs:.1} dBFS, \
         {seconds:.3} s in, over {below} dBFS."
    ))
}

/// A file's samples, interleaved, exactly as it has them, and what it is.
fn samples(path: &Path) -> Result<(Vec<f64>, musicable_io::audio::Info), String> {
    let read = || -> Result<_, musicable_io::audio::AudioError> {
        let mut decoder = Decoder::open(path)?;
        let info = decoder.info();
        let mut samples = vec![0.0; info.frames as usize * info.channels];
        let mut done = 0;
        while done < samples.len() {
            let frames =
                decoder.read_f64(done as u64 / info.channels as u64, &mut samples[done..])?;
            if frames == 0 {
                break;
            }
            done += frames * info.channels;
        }
        samples.truncate(done);
        Ok((samples, info))
    };
    read().map_err(|error| format!("Couldn't read {}: {error}.", path.display()))
}

/// Whether `render` meets its measures against `golden`, as audio_compare
/// measures them: loudness, and the levels in third octaves.
fn measures(render: &Path, golden: &Path, expect: &Expect) -> Result<(), String> {
    let mut arguments = Map::new();
    arguments.insert("a".into(), json!(golden));
    arguments.insert("b".into(), json!(render));
    arguments.insert("align".into(), json!(false));
    arguments.insert("match_level".into(), json!(false));
    let compared = crate::toolkit::Tools.call("audio_compare", &arguments, &|| false, &|_| {})?;
    let mut problems = String::new();
    let loudness = expect.loudness_within_lu.unwrap_or(LOUDNESS_WITHIN);
    if let Some(difference) = compared["loudness_difference_lu"].as_f64()
        && difference.abs() > loudness
    {
        let _ = write!(
            problems,
            " Its loudness is {difference:+.3} LU off, over {loudness}."
        );
    }
    let bands = expect.bands_within_db.unwrap_or(BANDS_WITHIN);
    let off: Vec<String> = (compared["third_octaves"].as_array().into_iter().flatten())
        .filter_map(|third| {
            let difference = third["difference_db"].as_f64()?;
            (difference.abs() > bands)
                .then(|| format!("{} Hz {difference:+.2} dB", third["centre_hz"]))
        })
        .collect();
    match off.len() {
        0 => {}
        1..=3 => {
            let _ = write!(
                problems,
                " Third octaves are off by more than {bands} dB: {}.",
                off.join(", ")
            );
        }
        more => {
            let _ = write!(
                problems,
                " Third octaves are off by more than {bands} dB: {}, and {} more.",
                off[..3].join(", "),
                more - 3
            );
        }
    }
    match problems.is_empty() {
        true => Ok(()),
        false => Err(format!("The render doesn't meet its measures:{problems}")),
    }
}

/// A folder of the run's own, for the fixtures, renders and the headless
/// Musicable's settings, removed once it's done.
struct Scratch(PathBuf, std::cell::Cell<u32>);

impl Scratch {
    fn new() -> Result<Self, String> {
        static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let run = RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let folder =
            std::env::temp_dir().join(format!("musicable-check-{}-{run}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder)
            .map_err(|error| format!("Couldn't make {}: {error}.", folder.display()))?;
        Ok(Self(folder, std::cell::Cell::new(0)))
    }

    /// A number for the next file in it.
    fn next(&self) -> u32 {
        self.1.replace(self.1.get() + 1)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The Musicable a scenario runs in, and the connection to it.
struct Musicable {
    client: Client,
    /// The headless Musicable started for it, which quits after it.
    child: Option<Child>,
}

impl Musicable {
    /// Starts a headless Musicable with `project` open, with settings of
    /// its own in `scratch`, playing through no audio device, and connects
    /// to it.
    fn headless(project: Option<&Path>, scratch: &Scratch) -> Result<Self, String> {
        let program = std::env::current_exe()
            .map_err(|error| format!("Couldn't find this program: {error}."))?;
        let config = scratch.0.join("settings");
        std::fs::create_dir_all(&config)
            .map_err(|error| format!("Couldn't make {}: {error}.", config.display()))?;
        // What the last plugin scan found, so the scan only checks it.
        if let Some(own) = crate::config_dir() {
            let _ = std::fs::copy(
                own.join(crate::PLUGINS_FILE),
                config.join(crate::PLUGINS_FILE),
            );
        }
        let mut command = Command::new(&program);
        command.args(["--headless", "--no-audio"]);
        if let Some(project) = project {
            command.arg(project);
        }
        let mut child = command
            .env("MUSICABLE_CONFIG_DIR", &config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Couldn't start {}: {error}.", program.display()))?;
        let address = Address::new(&config).map_err(|error| error.to_string())?;
        let log = config.join(crate::log_file::LOG_FILE);
        let gone = |child: &mut Child| {
            let status = child.try_wait().ok().flatten()?;
            let said = std::fs::read_to_string(&log).unwrap_or_default();
            Some(format!(
                "Headless Musicable stopped as it started ({status}):\n{said}"
            ))
        };
        match Client::connect_within(&address, "musicable-check", STARTING, || gone(&mut child)) {
            Ok(client) => Ok(Self {
                client,
                child: Some(child),
            }),
            Err(why) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(why)
            }
        }
    }

    /// Connects to the Musicable that's open, and opens `project` in it,
    /// or a new project.
    fn open_app(project: Option<&Path>) -> Result<Self, String> {
        let dir =
            crate::config_dir().ok_or("The system doesn't say where Musicable's settings are.")?;
        let address = Address::new(&dir).map_err(|error| error.to_string())?;
        let client = Client::connect(&address, "musicable-check").map_err(|_| {
            "Musicable isn't open, or doesn't let agents in: switch on \"Let agents control \
             Musicable\" on the Agents page of its Settings window."
                .to_owned()
        })?;
        let (id, args) = match project {
            Some(project) => ("file.open", json!({ "file": project })),
            None => ("file.new", json!({})),
        };
        // The window asks whether to save the changes first.
        if client.call("get_project", json!({}))?["modified"] == true {
            if client.call("get_status", json!({}))?["headless"] == true {
                return Err(
                    "The headless Musicable has unsaved changes, and no one to ask \
                            whether to save them: answer_question answers, or run the \
                            scenario without --app."
                        .to_owned(),
                );
            }
            println!("      Musicable asks whether to save its changes first: answer it there.");
        }
        client.call("run_command", json!({ "id": id, "args": args }))?;
        Ok(Self {
            client,
            child: None,
        })
    }

    /// Quits the headless Musicable started for the scenario, if one was,
    /// letting its changes go.
    fn quit(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = self
            .client
            .call("quit_app", json!({ "unsaved": "discard" }));
        let started = Instant::now();
        while started.elapsed() < QUITTING {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Musicable {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The scenarios under `folder`, and its folders, by their paths, in
/// order: the `.toml` files.
pub fn scenarios(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut folders = vec![folder.to_owned()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|e| e == "toml") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scenario_reads_from_toml() {
        let scenario: Scenario = toml::from_str(
            r#"
            covers = ["sidechains.muted-key"]
            project = "kick-and-bass"

            [[run]]
            command = "device.add"
            args = { kind = "compressor", track = 2 }
            as = "comp"

            [[run]]
            command = "mixer.set_mute"
            args = { track = 1, muted = true }
            fails = "no track"

            [[expect]]
            at = "tracks.1.muted"
            equals = true

            [[expect]]
            render = { from = 0, to = 8 }
            golden = "a-muted-kick-still-keys.flac"
            "#,
        )
        .unwrap();
        assert_eq!(scenario.covers, ["sidechains.muted-key"]);
        assert_eq!(scenario.run[0].named.as_deref(), Some("comp"));
        assert_eq!(scenario.run[0].args["kind"], "compressor");
        assert_eq!(scenario.run[1].fails.as_deref(), Some("no track"));
        assert_eq!(scenario.expect[0].equals, Some(json!(true)));
        assert_eq!(scenario.expect[1].render, Some(Span { from: 0.0, to: 8.0 }));
        // A misspelt key is an error, not a step that does nothing.
        let wrong = toml::from_str::<Scenario>("[[run]]\ncomand = \"track.new_midi_track\"");
        assert!(wrong.is_err());
    }

    #[test]
    fn values_are_found_by_their_paths_and_compared() {
        let answer = json!({"tracks": [{"id": 0, "name": "Kick"}, {"id": 1, "gain": 0.5}]});
        assert_eq!(part(&answer, "tracks.1.gain"), Some(&json!(0.5)));
        assert_eq!(part(&answer, "tracks.0.name"), Some(&json!("Kick")));
        assert_eq!(part(&answer, "tracks.2"), None);
        assert_eq!(part(&answer, "tracks.x"), None);
        assert!(same(&json!(1), &json!(1.0)));
        assert!(same(&json!([1, {"a": 2.0}]), &json!([1.0, {"a": 2}])));
        assert!(!same(&json!([1]), &json!([1, 2])));
        assert!(!same(&json!(0.5), &json!(0.5001)));
    }
}
