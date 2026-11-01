//! Getting around the timelines: each panel's history of views, which Zoom
//! Back and Zoom Forward step through, whether Follow is suspended in it,
//! and the sizes the arrangement fits its tracks to.
//!
//! A step of the history is a gesture, not each change the gesture makes:
//! a wheel burst, a pinch or a ruler drag changes the view many times, and
//! the view from before it is kept once, when it ends. Commands that change
//! the view, such as Zoom In and Fit Song Width, are gestures of their own.
//! Movement the user didn't make, such as Follow's, is never kept.
//!
//! The arrangement, the piano roll and the Sample Editor each have a
//! history, and a Follow state, which navigating while the song plays
//! suspends: until the playhead plays back into sight, or Play, Scroll to
//! Playhead or turning Follow on resumes it.

use std::collections::BTreeMap;

use musicable_model::time::{BeatRange, Beats};
use musicable_model::{Chain, Project, TrackId};

use crate::editor::{Editor, Focus, Viewport};

/// The most steps each panel's history keeps.
pub const MAX_STEPS: usize = 32;

/// The timeline panels, each with its own view, history and Follow state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Panel {
    Arrangement,
    PianoRoll,
    SampleEditor,
}

impl Panel {
    /// The panel the zoom commands act on while `focus` has the focus: the
    /// piano roll or the Sample Editor while they have it, and else the
    /// arrangement, which the mixer shows the tracks of.
    pub fn of(focus: Focus) -> Self {
        match focus {
            Focus::PIANO_ROLL => Panel::PianoRoll,
            Focus::SAMPLE_EDITOR => Panel::SampleEditor,
            _ => Panel::Arrangement,
        }
    }
}

/// How a change to a panel's view counts, in its history and for Follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Record {
    /// A gesture on its own, or the end of one: the view from before it,
    /// or from before the gesture it ends, is a step of the history.
    #[default]
    Step,
    /// Part of a gesture that goes on, such as a wheel burst or a drag of
    /// the ruler: the view from before it is kept until the gesture ends.
    Gesture,
    /// Follow moved it, or found the playhead in sight again: Follow goes
    /// on following, and the history is left alone.
    Follow,
    /// Movement the user didn't make, such as scrolling while a drag
    /// reaches the edge, or the view keeping within the song: neither the
    /// history nor Follow takes it as navigating.
    None,
    /// A drag ended while Continuous Follow held the view still: the
    /// playhead has moved from its place, so Follow waits rather than jump
    /// to it, and the history is left alone.
    Suspend,
}

impl Record {
    /// Their names, as the view commands' `history` argument takes them.
    pub const NAMES: &'static [&'static str] = &["step", "gesture", "follow", "none", "suspend"];

    /// The way of counting `name` names.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "step" => Record::Step,
            "gesture" => Record::Gesture,
            "follow" => Record::Follow,
            "none" => Record::None,
            "suspend" => Record::Suspend,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Record::Step => "step",
            Record::Gesture => "gesture",
            Record::Follow => "follow",
            Record::None => "none",
            Record::Suspend => "suspend",
        }
    }
}

/// A panel's view, as a step of its history keeps it: where it starts, its
/// zoom, how far down it's scrolled, and for the arrangement the tracks'
/// heights, and for the piano roll its keys'. Not its width: going back
/// after the window is resized shows the same start at the same zoom.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub start: Beats,
    pub zoom: f64,
    pub top: f32,
    /// The arrangement's tracks' heights, and the master's row's, where
    /// they aren't the theme's.
    pub heights: BTreeMap<Chain, f32>,
    /// The piano roll's keys' height.
    pub key_height: f32,
}

/// A panel's history of views, and its Follow state.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    /// The views to go back to, the latest last.
    back: Vec<Place>,
    /// The views gone back from, the latest last.
    forward: Vec<Place>,
    /// The view from before the gesture going on, if one is.
    pending: Option<Place>,
    /// Whether Follow is on in the panel: each has its own switch, as in
    /// Bitwig, and they share the mode.
    pub follow: bool,
    /// Whether the user navigated away from the playhead while the song
    /// played, so Follow leaves the view alone.
    pub suspended: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
            pending: None,
            follow: true,
            suspended: false,
        }
    }
}

impl History {
    /// Whether Zoom Back has a view to go back to.
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty() || self.pending.is_some()
    }

    /// Whether Zoom Forward has a view to go forward to.
    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty() && self.pending.is_none()
    }

    /// How many steps back there are.
    pub fn len(&self) -> usize {
        self.back.len()
    }

    pub fn is_empty(&self) -> bool {
        self.back.is_empty()
    }

    /// Forgets every step: the panel shows something else.
    pub fn clear(&mut self) {
        self.back.clear();
        self.forward.clear();
        self.pending = None;
    }

    /// Keeps `place` as the latest step back. A new step drops the steps
    /// forward, as a browser's history does.
    fn push(&mut self, place: Place) {
        if self.back.last() == Some(&place) {
            return;
        }
        self.back.push(place);
        if self.back.len() > MAX_STEPS {
            self.back.remove(0);
        }
        self.forward.clear();
    }

    /// Ends the gesture going on, if one is, keeping the view from before
    /// it unless it ended where it started.
    fn end_gesture(&mut self, now: &Place) {
        if let Some(pending) = self.pending.take()
            && pending != *now
        {
            self.push(pending);
        }
    }
}

/// The timeline panels' histories.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Navigation {
    pub arrangement: History,
    pub piano_roll: History,
    pub sample_editor: History,
}

impl Navigation {
    pub fn of(&self, panel: Panel) -> &History {
        match panel {
            Panel::Arrangement => &self.arrangement,
            Panel::PianoRoll => &self.piano_roll,
            Panel::SampleEditor => &self.sample_editor,
        }
    }

    pub fn of_mut(&mut self, panel: Panel) -> &mut History {
        match panel {
            Panel::Arrangement => &mut self.arrangement,
            Panel::PianoRoll => &mut self.piano_roll,
            Panel::SampleEditor => &mut self.sample_editor,
        }
    }

    /// Lets Follow follow again in every panel: the song starts playing.
    pub fn resume_all(&mut self) {
        for history in [
            &mut self.arrangement,
            &mut self.piano_roll,
            &mut self.sample_editor,
        ] {
            history.suspended = false;
        }
    }
}

/// The sizes the arrangement drew its lanes at last, for fitting tracks to
/// its height and scrolling them into sight: they come from the theme and
/// the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lanes {
    /// How tall the tracks' part of the lanes is, over the master's.
    pub height: f32,
    /// A track's height, where it hasn't one of its own.
    pub track: f32,
    /// A take lane's height.
    pub take: f32,
    /// An automation lane's height.
    pub automation: f32,
}

impl Default for Lanes {
    /// The default theme's, in a window of a usual size.
    fn default() -> Self {
        Self {
            height: 480.0,
            track: 60.0,
            take: 36.0,
            automation: 48.0,
        }
    }
}

/// A track's rows in the arrangement: where its own row starts, down from
/// the top of the first track's, how tall it is, and how tall its take and
/// automation lanes under it are together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackRows {
    pub id: TrackId,
    pub top: f32,
    pub height: f32,
    pub lanes: f32,
}

impl TrackRows {
    /// Where its last lane ends.
    pub fn bottom(&self) -> f32 {
        self.top + self.height + self.lanes
    }
}

/// Where the song ends, for fitting it across: the latest of its clips'
/// ends, its takes' clips', its locators, its automation's last points and
/// the loop's end. Sixteen bars for a song with nothing in it.
pub fn song_end(project: &Project) -> Beats {
    let clips = project.tracks.iter().flat_map(|track| {
        let takes = track.takes.iter().flat_map(|take| &take.clips);
        track.clips.iter().chain(takes).map(|clip| clip.end())
    });
    let locators = project.locators.iter().map(|locator| locator.at);
    let points = (project.automation.iter()).filter_map(|lane| lane.points.last().map(|p| p.at));
    let content = clips.chain(locators).chain(points).max();
    let empty = project.tempo.bar_start(16);
    content
        .unwrap_or(empty)
        .max(project.loop_region.end)
        .max(Beats::ZERO)
}

impl Editor {
    /// Whether Follow is on in `panel`.
    pub fn follows(&self, panel: Panel) -> bool {
        self.navigation.of(panel).follow
    }

    /// The view of `panel` now.
    pub fn place(&self, panel: Panel) -> Place {
        let (viewport, heights, key_height) = match panel {
            Panel::Arrangement => (self.viewport, self.track_heights.clone(), 0.0),
            Panel::PianoRoll => (
                self.piano_roll.viewport,
                BTreeMap::new(),
                self.piano_roll.key_height,
            ),
            Panel::SampleEditor => (self.sample_editor.viewport, BTreeMap::new(), 0.0),
        };
        Place {
            start: viewport.start,
            zoom: viewport.zoom,
            top: viewport.top,
            heights,
            key_height,
        }
    }

    /// Shows `place` in `panel`, at the panel's width now. Heights of
    /// tracks that `project` doesn't have any more are skipped.
    fn show_place(&mut self, project: &Project, panel: Panel, place: &Place) {
        let viewport = self.viewport_mut(panel);
        viewport.start = place.start;
        viewport.zoom = place.zoom;
        viewport.top = place.top;
        match panel {
            Panel::Arrangement => {
                self.track_heights = (place.heights.iter())
                    .filter(|(chain, _)| chain.track().is_none_or(|id| project.track(id).is_some()))
                    .map(|(&chain, &height)| (chain, height))
                    .collect();
            }
            Panel::PianoRoll => self.piano_roll.key_height = place.key_height,
            Panel::SampleEditor => {}
        }
    }

    /// The viewport of `panel`.
    pub fn viewport_mut(&mut self, panel: Panel) -> &mut Viewport {
        match panel {
            Panel::Arrangement => &mut self.viewport,
            Panel::PianoRoll => &mut self.piano_roll.viewport,
            Panel::SampleEditor => &mut self.sample_editor.viewport,
        }
    }

    pub fn viewport_of(&self, panel: Panel) -> Viewport {
        match panel {
            Panel::Arrangement => self.viewport,
            Panel::PianoRoll => self.piano_roll.viewport,
            Panel::SampleEditor => self.sample_editor.viewport,
        }
    }

    /// Changes the view of `panel` with `change`, counting it as `record`
    /// says, while the song plays or not. Returns whether the view changed.
    pub fn navigate(
        &mut self,
        panel: Panel,
        record: Record,
        playing: bool,
        change: impl FnOnce(&mut Editor),
    ) -> bool {
        let before = self.place(panel);
        change(self);
        let after = self.place(panel);
        let changed = before != after;
        let history = self.navigation.of_mut(panel);
        match record {
            Record::Step => {
                let from = history.pending.take().unwrap_or(before);
                if from != after {
                    history.push(from);
                }
                if changed && playing {
                    history.suspended = true;
                }
            }
            Record::Gesture => {
                if changed {
                    history.pending.get_or_insert(before);
                    if playing {
                        history.suspended = true;
                    }
                }
            }
            Record::Follow => history.suspended = false,
            Record::None => {}
            Record::Suspend => history.suspended |= playing,
        }
        changed
    }

    /// Goes back to the view before the last step in `panel`, or forward
    /// to the one gone back from. Returns whether there was one.
    pub fn step_view(
        &mut self,
        project: &Project,
        panel: Panel,
        forward: bool,
        playing: bool,
    ) -> bool {
        let now = self.place(panel);
        let history = self.navigation.of_mut(panel);
        history.end_gesture(&now);
        let (from, to) = match forward {
            false => (&mut history.back, &mut history.forward),
            true => (&mut history.forward, &mut history.back),
        };
        let Some(place) = from.pop() else {
            return false;
        };
        to.push(now);
        if playing {
            history.suspended = true;
        }
        self.show_place(project, panel, &place);
        true
    }

    /// The height of `chain`'s row in the arrangement.
    pub fn height_of(&self, chain: Chain) -> f32 {
        (self.track_heights.get(&chain).copied()).unwrap_or(self.lanes.track)
    }

    /// Each track's rows, top to bottom, as the arrangement lays them out
    /// at the sizes it drew last.
    pub fn track_rows(&self, project: &Project) -> Vec<TrackRows> {
        let mut top = 0.0;
        let automation = |chain: Chain| {
            let shown = self.automation.get(&chain).map_or(0, Vec::len);
            shown as f32 * self.lanes.automation
        };
        (project.tracks.iter())
            .map(|track| {
                let height = self.height_of(Chain::Track(track.id));
                let takes = match self.take_lanes.contains(&track.id) {
                    true => track.takes.len() as f32 * self.lanes.take,
                    false => 0.0,
                };
                let rows = TrackRows {
                    id: track.id,
                    top,
                    height,
                    lanes: takes + automation(Chain::Track(track.id)),
                };
                top = rows.bottom();
                rows
            })
            .collect()
    }

    /// Scrolls the arrangement down or up so the track `id` is in sight, if
    /// none of it is. Returns whether it scrolled.
    pub fn reveal_track(&mut self, project: &Project, id: TrackId) -> bool {
        let Some(rows) = self.track_rows(project).into_iter().find(|r| r.id == id) else {
            return false;
        };
        let (top, bottom) = (self.viewport.top, self.viewport.top + self.lanes.height);
        let end = rows.top + rows.height;
        if end > top && rows.top < bottom {
            return false;
        }
        self.viewport.top = match rows.top < top {
            true => rows.top,
            false => (end - self.lanes.height).max(0.0).min(rows.top),
        };
        true
    }

    /// Sets every track's height so they fill the tracks' part of the
    /// arrangement together, with their take and automation lanes as tall
    /// as they are, and scrolls to the top. When they can't fit, each is
    /// as short as tracks go, and the tracks scroll. Returns how many
    /// tracks are in sight, and how many there are.
    pub fn fit_track_heights(&mut self, project: &Project) -> (usize, usize) {
        let count = project.tracks.len();
        if count == 0 {
            return (0, 0);
        }
        let rows = self.track_rows(project);
        let fixed: f32 = rows.iter().map(|rows| rows.lanes).sum();
        let budget = self.lanes.height - fixed;
        let each = budget / count as f32;
        let height = each.clamp(crate::view::MIN_TRACK_HEIGHT, crate::view::MAX_TRACK_HEIGHT);
        for track in &project.tracks {
            self.track_heights.insert(Chain::Track(track.id), height);
        }
        self.viewport.top = 0.0;
        let shown = (self.track_rows(project).iter())
            .filter(|rows| rows.top + rows.height <= self.lanes.height + 0.5)
            .count();
        (shown, count)
    }
}

/// The range W fits across the arrangement: from the song's start to its
/// end.
pub fn song_range(project: &Project) -> BeatRange {
    BeatRange::new(Beats::ZERO, song_end(project))
}

#[cfg(test)]
mod tests {
    use musicable_model::clip::{ClipContent, MidiClip};
    use musicable_model::{Project, TrackKind};

    use super::*;

    fn place(start: i64) -> Place {
        Place {
            start: Beats::new(start),
            zoom: 10.0,
            top: 0.0,
            heights: BTreeMap::new(),
            key_height: 0.0,
        }
    }

    #[test]
    fn a_gesture_is_one_step_however_many_changes_it_makes() {
        let project = Project::new();
        let mut editor = Editor::default();
        let panel = Panel::Arrangement;
        let start = editor.viewport;
        for n in 1..=40 {
            editor.navigate(panel, Record::Gesture, false, |editor| {
                editor.viewport.zoom = 10.0 + f64::from(n);
            });
        }
        assert_eq!(editor.navigation.arrangement.len(), 0);
        // The gesture ends with the last change, or with none.
        editor.navigate(panel, Record::Step, false, |_| {});
        assert_eq!(editor.navigation.arrangement.len(), 1);
        assert!(editor.step_view(&project, panel, false, false));
        assert_eq!(editor.viewport, start);
        // Forward, then back, then a new step drops the way forward.
        assert!(editor.step_view(&project, panel, true, false));
        assert_eq!(editor.viewport.zoom, 50.0);
        assert!(editor.step_view(&project, panel, false, false));
        editor.navigate(panel, Record::Step, false, |editor| {
            editor.viewport.zoom = 99.0;
        });
        assert!(!editor.navigation.arrangement.can_go_forward());
        assert!(!editor.step_view(&project, panel, true, false));
    }

    #[test]
    fn what_changes_nothing_or_isn_t_the_user_s_isn_t_kept() {
        let mut editor = Editor::default();
        let panel = Panel::Arrangement;
        editor.navigate(panel, Record::Step, true, |_| {});
        editor.navigate(panel, Record::Follow, true, |editor| {
            editor.viewport.start = Beats::new(64);
        });
        editor.navigate(panel, Record::None, true, |editor| {
            editor.viewport.start = Beats::new(32);
        });
        // A gesture that ends where it started.
        editor.navigate(panel, Record::Gesture, false, |editor| {
            editor.viewport.zoom *= 2.0;
        });
        editor.navigate(panel, Record::Step, false, |editor| {
            editor.viewport.zoom /= 2.0;
        });
        let history = &editor.navigation.arrangement;
        assert!(history.is_empty() && !history.suspended);
        // The width isn't part of the view.
        editor.navigate(panel, Record::Step, false, |editor| {
            editor.viewport.width = 1234.0;
        });
        assert!(editor.navigation.arrangement.is_empty());
    }

    #[test]
    fn navigating_while_playing_suspends_follow_until_it_follows_again() {
        let mut editor = Editor::default();
        editor.navigate(Panel::PianoRoll, Record::Gesture, true, |editor| {
            editor.piano_roll.viewport.start = Beats::new(8);
        });
        let navigation = &editor.navigation;
        assert!(navigation.piano_roll.suspended && !navigation.arrangement.suspended);
        editor.navigate(Panel::PianoRoll, Record::Follow, true, |_| {});
        assert!(!editor.navigation.piano_roll.suspended);
        // Stopped, it isn't suspended.
        editor.navigate(Panel::Arrangement, Record::Step, false, |editor| {
            editor.viewport.start = Beats::new(8);
        });
        assert!(!editor.navigation.arrangement.suspended);
    }

    #[test]
    fn the_history_keeps_its_latest_steps() {
        let mut history = History::default();
        for n in 0..40 {
            history.push(place(n));
        }
        assert_eq!(history.len(), MAX_STEPS);
        assert_eq!(history.back[0], place(8));
        // The same view twice is one step.
        history.push(place(39));
        assert_eq!(history.len(), MAX_STEPS);
    }

    #[test]
    fn the_song_ends_at_the_last_of_what_s_in_it() {
        let mut project = Project::new();
        // Nothing in it: sixteen bars, or the loop if that's longer.
        assert_eq!(song_end(&project), Beats::new(64));
        project.loop_region = BeatRange::new(Beats::ZERO, Beats::new(80));
        assert_eq!(song_end(&project), Beats::new(80));
        project.loop_region = BeatRange::new(Beats::ZERO, Beats::new(8));
        let keys = project.add_track("Keys", TrackKind::Midi);
        let range = BeatRange::new(Beats::new(4), Beats::new(12));
        let clip = project.new_clip("K", range, ClipContent::Midi(MidiClip::default()));
        project.track_mut(keys).unwrap().clips.push(clip);
        assert_eq!(song_end(&project), Beats::new(12));
        project.locators.push(musicable_model::Locator {
            id: musicable_model::LocatorId(1),
            name: "Outro".to_owned(),
            at: Beats::new(20),
        });
        assert_eq!(song_end(&project), Beats::new(20));
    }

    #[test]
    fn heights_fit_the_tracks_part_or_go_as_short_as_tracks_go() {
        let mut project = Project::new();
        let tracks: Vec<TrackId> = (0..4)
            .map(|n| project.add_track(format!("T{n}"), TrackKind::Audio))
            .collect();
        let mut editor = Editor::default();
        editor.lanes.height = 400.0;
        editor.viewport.top = 50.0;
        assert_eq!(editor.fit_track_heights(&project), (4, 4));
        assert_eq!(editor.height_of(Chain::Track(tracks[0])), 100.0);
        assert_eq!(editor.viewport.top, 0.0);
        // Automation lanes keep their height.
        let volume = musicable_model::param::ParamAddress::Volume(Chain::Track(tracks[1]));
        editor
            .automation
            .insert(Chain::Track(tracks[1]), vec![volume]);
        editor.fit_track_heights(&project);
        assert_eq!(editor.height_of(Chain::Track(tracks[0])), 88.0);
        // Too many to fit.
        for n in 4..40 {
            project.add_track(format!("T{n}"), TrackKind::Audio);
        }
        let (shown, count) = editor.fit_track_heights(&project);
        assert_eq!(count, 40);
        assert!(shown < count, "{shown}");
        let last = *project.tracks.last().map(|t| &t.id).unwrap();
        assert_eq!(
            editor.height_of(Chain::Track(last)),
            crate::view::MIN_TRACK_HEIGHT
        );
        // Out of sight, a track scrolls into it.
        assert!(editor.reveal_track(&project, last));
        assert!(editor.viewport.top > 0.0);
        assert!(!editor.reveal_track(&project, last));
    }
}
