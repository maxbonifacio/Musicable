//! The transport as the engine runs it: whether it plays, the song position,
//! the loop, and the tempo map that places musical positions in samples.

use musicable_model::time::{Beats, TempoMap};

use crate::{TransportCommand, TransportState};

pub(crate) struct Transport {
    state: TransportState,
    tempo: Box<TempoMap>,
    sample_rate: u32,
    /// The loop region in song positions, while looping over at least one
    /// frame.
    loop_frames: Option<(u64, u64)>,
}

impl Transport {
    /// Stopped at the start of the song, at 120 BPM in 4/4.
    pub fn new(sample_rate: u32) -> Self {
        Self {
            state: TransportState::default(),
            tempo: Box::default(),
            sample_rate,
            loop_frames: None,
        }
    }

    pub fn state(&self) -> TransportState {
        self.state
    }

    pub fn tempo(&self) -> &TempoMap {
        &self.tempo
    }

    pub fn apply(&mut self, command: TransportCommand) {
        match command {
            TransportCommand::Play => self.state.playing = true,
            TransportCommand::Stop => self.state.playing = false,
            TransportCommand::Locate(at) => self.state.position = self.frame_at(at),
            TransportCommand::Loop(region) => {
                self.state.looping = region;
                self.place_loop();
            }
        }
    }

    /// Replaces the tempo map and returns the old one. The playhead keeps its
    /// musical position, and the loop region its musical place.
    pub fn set_tempo(&mut self, mut tempo: Box<TempoMap>) -> Box<TempoMap> {
        let position = self.state.position as f64;
        let at = Beats::from_f64(self.tempo.beats_at(position, self.sample_rate));
        std::mem::swap(&mut self.tempo, &mut tempo);
        self.state.position = self.frame_at(at);
        self.place_loop();
        tempo
    }

    /// Call at the start of each block, after applying the transport changes
    /// due: a playing playhead at the loop end jumps back to the loop start.
    pub fn wrap(&mut self) {
        if let Some((start, end)) = self.loop_frames
            && self.state.playing
            && self.state.position == end
        {
            self.state.position = start;
        }
    }

    /// How many frames the playhead plays before it reaches the loop end, if
    /// it is going to. The block must end there.
    pub fn frames_to_loop_end(&self) -> Option<u64> {
        match self.loop_frames {
            Some((_, end)) if self.state.playing && self.state.position < end => {
                Some(end - self.state.position)
            }
            _ => None,
        }
    }

    /// Moves the playhead past a block of `frames`.
    pub fn advance(&mut self, frames: u64) {
        if self.state.playing {
            self.state.position += frames;
        }
    }

    /// The song position `at` plays on. The song starts at zero.
    fn frame_at(&self, at: Beats) -> u64 {
        self.tempo.frame_at(at, self.sample_rate).max(0) as u64
    }

    fn place_loop(&mut self) {
        self.loop_frames = self.state.looping.and_then(|region| {
            let (start, end) = (self.frame_at(region.start), self.frame_at(region.end));
            (start < end).then_some((start, end))
        });
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::time::{BeatRange, TimeSignature};

    use super::*;

    /// 120 BPM at 48 kHz: 24,000 frames to the beat.
    fn transport() -> Transport {
        Transport::new(48_000)
    }

    fn looping(start: i64, end: i64) -> TransportCommand {
        TransportCommand::Loop(Some(BeatRange::new(Beats::new(start), Beats::new(end))))
    }

    #[test]
    fn the_loop_end_ends_the_block_and_jumps_back() {
        let mut t = transport();
        t.apply(looping(1, 2));
        t.apply(TransportCommand::Play);
        assert_eq!(t.frames_to_loop_end(), Some(48_000));
        t.advance(48_000);
        t.wrap();
        assert_eq!(t.state().position, 24_000);
        assert_eq!(t.frames_to_loop_end(), Some(24_000));
        t.advance(24_000);
        t.wrap();
        assert_eq!(t.state().position, 24_000);
    }

    #[test]
    fn a_playhead_past_the_loop_plays_on() {
        let mut t = transport();
        t.apply(TransportCommand::Locate(Beats::new(3)));
        t.apply(looping(1, 2));
        t.apply(TransportCommand::Play);
        t.wrap();
        assert_eq!(t.state().position, 72_000);
        assert_eq!(t.frames_to_loop_end(), None);
    }

    #[test]
    fn a_stopped_playhead_waits_at_the_loop_end() {
        let mut t = transport();
        t.apply(looping(0, 1));
        t.apply(TransportCommand::Locate(Beats::new(1)));
        t.wrap();
        assert_eq!(t.state().position, 24_000);
        assert_eq!(t.frames_to_loop_end(), None);
        t.apply(TransportCommand::Play);
        t.wrap();
        assert_eq!(t.state().position, 0);
    }

    #[test]
    fn empty_loops_and_early_positions_are_ignored() {
        let mut t = transport();
        t.apply(TransportCommand::Play);
        t.apply(TransportCommand::Loop(Some(BeatRange::new(
            Beats::new(2),
            Beats::new(2),
        ))));
        assert_eq!(t.frames_to_loop_end(), None);
        assert!(t.state().looping.is_some());
        t.apply(TransportCommand::Locate(Beats::new(-2)));
        assert_eq!(t.state().position, 0);
        t.apply(TransportCommand::Loop(None));
        assert_eq!(t.state().looping, None);
    }

    #[test]
    fn a_new_tempo_keeps_the_musical_position() {
        let mut t = transport();
        t.apply(looping(4, 8));
        t.apply(TransportCommand::Locate(Beats::from_ratio(13, 2)));
        assert_eq!(t.state().position, 156_000);
        let old = t.set_tempo(Box::new(TempoMap::new(60.0, TimeSignature::FOUR_FOUR)));
        assert_eq!(*old, TempoMap::default());
        // Twice as long per beat.
        assert_eq!(t.state().position, 312_000);
        t.apply(TransportCommand::Play);
        assert_eq!(t.frames_to_loop_end(), Some(72_000));

        // The same map again changes nothing, wherever the playhead is.
        for position in (0..1_000_000).step_by(9_973) {
            t.state.position = position;
            t.set_tempo(Box::new(TempoMap::new(60.0, TimeSignature::FOUR_FOUR)));
            assert_eq!(t.state().position, position);
        }
    }
}
