//! The arpeggiator's steps, as both formats' Test Arpeggiator plays them.

/// Notes the arpeggiator holds at once.
const HELD: usize = 16;

/// How long each of the arpeggiator's steps is, in seconds.
pub const ARP_STEP: f64 = 0.125;

/// A note the arpeggiator starts or ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Stepped {
    /// When in the block, in frames.
    pub(crate) time: u32,
    /// Whether it starts.
    pub(crate) on: bool,
    pub(crate) id: i32,
    pub(crate) key: i16,
    pub(crate) velocity: f64,
}

/// What the arpeggiator holds and plays: the keys held in turn, lowest
/// first, a step of [`ARP_STEP`] each, from the first key held.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Arp {
    /// The keys held, lowest first, with their velocities: the first
    /// `count`.
    held: [(i16, f64); HELD],
    count: usize,
    /// The note it plays, by its id and key.
    playing: Option<(i32, i16)>,
    /// Frames to its next step, while it steps.
    left: Option<u32>,
    /// How many frames a step lasts.
    step: u32,
    /// The key held its last step played: the next plays the next key up.
    last: Option<i16>,
    /// The id its next note gets.
    next_id: i32,
    /// Semitones, rounded.
    pub(crate) transpose: f64,
}

impl Arp {
    pub(crate) fn new(sample_rate: f64, transpose: f64) -> Self {
        Self {
            held: [(0, 0.0); HELD],
            count: 0,
            playing: None,
            left: None,
            step: (ARP_STEP * sample_rate).round().max(1.0) as u32,
            last: None,
            next_id: 0,
            transpose,
        }
    }

    /// Forgets the keys held and the note it plays, as a reset does.
    pub(crate) fn clear(&mut self) {
        self.count = 0;
        self.playing = None;
        self.left = None;
        self.last = None;
    }

    /// Holds `key`, and starts stepping at `time` if it wasn't.
    pub(crate) fn hold(
        &mut self,
        key: i16,
        velocity: f64,
        time: u32,
        emit: &mut dyn FnMut(Stepped),
    ) {
        let held = &mut self.held[..self.count];
        match held.iter().position(|&(k, _)| k >= key) {
            Some(at) if held[at].0 == key => held[at].1 = velocity,
            _ if self.count == HELD => return,
            at => {
                let at = at.unwrap_or(self.count);
                self.held.copy_within(at..self.count, at + 1);
                self.held[at] = (key, velocity);
                self.count += 1;
            }
        }
        if self.left.is_none() {
            self.last = None;
            self.play_step(time, emit);
        }
    }

    /// Lets go of `key`: once none is held, the note it plays ends at
    /// `time`.
    pub(crate) fn let_go(&mut self, key: i16, time: u32, emit: &mut dyn FnMut(Stepped)) {
        let held = &self.held[..self.count];
        if let Some(at) = held.iter().position(|&(k, _)| k == key) {
            self.held.copy_within(at + 1..self.count, at);
            self.count -= 1;
        }
        if self.count == 0 {
            self.end(time, emit);
            self.left = None;
        }
    }

    /// Plays the steps due from frame `from` to before frame `to`.
    pub(crate) fn run(&mut self, from: u32, to: u32, emit: &mut dyn FnMut(Stepped)) {
        let mut at = from;
        while let Some(left) = self.left {
            if left >= to - at {
                self.left = Some(left - (to - at));
                return;
            }
            at += left;
            self.play_step(at, emit);
        }
    }

    /// Ends the note it plays and plays the next key held, at `time`.
    fn play_step(&mut self, time: u32, emit: &mut dyn FnMut(Stepped)) {
        self.end(time, emit);
        if self.count == 0 {
            self.left = None;
            return;
        }
        let held = &self.held[..self.count];
        let next = self
            .last
            .and_then(|last| held.iter().find(|&&(k, _)| k > last));
        let (key, velocity) = *next.unwrap_or(&held[0]);
        self.last = Some(key);
        let key = (f64::from(key) + self.transpose.round()).clamp(0.0, 127.0) as i16;
        let id = self.next_id;
        self.next_id = id.checked_add(1).unwrap_or(0);
        emit(Stepped {
            time,
            on: true,
            id,
            key,
            velocity,
        });
        self.playing = Some((id, key));
        self.left = Some(self.step);
    }

    /// Ends the note it plays, if it plays one, at `time`.
    fn end(&mut self, time: u32, emit: &mut dyn FnMut(Stepped)) {
        if let Some((id, key)) = self.playing.take() {
            emit(Stepped {
                time,
                on: false,
                id,
                key,
                velocity: 0.0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(time: u32, on: bool, id: i32, key: i16) -> Stepped {
        Stepped {
            time,
            on,
            id,
            key,
            velocity: if on { 0.5 } else { 0.0 },
        }
    }

    #[test]
    fn it_plays_the_keys_held_in_turn_lowest_first() {
        // Steps of 10 frames.
        let mut arp = Arp::new(80.0, 12.0);
        let mut played = Vec::new();
        let emit = &mut |n| played.push(n);
        arp.hold(64, 0.5, 3, emit);
        arp.hold(60, 0.5, 5, emit);
        arp.run(3, 30, emit);
        arp.let_go(64, 30, emit);
        arp.run(30, 40, emit);
        arp.let_go(60, 40, emit);
        arp.run(40, 60, emit);
        assert_eq!(
            played,
            [
                note(3, true, 0, 76),
                note(13, false, 0, 76),
                note(13, true, 1, 72),
                note(23, false, 1, 72),
                note(23, true, 2, 76),
                note(33, false, 2, 76),
                note(33, true, 3, 72),
                note(40, false, 3, 72),
            ]
        );
    }
}
