//! The arrangement in a window, its gestures carried out on a document as
//! the app carries them out.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::accesskit::Toggled;
use egui::{DroppedFile, Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use musicable_commands::editor::AudioFile;
use musicable_commands::{
    CommandError, Context, Document, Keymap, Named, Registry, Request, Transport,
};
use musicable_model::clip::{AudioClip, ClipContent, ClipId, MidiClip};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{LocatorId, Project, TakeId, TrackId, TrackKind};

use super::*;
use crate::shell::{Audio, KeymapStatus, Playback};
use crate::{Levels, Plugins, Waveforms};

/// A transport that plays along: locating moves its playhead.
#[derive(Debug, Default)]
struct Player {
    playing: bool,
    recording: bool,
    playhead: Beats,
}

impl Transport for Player {
    fn is_playing(&self) -> bool {
        self.playing
    }

    fn play(&mut self) {
        self.playing = true;
    }

    fn stop(&mut self) {
        self.playing = false;
        self.recording = false;
    }

    fn locate(&mut self, at: Beats) {
        self.playhead = at;
    }

    fn playhead(&self) -> Beats {
        self.playhead
    }

    fn is_recording(&self) -> bool {
        self.recording
    }

    fn set_recording(&mut self, on: bool) {
        self.recording = on;
    }
}

struct State {
    arrangement: Arrangement,
    document: Document,
    commands: Registry,
    keymap: Keymap,
    player: Player,
    levels: Levels,
    waveforms: Waveforms,
    theme: Theme,
    mouse: MouseMap,
    /// What the arrangement asked for, and what the commands asked of the
    /// app.
    actions: Vec<Action>,
    requests: Vec<Request>,
    errors: Vec<CommandError>,
}

impl State {
    fn project(&self) -> &Project {
        self.document.project()
    }

    fn editor(&self) -> &Editor {
        self.document.editor()
    }

    /// Carries out `action` as the app does.
    fn carry_out(&mut self, action: &Action) {
        let mut ctx = Context {
            document: &mut self.document,
            transport: &mut self.player,
            requests: &mut self.requests,
        };
        let ran = match action {
            Action::Run(id) => self.commands.run_with(id, &Args::new(), &mut ctx),
            Action::RunWith(id, args) => self.commands.run_with(id, args, &mut ctx),
            Action::Mixer(change) => {
                musicable_commands::mixer::change(&mut self.document, change.clone());
                Ok(musicable_commands::Value::None)
            }
            _ => Ok(musicable_commands::Value::None),
        };
        if let Err(error) = ran {
            self.errors.push(error);
        }
    }
}

/// The ids of the test project's tracks and clips.
struct Ids {
    drums: TrackId,
    keys: TrackId,
    bass: TrackId,
    kick: ClipId,
    chords: ClipId,
    line: ClipId,
    verse: LocatorId,
}

/// Drums, an audio track with Kick over the first two bars; Keys, a MIDI
/// track with Chords over the next two; Bass, an audio track with Line over
/// the two after; and a reverb return. The locator Verse is at bar 5, and
/// Outro is a clip far out of sight.
fn project() -> (Project, Ids) {
    let mut project = Project::new();
    let drums = project.add_track("Drums", TrackKind::Audio);
    let keys = project.add_track("Keys", TrackKind::Midi);
    let bass = project.add_track("Bass", TrackKind::Audio);
    project.add_track("Reverb", TrackKind::Return);
    let beats = |start, end| BeatRange::new(Beats::new(start), Beats::new(end));
    let audio = |file: &str| ClipContent::Audio(AudioClip::new(file));
    let kick = project.new_clip("Kick", beats(0, 8), audio("kick.wav"));
    let outro = project.new_clip("Outro", beats(1000, 1008), audio("kick.wav"));
    let mut notes = MidiClip::default();
    notes.add_note(Beats::ZERO, Beats::new(2), 60, 0.8);
    notes.add_note(Beats::new(2), Beats::new(2), 67, 0.8);
    let chords = project.new_clip("Chords", beats(8, 16), ClipContent::Midi(notes));
    let line = project.new_clip("Line", beats(16, 24), audio("bass.wav"));
    let ids = (kick.id, chords.id, line.id);
    project
        .track_mut(drums)
        .unwrap()
        .clips
        .extend([kick, outro]);
    project.track_mut(keys).unwrap().clips.push(chords);
    project.track_mut(bass).unwrap().clips.push(line);
    let verse = project.new_locator("Verse", Beats::new(16));
    let verse_id = verse.id;
    project.locators.push(verse);
    let ids = Ids {
        drums,
        keys,
        bass,
        kick: ids.0,
        chords: ids.1,
        line: ids.2,
        verse: verse_id,
    };
    (project, ids)
}

fn harness(project: Project) -> Harness<'static, State> {
    let mut commands = Registry::new();
    musicable_commands::register_all(&mut commands);
    let mut document = Document::new(project);
    // A minute of each file, as if the app had read them.
    for file in ["kick.wav", "bass.wav"] {
        let info = AudioFile {
            sample_rate: 48_000,
            frames: 48_000 * 60,
            channels: 2,
        };
        document.editor_mut().files.insert(file.to_owned(), info);
    }
    let state = State {
        arrangement: Arrangement::default(),
        document,
        commands,
        keymap: Keymap::default_keymap(),
        player: Player::default(),
        levels: Levels::default(),
        waveforms: Waveforms::default(),
        theme: Theme::default(),
        mouse: MouseMap::live(),
        actions: Vec::new(),
        requests: Vec::new(),
        errors: Vec::new(),
    };
    // Frames as quick as a screen's, so two clicks make a double click, and
    // enough of them for egui to smooth out a turn of the wheel, which takes
    // a few.
    let mut harness = Harness::builder()
        .with_size([1000.0, 420.0])
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
                        undo: None,
                        redo: None,
                        playback: Playback {
                            playing: state.player.playing,
                            record: state.player.recording,
                            playhead: state.player.playhead,
                            seconds: 0.0,
                        },
                        recordings: &[],
                        levels: &state.levels,
                        inputs: &[],
                        audio: Audio {
                            summary: "",
                            running: true,
                            load: None,
                            dropouts: 0,
                            details: "",
                        },
                        keymap_file: KeymapStatus::default(),
                        messages: &[],
                        plugins: Plugins::default(),
                        native: &[],
                    };
                    let (theme, mouse) = (&state.theme, &state.mouse);
                    state
                        .arrangement
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

/// Where the timeline shows `at`.
fn x_of(harness: &Harness<'_, State>, at: f64) -> f32 {
    let lanes = harness.get_by_label("Arrangement").rect();
    let viewport = harness.state().editor().viewport;
    lanes.left() + viewport.x_of(Beats::from_f64(at))
}

/// Somewhere in `name`'s lane, at `at`.
fn in_lane(harness: &Harness<'_, State>, name: &str, at: f64) -> Pos2 {
    let row = harness.get_by_label(&format!("Track {name}")).rect();
    pos2(x_of(harness, at), row.center().y)
}

/// A point in the clip's title bar, `along` points from its start.
fn title(harness: &Harness<'_, State>, clip: &str, along: f32) -> Pos2 {
    let rect = harness.get_by_label(&format!("Clip {clip}")).rect();
    pos2(rect.left() + along, rect.top() + 4.0)
}

/// A point in the clip's body, `along` points from its start.
fn body(harness: &Harness<'_, State>, clip: &str, along: f32) -> Pos2 {
    let rect = harness.get_by_label(&format!("Clip {clip}")).rect();
    pos2(rect.left() + along, rect.bottom() - 6.0)
}

/// A point in the ruler's strip, at `at`.
fn strip(harness: &Harness<'_, State>, at: f64) -> Pos2 {
    let ruler = harness.get_by_label("Ruler").rect();
    pos2(
        x_of(harness, at),
        ruler.top() + 0.75 * Theme::default().ruler_strip,
    )
}

/// A point in the ruler under its strip, at `at`.
fn ruler(harness: &Harness<'_, State>, at: f64) -> Pos2 {
    let ruler = harness.get_by_label("Ruler").rect();
    pos2(x_of(harness, at), ruler.bottom() - 6.0)
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
    harness.event(Event::ModifiersChanged(mods));
    harness.hover_at(at);
    harness.run();
    press(harness, at, true, mods);
    press(harness, at, false, mods);
    harness.event(Event::ModifiersChanged(Modifiers::NONE));
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

/// The distance `beats` beats take on the timeline.
fn width(harness: &Harness<'_, State>, beats: f64) -> f32 {
    (beats * harness.state().editor().viewport.zoom) as f32
}

fn clip(harness: &Harness<'_, State>, id: ClipId) -> (TrackId, musicable_model::clip::Clip) {
    let (track, clip) = harness.state().project().find_clip(id).unwrap();
    (track.id, clip.clone())
}

fn selected(harness: &Harness<'_, State>) -> Vec<ClipId> {
    harness
        .state()
        .editor()
        .selection
        .clips
        .iter()
        .copied()
        .collect()
}

#[test]
fn clips_tracks_and_locators_are_named() {
    let (project, _) = project();
    let harness = harness(project);
    for name in [
        "Clip Kick",
        "Clip Chords",
        "Clip Line",
        "Track Drums",
        "Track Keys",
        "Track Bass",
        "Track Reverb",
        "Locator Verse",
        "Mute track Drums",
        "Solo track Reverb",
        "Arm track Keys",
    ] {
        harness.get_by_label(name);
    }
    // Returns have no arm, and clips out of sight aren't there.
    assert!(harness.query_by_label("Arm track Reverb").is_none());
    assert!(harness.query_by_label("Clip Outro").is_none());
    // The timeline knows how wide it is, for zooming to fit.
    let lanes = harness.get_by_label("Arrangement").rect();
    let viewport = harness.state().editor().viewport;
    assert_eq!(viewport.width, lanes.width());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn clicks_select_clips_and_place_the_start_marker() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let at = title(&harness, "Kick", 30.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(selected(&harness), [ids.kick]);
    let at = title(&harness, "Line", 30.0);
    click(&mut harness, at, Modifiers::COMMAND);
    assert_eq!(selected(&harness), [ids.kick, ids.line]);
    // Clicking a clip's body selects it, and marks where it was clicked,
    // on the grid: at this zoom, every two beats.
    let at = pos2(x_of(&harness, 10.7), body(&harness, "Chords", 0.0).y);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(selected(&harness), [ids.chords]);
    assert_eq!(harness.state().editor().start_marker, Beats::new(10));
    // An empty lane clears the selection and selects the track.
    let at = in_lane(&harness, "Bass", 30.2);
    click(&mut harness, at, Modifiers::NONE);
    let editor = harness.state().editor();
    assert!(editor.selection.clips.is_empty());
    assert_eq!(editor.selected_track, Some(ids.bass));
    assert_eq!(editor.start_marker, Beats::new(30));
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_a_title_moves_clips_on_the_grid_and_to_other_tracks() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = title(&harness, "Kick", 30.0);
    let to = from + vec2(width(&harness, 2.3), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let (track, kick) = clip(&harness, ids.kick);
    assert_eq!((track, kick.start), (ids.drums, Beats::new(2)));
    assert_eq!(selected(&harness), [ids.kick]);
    assert_eq!(harness.state().document.undo_name(), Some("Move Clips"));
    // Keys is a MIDI track, so Kick can't go there, but Bass is two down.
    let from = title(&harness, "Kick", 30.0);
    let to = in_lane(&harness, "Keys", 0.0);
    drag(&mut harness, from, pos2(from.x, to.y), Modifiers::NONE);
    assert_eq!(clip(&harness, ids.kick).0, ids.drums);
    let to = in_lane(&harness, "Bass", 0.0);
    drag(&mut harness, from, pos2(from.x, to.y), Modifiers::NONE);
    let (track, kick) = clip(&harness, ids.kick);
    assert_eq!((track, kick.start), (ids.bass, Beats::new(2)));
    // A drag is one step to undo.
    assert!(harness.state_mut().document.undo());
    assert_eq!(clip(&harness, ids.kick).0, ids.drums);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn ctrl_drag_copies_and_alt_drag_moves_off_the_grid() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = title(&harness, "Line", 30.0);
    let to = from + vec2(width(&harness, 8.0), 0.0);
    drag(&mut harness, from, to, Modifiers::COMMAND);
    let starts: Vec<_> = harness.state().project().tracks[2]
        .clips
        .iter()
        .map(|clip| clip.start)
        .collect();
    assert_eq!(starts, [Beats::new(16), Beats::new(24)]);
    // The copy is selected.
    let copy = harness.state().project().tracks[2].clips[1].id;
    assert_eq!(selected(&harness), [copy]);
    assert_eq!(harness.state().document.undo_name(), Some("Copy Clips"));

    let from = title(&harness, "Kick", 30.0);
    let to = from + vec2(width(&harness, 1.25), 0.0);
    drag(&mut harness, from, to, Modifiers::ALT);
    let start = clip(&harness, ids.kick).1.start.to_f64();
    assert!((start - 1.25).abs() < 0.01, "{start}");
}

#[test]
fn edges_trim_and_handles_fade() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let rect = harness.get_by_label("Clip Kick").rect();
    let from = pos2(rect.right() - 2.0, rect.center().y);
    let to = from - vec2(width(&harness, 2.0), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let kick = clip(&harness, ids.kick).1;
    assert_eq!((kick.start, kick.end()), (Beats::ZERO, Beats::new(6)));
    assert_eq!(harness.state().document.undo_name(), Some("Trim Clip"));
    let from = pos2(rect.left() + 2.0, rect.center().y);
    let to = from + vec2(width(&harness, 2.0), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let kick = clip(&harness, ids.kick).1;
    assert_eq!((kick.start, kick.end()), (Beats::new(2), Beats::new(6)));
    let ClipContent::Audio(audio) = &kick.content else {
        panic!("Kick is audio");
    };
    // Two beats at 120 BPM is a second of the file.
    assert_eq!(audio.offset, 48_000);

    // The fade-in's handle is at the top of the body, clear of the edge.
    let rect = harness.get_by_label("Clip Kick").rect();
    let theme = Theme::default();
    let handle = pos2(
        rect.left() + theme.edge_grab + theme.fade_handle / 2.0,
        rect.top() + theme.clip_title + theme.fade_handle / 2.0,
    );
    let to = handle + vec2(width(&harness, 1.0), 0.0);
    drag(&mut harness, handle, to, Modifiers::NONE);
    let ClipContent::Audio(audio) = &clip(&harness, ids.kick).1.content else {
        panic!("Kick is audio");
    };
    let fade = audio.fade_in.to_f64();
    assert!((fade - 1.0).abs() < 0.01, "{fade}");
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_over_lanes_selects_time_on_the_tracks_crossed() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = in_lane(&harness, "Drums", 26.3);
    let to = in_lane(&harness, "Keys", 34.4);
    drag(&mut harness, from, to, Modifiers::NONE);
    let editor = harness.state().editor();
    let time = editor.selection.time.as_ref().expect("a time selection");
    assert_eq!(time.range, BeatRange::new(Beats::new(26), Beats::new(34)));
    assert_eq!(
        time.tracks.iter().copied().collect::<Vec<_>>(),
        [ids.drums, ids.keys]
    );
    // Playing starts from it.
    assert_eq!(editor.start_marker, Beats::new(26));
}

#[test]
fn the_ruler_locates_and_its_strip_draws_moves_and_toggles_the_loop() {
    let (project, _) = project();
    let mut harness = harness(project);
    let at = ruler(&harness, 12.3);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(harness.state().player.playhead, Beats::new(12));
    assert_eq!(harness.state().editor().start_marker, Beats::new(12));

    let (from, to) = (strip(&harness, 20.0), strip(&harness, 28.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let project = harness.state().project();
    assert!(project.looping);
    let region = BeatRange::new(Beats::new(20), Beats::new(28));
    assert_eq!(project.loop_region, region);

    let at = strip(&harness, 24.0);
    click(&mut harness, at, Modifiers::NONE);
    assert!(!harness.state().project().looping);
    // Its end moves on its own.
    let (from, to) = (strip(&harness, 28.0), strip(&harness, 32.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let region = BeatRange::new(Beats::new(20), Beats::new(32));
    assert_eq!(harness.state().project().loop_region, region);
    // And all of it, from the middle.
    let (from, to) = (strip(&harness, 24.0), strip(&harness, 28.0));
    drag(&mut harness, from, to, Modifiers::NONE);
    let region = BeatRange::new(Beats::new(24), Beats::new(36));
    assert_eq!(harness.state().project().loop_region, region);
    assert!(harness.state().errors.is_empty());
}

#[test]
fn locators_move_and_a_double_click_renames_them() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let at = harness.get_by_label("Locator Verse").rect();
    let from = pos2(at.center().x, at.top() + 2.0);
    let to = from + vec2(width(&harness, 4.0), 0.0);
    drag(&mut harness, from, to, Modifiers::NONE);
    let verse = harness.state().project().locator(ids.verse).unwrap();
    assert_eq!(verse.at, Beats::new(20));
    let at = harness.get_by_label("Locator Verse").rect();
    let at = pos2(at.center().x, at.top() + 2.0);
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
    let rename = Request::Rename(Named::Locator(ids.verse));
    assert_eq!(harness.state().requests, [rename]);
    // A click on a locator jumps there.
    assert_eq!(harness.state().player.playhead, Beats::new(20));
}

#[test]
fn only_what_is_in_sight_is_laid_out() {
    // A hundred tracks of clips, as the arrangement has to stay smooth with.
    let mut project = Project::new();
    for t in 0..100 {
        let track = project.add_track(format!("T{t}"), TrackKind::Midi);
        for c in 0..20 {
            let range = BeatRange::new(Beats::new(8 * c), Beats::new(8 * c + 8));
            let content = ClipContent::Midi(MidiClip::default());
            let clip = project.new_clip(format!("C{t}.{c}"), range, content);
            project.track_mut(track).unwrap().clips.push(clip);
        }
    }
    let mut harness = harness(project);
    for name in ["Track T0", "Clip C0.0", "Clip C3.2"] {
        harness.get_by_label(name);
    }
    for name in ["Track T99", "Clip C99.0", "Clip C0.19"] {
        assert!(harness.query_by_label(name).is_none(), "{name}");
    }
    // Scrolled to the bottom, the last tracks are there instead.
    let at = in_lane(&harness, "T0", 4.0);
    harness.hover_at(at);
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -10_000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::NONE,
    });
    harness.run();
    harness.get_by_label("Track T99");
    harness.get_by_label("Clip C99.0");
    assert!(harness.query_by_label("Track T0").is_none());
}

#[test]
fn the_wheel_scrolls_and_ctrl_wheel_zooms_around_the_pointer() {
    let (project, ids) = project();
    let mut harness = harness(project);
    for track in [ids.drums, ids.keys, ids.bass] {
        let heights = &mut harness.state_mut().document.editor_mut().track_heights;
        heights.insert(track, 150.0);
    }
    harness.run();
    let at = in_lane(&harness, "Drums", 20.0);
    let wheel = |harness: &mut Harness<'_, State>, delta, mods| {
        harness.hover_at(at);
        harness.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: mods,
        });
        harness.run();
    };
    wheel(&mut harness, vec2(0.0, -50.0), Modifiers::NONE);
    assert_eq!(harness.state().editor().viewport.top, 50.0);
    let sideways = vec2(0.0, -width(&harness, 4.0));
    wheel(&mut harness, sideways, Modifiers::SHIFT);
    assert_eq!(harness.state().editor().viewport.start, Beats::new(4));
    let before = harness.state().editor().viewport;
    let lanes = harness.get_by_label("Arrangement").rect();
    let under = before.beats_at(at.x - lanes.left());
    wheel(&mut harness, vec2(0.0, 100.0), Modifiers::COMMAND);
    let viewport = harness.state().editor().viewport;
    assert!(viewport.zoom > before.zoom * 1.5, "{viewport:?}");
    // What was under the pointer stays there.
    let x = lanes.left() + viewport.x_of(under);
    assert!((x - at.x).abs() < 0.5, "{x} {}", at.x);
}

#[test]
fn header_buttons_mute_solo_and_arm() {
    let (project, ids) = project();
    let mut harness = harness(project);
    harness.get_by_label("Mute track Drums").click();
    harness.run();
    harness.get_by_label("Solo track Keys").click();
    harness.run();
    harness
        .get_by_label("Solo track Bass")
        .click_modifiers(Modifiers::COMMAND);
    harness.run();
    harness.get_by_label("Arm track Bass").click();
    harness.run();
    let project = harness.state().project();
    let track = |id| project.track(id).unwrap();
    assert!(track(ids.drums).muted);
    assert!(track(ids.keys).solo && track(ids.bass).solo);
    assert!(track(ids.bass).armed);
    let mute = harness.get_by_label("Mute track Drums");
    assert_eq!(mute.accesskit_node().toggled(), Some(Toggled::True));
}

#[test]
fn context_menus_offer_commands_for_what_was_clicked() {
    let (project, ids) = project();
    let mut harness = harness(project);
    harness.get_by_label("Clip Kick").click_secondary();
    harness.run();
    for item in [
        "Cut Ctrl+X",
        "Copy Ctrl+C",
        "Split Ctrl+E",
        "Snap to Grid Ctrl+4",
    ] {
        harness.get_by_label(item);
    }
    harness.get_by_label("Delete Backspace, Delete").click();
    harness.run();
    assert!(harness.state().project().find_clip(ids.kick).is_none());

    // On a MIDI track's lane, a new clip where it was clicked.
    let at = in_lane(&harness, "Keys", 30.3);
    harness.event(Event::PointerMoved(at));
    harness.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Secondary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    harness.event(Event::PointerButton {
        pos: at,
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    harness.run();
    harness
        .get_by_label("Insert MIDI Clip Ctrl+Shift+M")
        .click();
    harness.run();
    let keys = &harness.state().project().tracks[1];
    let new = keys
        .clips
        .iter()
        .find(|clip| clip.id != ids.chords)
        .unwrap();
    assert_eq!(new.range(), BeatRange::new(Beats::new(30), Beats::new(34)));

    // A track's header has the track's commands.
    harness.get_by_label("Track Drums").click_secondary();
    harness.run();
    harness.get_by_label("Delete Tracks").click();
    harness.run();
    assert!(harness.state().project().track(ids.drums).is_none());
    assert!(harness.state().errors.is_empty());
}

#[test]
fn dragging_a_tracks_edge_changes_its_height() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let header = harness.get_by_label("Track Drums").rect();
    let from = pos2(header.center().x, header.bottom() - 1.0);
    drag(&mut harness, from, from + vec2(0.0, 40.0), Modifiers::NONE);
    let height = harness.state().editor().track_heights[&ids.drums];
    assert_eq!(height, Theme::default().track_height + 40.0);
}

#[test]
fn escape_cancels_a_drag() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let from = title(&harness, "Kick", 30.0);
    harness.hover_at(from);
    harness.run();
    press(&mut harness, from, true, Modifiers::NONE);
    for step in 1..=3 {
        harness.hover_at(from + vec2(20.0 * step as f32, 0.0));
        harness.run();
    }
    harness.key_press(egui::Key::Escape);
    harness.run();
    press(&mut harness, from + vec2(60.0, 0.0), false, Modifiers::NONE);
    assert_eq!(clip(&harness, ids.kick).1.start, Beats::ZERO);
}

#[test]
fn follow_turns_the_page_while_playing() {
    let (project, _) = project();
    let mut harness = harness(project);
    let player = &mut harness.state_mut().player;
    player.playing = true;
    player.playhead = Beats::new(80);
    harness.run();
    let viewport = harness.state().editor().viewport;
    assert!(viewport.visible().contains(Beats::new(80)), "{viewport:?}");
    // Scrolling away while it plays stops it following, until the
    // playhead's in sight again.
    let at = in_lane(&harness, "Drums", 80.0);
    harness.hover_at(at);
    harness.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -2000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
        modifiers: Modifiers::SHIFT,
    });
    harness.run();
    let away = harness.state().editor().viewport;
    assert!(away.start > Beats::new(90), "{away:?}");
    harness.state_mut().player.playhead = Beats::new(90);
    harness.run();
    assert_eq!(harness.state().editor().viewport, away);
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not read in the tests".to_owned())
    }
}

#[test]
fn files_dropped_on_an_audio_track_go_there() {
    let (project, ids) = project();
    let mut harness = harness(project);
    let at = in_lane(&harness, "Bass", 40.3);
    harness.hover_at(at);
    harness.run();
    let file = PathBuf::from("/music/take.wav");
    harness
        .input_mut()
        .dropped_files
        .push(Arc::new(Dropped(file.clone())));
    harness.run();
    let drop = Action::DropFiles {
        files: vec![file],
        track: Some(ids.bass),
        at: Some(Beats::new(40)),
    };
    assert!(harness.state().actions.contains(&drop));
}

/// Bass, with two takes of its two bars: Take 1, which plays bass.wav from
/// its start and which the Line clip is the comp of, as recording leaves
/// it, and Take 2, which plays it from a second in.
fn with_takes() -> (Project, Ids, [TakeId; 2]) {
    let (mut project, ids) = project();
    let range = BeatRange::new(Beats::new(16), Beats::new(24));
    let mut takes = Vec::new();
    for (name, offset) in [("Take 1", 0), ("Take 2", 48_000)] {
        let mut audio = AudioClip::new("bass.wav");
        audio.offset = offset;
        let clip = project.new_clip("Bass", range, ClipContent::Audio(audio));
        let mut take = project.new_take(name);
        take.clips.push(clip);
        takes.push(take);
    }
    let ids_of = [takes[0].id, takes[1].id];
    project.track_mut(ids.bass).unwrap().takes = takes;
    (project, ids, ids_of)
}

/// Somewhere in the lane of the take called `name`, at `at`.
fn in_take(harness: &Harness<'_, State>, name: &str, at: f64) -> Pos2 {
    let row = harness.get_by_label(name).rect();
    pos2(x_of(harness, at), row.center().y)
}

/// The track's clips, as where they are and where in their files they
/// start.
fn played(harness: &Harness<'_, State>, track: TrackId) -> Vec<(BeatRange, u64)> {
    let track = harness.state().project().track(track).unwrap();
    let mut clips: Vec<_> = track
        .clips
        .iter()
        .map(|clip| match &clip.content {
            ClipContent::Audio(audio) => (clip.range(), audio.offset),
            ClipContent::Midi(_) => (clip.range(), 0),
        })
        .collect();
    clips.sort_by_key(|(range, _)| range.start);
    clips
}

#[test]
fn takes_show_in_lanes_and_dragging_or_clicking_comps_them() {
    let (project, ids, [first, second]) = with_takes();
    let mut harness = harness(project);
    // Hidden until the track shows them.
    assert!(harness.query_by_label("Take 1 of Bass").is_none());
    harness.get_by_label("Track Bass").click_secondary();
    harness.run();
    harness
        .get_by_label("Show/Hide Take Lanes Ctrl+Alt+U")
        .click();
    harness.run();
    let bass = harness.get_by_label("Track Bass").rect();
    let (one, two) = (
        harness.get_by_label("Take 1 of Bass").rect(),
        harness.get_by_label("Take 2 of Bass").rect(),
    );
    let reverb = harness.get_by_label("Track Reverb").rect();
    assert_eq!((one.top(), one.height()), (bass.bottom(), 36.0));
    assert_eq!((two.top(), reverb.top()), (one.bottom(), two.bottom()));
    let bars = |a, b| BeatRange::new(Beats::new(a), Beats::new(b));
    assert_eq!(played(&harness, ids.bass), [(bars(16, 24), 0)]);

    // A drag across Take 2 copies what's under it, on the grid, to the
    // track.
    let from = in_take(&harness, "Take 2 of Bass", 18.1);
    let to = in_take(&harness, "Take 2 of Bass", 19.9);
    drag(&mut harness, from, to, Modifiers::NONE);
    let comped = [
        (bars(16, 18), 0),
        // Two beats into Take 2 at 120 BPM: a second past its start.
        (bars(18, 20), 96_000),
        (bars(20, 24), 96_000),
    ];
    assert_eq!(played(&harness, ids.bass), comped);
    assert_eq!(harness.state().document.undo_name(), Some("Comp"));

    // A click takes a take's whole clip.
    let at = in_take(&harness, "Take 1 of Bass", 21.0);
    click(&mut harness, at, Modifiers::NONE);
    assert_eq!(played(&harness, ids.bass), [(bars(16, 24), 0)]);

    // A double click on a take's name renames it, and its menu deletes it.
    // Long enough after the last click that it isn't a triple click.
    for _ in 0..40 {
        harness.step();
    }
    let header = harness.get_by_label("Take 2 of Bass").rect().center();
    harness.hover_at(header);
    harness.run();
    for pressed in [true, false, true, false] {
        harness.event(Event::PointerButton {
            pos: header,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    harness.run();
    let rename = Request::Rename(Named::Take(second));
    assert_eq!(harness.state().requests, [rename]);
    harness.get_by_label("Take 1 of Bass").click_secondary();
    harness.run();
    harness.get_by_label("Delete Take").click();
    harness.run();
    assert!(harness.state().project().find_take(first).is_none());
    assert!(harness.query_by_label("Take 1 of Bass").is_none());
    assert!(harness.query_by_label("Take 2 of Bass").is_some());
    assert!(harness.state().errors.is_empty());
}
