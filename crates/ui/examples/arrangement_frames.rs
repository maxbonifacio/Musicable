//! How long the window takes to build and tessellate a frame of a big
//! arrangement: a hundred tracks of clips, with waveforms and notes, and the
//! mixer's hundred strips under them. The CPU's share of a frame, without a
//! GPU, as the app's frames depend on it.
//!
//! ```sh
//! cargo run --release -p musicable-ui --example arrangement_frames
//! ```
//!
//! It prints a table in Markdown, for `docs/app.md`.

use std::f32::consts::TAU;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Pos2, RawInput, Rect, vec2};
use musicable_commands::editor::AudioFile;
use musicable_commands::{Editor, Keymap, Registry, Steps, view};
use musicable_model::clip::{AudioClip, ClipContent, MidiClip};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Chain, Project, TrackKind};
use musicable_ui::{
    Action, Audio, KeymapStatus, Level, Levels, Library, MIXER, PeaksBuilder, Playback, Plugins,
    Shell, View, Waveforms, Workspaces,
};

const TRACKS: usize = 100;
/// Each track's clips, each four bars long, end to end.
const CLIPS: usize = 32;
const FRAMES: usize = 300;
const WARM_UP: usize = 30;
/// A window as big as a 1080p screen.
const SIZE: [f32; 2] = [1920.0, 1080.0];
const SAMPLE_RATE: u32 = 48_000;

fn main() {
    let mut bench = Bench::new();
    println!(
        "{TRACKS} tracks of {CLIPS} clips each, in a {}×{} window:\n",
        SIZE[0], SIZE[1]
    );
    println!("| Frame | Built in | Tessellated in | Slowest frame | Triangles |");
    println!("|---|---|---|---|---|");
    bench.measure("Stopped, mixer shown", |_| {});
    bench.hide_mixer();
    bench.measure("Stopped, mixer hidden", |_| {});
    bench.measure("Playing, the playhead moving", |bench| {
        bench.playing = true;
        bench.playhead += Beats::from_ratio(1, 32);
    });
    bench.playing = false;
    bench.measure("Scrolling sideways every frame", |bench| {
        let viewport = &mut bench.editor.viewport;
        viewport.start += Beats::from_ratio(1, 4);
    });
    let width = bench.editor.viewport.width;
    bench.editor.viewport.start = Beats::ZERO;
    // The whole song in sight, with the tracks at their smallest.
    bench.editor.viewport.zoom = f64::from(width) / (CLIPS as f64 * 16.0);
    for track in &bench.project.tracks {
        bench
            .editor
            .track_heights
            .insert(Chain::Track(track.id), 24.0);
    }
    bench.measure("Zoomed out to the whole song, tracks small", |_| {});
    bench.measure("The same, scrolling down every frame", |bench| {
        let viewport = &mut bench.editor.viewport;
        viewport.top = (viewport.top + 7.0) % 1500.0;
    });
}

struct Bench {
    ctx: egui::Context,
    shell: Shell,
    project: Project,
    editor: Editor,
    waveforms: Waveforms,
    commands: Registry,
    keymap: Keymap,
    levels: Levels,
    playing: bool,
    playhead: Beats,
    time: f64,
}

impl Bench {
    fn new() -> Self {
        let mut editor = Editor::default();
        let mut waveforms = Waveforms::new();
        // Eight files, eight seconds each.
        let files: Vec<String> = (0..8).map(|n| format!("Audio/Loop {n}.wav")).collect();
        for (n, file) in files.iter().enumerate() {
            let frames = 8 * SAMPLE_RATE as usize;
            let mut peaks = PeaksBuilder::new(SAMPLE_RATE, 2);
            let frequency = 2.0 + n as f32;
            let samples: Vec<f32> = (0..frames)
                .flat_map(|i| {
                    let t = i as f32 / SAMPLE_RATE as f32;
                    let envelope = (t * frequency * TAU).sin().abs();
                    let sample = envelope * (t * 440.0 * TAU).sin();
                    [sample, sample * 0.8]
                })
                .collect();
            peaks.push(&samples);
            waveforms.insert(file.clone(), Arc::new(peaks.finish()));
            let info = AudioFile {
                sample_rate: SAMPLE_RATE,
                frames: frames as u64,
                channels: 2,
                tempo: None,
            };
            editor.files.insert(file.clone(), info);
        }
        let mut project = Project::new();
        let mut levels = Levels::default();
        for t in 0..TRACKS {
            let midi = t % 3 == 2;
            let kind = if midi {
                TrackKind::Midi
            } else {
                TrackKind::Audio
            };
            let id = project.add_track(format!("Track {}", t + 1), kind);
            for c in 0..CLIPS {
                let start = Beats::new(16 * c as i64);
                let range = BeatRange::new(start, start + Beats::new(16));
                let content = if midi {
                    let mut clip = MidiClip::default();
                    for n in 0..32 {
                        let key = 48 + ((n * 7 + t) % 24) as u8;
                        let start = Beats::from_ratio(n as i64, 2);
                        clip.add_note(start, Beats::from_ratio(1, 2), key, 0.8);
                    }
                    ClipContent::Midi(clip)
                } else {
                    let mut clip = AudioClip::new(files[(t + c) % files.len()].clone());
                    clip.fade_in = Beats::from_ratio(1, 4);
                    clip.fade_out = Beats::new(1);
                    ClipContent::Audio(clip)
                };
                let clip = project.new_clip(format!("Clip {}", c + 1), range, content);
                project.track_mut(id).unwrap().clips.push(clip);
            }
            let level = Level {
                peak: [0.5, 0.4],
                rms: [0.3, 0.25],
                highest: 0.6,
            };
            levels.tracks.insert(id, level);
        }
        let mut commands = Registry::new();
        musicable_commands::register_all(&mut commands);
        let ctx = egui::Context::default();
        Self {
            ctx,
            shell: Shell::new(),
            project,
            editor,
            waveforms,
            commands,
            keymap: Keymap::default_keymap(),
            levels,
            playing: false,
            playhead: Beats::ZERO,
            time: 0.0,
        }
    }

    fn hide_mixer(&mut self) {
        self.shell.toggle_panel(MIXER);
    }

    /// Builds frames, changing what they show with `change` before each, and
    /// prints how long they took.
    fn measure(&mut self, name: &str, mut change: impl FnMut(&mut Self)) {
        let mut built = Vec::with_capacity(FRAMES);
        let mut tessellated = Vec::with_capacity(FRAMES);
        let mut triangles = 0;
        for frame in 0..WARM_UP + FRAMES {
            change(self);
            let (build, tessellate, count) = self.frame();
            if frame >= WARM_UP {
                built.push(build);
                tessellated.push(tessellate);
                triangles = triangles.max(count);
            }
        }
        let slowest = built
            .iter()
            .zip(&tessellated)
            .map(|(b, t)| *b + *t)
            .max()
            .unwrap_or_default();
        println!(
            "| {name} | {} | {} | {} | {} |",
            millis(mean(&built)),
            millis(mean(&tessellated)),
            millis(slowest),
            thousands(triangles)
        );
    }

    /// One frame: how long it took to build and to tessellate, and how many
    /// triangles it has.
    fn frame(&mut self) -> (Duration, Duration, usize) {
        self.time += 1.0 / 60.0;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(SIZE[0], SIZE[1]))),
            time: Some(self.time),
            ..RawInput::default()
        };
        let start = Instant::now();
        let mut actions = Vec::new();
        let output = self.ctx.run_ui(input, |ui| {
            let view = View {
                project: &self.project,
                editor: &self.editor,
                waveforms: &self.waveforms,
                name: "Big",
                modified: false,
                commands: &self.commands,
                keymap: &self.keymap,
                default_keymap: &self.keymap,
                history: Steps::default(),
                playback: Playback {
                    playing: self.playing,
                    record: false,
                    playhead: self.playhead,
                    seconds: 0.0,
                },
                recordings: &[],
                levels: &self.levels,
                inputs: &[],
                midi_inputs: &[],
                audio: Audio {
                    summary: "48 kHz · 128 samples · 5.3 ms",
                    running: true,
                    load: Some(0.25),
                    dropouts: 0,
                    details: "",
                },
                keymap_file: KeymapStatus {
                    path: None,
                    problems: &[],
                    error: None,
                    notice: None,
                },
                messages: &[],
                agent: None,
                analysing: None,
                plugins: Plugins::default(),
                native: &[],
                library: Library::default(),
                workspaces: Workspaces::default(),
                monitors: &[],
            };
            actions = self.shell.show(ui, &view);
        });
        let built = start.elapsed();
        let start = Instant::now();
        let primitives = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        let tessellated = start.elapsed();
        let triangles = primitives
            .iter()
            .map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => mesh.indices.len() / 3,
                egui::epaint::Primitive::Callback(_) => 0,
            })
            .sum();
        // As the app does: keep the viewport the arrangement asks for.
        for action in actions {
            if let Action::RunWith(id, args) = action
                && id == view::SET_VIEWPORT
            {
                let number = |name| args.get::<f64>(name).unwrap_or_default();
                let viewport = &mut self.editor.viewport;
                viewport.start = args.get("start").unwrap_or_default();
                viewport.zoom = number("zoom");
                viewport.top = number("top") as f32;
                viewport.width = number("width") as f32;
            }
        }
        (built, tessellated, triangles)
    }
}

fn mean(durations: &[Duration]) -> Duration {
    durations.iter().sum::<Duration>() / durations.len().max(1) as u32
}

fn millis(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1000.0)
}

fn thousands(n: usize) -> String {
    if n >= 10_000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}
