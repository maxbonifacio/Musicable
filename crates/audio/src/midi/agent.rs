//! Musicable Agent MIDI: a MIDI input that agents play, as if from a
//! keyboard plugged in, for the checks that need one. A track takes MIDI
//! from it as from any other input, and what an agent plays goes into its
//! buffer stamped with the engine frame heard at each message's time, as a
//! real input's messages are.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::ring::{MidiBuffer, MidiWriter};
use crate::input::Playback;

/// The input's id and name.
pub const AGENT_INPUT: &str = "Musicable Agent MIDI";

/// The input while a stream has it open: where its messages go, and the
/// output's clock to stamp them with.
struct Open {
    writer: MidiWriter,
    playback: Arc<Playback>,
    /// Which opening this is, so an older one's closing leaves a newer
    /// one be.
    opening: u64,
}

static OPEN: Mutex<Option<Open>> = Mutex::new(None);
static OPENINGS: Mutex<u64> = Mutex::new(0);

/// The input, open while it lives.
pub(super) struct Input {
    opening: u64,
}

impl Drop for Input {
    fn drop(&mut self) {
        let mut open = OPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if open
            .as_ref()
            .is_some_and(|open| open.opening == self.opening)
        {
            *open = None;
        }
    }
}

/// Opens the input, whose messages are stamped with the frame `playback`
/// says is heard as they come: its buffer, and the input.
pub(super) fn open(capacity: usize, playback: &Arc<Playback>) -> (MidiBuffer, Input) {
    let (buffer, writer) = MidiBuffer::new(capacity);
    let opening = {
        let mut openings = OPENINGS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *openings += 1;
        *openings
    };
    *OPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Open {
        writer,
        playback: playback.clone(),
        opening,
    });
    (buffer, Input { opening })
}

/// Plays `messages` into the input: each one's seconds from now, and its
/// three bytes, a channel message. It returns once the last has been
/// played, or says why it can't play them.
pub fn play(messages: &[(f64, [u8; 3])]) -> Result<(), String> {
    let start = Instant::now();
    let started = now_ns();
    {
        let open = OPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(open) = open.as_ref() else {
            return Err(format!(
                "{AGENT_INPUT} isn't open: a MIDI track takes it as its input with \
                 mixer.set_input, and then it opens with the audio device."
            ));
        };
        if open.playback.reading().is_none() {
            return Err(
                "The audio device isn't playing, so there's no time to play MIDI at.".into(),
            );
        }
    }
    let mut messages = messages.to_vec();
    messages.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (seconds, bytes) in messages {
        let due = Duration::from_secs_f64(seconds.max(0.0));
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
        let mut open = OPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(open) = open.as_mut() else {
            return Err(format!("{AGENT_INPUT} closed while it played."));
        };
        let Some(reading) = open.playback.reading() else {
            return Err("The audio device stopped while it played.".into());
        };
        // Stamped with when it was meant to come, not when the thread woke
        // up, so it keeps its timing.
        let time = started + due.as_nanos() as u64;
        let frame = reading.frame_at(time).max(0.0) as u64;
        open.writer.push(frame, bytes);
    }
    Ok(())
}

/// Now on the clock the outputs stamp their readings with: the
/// performance counter on Windows, in nanoseconds.
#[cfg(windows)]
fn now_ns() -> u64 {
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
    let (mut count, mut frequency) = (0, 0);
    // SAFETY: each writes one i64. They can't fail on Windows XP or later.
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    (u128::from(count as u64) * 1_000_000_000 / u128::from((frequency as u64).max(1))) as u64
}

#[cfg(not(windows))]
fn now_ns() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_an_agent_plays_goes_into_the_input_on_time() {
        let playback = Arc::new(Playback::default());
        assert!(play(&[(0.0, [0x90, 60, 100])]).is_err_and(|why| why.contains("isn't open")));
        let (buffer, input) = open(64, &playback);
        assert_eq!(
            play(&[(0.0, [0x90, 60, 100])]),
            Err("The audio device isn't playing, so there's no time to play MIDI at.".into())
        );
        // Frame 48 000 was heard just now, at 48 kHz.
        playback.publish(48_000, now_ns(), 48_000);
        let mut reader = buffer.reader();
        play(&[(0.1, [0x80, 60, 0]), (0.0, [0x90, 60, 100])]).unwrap();
        let on = reader.next_before(u64::MAX).unwrap();
        let off = reader.next_before(u64::MAX).unwrap();
        assert_eq!(on.bytes, [0x90, 60, 100]);
        assert_eq!(off.bytes, [0x80, 60, 0]);
        // A tenth of a second apart, at 48 kHz.
        assert!((off.frame - on.frame).abs_diff(4800) < 10, "{on:?} {off:?}");
        assert!(on.frame >= 48_000 && on.frame < 48_000 + 4800, "{on:?}");
        drop(input);
        assert!(play(&[(0.0, [0x90, 60, 100])]).is_err());
    }
}
