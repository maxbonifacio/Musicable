//! The Sample Editor in a window, its gestures carried out on a document as
//! the app carries them out.

use std::sync::Arc;

use egui::accesskit::Toggled;
use egui::{Event, Modifiers, PointerButton, Vec2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::editor::AudioFile;
use musicable_commands::{
    CommandError, Context, Document, Keymap, NoLibrary, Registry, Request, Value,
};
use musicable_model::TrackKind;
use musicable_model::clip::{ClipId, Offset};
use musicable_model::time::BeatRange;
use musicable_model::warp::Onsets;

use super::*;
use crate::shell::{Audio, KeymapStatus, Playback};
use crate::testing::Player;
use crate::{Levels, PeaksBuilder, Plugins, Waveforms};

/// The file the clip plays: four seconds at 48 kHz, eight beats at 120 BPM.
const FILE: &str = "loop.wav";
const RATE: u32 = 48_000;
const FRAMES: u64 = 4 * RATE as u64;
/// The file's frames in a beat at 120 BPM, the song's tempo and the
/// file's.
const BEAT: u64 = RATE as u64 / 2;

struct State {
    sample_editor: SampleEditor,
    document: Document,
    commands: Registry,
    keymap: Keymap,
    player: Player,
    levels: Levels,
    waveforms: Waveforms,
    theme: Theme,
    mouse: MouseMap,
    /// What the Sample Editor asked for, and what the commands asked of
    /// the app.
    actions: Vec<Action>,
    requests: Vec<Request>,
    errors: Vec<CommandError>,
}

impl State {
    fn editor(&self) -> &Editor {
        self.document.editor()
    }

    /// Carries out `action` as the app does.
    fn carry_out(&mut self, action: &Action) {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.player,
            requests: &mut self.requests,
            library: &mut NoLibrary,
        };
        let ran = match action {
            Action::Run(id) => self.commands.run_with(id, &Args::new(), &mut ctx),
            Action::RunWith(id, args) => self.commands.run_with(id, args, &mut ctx),
            Action::RunBatch { name, commands } => (self.commands)
                .run_batch(name, commands, &mut ctx)
                .map(|_| Value::None)
                .map_err(|failed| failed.error),
            _ => Ok(Value::None),
        };
        if let Err(error) = ran {
            self.errors.push(error);
        }
    }

    /// The clip the Sample Editor shows, and what it plays.
    fn clip(&self) -> (&Clip, &AudioClip) {
        let id = self.editor().sample_editor.clip.expect("a clip shown");
        let (_, clip) = self.document.project().find_clip(id).unwrap();
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("the Sample Editor shows an audio clip");
        };
        (clip, audio)
    }

    /// The clip's warp markers, as (frame, beat of its own time).
    fn markers(&self) -> Vec<(u64, f64)> {
        let (_, audio) = self.clip();
        let warp = audio.warp.as_ref().expect("a warped clip");
        warp.markers
            .iter()
            .map(|m| (m.frame, m.beat.to_f64()))
            .collect()
    }

    /// The frames of the clip's transients in sight.
    fn transients(&self) -> Vec<u64> {
        let (_, audio) = self.clip();
        let warp = audio.warp.as_ref().expect("a warped clip");
        warp.transients().map(|o| o.frame).collect()
    }

    /// The selected markers' beats.
    fn selected(&self) -> Vec<f64> {
        let markers = &self.editor().sample_editor.markers;
        markers.iter().map(|beat| beat.to_f64()).collect()
    }

    /// The clip's curve of `param`, as (beat of its own time, value).
    fn curve_of(&self, param: ClipParam) -> Vec<(f64, f64)> {
        let (_, audio) = self.clip();
        let lane = audio.lane(param);
        let points = lane.map_or(&[][..], |lane| &lane.points[..]);
        points.iter().map(|p| (p.at.to_f64(), p.value)).collect()
    }

    /// The clip's gain curve.
    fn curve(&self) -> Vec<(f64, f64)> {
        self.curve_of(ClipParam::Gain)
    }

    /// The clip's warp.
    fn warp(&self) -> &Warp {
        let (_, audio) = self.clip();
        audio.warp.as_ref().expect("a warped clip")
    }
}

/// Drums, an audio track with Loop, a clip from beat 4 to beat 12 that
/// plays the file from its start, and Keys, a MIDI track with Chords over
/// the same beats. Loop is warped with `markers`, as (frame, beat of its
/// own time), or unwarped without them.
fn project(markers: &[(u64, f64)]) -> (Project, ClipId) {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    let mut audio = AudioClip::new(FILE);
    if let Some(&(frame, beat)) = markers.first() {
        let mut warp = Warp::new(frame, Beats::from_f64(beat), 120.0);
        warp.markers = (markers.iter())
            .map(|&(frame, beat)| WarpMarker {
                frame,
                beat: Beats::from_f64(beat),
            })
            .collect();
        audio.offset = Offset::Beats(Beats::ZERO);
        audio.warp = Some(warp);
    }
    let range = BeatRange::new(Beats::new(4), Beats::new(12));
    let clip = project.new_clip("Loop", range, ClipContent::Audio(audio));
    let midi = musicable_model::clip::MidiClip::default();
    let chords = project.new_clip("Chords", range, ClipContent::Midi(midi));
    let id = clip.id;
    project.track_mut(drums).unwrap().clips.push(clip);
    project.track_mut(keys).unwrap().clips.push(chords);
    (project, id)
}

/// The [`project`], with Loop warped with `markers` and transients at the
/// file's `frames`, as analysis found them.
fn with_transients(markers: &[(u64, f64)], frames: &[u64]) -> (Project, ClipId) {
    let (mut project, clip) = project(markers);
    let ClipContent::Audio(audio) = &mut project.tracks[0].clips[0].content else {
        panic!("Loop is an audio clip");
    };
    let found = frames.iter().map(|&frame| Onset {
        frame,
        strength: 0.5,
        percussiveness: 1.0,
        origin: Origin::Found,
    });
    audio.warp.as_mut().expect("a warped clip").onsets = Some(Onsets::new(found));
    (project, clip)
}

/// The [`project`], with Loop warped with `markers` in Slice.
fn sliced(markers: &[(u64, f64)]) -> (Project, ClipId) {
    let (mut project, clip) = project(markers);
    let ClipContent::Audio(audio) = &mut project.tracks[0].clips[0].content else {
        panic!("Loop is an audio clip");
    };
    audio.warp.as_mut().expect("a warped clip").mode = WarpMode::Slice;
    (project, clip)
}

/// The file's peaks: a click on each beat, dying away.
fn peaks() -> Peaks {
    let samples: Vec<f32> = (0..FRAMES)
        .map(|frame| {
            let since = (frame % BEAT) as f32 / RATE as f32;
            (since * 900.0).sin() * (-since * 20.0).exp()
        })
        .collect();
    let mut builder = PeaksBuilder::new(RATE, 1);
    builder.push(&samples);
    builder.finish()
}

/// The Sample Editor showing `clip` in a window, with the focus, a beat a
/// hundred points wide from the song's third beat, so the grid's step is a
/// sixteenth.
fn harness(project: Project, clip: Option<ClipId>) -> Harness<'static, State> {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    let mut document = Document::new(project);
    let editor = document.editor_mut();
    let file = AudioFile {
        sample_rate: RATE,
        frames: FRAMES,
        channels: 1,
        tempo: Some(120.0),
    };
    editor.files.insert(FILE.to_owned(), file);
    if let Some(id) = clip {
        let (project, editor) = document.parts();
        let (_, clip) = project.find_clip(id).unwrap();
        editor.open_in_sample_editor(clip);
        editor.sample_editor.viewport = Viewport {
            start: Beats::new(3),
            zoom: 100.0,
            width: 750.0,
            top: 0.0,
        };
    }
    let mut waveforms = Waveforms::default();
    waveforms.insert(FILE.to_owned(), Arc::new(peaks()));
    let state = State {
        sample_editor: SampleEditor::default(),
        document,
        commands,
        keymap: Keymap::default_keymap(),
        player: Player::default(),
        levels: Levels::default(),
        waveforms,
        theme: Theme::default(),
        mouse: MouseMap::live(),
        actions: Vec::new(),
        requests: Vec::new(),
        errors: Vec::new(),
    };
    // Frames as quick as a screen's, so two clicks make a double click.
    let mut harness = Harness::builder()
        .with_size([900.0, 400.0])
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(40)
        .build_ui_state(
            |ui, state: &mut State| {
                let mut actions = Vec::new();
                {
                    let view = View {
                        project: state.document.project(),
                        editor: state.document.editor(),
                        waveforms: &state.waveforms,
                        name: "Untitled",
                        modified: false,
                        commands: &state.commands,
                        keymap: &state.keymap,
                        default_keymap: &state.keymap,
                        history: state.document.steps(),
                        playback: Playback {
                            playing: state.player.playing,
                            record: state.player.recording,
                            playhead: state.player.playhead,
                            seconds: 0.0,
                        },
                        recordings: &[],
                        levels: &state.levels,
                        performance: &crate::Performance::default(),
                        inputs: &[],
                        midi_inputs: &[],
                        audio: Audio {
                            summary: "",
                            running: true,
                            load: None,
                            dropouts: 0,
                            details: "",
                            lead: None,
                        },
                        keymap_file: KeymapStatus::default(),
                        messages: &[],
                        agent: None,
                        analysing: None,
                        rendering: None,
                        plugins: Plugins::default(),
                        native: &[],
                        library: crate::Library::default(),
                        workspaces: crate::Workspaces::default(),
                        monitors: &[],
                    };
                    let (theme, mouse) = (&state.theme, &state.mouse);
                    state
                        .sample_editor
                        .show(ui, &view, theme, mouse, &mut actions);
                }
                for action in &actions {
                    state.carry_out(action);
                }
                state.actions.extend(actions);
            },
            state,
        );
    harness.run();
    harness
}

/// Where the Sample Editor shows song position `at`, across.
fn x_of(harness: &Harness<'_, State>, at: f64) -> f32 {
    let wave = harness.get_by_label("Sample Editor").rect();
    let viewport = harness.state().editor().sample_editor.viewport;
    wave.left() + viewport.x_of(Beats::from_f64(at))
}

/// On the markers' handles, at song position `at`.
fn on_handle(harness: &Harness<'_, State>, at: f64) -> Pos2 {
    let wave = harness.get_by_label("Sample Editor").rect();
    let handle = Theme::default().sample_editor.handle;
    pos2(x_of(harness, at), wave.top() + handle / 2.0)
}

/// In the waveform's top half, under the handles, at `at`.
fn in_top(harness: &Harness<'_, State>, at: f64) -> Pos2 {
    let wave = harness.get_by_label("Sample Editor").rect();
    pos2(x_of(harness, at), wave.top() + wave.height() / 3.0)
}

/// In the waveform's bottom half, at `at`.
fn in_bottom(harness: &Harness<'_, State>, at: f64) -> Pos2 {
    let wave = harness.get_by_label("Sample Editor").rect();
    pos2(x_of(harness, at), wave.bottom() - wave.height() / 4.0)
}

/// On the lane, while it shows `param`, at `at`, where it shows `value`.
fn on_lane_of(harness: &Harness<'_, State>, param: ClipParam, at: f64, value: f64) -> Pos2 {
    let spec = param.spec();
    let lane = harness.get_by_label(&format!("{} Lane", spec.name)).rect();
    let theme = Theme::default();
    let margin = theme.point + 2.0;
    let unit = spec.scale.to_unit(value, spec.min, spec.max) as f32;
    let (bottom, top) = (lane.bottom() - margin, lane.top() + margin);
    pos2(x_of(harness, at), bottom + (top - bottom) * unit)
}

/// On the gain lane at `at`, where it shows the clip's `gain`.
fn on_lane(harness: &Harness<'_, State>, at: f64, gain: f64) -> Pos2 {
    on_lane_of(harness, ClipParam::Gain, at, gain)
}

/// Picks `option` from the menu named `menu`.
fn pick(harness: &mut Harness<'_, State>, menu: &str, option: &str) {
    harness.get_by_role_and_label(Role::ComboBox, menu).click();
    harness.run();
    harness.get_by_role_and_label(Role::Button, option).click();
    harness.run();
}

fn press(harness: &mut Harness<'_, State>, at: Pos2, pressed: bool, mods: Modifiers) {
    harness.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: mods,
    });
    harness.run();
}

/// Clicks at `at` with `mods` held.
fn click(harness: &mut Harness<'_, State>, at: Pos2, mods: Modifiers) {
    // Long enough after any click before that egui doesn't count this one
    // with it.
    harness.run_steps(30);
    harness.event(Event::ModifiersChanged(mods));
    harness.hover_at(at);
    harness.run();
    press(harness, at, true, mods);
    press(harness, at, false, mods);
    harness.event(Event::ModifiersChanged(Modifiers::NONE));
    harness.run();
}

fn double_click(harness: &mut Harness<'_, State>, at: Pos2) {
    harness.run_steps(30);
    harness.hover_at(at);
    harness.run();
    for pressed in [true, false, true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
}

/// Drags from `from` to `to` with `mods` held, in a few moves.
fn drag(harness: &mut Harness<'_, State>, from: Pos2, to: Pos2, mods: Modifiers) {
    harness.event(Event::ModifiersChanged(mods));
    harness.hover_at(from);
    harness.run();
    press(harness, from, true, mods);
    for step in 1..=4 {
        harness.hover_at(from + (to - from) * step as f32 / 4.0);
        harness.run();
    }
    press(harness, to, false, mods);
    harness.event(Event::ModifiersChanged(Modifiers::NONE));
    harness.run();
}

/// Opens the context menu at `at`.
fn secondary_click(harness: &mut Harness<'_, State>, at: Pos2) {
    harness.run_steps(30);
    harness.hover_at(at);
    harness.run();
    for pressed in [true, false] {
        harness.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
}

fn wheel(harness: &mut Harness<'_, State>, over: Pos2, delta: Vec2, mods: Modifiers) {
    harness.hover_at(over);
    harness.run();
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: mods,
    });
    harness.run();
}

/// Whether `a` and `b` are within a tick or so of each other.
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

#[test]
fn the_clip_s_parts_and_markers_are_named_where_they_are() {
    let (project, clip) = project(&[(0, 0.0), (4 * BEAT, 4.0)]);
    let harness = harness(project, Some(clip));
    for name in [
        "Sample Editor Ruler",
        "Sample Editor",
        "File Time",
        "Gain Lane",
        "Warp",
        "\u{f7}2",
        "\u{d7}2",
        "Warp marker at 2. 1. 1",
        "Warp marker at 3. 1. 1",
    ] {
        harness.get_by_label(name);
    }
    // The markers are on their beats.
    let marker = harness.get_by_label("Warp marker at 3. 1. 1").rect();
    let x = x_of(&harness, 8.0);
    assert!((marker.center().x - x).abs() < 1.0, "{marker:?} {x}");
    // The waveform knows how wide it is, for scrolling markers into sight.
    let wave = harness.get_by_label("Sample Editor").rect();
    let shown = &harness.state().editor().sample_editor;
    assert_eq!(shown.viewport.width, wave.width());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn without_an_audio_clip_it_says_how_to_show_one() {
    let (project, _) = project(&[]);
    let harness = harness(project, None);
    harness.get_by_label("Sample Editor");
    assert!(harness.query_by_label("Gain Lane").is_none());
    assert!(harness.query_by_label("Warp").is_none());
    assert!(harness.state().actions.is_empty());
}

#[test]
fn the_warp_button_warps_the_clip_at_its_file_s_tempo() {
    let (project, clip) = project(&[]);
    let mut harness = harness(project, Some(clip));
    // Unwarped, the clip has no markers or tempo to show.
    assert!(harness.query_by_label("\u{d7}2").is_none());
    harness.get_by_label("Warp").click();
    harness.run();
    // One marker at its start, at the tempo its loop data gives.
    assert_eq!(harness.state().markers(), [(0, 0.0)]);
    let (clip, audio) = harness.state().clip();
    assert_eq!(audio.warp.as_ref().unwrap().tempo, 120.0);
    assert_eq!(clip.range(), BeatRange::new(Beats::new(4), Beats::new(12)));
    harness.get_by_label("Warp marker at 2. 1. 1");
    harness.get_by_label("Warp").click();
    harness.run();
    let (_, audio) = harness.state().clip();
    assert_eq!(
        (audio.warp.as_ref(), audio.offset),
        (None, Offset::Frames(0))
    );
    assert!(harness.state().errors.is_empty());
}

#[test]
fn x2_and_halve_change_the_clip_s_tempo() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    harness.get_by_label("\u{d7}2").click();
    harness.run();
    // Twice the tempo, so it plays half as fast, through twice the beats.
    let (clip, audio) = harness.state().clip();
    assert_eq!(audio.warp.as_ref().unwrap().clip_tempo(RATE), 240.0);
    assert_eq!(clip.range(), BeatRange::new(Beats::new(4), Beats::new(20)));
    harness.get_by_label("\u{f7}2").click();
    harness.run();
    let (clip, audio) = harness.state().clip();
    assert_eq!(audio.warp.as_ref().unwrap().clip_tempo(RATE), 120.0);
    assert_eq!(clip.range(), BeatRange::new(Beats::new(4), Beats::new(12)));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn double_clicking_the_waveform_s_top_half_adds_a_marker_on_the_grid() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    harness.state_mut().document.editor_mut().focus = Focus::ARRANGEMENT;
    let at = in_top(&harness, 6.2);
    double_click(&mut harness, at);
    // Song beat 6.25 is beat 2.25 of the clip's own time, where the file's
    // frame two and a quarter beats in plays.
    let quarter = BEAT / 4;
    assert_eq!(harness.state().markers(), [(0, 0.0), (9 * quarter, 2.25)]);
    // Clicking gives the Sample Editor the focus, so the keys act on its
    // markers.
    assert_eq!(harness.state().editor().focus, Focus::SAMPLE_EDITOR);
    harness.get_by_label("Warp marker at 2. 3. 2");
    // The bottom half places the start marker instead.
    let at = in_bottom(&harness, 7.0);
    double_click(&mut harness, at);
    assert_eq!(harness.state().markers().len(), 2);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_a_marker_moves_it_on_the_grid_and_alt_frees_it() {
    let (project, clip) = project(&[(0, 0.0), (4 * BEAT, 4.0)]);
    let mut harness = harness(project, Some(clip));
    let from = on_handle(&harness, 8.0);
    drag(&mut harness, from, from + vec2(110.0, 0.0), Modifiers::NONE);
    // A beat later, on the grid: the frame stays, so the audio before it
    // stretches and the audio after it follows.
    assert_eq!(harness.state().markers(), [(0, 0.0), (4 * BEAT, 5.0)]);
    // A marker dragged is selected, and stays selected as it moves.
    assert_eq!(harness.state().selected(), [5.0]);
    let from = on_handle(&harness, 9.0);
    drag(&mut harness, from, from + vec2(12.0, 0.0), Modifiers::ALT);
    let markers = harness.state().markers();
    assert!(near(markers[1].1, 5.12), "{markers:?}");
    assert_eq!(harness.state().selected().len(), 1);
    // It stops where the audio before it would go slower than the slowest
    // tempo, 5 BPM: its four beats at 120 BPM squeezed into a sixth of a
    // beat.
    let from = on_handle(&harness, 9.12);
    drag(&mut harness, from, from - vec2(900.0, 0.0), Modifiers::NONE);
    let markers = harness.state().markers();
    assert!(near(markers[1].1, 1.0 / 6.0), "{markers:?}");
    assert!(harness.state().errors.is_empty());
}

#[test]
fn shift_dragging_a_marker_moves_the_audio_under_it() {
    let (project, clip) = project(&[(0, 0.0), (4 * BEAT, 4.0)]);
    let mut harness = harness(project, Some(clip));
    let from = on_handle(&harness, 8.0);
    drag(&mut harness, from, from + vec2(50.0, 0.0), Modifiers::SHIFT);
    // The audio went half a beat later: what played half a beat before the
    // marker plays at it, on its beat.
    assert_eq!(
        harness.state().markers(),
        [(0, 0.0), (4 * BEAT - BEAT / 2, 4.0)]
    );
    assert!(harness.state().errors.is_empty());
}

#[test]
fn clicks_select_markers_and_double_clicks_delete_them() {
    let (project, clip) = project(&[(0, 0.0), (2 * BEAT, 2.0), (4 * BEAT, 4.0)]);
    let mut harness = harness(project, Some(clip));
    let at = on_handle(&harness, 6.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(harness.state().selected(), [2.0]);
    let at = on_handle(&harness, 8.0);
    click(&mut harness, at, Modifiers::COMMAND);
    assert_eq!(harness.state().selected(), [2.0, 4.0]);
    let selected = harness.get_by_label("Warp marker at 3. 1. 1");
    assert_eq!(selected.accesskit_node().toggled(), Some(Toggled::True));
    let at = on_handle(&harness, 8.0);
    click(&mut harness, at, Modifiers::COMMAND);
    assert_eq!(harness.state().selected(), [2.0]);
    // Clicking the waveform clears the selection and places the start
    // marker, on the grid.
    let at = in_bottom(&harness, 9.1);
    click(&mut harness, at, Modifiers::NONE);
    assert!(harness.state().selected().is_empty());
    assert_eq!(harness.state().editor().start_marker, Beats::new(9));
    let at = on_handle(&harness, 6.0);
    double_click(&mut harness, at);
    let markers = harness.state().markers();
    assert_eq!(markers, [(0, 0.0), (4 * BEAT, 4.0)]);
    assert!(harness.query_by_label("Warp marker at 2. 3. 1").is_none());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_a_transient_s_pseudo_marker_makes_it_a_warp_marker() {
    let found = [BEAT, 9 * BEAT / 4, 3 * BEAT];
    let (project, clip) = with_transients(&[(0, 0.0)], &found);
    let mut harness = harness(project, Some(clip));
    // Song beat 6.25 is the clip's 2.25, where the second transient plays.
    let from = in_top(&harness, 6.25);
    drag(&mut harness, from, from + vec2(78.0, 0.0), Modifiers::NONE);
    // A marker pins the transient's frame, moved on the grid, as one step
    // in the undo history: the audio before it stretches.
    assert_eq!(harness.state().markers(), [(0, 0.0), (9 * BEAT / 4, 3.0)]);
    assert_eq!(harness.state().selected(), [3.0]);
    let document = &harness.state().document;
    assert_eq!(document.undo_name(), Some("Insert Warp Marker"));
    // The transients stay with the file.
    assert_eq!(harness.state().transients(), found);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn shift_dragging_a_transient_moves_it_and_double_clicking_one_pins_it() {
    let off_grid = 3 * BEAT + BEAT / 10;
    let (project, clip) = with_transients(&[(0, 0.0)], &[BEAT, 9 * BEAT / 4, off_grid]);
    let mut harness = harness(project, Some(clip));
    // Half a beat later in the file.
    let from = in_top(&harness, 5.0);
    drag(&mut harness, from, from + vec2(50.0, 0.0), Modifiers::SHIFT);
    let moved = [3 * BEAT / 2, 9 * BEAT / 4, off_grid];
    assert_eq!(harness.state().transients(), moved);
    // No further than the next.
    let from = in_top(&harness, 5.5);
    drag(
        &mut harness,
        from,
        from + vec2(300.0, 0.0),
        Modifiers::SHIFT,
    );
    let squeezed = [9 * BEAT / 4 - 1, 9 * BEAT / 4, off_grid];
    assert_eq!(harness.state().transients(), squeezed);
    assert_eq!(harness.state().markers(), [(0, 0.0)]);
    // Double-clicking near one pins it where it plays, off the grid.
    let at = in_top(&harness, 7.1) + vec2(2.0, 0.0);
    double_click(&mut harness, at);
    let markers = harness.state().markers();
    assert_eq!(markers.len(), 2);
    assert!(
        markers[1].0 == off_grid && near(markers[1].1, 3.1),
        "{markers:?}"
    );
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_the_waveform_selects_time_with_its_markers_and_transients() {
    let markers = [(0, 0.0), (2 * BEAT, 2.0), (6 * BEAT, 6.0)];
    let (project, clip) = with_transients(&markers, &[BEAT, 3 * BEAT, 5 * BEAT]);
    let mut harness = harness(project, Some(clip));
    let (from, to) = (in_bottom(&harness, 5.5), in_bottom(&harness, 8.5));
    drag(&mut harness, from, to, Modifiers::NONE);
    // The clip's own time from 1.5 to 4.5, with the marker and the
    // transient in it.
    let own = |beats: f64| Beats::from_f64(beats);
    let time = harness.state().editor().sample_editor.time;
    assert_eq!(time, Some(BeatRange::new(own(1.5), own(4.5))));
    assert_eq!(harness.state().selected(), [2.0]);
    let state = harness.state();
    let picked = warp::selected_transients(state.document.project(), state.editor());
    assert_eq!(picked, Some((clip, vec![3 * BEAT])));
    // In it, the context menu puts markers on its transients.
    let inside = in_bottom(&harness, 6.5);
    secondary_click(&mut harness, inside);
    harness.get_by_label("Insert Warp Marker Ctrl+I").click();
    harness.run();
    assert_eq!(
        harness.state().markers(),
        [(0, 0.0), (2 * BEAT, 2.0), (3 * BEAT, 3.0), (6 * BEAT, 6.0)]
    );
    // A click takes it away.
    let at = in_bottom(&harness, 10.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(harness.state().editor().sample_editor.time, None);
    assert!(harness.state().selected().is_empty());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_context_menu_inserts_deletes_and_resets_transients() {
    let (project, clip) = with_transients(&[(0, 0.0)], &[BEAT, 3 * BEAT]);
    let mut harness = harness(project, Some(clip));
    // On a transient's pseudo marker, for that transient.
    let at = in_top(&harness, 5.0);
    secondary_click(&mut harness, at);
    harness.get_by_label("Insert Warp Marker Ctrl+I");
    harness
        .get_by_label("Delete Transients Ctrl+Shift+Backspace")
        .click();
    harness.run();
    assert_eq!(harness.state().transients(), [3 * BEAT]);
    // Elsewhere, where it was opened, off the grid.
    let at = in_top(&harness, 6.1);
    secondary_click(&mut harness, at);
    harness
        .get_by_label("Insert Transient Ctrl+Shift+I")
        .click();
    harness.run();
    assert_eq!(
        harness.state().transients(),
        [2 * BEAT + BEAT / 10, 3 * BEAT]
    );
    // Quantize on its pseudo marker moves it to the nearest sixteenth,
    // with a warp marker.
    let at = in_top(&harness, 6.1);
    secondary_click(&mut harness, at);
    harness.get_by_label("Quantize Ctrl+U").click();
    harness.run();
    assert_eq!(
        harness.state().markers(),
        [(0, 0.0), (2 * BEAT + BEAT / 10, 2.0)]
    );
    // Reset Transients brings back those analysis found.
    let at = in_bottom(&harness, 8.0);
    secondary_click(&mut harness, at);
    harness.get_by_label("Reset Transients").click();
    harness.run();
    assert_eq!(harness.state().transients(), [BEAT, 3 * BEAT]);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_context_menu_sets_the_downbeat_and_warps_again_from_a_point() {
    let (project, clip) = project(&[(0, 0.0), (4 * BEAT, 4.0)]);
    let mut harness = harness(project, Some(clip));
    // On a marker, it's the clip's first downbeat, on the bar line where it
    // plays.
    let at = on_handle(&harness, 8.0);
    secondary_click(&mut harness, at);
    harness.get_by_label("Set 1.1.1 Here").click();
    harness.run();
    assert_eq!(harness.state().markers(), [(0, -4.0), (4 * BEAT, 0.0)]);
    assert_eq!(harness.state().clip().0.start, Beats::new(4));
    // On the waveform, where the menu was opened: one tempo from there to
    // the file's end, the clip's.
    let at = in_bottom(&harness, 6.0);
    secondary_click(&mut harness, at);
    harness.get_by_label("Warp From Here (Straight)").click();
    harness.run();
    let straight = [(0, -4.0), (2 * BEAT, -2.0), (FRAMES, 4.0)];
    assert_eq!(harness.state().markers(), straight);
    // In the time selected, a bar of it: a loop of a bar from beat 4.
    let (from, to) = (in_bottom(&harness, 5.0), in_bottom(&harness, 9.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let inside = in_bottom(&harness, 7.0);
    secondary_click(&mut harness, inside);
    harness.get_by_label("Warp Selection as a Loop").click();
    harness.run();
    assert_eq!(harness.state().markers(), [(BEAT, 0.0)]);
    let (clip, _) = harness.state().clip();
    assert_eq!((clip.start, clip.length), (Beats::new(4), Beats::new(4)));
    assert!(
        harness.state().errors.is_empty(),
        "{:?}",
        harness.state().errors
    );
}

#[test]
fn the_gain_lane_adds_moves_and_draws_points() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    // Clicking the line at the clip's gain adds a point on it, in the
    // clip's own time, which changes nothing until it's moved.
    let at = on_lane(&harness, 6.0, 1.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(harness.state().curve(), [(2.0, 1.0)]);
    let from = on_lane(&harness, 6.0, 1.0);
    drag(&mut harness, from, from + vec2(0.0, 8.0), Modifiers::NONE);
    let curve = harness.state().curve();
    assert!(curve.len() == 1 && curve[0].1 < 1.0, "{curve:?}");
    // Dragging the lane away from the line draws a step of the grid at a
    // time.
    let loud = on_lane(&harness, 9.0, 8.0);
    drag(&mut harness, loud, loud + vec2(100.0, 0.0), Modifiers::NONE);
    let curve = harness.state().curve();
    assert!(curve.len() > 3, "{curve:?}");
    let drawn: Vec<_> = curve
        .iter()
        .filter(|(at, _)| *at > 5.0 && *at < 6.0)
        .collect();
    assert!(drawn.iter().all(|(_, value)| *value > 6.0), "{curve:?}");
    // Clicking a point deletes it.
    let point = on_lane(&harness, 6.0, curve[0].1);
    click(&mut harness, point, Modifiers::NONE);
    assert_eq!(harness.state().curve().len(), curve.len() - 1);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_rulers_locate_and_scrub_zoom() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    let ruler = harness.get_by_label("Sample Editor Ruler").rect();
    let at = pos2(x_of(&harness, 7.0), ruler.center().y);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(harness.state().player.playhead, Beats::new(7));
    // Dragging down zooms in around where it started.
    drag(&mut harness, at, at + vec2(0.0, 60.0), Modifiers::NONE);
    let viewport = harness.state().editor().sample_editor.viewport;
    assert!(viewport.zoom > 150.0, "{viewport:?}");
    assert!((x_of(&harness, 7.0) - at.x).abs() < 1.0);
    let time = harness.get_by_label("File Time").rect();
    let at = pos2(time.center().x, time.center().y);
    drag(&mut harness, at, at - vec2(0.0, 60.0), Modifiers::NONE);
    let zoomed = harness.state().editor().sample_editor.viewport.zoom;
    assert!(zoomed < viewport.zoom, "{zoomed}");
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_wheel_scrolls_and_zooms() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    let over = in_bottom(&harness, 8.0);
    wheel(&mut harness, over, vec2(0.0, -100.0), Modifiers::NONE);
    let viewport = harness.state().editor().sample_editor.viewport;
    assert_eq!(viewport.start, Beats::new(4));
    wheel(&mut harness, over, vec2(0.0, 100.0), Modifiers::COMMAND);
    let zoomed = harness.state().editor().sample_editor.viewport;
    assert!(zoomed.zoom > viewport.zoom, "{zoomed:?}");
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_mode_menu_and_slice_s_settings_set_the_clip_s_warp() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    let mode = harness.get_by_role_and_label(Role::ComboBox, "Warp Mode");
    assert_eq!(mode.value().as_deref(), Some("Re-Pitch"));
    // Re-Pitch's pitch follows its speed: its transpose and detune are
    // greyed, and it has no settings of its own.
    for name in ["Transpose", "Detune"] {
        let field = harness.get_by_label(name);
        assert!(field.accesskit_node().is_disabled(), "{name}");
    }
    let preserve = harness.query_by_role_and_label(Role::ComboBox, "Preserve");
    assert!(preserve.is_none());
    pick(&mut harness, "Warp Mode", "Slice");
    assert_eq!(harness.state().warp().mode, WarpMode::Slice);
    assert_eq!(harness.state().document.undo_name(), Some("Set Warp Mode"));
    // Slice's settings show, and its transpose and detune play.
    let menus = [
        ("Preserve", "Transients", "1/8"),
        ("Transient Loop Mode", "Back-and-Forth", "Forward"),
    ];
    for (menu, shown, option) in menus {
        let chooser = harness.get_by_role_and_label(Role::ComboBox, menu);
        assert_eq!(chooser.value().as_deref(), Some(shown));
        pick(&mut harness, menu, option);
        let chooser = harness.get_by_role_and_label(Role::ComboBox, menu);
        assert_eq!(chooser.value().as_deref(), Some(option));
    }
    let fields = [
        ("Transient Envelope", 40.0),
        ("Transpose", -7.0),
        ("Detune", 12.0),
    ];
    for (name, value) in fields {
        let field = harness.get_by_label(name);
        assert!(!field.accesskit_node().is_disabled(), "{name}");
        field.set_value_accesskit(value);
        harness.run();
    }
    let warp = harness.state().warp();
    let settings = SliceSettings {
        preserve: Preserve::Eighth,
        transient_loop: TransientLoop::Forward,
        envelope: 40.0,
    };
    assert_eq!(warp.slice, settings);
    assert_eq!((warp.transpose, warp.detune), (-7, 12));
    assert_eq!(
        harness.state().document.undo_name(),
        Some("Set Clip Detune")
    );
    // Back in Re-Pitch, they're kept for when Slice plays again.
    pick(&mut harness, "Warp Mode", "Re-Pitch");
    let warp = harness.state().warp();
    assert_eq!((warp.mode, warp.slice), (WarpMode::RePitch, settings));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn spectral_s_formants_set_the_clip_s_warp() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    assert!(harness.query_by_label("Formants").is_none());
    pick(&mut harness, "Warp Mode", "Spectral");
    assert_eq!(harness.state().warp().mode, WarpMode::Spectral);
    // Spectral's setting shows, not Slice's, and its transpose and detune
    // play.
    let preserve = harness.query_by_role_and_label(Role::ComboBox, "Preserve");
    assert!(preserve.is_none());
    let formants = harness.get_by_role_and_label(Role::SpinButton, "Formants");
    assert_eq!(formants.accesskit_node().numeric_value(), Some(100.0));
    for (name, value) in [("Transpose", 5.0), ("Formants", 30.0)] {
        let field = harness.get_by_role_and_label(Role::SpinButton, name);
        assert!(!field.accesskit_node().is_disabled(), "{name}");
        field.set_value_accesskit(value);
        harness.run();
    }
    let warp = harness.state().warp();
    assert_eq!((warp.transpose, warp.spectral.formants), (5, 30.0));
    assert_eq!(harness.state().document.undo_name(), Some("Set Formants"));
    // Its lane is one of those the mode plays.
    pick(&mut harness, "Lane", "Formants");
    assert_eq!(
        harness.state().editor().sample_editor.lane,
        ClipParam::Formants
    );
    // In another mode, it's kept for when Spectral plays again.
    pick(&mut harness, "Warp Mode", "Slice");
    assert!(harness.query_by_label("Formants").is_none());
    let warp = harness.state().warp();
    assert_eq!((warp.mode, warp.spectral.formants), (WarpMode::Slice, 30.0));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn high_quality_shows_spectral_s_envelope() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    pick(&mut harness, "Warp Mode", "Spectral");
    // Off, as a clip starts, it renders with Signalsmith Stretch, which has
    // no Envelope.
    let toggle = harness.get_by_role_and_label(Role::CheckBox, "High quality");
    assert_eq!(toggle.accesskit_node().toggled(), Some(Toggled::False));
    assert!(harness.query_by_label("Envelope").is_none());
    toggle.click();
    harness.run();
    assert!(harness.state().warp().spectral.high_quality);
    assert_eq!(
        harness.state().document.undo_name(),
        Some("Set High Quality")
    );
    let envelope = harness.get_by_role_and_label(Role::SpinButton, "Envelope");
    assert_eq!(envelope.accesskit_node().numeric_value(), Some(128.0));
    envelope.set_value_accesskit(64.0);
    harness.run();
    assert_eq!(harness.state().warp().spectral.envelope, 64.0);
    assert_eq!(
        harness.state().document.undo_name(),
        Some("Set Spectral Envelope")
    );
    // Its lane plays now, and the clip keeps it with High quality off.
    pick(&mut harness, "Lane", "Envelope");
    assert_eq!(
        harness.state().editor().sample_editor.lane,
        ClipParam::Envelope
    );
    harness
        .get_by_role_and_label(Role::CheckBox, "High quality")
        .click();
    harness.run();
    assert!(
        harness
            .query_by_role_and_label(Role::SpinButton, "Envelope")
            .is_none()
    );
    let spectral = harness.state().warp().spectral;
    assert_eq!((spectral.high_quality, spectral.envelope), (false, 64.0));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn granular_shows_fluctuation_while_follow_pitch_is_off() {
    let (project, clip) = project(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    pick(&mut harness, "Warp Mode", "Granular");
    // On, as a clip starts, the grains follow the pitch, so they don't
    // stray.
    let toggle = harness.get_by_role_and_label(Role::CheckBox, "Follow pitch");
    assert_eq!(toggle.accesskit_node().toggled(), Some(Toggled::True));
    let no_fluctuation = |harness: &Harness<'_, State>| {
        (harness.query_by_role_and_label(Role::SpinButton, "Fluctuation")).is_none()
    };
    assert!(no_fluctuation(&harness));
    let size = harness.get_by_role_and_label(Role::SpinButton, "Grain Size");
    assert_eq!(size.accesskit_node().numeric_value(), Some(30.0));
    size.set_value_accesskit(80.0);
    harness.run();
    assert_eq!(harness.state().warp().granular.grain_size, 80.0);
    assert_eq!(harness.state().document.undo_name(), Some("Set Grain Size"));
    harness
        .get_by_role_and_label(Role::CheckBox, "Follow pitch")
        .click();
    harness.run();
    assert!(!harness.state().warp().granular.follow_pitch);
    let fluctuation = harness.get_by_role_and_label(Role::SpinButton, "Fluctuation");
    assert_eq!(fluctuation.accesskit_node().numeric_value(), Some(25.0));
    fluctuation.set_value_accesskit(70.0);
    harness.run();
    assert_eq!(harness.state().warp().granular.fluctuation, 70.0);
    assert_eq!(
        harness.state().document.undo_name(),
        Some("Set Fluctuation")
    );
    // Its lane plays now.
    pick(&mut harness, "Lane", "Fluctuation");
    assert_eq!(
        harness.state().editor().sample_editor.lane,
        ClipParam::Fluctuation
    );
    // In another mode, they're kept for when Granular plays again.
    pick(&mut harness, "Warp Mode", "Spectral");
    assert!(harness.query_by_label("Grain Size").is_none());
    assert!(no_fluctuation(&harness));
    let granular = harness.state().warp().granular;
    assert_eq!(
        (
            granular.follow_pitch,
            granular.grain_size,
            granular.fluctuation
        ),
        (false, 80.0, 70.0)
    );
    assert!(harness.state().errors.is_empty());
}

#[test]
fn the_lane_s_menu_shows_the_curve_of_each_of_the_clip_s_parameters() {
    let (project, clip) = sliced(&[(0, 0.0)]);
    let mut harness = harness(project, Some(clip));
    let chooser = harness.get_by_role_and_label(Role::ComboBox, "Lane");
    assert_eq!(chooser.value().as_deref(), Some("Gain"));
    chooser.click();
    harness.run();
    // They're all there: those Slice plays, then Spectral's Formants and
    // Envelope.
    for param in ClipParam::ALL {
        harness.get_by_role_and_label(Role::Button, &param.spec().name);
    }
    harness
        .get_by_role_and_label(Role::Button, "Transpose")
        .click();
    harness.run();
    assert_eq!(
        harness.state().editor().sample_editor.lane,
        ClipParam::Transpose
    );
    assert!(harness.query_by_label("Gain Lane").is_none());
    // Choosing isn't a change to undo.
    assert_eq!(harness.state().document.undo_name(), None);
    // Clicking the line at the clip's transpose adds a point on it, and
    // dragging the point up moves the pitch up.
    let at = on_lane_of(&harness, ClipParam::Transpose, 6.0, 0.0);
    click(&mut harness, at, Modifiers::NONE);
    let transpose = harness.state().curve_of(ClipParam::Transpose);
    assert_eq!(transpose, [(2.0, 0.0)]);
    drag(&mut harness, at, at - vec2(0.0, 11.0), Modifiers::NONE);
    let transpose = harness.state().curve_of(ClipParam::Transpose);
    assert!(
        transpose.len() == 1 && transpose[0].1 > 12.0,
        "{transpose:?}"
    );
    assert!(harness.state().curve().is_empty());
    // On a lane of steps, drawing draws whole ones.
    pick(&mut harness, "Lane", "Preserve");
    let from = on_lane_of(&harness, ClipParam::Preserve, 9.0, 3.4);
    drag(&mut harness, from, from + vec2(100.0, 0.0), Modifiers::NONE);
    let preserve = harness.state().curve_of(ClipParam::Preserve);
    assert!(preserve.len() > 2, "{preserve:?}");
    assert!(
        preserve.iter().all(|&(_, value)| value == value.round()),
        "{preserve:?}"
    );
    assert!(
        preserve.iter().any(|&(_, value)| value == 3.0),
        "{preserve:?}"
    );
    // Unwarped, the clip plays its gain alone, and the lane shows it.
    harness.get_by_label("Warp").click();
    harness.run();
    harness.get_by_label("Gain Lane");
    assert!(
        harness
            .query_by_role_and_label(Role::ComboBox, "Lane")
            .is_none()
    );
    assert!(harness.state().errors.is_empty());
}

#[test]
fn a_lane_of_steps_holds_each_point_until_the_next() {
    let (mut project, clip) = sliced(&[(0, 0.0)]);
    let ClipContent::Audio(audio) = &mut project.tracks[0].clips[0].content else {
        panic!("Loop is an audio clip");
    };
    // Sixteenths from the clip's start, and the transients from its third
    // beat.
    let points = vec![Point::new(Beats::ZERO, 5.0), Point::new(Beats::new(2), 0.0)];
    audio.set_lane(ClipParam::Preserve, points);
    let mut harness = harness(project, Some(clip));
    pick(&mut harness, "Lane", "Preserve");
    // Halfway between the points, the line holds the first one's value: a
    // drag there moves that point alone, a step at a time.
    let from = on_lane_of(&harness, ClipParam::Preserve, 5.0, 5.0);
    let to = on_lane_of(&harness, ClipParam::Preserve, 5.0, 3.2);
    drag(&mut harness, from, to, Modifiers::NONE);
    let preserve = harness.state().curve_of(ClipParam::Preserve);
    assert_eq!(preserve, [(0.0, 3.0), (2.0, 0.0)]);
    assert!(harness.state().errors.is_empty());
}
