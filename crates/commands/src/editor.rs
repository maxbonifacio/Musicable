//! What the arrangement, the piano roll and the Sample Editor edit besides
//! the project: the selection, the start marker, the grid, the zoom and how
//! the editing behaves.
//!
//! It lives with the [`Document`](crate::Document), outside the widgets, so
//! commands read and change it just as the UI does, and so will extensions
//! and agents. It isn't saved with the project, and undo leaves it alone.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use musicable_analysis::Analysis;
use musicable_model::clip::{Clip, ClipContent, ClipId, Controller, Note, NoteId};
use musicable_model::param::ParamAddress;
use musicable_model::time::{BeatRange, Beats, TempoMap};
use musicable_model::{Chain, Project, TrackId};
use serde::{Deserialize, Serialize};

use crate::auto_warp::Pending;
use crate::panel;

/// The arrangement's and the piano roll's editing state.
#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    pub selection: Selection,
    /// The track the detail panels show, such as its devices: the one whose
    /// header, clip or mixer strip was clicked last, as Live's selected
    /// track is, or the master track. Always one of the project's tracks,
    /// or the master.
    pub selected_track: Option<Chain>,
    /// Where Play/Stop starts playing from, unless the behaviour says
    /// otherwise: Live's start marker. Clicking in the arrangement moves it.
    pub start_marker: Beats,
    pub grid: Grid,
    pub viewport: Viewport,
    /// Whether the arrangement scrolls to keep the playhead in sight while
    /// the transport plays.
    pub follow: bool,
    /// The height of each track in the arrangement, and of the master's
    /// row, in points, where it isn't the theme's.
    pub track_heights: BTreeMap<Chain, f32>,
    pub behaviour: Behaviour,
    /// What the app has read about the audio files that clips play, by the
    /// path the clips give.
    pub files: BTreeMap<String, AudioFile>,
    /// What analysis found in those files, by the same paths, as far as
    /// the app has analysed them.
    pub analyses: BTreeMap<String, Arc<Analysis>>,
    /// Files dropped into the song that auto-warp waits for the analyses
    /// of, a drop at a time.
    pub auto_warp: Vec<Pending>,
    /// The engine's sample rate: an audio file that isn't in
    /// [`files`](Self::files) yet is taken to have it.
    pub sample_rate: u32,
    /// What Cut and Copy took, for Paste.
    pub clipboard: Option<Clipboard>,
    /// The tracks whose takes the arrangement shows in lanes under them.
    pub take_lanes: BTreeSet<TrackId>,
    /// The channels whose automation the arrangement shows, each with the
    /// parameters it shows lanes for under the channel, top to bottom:
    /// at least one each, whether or not they're automated yet.
    pub automation: BTreeMap<Chain, Vec<ParamAddress>>,
    /// Which view the edit commands and the keys act on.
    pub focus: Focus,
    /// What the piano roll shows and has selected.
    pub piano_roll: PianoRoll,
    /// What the Sample Editor shows and has selected.
    pub sample_editor: SampleEditor,
    /// The panel of the editor that fits the clip selected or opened last:
    /// the piano roll's for a MIDI clip, the Sample Editor's for an audio
    /// clip. The piano roll's keys show and hide it.
    pub clip_editor: &'static str,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            selection: Selection::default(),
            selected_track: None,
            start_marker: Beats::ZERO,
            grid: Grid::default(),
            viewport: Viewport::default(),
            follow: true,
            track_heights: BTreeMap::new(),
            behaviour: Behaviour::default(),
            files: BTreeMap::new(),
            analyses: BTreeMap::new(),
            auto_warp: Vec::new(),
            sample_rate: 48_000,
            clipboard: None,
            take_lanes: BTreeSet::new(),
            automation: BTreeMap::new(),
            focus: Focus::default(),
            piano_roll: PianoRoll::default(),
            sample_editor: SampleEditor::default(),
            clip_editor: panel::PIANO_ROLL,
        }
    }
}

impl Editor {
    /// The sample rate of the audio file at `path`, as a clip names it.
    pub fn sample_rate_of(&self, path: &str) -> u32 {
        self.files
            .get(path)
            .map_or(self.sample_rate, |file| file.sample_rate)
    }

    /// The stretch of the song the selection covers: the time selection, or
    /// else from the start of the first selected clip to the end of the
    /// last.
    pub fn selected_range(&self, project: &Project) -> Option<BeatRange> {
        if let Some(time) = &self.selection.time {
            return Some(time.range);
        }
        let selected = &self.selection.clips;
        if selected.is_empty() {
            return None;
        }
        let clips = project.tracks.iter().flat_map(|t| &t.clips);
        clips.filter(|clip| selected.contains(&clip.id)).fold(
            None,
            |range: Option<BeatRange>, clip| {
                Some(match range {
                    Some(range) => {
                        BeatRange::new(range.start.min(clip.start), range.end.max(clip.end()))
                    }
                    None => clip.range(),
                })
            },
        )
    }

    /// Shows `clip` in the piano roll, if it's a MIDI clip, as selecting it
    /// in the arrangement does. A clip it doesn't show already comes into
    /// sight with no notes selected. Returns whether it's a MIDI clip.
    pub fn show_in_piano_roll(&mut self, clip: &Clip) -> bool {
        let ClipContent::Midi(midi) = &clip.content else {
            return false;
        };
        let roll = &mut self.piano_roll;
        if roll.clip != Some(clip.id) {
            roll.clip = Some(clip.id);
            roll.notes.clear();
            roll.show(clip, &midi.notes);
        }
        self.clip_editor = panel::PIANO_ROLL;
        true
    }

    /// Shows `clip` in the piano roll, if it's a MIDI clip, and gives the
    /// piano roll the focus.
    pub fn open_in_piano_roll(&mut self, clip: &Clip) {
        if self.show_in_piano_roll(clip) {
            self.focus = Focus::PIANO_ROLL;
        }
    }

    /// Shows `clip` in the Sample Editor, if it's an audio clip, as
    /// selecting it in the arrangement does. A clip it doesn't show already
    /// comes into sight with no markers selected. Returns whether it's an
    /// audio clip.
    pub fn show_in_sample_editor(&mut self, clip: &Clip) -> bool {
        if !matches!(clip.content, ClipContent::Audio(_)) {
            return false;
        }
        let shown = &mut self.sample_editor;
        if shown.clip != Some(clip.id) {
            shown.clip = Some(clip.id);
            shown.markers.clear();
            shown.time = None;
            shown.viewport.show(clip.range());
        }
        self.clip_editor = panel::SAMPLE_EDITOR;
        true
    }

    /// Shows `clip` in the Sample Editor, if it's an audio clip, and gives
    /// the Sample Editor the focus.
    pub fn open_in_sample_editor(&mut self, clip: &Clip) {
        if self.show_in_sample_editor(clip) {
            self.focus = Focus::SAMPLE_EDITOR;
        }
    }

    /// Gives the arrangement the focus, as selecting in it does, unless a
    /// panel that edits its selection, such as the mixer, has it.
    pub fn focus_arrangement(&mut self) {
        if self.focus.edits() != Edits::Arrangement {
            self.focus = Focus::ARRANGEMENT;
        }
    }

    /// Forgets what `project` doesn't have any more: selected clips and
    /// tracks that went, their heights and their take lanes, and the piano
    /// roll's clip and notes.
    pub fn prune(&mut self, project: &Project) {
        let tracks: BTreeSet<TrackId> = project.tracks.iter().map(|t| t.id).collect();
        let selection = &mut self.selection;
        if !selection.clips.is_empty() {
            let clips: BTreeSet<ClipId> = project
                .tracks
                .iter()
                .flat_map(|t| &t.clips)
                .map(|c| c.id)
                .collect();
            selection.clips.retain(|id| clips.contains(id));
        }
        selection.tracks.retain(|id| tracks.contains(id));
        if let Some(time) = &mut selection.time {
            time.tracks.retain(|id| tracks.contains(id));
            if time.tracks.is_empty() && !time.master {
                selection.time = None;
            }
        }
        if let Some(Chain::Track(id)) = self.selected_track
            && !tracks.contains(&id)
        {
            self.selected_track = None;
        }
        (self.track_heights).retain(|chain, _| chain.track().is_none_or(|id| tracks.contains(&id)));
        self.take_lanes.retain(|id| tracks.contains(id));
        self.automation.retain(|&chain, params| {
            params.retain(|&param| project.param_chain(param) == Some(chain));
            !params.is_empty()
        });
        let roll = &mut self.piano_roll;
        let shown = roll
            .clip
            .and_then(|id| match &project.find_clip(id)?.1.content {
                ClipContent::Midi(midi) => Some(midi),
                ClipContent::Audio(_) => None,
            });
        match shown {
            Some(midi) => {
                if !roll.notes.is_empty() {
                    let notes: BTreeSet<NoteId> = midi.notes.iter().map(|n| n.id).collect();
                    roll.notes.retain(|id| notes.contains(id));
                }
            }
            None => {
                roll.clip = None;
                roll.notes.clear();
                if self.focus == Focus::PIANO_ROLL {
                    self.focus = Focus::ARRANGEMENT;
                }
            }
        }
        let shown = &mut self.sample_editor;
        let audio = shown
            .clip
            .and_then(|id| match &project.find_clip(id)?.1.content {
                ClipContent::Audio(audio) => Some(audio),
                ClipContent::Midi(_) => None,
            });
        match audio {
            Some(audio) => match &audio.warp {
                Some(warp) => (shown.markers).retain(|&beat| warp.marker_at(beat).is_some()),
                None => {
                    shown.markers.clear();
                    shown.time = None;
                }
            },
            None => {
                shown.clip = None;
                shown.markers.clear();
                shown.time = None;
                if self.focus == Focus::SAMPLE_EDITOR {
                    self.focus = Focus::ARRANGEMENT;
                }
            }
        }
    }
}

/// The panel with the focus, by id: where the edit commands, such as Copy
/// and Delete, and the keys act. Clicking in a panel gives it the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Focus(&'static str);

impl Focus {
    pub const ARRANGEMENT: Self = Self(panel::ARRANGEMENT);
    pub const MIXER: Self = Self(panel::MIXER);
    pub const DEVICES: Self = Self(panel::DEVICES);
    pub const PIANO_ROLL: Self = Self(panel::PIANO_ROLL);
    pub const SAMPLE_EDITOR: Self = Self(panel::SAMPLE_EDITOR);
    pub const BROWSER: Self = Self(panel::BROWSER);
    pub const HISTORY: Self = Self(panel::HISTORY);

    /// The focus on the panel with id `id`, if there's one.
    pub fn on(id: &str) -> Option<Self> {
        panel::find(id).map(Self)
    }

    /// The id of the panel with the focus.
    pub fn panel(self) -> &'static str {
        self.0
    }

    /// What the edit commands act on while the panel has the focus.
    pub fn edits(self) -> Edits {
        match self.0 {
            panel::ARRANGEMENT | panel::MIXER => Edits::Arrangement,
            panel::PIANO_ROLL => Edits::Notes,
            panel::SAMPLE_EDITOR => Edits::Markers,
            _ => Edits::Nothing,
        }
    }
}

impl Default for Focus {
    fn default() -> Self {
        Self::ARRANGEMENT
    }
}

/// What the edit commands, such as Copy and Delete, act on while a panel
/// has the focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edits {
    /// The arrangement's selected clips, time or tracks: in the arrangement,
    /// and in the mixer, which shows its tracks, as Live's Arrangement View
    /// has its mixer.
    Arrangement,
    /// The piano roll's selected notes.
    Notes,
    /// The Sample Editor's selected warp markers: Delete and Select All act
    /// on them, and the clipboard leaves them alone.
    Markers,
    /// Nothing: the panel has nothing selected that they take.
    Nothing,
}

/// What the piano roll edits: the MIDI clip it shows, its selected notes,
/// its zoom and scrolling, and what Cut and Copy took from it.
#[derive(Debug, Clone, PartialEq)]
pub struct PianoRoll {
    /// The MIDI clip it shows: the one selected or opened last. Always one
    /// of the tracks' clips.
    pub clip: Option<ClipId>,
    /// The selected notes of that clip.
    pub notes: BTreeSet<NoteId>,
    /// The zoom and the scrolling sideways, in song positions, as the
    /// arrangement's, with `top` how far down the keys are scrolled from the
    /// top of the highest one, in points.
    pub viewport: Viewport,
    /// How tall each key's row is, in points.
    pub key_height: f32,
    /// How tall the rows of keys are together on screen, as the piano roll
    /// last drew them, for scrolling notes into sight.
    pub height: f32,
    /// Whether clicks and drags on the grid draw notes, and clicks on notes
    /// delete them: Live's Draw Mode.
    pub draw: bool,
    /// What Cut and Copy took from it, for Paste.
    pub clipboard: Option<NoteClipboard>,
    /// What the lane under the notes shows.
    pub lane: Lane,
}

impl Default for PianoRoll {
    /// Four bars of 4/4 across a thousand points, from the song's start, a
    /// few octaves around middle C in sight.
    fn default() -> Self {
        let key_height = 12.0;
        let height = 240.0;
        Self {
            clip: None,
            notes: BTreeSet::new(),
            viewport: Viewport {
                start: Beats::ZERO,
                zoom: 62.5,
                width: 1000.0,
                top: Self::top_for(60.0, key_height, height),
            },
            key_height,
            height,
            draw: false,
            clipboard: None,
            lane: Lane::default(),
        }
    }
}

impl PianoRoll {
    /// The shortest and the tallest a key's row gets, in points.
    pub const MIN_KEY_HEIGHT: f32 = 4.0;
    pub const MAX_KEY_HEIGHT: f32 = 40.0;

    /// Shows `clip`, whose notes are `notes`: its whole length across, and
    /// the keys its notes are on, or middle C, in the middle.
    pub fn show(&mut self, clip: &Clip, notes: &[Note]) {
        self.viewport.show(clip.range());
        let keys = notes.iter().map(|n| f32::from(n.key));
        let (low, high) = keys.fold((f32::MAX, f32::MIN), |(low, high), key| {
            (low.min(key), high.max(key))
        });
        let middle = if low <= high {
            (low + high) / 2.0
        } else {
            60.0
        };
        self.viewport.top = Self::top_for(middle, self.key_height, self.height);
    }

    /// Zooms and scrolls to show `range`, song positions, across, and the
    /// keys from `low` to `high` around the middle.
    pub fn show_notes(&mut self, range: BeatRange, low: u8, high: u8) {
        self.viewport.show(range);
        let middle = (f32::from(low) + f32::from(high)) / 2.0;
        self.viewport.top = Self::top_for(middle, self.key_height, self.height);
    }

    /// How far down to scroll for `key` to be in the middle of rows
    /// `height` points tall together, each `key_height` points tall.
    fn top_for(key: f32, key_height: f32, height: f32) -> f32 {
        let rows = f32::from(Note::MAX_KEY) + 1.0;
        let middle = (rows - key - 0.5) * key_height;
        (middle - height / 2.0).clamp(0.0, (rows * key_height - height).max(0.0))
    }
}

/// What the Sample Editor edits: the audio clip it shows, its selected warp
/// markers and time, and its zoom and scrolling. Clicking its waveform
/// moves the start marker, as clicking the arrangement does, and Insert
/// Warp Marker adds a marker there.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleEditor {
    /// The audio clip it shows: the one selected or opened last. Always one
    /// of the tracks' clips.
    pub clip: Option<ClipId>,
    /// The selected warp markers of that clip, by their beats in its own
    /// time, which are each marker's own.
    pub markers: BTreeSet<Beats>,
    /// The stretch of the warped clip's own time selected, never empty: its
    /// transients are the selected ones, which Insert Warp Marker puts
    /// markers at.
    pub time: Option<BeatRange>,
    /// The zoom and the scrolling sideways, in song positions, as the
    /// arrangement's.
    pub viewport: Viewport,
}

impl Default for SampleEditor {
    /// Four bars of 4/4 across a thousand points, from the song's start.
    fn default() -> Self {
        Self {
            clip: None,
            markers: BTreeSet::new(),
            time: None,
            viewport: Viewport {
                zoom: 62.5,
                ..Viewport::default()
            },
        }
    }
}

/// What the lane under the piano roll's notes shows and edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lane {
    /// How hard each note starts.
    #[default]
    Velocity,
    /// How the clip moves a controller, such as the mod wheel.
    Controller(Controller),
}

/// Notes that Cut or Copy took in the piano roll.
#[derive(Debug, Clone, PartialEq)]
pub struct NoteClipboard {
    /// In their clip's own time.
    pub notes: Vec<Note>,
    /// When they sound together, in the same time: from the first note's
    /// start to the last note's end. Paste puts its start at the start
    /// marker.
    pub range: BeatRange,
}

/// What's selected in the arrangement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    pub clips: BTreeSet<ClipId>,
    /// Tracks selected by their headers.
    pub tracks: BTreeSet<TrackId>,
    /// A stretch of time on some tracks, which Split, Delete, Duplicate and
    /// Copy act on as they do on clips.
    pub time: Option<TimeSelection>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && self.tracks.is_empty() && self.time.is_none()
    }

    /// Just the clips `clips`.
    pub fn clips(clips: impl IntoIterator<Item = ClipId>) -> Self {
        Self {
            clips: clips.into_iter().collect(),
            ..Self::default()
        }
    }
}

/// A stretch of time on some tracks, or on the master, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeSelection {
    /// Never empty.
    pub range: BeatRange,
    /// Empty only when it's on the master alone.
    pub tracks: BTreeSet<TrackId>,
    /// Whether it's on the master too: the master has no clips for the
    /// edit commands to act on, but its stretch is looped, zoomed to and
    /// exported as the tracks' is.
    pub master: bool,
}

/// The arrangement's horizontal zoom and where it's scrolled to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// The song position at the left edge of the timeline: the song's
    /// start, or later.
    pub start: Beats,
    /// How many points a beat takes.
    pub zoom: f64,
    /// How wide the timeline is, in points, as the arrangement last drew
    /// it, for zooming to fit.
    pub width: f32,
    /// How far down the tracks are scrolled, in points.
    pub top: f32,
}

impl Default for Viewport {
    /// Sixteen bars of 4/4 across a timeline a thousand points wide.
    fn default() -> Self {
        Self {
            start: Beats::ZERO,
            zoom: 15.625,
            width: 1000.0,
            top: 0.0,
        }
    }
}

impl Viewport {
    /// The furthest out the arrangement zooms: about an hour across a wide
    /// window at 120 BPM.
    pub const MIN_ZOOM: f64 = 0.2;
    /// The furthest in: single samples, a few points apart.
    pub const MAX_ZOOM: f64 = 50_000.0;

    /// How far `at` is from the timeline's left edge, in points.
    pub fn x_of(&self, at: Beats) -> f32 {
        ((at - self.start).to_f64() * self.zoom) as f32
    }

    /// The song position `x` points from the timeline's left edge.
    pub fn beats_at(&self, x: f32) -> Beats {
        self.start + Beats::from_f64(f64::from(x) / self.zoom)
    }

    /// How long `points` points are.
    pub fn beats_in(&self, points: f32) -> Beats {
        Beats::from_f64(f64::from(points) / self.zoom)
    }

    /// The part of the song the timeline shows.
    pub fn visible(&self) -> BeatRange {
        BeatRange::new(self.start, self.beats_at(self.width))
    }

    /// Zooms by `factor`, keeping `at` where it is on screen, unless that
    /// would show what's before the song's start.
    pub fn zoom_around(&mut self, factor: f64, at: Beats) {
        let x = self.x_of(at);
        self.zoom = (self.zoom * factor).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.start = (at - Beats::from_f64(f64::from(x) / self.zoom)).max(Beats::ZERO);
    }

    /// Zooms and scrolls to show `range` across the timeline, with a little
    /// room either side.
    pub fn show(&mut self, range: BeatRange) {
        let length = range.len().to_f64();
        if length <= 0.0 {
            return;
        }
        let margin = f64::from(self.width) / 20.0;
        let room = (f64::from(self.width) - 2.0 * margin).max(1.0);
        self.zoom = (room / length).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
        self.start = (range.start - Beats::from_f64(margin / self.zoom)).max(Beats::ZERO);
    }

    /// Scrolls so `at` is in sight, if it isn't: a page on, as Live's Follow
    /// turns pages.
    pub fn follow(&mut self, at: Beats) {
        let visible = self.visible();
        if at < visible.start || at >= visible.end {
            self.start = (at - self.beats_in(self.width / 20.0)).max(Beats::ZERO);
        }
    }
}

/// The arrangement's grid, which clips snap to.
///
/// Its lines step by whole bars, two, four or eight of them, or by note
/// values within a bar, from a half note down to a 1/64. The adaptive grid
/// picks the step from the zoom, so its lines stay
/// [`MIN_SPACING`](Self::MIN_SPACING) apart. Lines within a bar start again
/// at each bar line, so they fit any time signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    /// A step the grid keeps at any zoom, or `None` for the adaptive grid.
    pub fixed: Option<Step>,
    /// How much wider the adaptive grid's lines are: each one up doubles
    /// the spacing. From -2, the narrowest, to 2, the widest, as Live's
    /// five widths.
    pub density: i8,
    /// Whether steps within a bar are triplets: two-thirds as long.
    pub triplet: bool,
    /// Whether clips and the time selection snap to the lines.
    pub snap: bool,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            fixed: None,
            density: 0,
            triplet: false,
            snap: true,
        }
    }
}

/// A grid step: 2ⁿ bars for n from 0 to 3, or a 1/2⁻ⁿ note for n from -1 (a
/// half note) to -6 (a 1/64 note).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Step(i8);

impl Step {
    pub const WIDEST: Step = Step(3);
    pub const BAR: Step = Step(0);
    pub const QUARTER: Step = Step(-2);
    pub const SIXTEENTH: Step = Step(-4);
    pub const NARROWEST: Step = Step(-6);

    /// The step `n`, kept within the widest and the narrowest.
    pub fn new(n: i8) -> Self {
        Self(n.clamp(Self::NARROWEST.0, Self::WIDEST.0))
    }

    pub fn get(self) -> i8 {
        self.0
    }

    /// The step's bars, if it's a whole number of them.
    pub fn bars(self) -> Option<i64> {
        (self.0 >= 0).then(|| 1 << self.0)
    }

    /// How long a step within a bar is, without triplets.
    fn note(self) -> Beats {
        // A whole note is four beats.
        Beats::from_ratio(4, 1 << -self.0)
    }

    /// The step's name, as the arrangement shows it: "1 Bar", "1/16".
    pub fn name(self, triplet: bool) -> String {
        match self.bars() {
            Some(1) => "1 Bar".to_owned(),
            Some(bars) => format!("{bars} Bars"),
            None if triplet => format!("1/{}T", 1 << -self.0),
            None => format!("1/{}", 1 << -self.0),
        }
    }
}

impl Grid {
    /// The closest the adaptive grid's lines get at density 0, in points.
    pub const MIN_SPACING: f64 = 16.0;

    /// The step at `zoom` points per beat, in a bar of `bar` beats.
    pub fn step(&self, zoom: f64, bar: Beats) -> Step {
        if let Some(step) = self.fixed {
            return step;
        }
        let spacing = Self::MIN_SPACING * 2_f64.powi(i32::from(self.density.clamp(-2, 2)));
        let mut step = Step::NARROWEST;
        while step < Step::WIDEST && self.length(step, bar).to_f64() * zoom < spacing {
            step = Step::new(step.0 + 1);
        }
        step
    }

    /// How long `step` is in a bar of `bar` beats.
    fn length(&self, step: Step, bar: Beats) -> Beats {
        match step.bars() {
            Some(bars) => bar * bars,
            None if self.triplet => Beats::from_ticks(step.note().ticks() * 2 / 3),
            None => step.note(),
        }
    }

    /// The grid's lines in `range` at `zoom`, in order.
    pub fn lines(&self, tempo: &TempoMap, range: BeatRange, zoom: f64) -> Vec<Beats> {
        let mut lines = Vec::new();
        let mut bar = tempo.bar_at(range.start).bar;
        loop {
            let start = tempo.bar_start(bar);
            if start >= range.end {
                break;
            }
            let next = tempo.bar_start(bar + 1);
            let step = self.step(zoom, next - start);
            match step.bars() {
                Some(bars) => {
                    if bar.rem_euclid(bars) == 0 && start >= range.start {
                        lines.push(start);
                    }
                }
                None => {
                    let length = self.length(step, next - start);
                    let mut at = start;
                    while at < next && at < range.end {
                        if at >= range.start {
                            lines.push(at);
                        }
                        at += length;
                    }
                }
            }
            bar += 1;
        }
        lines
    }

    /// The line nearest `at` at `zoom`, or `at` itself while snapping is off.
    pub fn snap(&self, tempo: &TempoMap, at: Beats, zoom: f64) -> Beats {
        if !self.snap {
            return at;
        }
        self.nearest(tempo, at, zoom)
    }

    /// The line nearest `at` at `zoom`, whether snapping is on or not.
    pub fn nearest(&self, tempo: &TempoMap, at: Beats, zoom: f64) -> Beats {
        let (before, after) = self.around(tempo, at, zoom);
        if at - before <= after - at {
            before
        } else {
            after
        }
    }

    /// The line at `at` or the last before it, at `zoom`, whether snapping
    /// is on or not: where a note drawn at `at` starts.
    pub fn floor(&self, tempo: &TempoMap, at: Beats, zoom: f64) -> Beats {
        self.around(tempo, at, zoom).0
    }

    /// The step of the grid `at` is in, at `zoom`, whether snapping is on or
    /// not: from the line at `at` or the last before it, up to the next.
    /// Drawing in the piano roll's controller lane fills steps.
    pub fn cell(&self, tempo: &TempoMap, at: Beats, zoom: f64) -> BeatRange {
        let (before, after) = self.around(tempo, at, zoom);
        BeatRange::new(before, after)
    }

    /// The line at `at` or the last before it, and the next, at `zoom`.
    fn around(&self, tempo: &TempoMap, at: Beats, zoom: f64) -> (Beats, Beats) {
        let bar = tempo.bar_at(at).bar;
        let start = tempo.bar_start(bar);
        let next = tempo.bar_start(bar + 1);
        let step = self.step(zoom, next - start);
        match step.bars() {
            // Steps of several bars count from bar 0.
            Some(bars) => {
                let first = bar - bar.rem_euclid(bars);
                (tempo.bar_start(first), tempo.bar_start(first + bars))
            }
            // Steps within a bar count from its start, and the next bar
            // line ends the last one.
            None => {
                let length = self.length(step, next - start).ticks();
                let into = (at - start).ticks();
                let before = start + Beats::from_ticks(into - into % length);
                (before, (before + Beats::from_ticks(length)).min(next))
            }
        }
    }

    /// How far apart the grid's lines are at `zoom`, in points, in a bar of
    /// `bar` beats.
    pub fn spacing(&self, zoom: f64, bar: Beats) -> f64 {
        self.length(self.step(zoom, bar), bar).to_f64() * zoom
    }

    /// How long the grid's step is in the bar at `at`, at `zoom`: what
    /// nudging notes moves them by, and how long a note drawn there is.
    pub fn step_at(&self, tempo: &TempoMap, at: Beats, zoom: f64) -> Beats {
        let bar = tempo.bar_at(at).bar;
        let length = tempo.bar_start(bar + 1) - tempo.bar_start(bar);
        self.length(self.step(zoom, length), length)
    }
}

/// How editing behaves where people's habits differ. Kept as settings, not
/// built in, so profiles can follow other DAWs: the app reads it from its
/// settings file, as `[behaviour]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Behaviour {
    /// Where Play/Stop starts playing from.
    pub play_from: PlayFrom,
    /// What the piano roll calls middle C, MIDI note 60.
    pub middle_c: MiddleC,
    /// Auto-warp's rules, each on or off (see [`auto_warp`](crate::auto_warp)).
    /// Recordings follow the song's tempo as it was when they were made.
    pub warp_recordings: bool,
    /// Loops of whole bars are warped to them.
    pub warp_loops: bool,
    /// Files at a steady tempo are warped from their first downbeat.
    pub warp_steady: bool,
    /// Files whose tempo drifts get a marker on every downbeat.
    pub warp_drifting: bool,
    /// Sounds without a pulse stay unwarped, and land with their hit, or a
    /// riser's peak, where they're dropped.
    pub sync_one_shots: bool,
    /// The first file with a pulse dropped into an empty song gives it its
    /// tempo and meter.
    pub tempo_from_first_file: bool,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            play_from: PlayFrom::default(),
            middle_c: MiddleC::default(),
            warp_recordings: true,
            warp_loops: true,
            warp_steady: true,
            warp_drifting: true,
            sync_one_shots: true,
            tempo_from_first_file: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayFrom {
    /// The start marker, as in Live: playing again starts where the last
    /// play did.
    #[default]
    StartMarker,
    /// Wherever the playhead stopped.
    Playhead,
}

/// What middle C, MIDI note 60, is called, which names every key: DAWs
/// count octaves from different places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiddleC {
    /// C3, as in Live, Cubase, Logic and Bitwig.
    #[default]
    C3,
    /// C4, as in scientific pitch notation and Reaper.
    C4,
    /// C5, as in FL Studio.
    C5,
}

impl MiddleC {
    /// The name of the MIDI note number `key`, such as "C3" or "F#-1".
    pub fn name(self, key: u8) -> String {
        const NAMES: [&str; 12] = [
            "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
        ];
        let octave = i32::from(key / 12)
            + match self {
                MiddleC::C3 => -2,
                MiddleC::C4 => -1,
                MiddleC::C5 => 0,
            };
        format!("{}{octave}", NAMES[usize::from(key % 12)])
    }
}

/// What the app read in an audio file's header.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioFile {
    pub sample_rate: u32,
    pub frames: u64,
    pub channels: usize,
    /// The tempo its loop data gives, as a WAV file's ACID chunk does, if
    /// it has any: what Warp starts it at.
    pub tempo: Option<f64>,
}

/// Clips that Cut or Copy took.
#[derive(Debug, Clone, PartialEq)]
pub struct Clipboard {
    /// Each clip, with how many tracks below the first track taken from it
    /// was on.
    pub clips: Vec<(usize, Clip)>,
    /// The stretch of time taken. Paste puts its start at the start marker.
    pub range: BeatRange,
}

#[cfg(test)]
mod tests {
    use musicable_model::TrackKind;
    use musicable_model::clip::{ClipContent, MidiClip};
    use musicable_model::time::TimeSignature;

    use super::*;

    #[test]
    fn the_viewport_turns_positions_into_points_and_back() {
        let mut viewport = Viewport {
            start: Beats::new(4),
            zoom: 10.0,
            width: 400.0,
            top: 0.0,
        };
        assert_eq!(viewport.x_of(Beats::new(6)), 20.0);
        assert_eq!(viewport.beats_at(20.0), Beats::new(6));
        assert_eq!(
            viewport.visible(),
            BeatRange::new(Beats::new(4), Beats::new(44))
        );
        // Zooming keeps the position under the pointer where it is.
        viewport.zoom_around(2.0, Beats::new(6));
        assert_eq!(viewport.x_of(Beats::new(6)), 20.0);
        assert_eq!(viewport.zoom, 20.0);
        viewport.zoom_around(1e9, Beats::new(6));
        assert_eq!(viewport.zoom, Viewport::MAX_ZOOM);

        viewport.show(BeatRange::new(Beats::new(16), Beats::new(32)));
        let visible = viewport.visible();
        assert!(visible.start < Beats::new(16) && visible.end > Beats::new(32));
        assert!((viewport.x_of(Beats::new(16)) - 20.0).abs() < 0.01);

        viewport.follow(Beats::new(100));
        assert!(viewport.visible().contains(Beats::new(100)));
    }

    #[test]
    fn the_adaptive_grid_keeps_its_lines_apart() {
        let grid = Grid::default();
        let bar = Beats::new(4);
        // At 16 points a beat, a beat is the narrowest step that fits.
        assert_eq!(grid.step(16.0, bar), Step::QUARTER);
        assert_eq!(grid.step(64.0, bar), Step::SIXTEENTH);
        assert_eq!(grid.step(4.0, bar), Step::BAR);
        assert_eq!(grid.step(0.001, bar), Step::WIDEST);
        let wide = Grid { density: 2, ..grid };
        assert_eq!(wide.step(64.0, bar), Step::QUARTER);
        let fixed = Grid {
            fixed: Some(Step::new(-3)),
            ..grid
        };
        assert_eq!(fixed.step(0.001, bar), Step::new(-3));
        assert_eq!(Step::new(-9), Step::NARROWEST);
        let names: Vec<_> = [3, 1, 0, -2, -4].map(|n| Step::new(n).name(false)).into();
        assert_eq!(names, ["8 Bars", "2 Bars", "1 Bar", "1/4", "1/16"]);
        assert_eq!(Step::new(-3).name(true), "1/8T");
    }

    #[test]
    fn grid_lines_start_again_at_each_bar() {
        let mut tempo = TempoMap::default();
        tempo.insert_signature(1, TimeSignature::new(7, 8).unwrap());
        let grid = Grid::default();
        // Quarter notes: four in the 4/4 bar, four in the bar of 7/8, the
        // last one an eighth short.
        let lines = grid.lines(&tempo, BeatRange::new(Beats::ZERO, Beats::new(8)), 16.0);
        let beats = |n| Beats::from_ratio(n, 2);
        assert_eq!(lines, [0, 2, 4, 6, 8, 10, 12, 14, 15].map(beats).to_vec());
        assert_eq!(grid.snap(&tempo, beats(13), 16.0), beats(12));
        // The bar's last step is an eighth short, so its bar line is nearer.
        let near_the_end = Beats::from_ratio(73, 10);
        assert_eq!(grid.snap(&tempo, near_the_end, 16.0), beats(15));
        // Bar lines snap to themselves: the bars after are 7/8 too.
        assert_eq!(grid.snap(&tempo, beats(29), 16.0), beats(29));
        let off = Grid {
            snap: false,
            ..grid
        };
        assert_eq!(off.snap(&tempo, beats(13), 16.0), beats(13));
        assert_eq!(off.nearest(&tempo, beats(13), 16.0), beats(12));
        // The step a place is in, which the bar line ends.
        assert_eq!(
            grid.cell(&tempo, beats(13), 16.0),
            BeatRange::new(beats(12), beats(14))
        );
        assert_eq!(
            grid.cell(&tempo, near_the_end, 16.0),
            BeatRange::new(beats(14), beats(15))
        );
        assert_eq!(
            grid.cell(&tempo, beats(15), 16.0),
            BeatRange::new(beats(15), beats(17))
        );
        // A note drawn there starts at the line before.
        assert_eq!(off.floor(&tempo, near_the_end, 16.0), beats(14));
        assert_eq!(off.floor(&tempo, beats(14), 16.0), beats(14));

        // Triplets, and steps of several bars.
        let triplets = Grid {
            triplet: true,
            ..grid
        };
        let lines = triplets.lines(
            &TempoMap::default(),
            BeatRange::new(Beats::ZERO, Beats::new(2)),
            24.0,
        );
        assert_eq!(lines, [0, 2, 4].map(|n| Beats::from_ratio(n, 3)).to_vec());
        let bars = Grid {
            fixed: Some(Step::new(1)),
            ..grid
        };
        let tempo = TempoMap::default();
        let lines = bars.lines(&tempo, BeatRange::new(Beats::new(1), Beats::new(20)), 16.0);
        assert_eq!(lines, [Beats::new(8), Beats::new(16)]);
        assert_eq!(bars.snap(&tempo, Beats::new(13), 16.0), Beats::new(16));
        assert_eq!(bars.snap(&tempo, Beats::new(11), 16.0), Beats::new(8));
    }

    #[test]
    fn pruning_forgets_what_went() {
        let mut project = Project::new();
        let keys = project.add_track("Keys", TrackKind::Midi);
        let gone = project.add_track("Gone", TrackKind::Midi);
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let clip = project.new_clip("A", range, ClipContent::Midi(MidiClip::default()));
        let kept = clip.id;
        project.track_mut(keys).unwrap().clips.push(clip);
        let mut editor = Editor {
            selection: Selection {
                clips: [kept, ClipId(9)].into(),
                tracks: [keys, gone].into(),
                time: Some(TimeSelection {
                    range,
                    tracks: [gone].into(),
                    master: false,
                }),
            },
            selected_track: Some(Chain::Track(gone)),
            ..Editor::default()
        };
        editor.track_heights.insert(Chain::Track(gone), 80.0);
        editor.track_heights.insert(Chain::Master, 90.0);
        project.tracks.retain(|t| t.id != gone);
        editor.prune(&project);
        assert_eq!(editor.selection.clips, [kept].into());
        assert_eq!(editor.selection.tracks, [keys].into());
        assert_eq!(editor.selection.time, None);
        assert_eq!(editor.selected_track, None);
        // The master's stays.
        let heights: Vec<_> = editor.track_heights.iter().collect();
        assert_eq!(heights, [(&Chain::Master, &90.0)]);
        // A time selection on the master stays without its tracks.
        let time = TimeSelection {
            range,
            tracks: [keys].into(),
            master: true,
        };
        editor.selection.time = Some(time.clone());
        project.tracks.clear();
        editor.prune(&project);
        let left = TimeSelection {
            tracks: BTreeSet::new(),
            ..time
        };
        assert_eq!(editor.selection.time, Some(left));

        editor.files.insert(
            "a.wav".into(),
            AudioFile {
                sample_rate: 44_100,
                frames: 10,
                channels: 2,
                tempo: None,
            },
        );
        assert_eq!(editor.sample_rate_of("a.wav"), 44_100);
        assert_eq!(editor.sample_rate_of("b.wav"), 48_000);
    }
}
