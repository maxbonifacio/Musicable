// Release builds on Windows open no console window. The commands that print
// attach to the console they were started from, if any.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui;
use musicable_app::export::{Bits, Export, ExportSettings, FileFormat, Job, Span};
use musicable_app::null_test::{self, Player, Render};
use musicable_app::{App, PLUGINS_FILE};
use musicable_commands::{Editor, Keymap, KeymapFile, Registry, Scope};
use musicable_engine::{EngineConfig, ParallelOptions};
use musicable_model::{Project, TrackId};
use musicable_plugins::scan::ScanCache;

// Debug builds catch heap use on the audio thread (see docs/engine.md).
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;

const USAGE: &str = "\
usage: musicable [PROJECT]                open the app, with PROJECT if given
       musicable keymap [FILE]            print the keys, with the changes in the keymap FILE
       musicable null-test [PROJECT]      check that the parallel scheduler plays PROJECT, or
                                          the demo, exactly as the reference renderer does
       musicable export PROJECT FILE      render PROJECT to the audio file FILE, as File >
                                          Export Audio does
       musicable scan-plugin LIBRARY      list the plugins in the plugin library LIBRARY, as
                                          the app's plugin scan does in a process of its own
       musicable mcp                      let the agent whose client starts it control the
                                          Musicable that's running, over MCP on the standard
                                          input and output: see docs/agents.md
       musicable api                      print the commands agents can run, with their
                                          arguments' JSON Schema, as JSON
       musicable analyse FILE...          analyse audio files as warping does, scoring their
                                          beats against labels: see analyse --help";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    match first.as_ref().and_then(|arg| arg.to_str()) {
        Some("keymap") => {
            attach_console();
            print_keymap(&musicable_app::commands(), args.next())
        }
        Some("null-test") => {
            attach_console();
            null_test(args)
        }
        Some("export") => {
            attach_console();
            export(args)
        }
        // The app runs it, and reads what it prints through a pipe, so it
        // doesn't attach to a console.
        Some(musicable_app::SCAN_PLUGIN) => scan_plugin(args.next()),
        // An agent's client runs it, and speaks MCP with it through pipes,
        // so it doesn't either.
        Some("mcp") => mcp(),
        Some("api") => {
            attach_console();
            print_api()
        }
        Some("analyse") => {
            attach_console();
            analyse(args)
        }
        Some("-h" | "--help") => {
            attach_console();
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(option) if option.starts_with('-') => {
            attach_console();
            eprintln!("unknown option \"{option}\"\n{USAGE}");
            ExitCode::FAILURE
        }
        _ => run(first.map(PathBuf::from)),
    }
}

/// Opens the app's window, with the project at `path` if given, and returns
/// once it closes.
fn run(path: Option<PathBuf>) -> ExitCode {
    if std::env::var_os(musicable_app::FRAME_TIMES).is_some() {
        attach_console();
    }
    musicable_app::log_file::start(musicable_app::config_dir().as_deref());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Musicable")
            .with_app_id("musicable")
            .with_inner_size([1100.0, 640.0])
            .with_min_inner_size([760.0, 360.0])
            // The system's default icon rather than egui's logo, until
            // Musicable has its own.
            .with_icon(egui::IconData::default()),
        // Ctrl+C, X and V go to the keymap like other keys. Text fields
        // still get them: see `musicable_ui::KeyPresses`.
        clipboard_shortcuts: false,
        wgpu_options: wgpu_options(),
        ..eframe::NativeOptions::default()
    };
    let result = eframe::run_native(
        "Musicable",
        options,
        Box::new(|cc| Ok(Box::new(App::new(path, cc)))),
    );
    match result {
        Ok(()) => {
            log::info!("Musicable closed");
            ExitCode::SUCCESS
        }
        Err(error) => {
            let message = format!("Musicable couldn't open its window: {error}");
            attach_console();
            log::error!("{message}");
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Musicable")
                .set_description(message)
                .show();
            ExitCode::FAILURE
        }
    }
}

/// How eframe draws with wgpu: as it would, but with Direct3D 12 on Windows,
/// unless the `WGPU_BACKEND` environment variable picks another backend,
/// such as `vulkan`. Vulkan lost the graphics device on an NVIDIA card as
/// the displays woke, which egui-wgpu can't carry on from. A window's
/// surface lost, as may happen then too, is logged.
fn wgpu_options() -> eframe::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuSetup, WgpuSetupCreateNew};
    use eframe::wgpu::{Backends, CurrentSurfaceTexture};
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    if cfg!(windows) && Backends::from_env().is_none() {
        setup.instance_descriptor.backends = Backends::DX12;
    }
    let options = eframe::WgpuConfiguration::default();
    let on_surface_status = Arc::clone(&options.on_surface_status);
    eframe::WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        on_surface_status: Arc::new(move |status| {
            if matches!(status, CurrentSurfaceTexture::Lost) {
                log::info!("A window's surface was lost, so it's made again");
            }
            on_surface_status(status)
        }),
        ..options
    }
}

/// `musicable scan-plugin LIBRARY`: lists the plugins in the plugin library
/// at `LIBRARY`, for the app's plugin scan, which runs it in a process of
/// its own so a plugin that crashes or hangs takes only this down.
fn scan_plugin(library: Option<OsString>) -> ExitCode {
    let Some(library) = library else {
        attach_console();
        eprintln!("scan-plugin needs the plugin library to scan\n{USAGE}");
        return ExitCode::FAILURE;
    };
    musicable_plugins::scan::child(Path::new(&library));
    ExitCode::SUCCESS
}

/// `musicable mcp`: lets the agent whose client started it control the
/// Musicable that's running, over MCP on the standard input and output,
/// until the client goes. See `docs/agents.md`.
fn mcp() -> ExitCode {
    match musicable_mcp::bridge::run(musicable_app::config_dir().as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("musicable mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `musicable api`: prints the commands agents can run, with their titles,
/// their arguments' JSON Schema and their default keys, as JSON, without
/// opening the app.
fn print_api() -> ExitCode {
    let commands = musicable_app::commands();
    let catalogue = musicable_mcp::catalogue(&commands, &Keymap::default_keymap());
    match serde_json::to_string_pretty(&catalogue) {
        Ok(text) => {
            // A reader that stops early, such as `head`, isn't a failure.
            let _ = writeln!(std::io::stdout().lock(), "{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("musicable api: {error}");
            ExitCode::FAILURE
        }
    }
}

/// `musicable keymap [FILE]`: prints the key bindings of the default keymap,
/// with the changes in the keymap file `FILE`, if given.
fn print_keymap(commands: &Registry, file: Option<OsString>) -> ExitCode {
    let mut keymap = Keymap::default_keymap();
    if let Some(path) = file {
        let path = Path::new(&path);
        match KeymapFile::read(path) {
            Ok((changes, problems)) => {
                for problem in problems {
                    eprintln!("{}: {problem}", path.display());
                }
                changes.apply_to(&mut keymap);
            }
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    // Keys that work in one panel only say which.
    let place = |scope| match scope {
        Scope::Everywhere => String::new(),
        Scope::Panel(panel) => format!(" in {panel}"),
    };
    for command in commands.iter() {
        let chords: Vec<_> = (keymap.bindings_for(&command.id))
            .map(|(scope, chord)| format!("{chord}{}", place(scope)))
            .collect();
        let chords = if chords.is_empty() {
            "(none)".to_owned()
        } else {
            chords.join(", ")
        };
        println!("{:<28} {:<18} {chords}", command.id, command.title);
    }
    for (scope, chord, id) in keymap.dangling(commands) {
        eprintln!("{chord}{} runs {id}, which isn't a command", place(scope));
    }
    ExitCode::SUCCESS
}

const NULL_TEST_USAGE: &str = "\
usage: musicable null-test [PROJECT] [options]

Plays PROJECT, or the Tier 0 demo, from the start of the song with the
parallel scheduler and with the reference renderer side by side, offline,
and checks they play the same, sample for sample. Then it saves the project,
opens it again, and checks it still plays the same.

  --seconds <s>       how long to play (default: 20)
  --rate <hz>         the sample rate (default: 48000)
  --buffer <frames>   the buffer size, which the engine renders in
                      (default: 128)";

/// What `musicable null-test` plays.
struct NullTest {
    project: Option<PathBuf>,
    seconds: f64,
    rate: u32,
    buffer: usize,
}

impl NullTest {
    /// The options in `args`, or `None` if they ask for help.
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Self>, String> {
        let mut test = Self {
            project: None,
            seconds: 20.0,
            rate: 48_000,
            buffer: 128,
        };
        while let Some(arg) = args.next() {
            let Some(option) = arg.to_str().filter(|arg| arg.starts_with('-')) else {
                if test.project.replace(PathBuf::from(&arg)).is_some() {
                    return Err("null-test takes one project".to_owned());
                }
                continue;
            };
            let mut value = || {
                args.next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| format!("{option} needs a number"))
            };
            match option {
                "-h" | "--help" => return Ok(None),
                "--seconds" => {
                    test.seconds = number(option, &value()?)?;
                    if !(test.seconds > 0.0 && test.seconds <= 3600.0) {
                        return Err(format!("{option} is more than 0 and at most 3600"));
                    }
                }
                "--rate" => test.rate = number(option, &value()?)?,
                "--buffer" => test.buffer = number(option, &value()?)?,
                _ => return Err(format!("unknown option \"{option}\"")),
            }
        }
        if test.rate == 0 || !(1..=65_536).contains(&test.buffer) {
            return Err("the rate is more than 0, and the buffer from 1 to 65536".to_owned());
        }
        Ok(Some(test))
    }
}

fn number<T: std::str::FromStr>(option: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{option} takes a number, not \"{text}\""))
}

/// `musicable null-test`: see [`NULL_TEST_USAGE`].
fn null_test(args: impl Iterator<Item = OsString>) -> ExitCode {
    let test = match NullTest::parse(args) {
        Ok(Some(test)) => test,
        Ok(None) => {
            println!("{NULL_TEST_USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{NULL_TEST_USAGE}");
            return ExitCode::FAILURE;
        }
    };
    // Audio clips' files are in the project's folder.
    let folder = test.project.as_deref().and_then(Path::parent);
    let (project, name) = match &test.project {
        Some(path) => match musicable_io::project::load(path) {
            Ok(project) => (project, path.display().to_string()),
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => (musicable_app::demo_project(), "the demo".to_owned()),
    };
    // Saved and opened again, as File > Save and File > Open would, without
    // the file.
    let reopened = musicable_io::project::to_string(&project)
        .map_err(|error| error.to_string())
        .and_then(|text| musicable_io::project::from_str(&text).map_err(|e| e.to_string()));
    let reopened = match reopened {
        Ok(reopened) => reopened,
        Err(error) => {
            eprintln!("Saving it and opening it again failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    let config = EngineConfig {
        sample_rate: test.rate,
        max_block: test.buffer,
    };
    let frames = (test.seconds * f64::from(test.rate)).round().max(1.0) as u64;
    let tracks = match project.tracks.len() {
        1 => "1 track".to_owned(),
        n => format!("{n} tracks"),
    };
    println!(
        "Null test of {name}: {tracks}, {} s at {} Hz in {}-frame buffers\n",
        test.seconds, test.rate, test.buffer,
    );
    let shared = Render::Parallel(ParallelOptions {
        inline_below: None,
        ..ParallelOptions::default()
    });
    let same_project = reopened == project;
    let runs: [(&str, Project, Render); 4] = [
        ("Reference renderer", project.clone(), Render::Reference),
        ("Parallel, as the app plays", project.clone(), Render::App),
        ("Parallel, every block shared", project, shared),
        ("Saved and opened again", reopened, Render::App),
    ];
    let names = runs.each_ref().map(|run| run.0);
    let mut players: Vec<Player> = runs
        .into_iter()
        .map(|(_, project, render)| Player::new(project, folder, &render, config))
        .collect();
    let differences = null_test::compare(&mut players, frames);
    // Every player reads the same files.
    let problems = players[0].take_problems();
    for problem in &problems {
        println!("{problem}");
    }
    if !problems.is_empty() {
        println!();
    }

    let speed = |player: &Player| {
        let took = player.took().as_secs_f64();
        let played = frames as f64 / f64::from(test.rate);
        format!("{took:.2} s, {:.0}x real time", played / took.max(1e-9))
    };
    let mut passed = true;
    for ((name, player), difference) in names.iter().zip(&players).zip(&differences) {
        let outcome = match (difference, player.parallel()) {
            // The reference.
            (None, None) => format!("{}, {}", level(player.peak()), speed(player)),
            (Some(d), _) => {
                passed = false;
                let channel = ["left", "right"][d.channel];
                format!(
                    "DIFFERENT at {:.3} s (frame {}), {channel} channel: {} instead of {}",
                    d.frame as f64 / f64::from(test.rate),
                    d.frame,
                    d.got,
                    d.expected,
                )
            }
            (None, Some(stats)) => {
                let blocks = stats.parallel_blocks + stats.inline_blocks;
                let mut outcome = format!(
                    "identical, {}; {} of {blocks} blocks shared, {} workers",
                    speed(player),
                    stats.parallel_blocks,
                    stats.threads,
                );
                if stats.parallel_blocks > 0 {
                    // While blocks were shared, counting the audio thread.
                    outcome += &format!(", {:.1} threads busy", stats.cores_used());
                }
                outcome
            }
        };
        println!("{name:<30} {outcome}");
    }
    if !same_project {
        passed = false;
        println!("\nSaved and opened again, the project wasn't the same.");
    }
    if players[0].peak() == 0.0 {
        println!("\nIt was silent all the way, so the test shows little.");
    }
    if passed {
        println!(
            "\nPassed: the parallel scheduler played it exactly as the reference renderer \
             did, before and after saving."
        );
        ExitCode::SUCCESS
    } else {
        println!("\nFailed.");
        ExitCode::FAILURE
    }
}

const EXPORT_USAGE: &str = "\
usage: musicable export PROJECT FILE [options]

Renders PROJECT to the audio file FILE as File > Export Audio does, on a
thread of its own, faster than real time: the song, from its start to the
end of its last clip, with the plugins the app's last plugin scan found.

  --loop              the loop region instead of the song
  --format <format>   wav, aiff or flac (default: FILE's extension, or wav)
  --bits <bits>       16, 24, or 32 for floats (default: 24)
  --rate <hz>         the sample rate the song renders at (default: 48000)
  --block <frames>    the most frames plugins get at once (default: 512)
  --stems             each track on its own too, in files next to FILE
  --no-mix            the stems without the mix
  --normalize         raise or lower each file so its peak is at full scale
  --no-dither         round integer samples without dither
  --no-tail           stop at the end, without the tail";

/// What `musicable export` renders, and how.
struct ExportArgs {
    project: PathBuf,
    file: PathBuf,
    span: Span,
    settings: ExportSettings,
    config: EngineConfig,
}

impl ExportArgs {
    /// The options in `args`, or `None` if they ask for help.
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Self>, String> {
        let mut paths = Vec::new();
        let mut span = Span::Song;
        let mut settings = ExportSettings::default();
        let mut format = None;
        let mut config = EngineConfig {
            sample_rate: 48_000,
            max_block: 512,
        };
        while let Some(arg) = args.next() {
            let Some(option) = arg.to_str().filter(|arg| arg.starts_with('-')) else {
                paths.push(PathBuf::from(arg));
                continue;
            };
            let mut value = || {
                args.next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| format!("{option} needs a value"))
            };
            match option {
                "-h" | "--help" => return Ok(None),
                "--loop" => span = Span::Loop,
                "--format" => {
                    let name = value()?;
                    let found = FileFormat::ALL
                        .into_iter()
                        .find(|f| f.name().eq_ignore_ascii_case(&name));
                    format = Some(found.ok_or(format!("there's no format \"{name}\""))?);
                }
                "--bits" => settings.bits = Bits::try_from(number::<u8>(option, &value()?)?)?,
                "--rate" => config.sample_rate = number(option, &value()?)?,
                "--block" => config.max_block = number(option, &value()?)?,
                "--stems" => settings.stems = true,
                "--no-mix" => settings.mixdown = false,
                "--normalize" => settings.normalize = true,
                "--no-dither" => settings.dither = false,
                "--no-tail" => settings.tail = false,
                _ => return Err(format!("unknown option \"{option}\"")),
            }
        }
        let [project, file] = <[PathBuf; 2]>::try_from(paths)
            .map_err(|_| "export takes a project and a file to write".to_owned())?;
        if config.sample_rate == 0 || !(1..=65_536).contains(&config.max_block) {
            return Err("the rate is more than 0, and the block from 1 to 65536".to_owned());
        }
        // The file's extension says, if it's one of the formats'.
        let extension = file.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        settings.format = format.unwrap_or_else(|| {
            let known = |f: &FileFormat| {
                let mut extensions = f.extensions().iter();
                extensions.any(|known| known.eq_ignore_ascii_case(extension))
            };
            FileFormat::ALL
                .into_iter()
                .find(known)
                .unwrap_or(FileFormat::Wav)
        });
        Ok(Some(Self {
            project,
            file,
            span,
            settings,
            config,
        }))
    }
}

/// `musicable export`: see [`EXPORT_USAGE`].
fn export(args: impl Iterator<Item = OsString>) -> ExitCode {
    let args = match ExportArgs::parse(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{EXPORT_USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{EXPORT_USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let project = match musicable_io::project::load(&args.project) {
        Ok(project) => project,
        Err(error) => {
            eprintln!("{}: {error}", args.project.display());
            return ExitCode::FAILURE;
        }
    };
    let Some(range) = args.span.range(&project, &Editor::default()) else {
        eprintln!(
            "There's nothing to export: the {} is empty.",
            args.span.name().to_lowercase()
        );
        return ExitCode::FAILURE;
    };
    let installed = musicable_app::config_dir()
        .map(|dir| dir.join(PLUGINS_FILE))
        .filter(|file| file.exists())
        .map_or(Vec::new(), |file| {
            ScanCache::read(&file).plugins().cloned().collect()
        });
    if let Some(folder) = args
        .file
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        && let Err(error) = std::fs::create_dir_all(folder)
    {
        eprintln!("Couldn't make the folder {}: {error}", folder.display());
        return ExitCode::FAILURE;
    }
    let tracks: BTreeSet<TrackId> = project.tracks.iter().map(|track| track.id).collect();
    let settings = args.settings;
    let (mixdown, stems) =
        musicable_app::export::files_for(&args.file, &project, &settings, &tracks);
    let tempo = &project.tempo;
    println!(
        "Exporting {} from {} to {} at {} Hz",
        args.project.display(),
        musicable_ui::position(tempo, range.start),
        musicable_ui::position(tempo, range.end),
        args.config.sample_rate,
    );
    let job = Job {
        project: project.clone(),
        folder: args.project.parent().map(Path::to_owned),
        range,
        settings,
        config: args.config,
        installed,
        waker: None,
        mixdown,
        stems,
    };
    let length = job.length();
    let mut export = match Export::start(job) {
        Ok(export) => export,
        Err(error) => {
            eprintln!("{}", capital(&error));
            return ExitCode::FAILURE;
        }
    };
    // Plugins that need the main thread get it here, every millisecond.
    while !export.is_finished() {
        export.tick();
        std::thread::sleep(Duration::from_millis(1));
    }
    let report = export.report().expect("it finished");
    for problem in &report.problems {
        println!("{problem}");
    }
    for file in &report.files {
        let mut line = format!("Wrote {}: {}", file.path.display(), level(file.peak));
        if let Some(gain) = file.gain {
            line += &format!(", normalised by {:+.1} dB", 20.0 * gain.log10());
        }
        if file.over > 0 {
            line += &format!(", {} samples over full scale", file.over);
        }
        println!("{line}");
    }
    if let Some(failed) = &report.failed {
        eprintln!("The export stopped: {failed}.");
        return ExitCode::FAILURE;
    }
    let seconds = length as f64 / f64::from(args.config.sample_rate);
    let took = report.took.as_secs_f64();
    println!(
        "{seconds:.1} s of audio in {took:.2} s: {:.0}x real time.",
        seconds / took.max(1e-9)
    );
    ExitCode::SUCCESS
}

const ANALYSE_USAGE: &str = "\
usage: musicable analyse [options] FILE...

Analyses each audio file as warping does, with the Beat This! network and
the classic tracker that checks it, and prints what it found: the tempo,
whether it's steady, whether the file has a pulse or loops, and its beats.
A file with labels beside it, a .beats file of the same name with a line
for each beat (its time in seconds, then 1 if it starts a bar), is scored
against them: the F-measure of its beats and downbeats within 70 ms.

  --fast           leave the network out, as Fast analysis does
  --labels <SET>   take the labels from the warp markers of the files'
                   clips in the Live Set SET, warped by hand, instead
  --live <SET>     score Live's auto-warp of the same files too, from the
                   warp markers of their clips in the Live Set SET
  --json           print each file's analysis as a line of JSON instead

A Live Set's bars are taken to be 4/4.";

/// What `musicable analyse` analyses.
struct Analyse {
    files: Vec<PathBuf>,
    fast: bool,
    labels: Option<PathBuf>,
    live: Option<PathBuf>,
    json: bool,
}

impl Analyse {
    /// The options in `args`, or `None` if they ask for help.
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Self>, String> {
        let mut analyse = Self {
            files: Vec::new(),
            fast: false,
            labels: None,
            live: None,
            json: false,
        };
        while let Some(arg) = args.next() {
            let Some(option) = arg.to_str().filter(|arg| arg.starts_with('-')) else {
                analyse.files.push(PathBuf::from(arg));
                continue;
            };
            match option {
                "-h" | "--help" => return Ok(None),
                "--fast" => analyse.fast = true,
                "--json" => analyse.json = true,
                "--labels" => {
                    let set = args.next().ok_or("--labels needs a Live Set")?;
                    analyse.labels = Some(PathBuf::from(set));
                }
                "--live" => {
                    let set = args.next().ok_or("--live needs a Live Set")?;
                    analyse.live = Some(PathBuf::from(set));
                }
                _ => return Err(format!("unknown option \"{option}\"")),
            }
        }
        if analyse.files.is_empty() {
            return Err("analyse needs a file to analyse".to_owned());
        }
        Ok(Some(analyse))
    }
}

/// `musicable analyse`: see [`ANALYSE_USAGE`].
fn analyse(args: impl Iterator<Item = OsString>) -> ExitCode {
    use musicable_analysis::evaluate::{self, Labels, Score};
    let options = match Analyse::parse(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{ANALYSE_USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}\n\n{ANALYSE_USAGE}");
            return ExitCode::FAILURE;
        }
    };
    // The beats of each file warped in a Live Set, by its name.
    let read_set = |set: &Option<PathBuf>| match set {
        Some(set) => {
            evaluate::live_beats(set).map(|warped| Some((set.display().to_string(), warped)))
        }
        None => Ok(None),
    };
    let (set_labels, live) = match (read_set(&options.labels), read_set(&options.live)) {
        (Ok(set_labels), Ok(live)) => (set_labels, live),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut network = None;
    if !options.fast {
        match musicable_analysis::models_folder() {
            Some(folder) => {
                let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
                match musicable_analysis::Network::load(&folder, threads, || {}, Default::default())
                {
                    Ok(loaded) => network = Some(loaded),
                    Err(error) => {
                        eprintln!("The Beat This! network didn't load: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }
            None => {
                eprintln!(
                    "The Beat This! models aren't there: fetch them with scripts/fetch-models.sh \
                     (or .ps1), point {} at them, or pass --fast.",
                    musicable_analysis::MODELS_VARIABLE,
                );
                return ExitCode::FAILURE;
            }
        }
    }
    let mut ours: Vec<Score> = Vec::new();
    let mut lives: Vec<Score> = Vec::new();
    let mut failed = false;
    for path in &options.files {
        let name = path.display();
        let started = std::time::Instant::now();
        let result = musicable_analysis::File::open(path)
            .map_err(|error| error.to_string())
            .and_then(|mut file| {
                musicable_analysis::analyse(&mut file, network.as_mut(), &mut || true)
                    .map_err(|error| error.to_string())
            });
        let (analysis, network_failed) = match result {
            Ok(result) => result,
            Err(error) => {
                eprintln!("{name}: {error}");
                failed = true;
                continue;
            }
        };
        let took = started.elapsed().as_secs_f64();
        if let Some(error) = network_failed {
            eprintln!("{name}: the network failed, so the classic tracker's beats stand: {error}");
        }
        if options.json {
            let line = serde_json::json!({ "file": path, "seconds": took, "analysis": analysis });
            println!("{line}");
            continue;
        }
        let rate = f64::from(analysis.sample_rate);
        let tracker = match (analysis.tracker, analysis.agreement) {
            (musicable_analysis::Tracker::Network, Some(agreement)) => {
                format!("the network, the classic tracker agreeing {agreement:.2}")
            }
            _ => "the classic tracker".to_owned(),
        };
        println!(
            "{name}: {:.1} s at {} Hz, by {tracker}, in {took:.2} s",
            analysis.frames as f64 / rate,
            analysis.sample_rate,
        );
        println!("  {}", summary(&analysis));
        let found = Labels {
            beats: analysis.beats.iter().map(|&b| b as f64 / rate).collect(),
            downbeats: (analysis.downbeats.iter())
                .map(|&i| analysis.beats[i] as f64 / rate)
                .collect(),
        };
        let file_name =
            (path.file_name()).map_or_else(String::new, |n| n.to_string_lossy().to_lowercase());
        let (labels, source) = match &set_labels {
            Some((set, warped)) => match warped.get(&file_name) {
                Some(labels) => (labels.clone(), set.clone()),
                None => {
                    println!("  not warped in {set}, so not scored");
                    continue;
                }
            },
            None => {
                let beside = [path.with_extension("beats"), {
                    let mut beside = path.clone().into_os_string();
                    beside.push(".beats");
                    PathBuf::from(beside)
                }]
                .into_iter()
                .find(|labels| labels.is_file());
                let Some(beside) = beside else {
                    continue;
                };
                match std::fs::read_to_string(&beside)
                    .map_err(|error| error.to_string())
                    .and_then(|text| evaluate::read_labels(&text))
                {
                    Ok(labels) => (labels, beside.display().to_string()),
                    Err(error) => {
                        eprintln!("{}: {error}", beside.display());
                        failed = true;
                        continue;
                    }
                }
            }
        };
        let score = evaluate::score(&found, &labels);
        ours.push(score);
        let mut line = format!("  against {source}: {}", scored(score));
        if let Some((set, warped)) = &live {
            match warped.get(&file_name) {
                Some(beats) => {
                    let score = evaluate::score(beats, &labels);
                    lives.push(score);
                    line += &format!("; Live's {}", scored(score));
                }
                None => line += &format!("; not warped in {set}"),
            }
        }
        println!("{line}");
    }
    if !ours.is_empty() {
        println!(
            "\nMean of {} labelled files: {}",
            ours.len(),
            scored(mean(&ours))
        );
        if !lives.is_empty() {
            println!(
                "Live's, on {} of them: {}",
                lives.len(),
                scored(mean(&lives))
            );
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// What an analysis found, in a line.
fn summary(analysis: &musicable_analysis::Analysis) -> String {
    let rate = f64::from(analysis.sample_rate);
    let mut parts = Vec::new();
    if analysis.pulse {
        parts.push("a pulse".to_owned());
    } else if analysis.riser {
        parts.push("no pulse, a riser".to_owned());
    } else {
        parts.push(match analysis.events {
            1 => "no pulse, one event".to_owned(),
            n => format!("no pulse, {n} events"),
        });
    }
    if let Some(steady) = analysis.steady {
        let steadiness = match steady.drift_ms <= musicable_analysis::STEADY_DRIFT_MS {
            true => "steady",
            false => "drifting",
        };
        parts.push(format!(
            "{:.2} BPM, {steadiness} (drift {:.1} ms, beats {:.1} ms off it)",
            steady.bpm, steady.drift_ms, steady.error_ms,
        ));
    }
    match analysis.breaks {
        0 => {}
        1 => parts.push("1 break in the beat".to_owned()),
        n => parts.push(format!("{n} breaks in the beat")),
    }
    if let Some(looped) = analysis.looped {
        let bars = match looped.bars {
            1 => "1 bar".to_owned(),
            n => format!("{n} bars"),
        };
        parts.push(format!("a loop of {bars} at {:.4} BPM", looped.bpm));
    }
    if let Some(sync) = analysis.sync {
        parts.push(format!("syncs at {:.3} s", sync as f64 / rate));
    }
    let tempi: Vec<String> = (analysis.tempi.iter().take(3))
        .map(|c| format!("{:.1} ({:.2})", c.bpm, c.score))
        .collect();
    if !tempi.is_empty() {
        parts.push(format!("flux tempi {}", tempi.join(", ")));
    }
    parts.push(format!(
        "{}/4, {} beats, {} downbeats, {} onsets",
        analysis.meter,
        analysis.beats.len(),
        analysis.downbeats.len(),
        analysis.onsets.len(),
    ));
    parts.join("; ")
}

/// A score, in words.
fn scored(score: musicable_analysis::evaluate::Score) -> String {
    match score.downbeats {
        Some(downbeats) => format!("beats {:.3}, downbeats {downbeats:.3}", score.beats),
        None => format!("beats {:.3}", score.beats),
    }
}

/// The mean of `scores`, the downbeats' over those that have them.
fn mean(scores: &[musicable_analysis::evaluate::Score]) -> musicable_analysis::evaluate::Score {
    let beats = scores.iter().map(|s| s.beats).sum::<f64>() / scores.len() as f64;
    let downbeats: Vec<f64> = scores.iter().filter_map(|s| s.downbeats).collect();
    musicable_analysis::evaluate::Score {
        beats,
        downbeats: (!downbeats.is_empty())
            .then(|| downbeats.iter().sum::<f64>() / downbeats.len() as f64),
    }
}

/// `text` starting with a capital letter.
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A peak level, in decibels relative to full scale.
fn level(peak: f32) -> String {
    if peak > 0.0 {
        format!("peak {:.1} dBFS", 20.0 * peak.log10())
    } else {
        "silent".to_owned()
    }
}

/// Lets a release build on Windows print to the console it was started from.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // Safety: AttachConsole takes no pointers. Without a console to attach
    // to, it fails and nothing changes.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn attach_console() {}
