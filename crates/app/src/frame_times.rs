//! Frame times, to check that the app draws quickly enough with windows of
//! its own open. With the `MUSICABLE_FRAME_TIMES` environment variable set,
//! the window draws all the time, as fast as the monitor takes frames, and
//! every five seconds prints to the console it was started from how long
//! frames took: from one to the next, and on the CPU, as eframe measures it,
//! with every window drawn. A 144 Hz monitor takes a frame every 6.9 ms.

use std::time::{Duration, Instant};

/// The environment variable that asks for frame times.
pub const FRAME_TIMES: &str = "MUSICABLE_FRAME_TIMES";

/// How often frame times are printed.
const EVERY: Duration = Duration::from_secs(5);

/// The frames drawn since frame times were last printed.
pub(crate) struct FrameTimes {
    /// When they were last printed.
    since: Instant,
    /// When the last frame was drawn.
    last: Option<Instant>,
    frames: u32,
    /// The time from frame to frame, all told and the longest.
    between: Duration,
    longest: Duration,
    /// The time frames took on the CPU, all told and the longest, in
    /// seconds.
    cpu: f32,
    slowest: f32,
    /// The most windows open.
    windows: usize,
}

impl FrameTimes {
    /// Frame times, if the environment asks for them.
    pub fn from_env() -> Option<Self> {
        std::env::var_os(FRAME_TIMES).map(|_| Self::new(Instant::now()))
    }

    fn new(now: Instant) -> Self {
        Self {
            since: now,
            last: None,
            frames: 0,
            between: Duration::ZERO,
            longest: Duration::ZERO,
            cpu: 0.0,
            slowest: 0.0,
            windows: 0,
        }
    }

    /// Notes a frame drawn now, with `windows` windows open, the frame
    /// before having taken `cpu` seconds on the CPU, and prints the frame
    /// times when it's time to.
    pub fn frame(&mut self, cpu: Option<f32>, windows: usize) {
        if let Some(line) = self.frame_at(Instant::now(), cpu, windows) {
            eprintln!("{line}");
        }
    }

    /// Notes a frame drawn at `now`, and returns the line to print if it's
    /// time to.
    fn frame_at(&mut self, now: Instant, cpu: Option<f32>, windows: usize) -> Option<String> {
        if let Some(last) = self.last.replace(now) {
            let between = now - last;
            self.frames += 1;
            self.between += between;
            self.longest = self.longest.max(between);
            let cpu = cpu.unwrap_or(0.0);
            self.cpu += cpu;
            self.slowest = self.slowest.max(cpu);
        }
        self.windows = self.windows.max(windows);
        if now - self.since < EVERY || self.frames == 0 {
            return None;
        }
        let frames = self.frames as f32;
        let ms = |seconds: f32| seconds * 1000.0;
        let windows = match self.windows {
            1 => "1 window".to_owned(),
            windows => format!("{windows} windows"),
        };
        let line = format!(
            "{} frames, {windows}: {:.2} ms apart, at most {:.2}; {:.2} ms on the CPU, at most {:.2}",
            self.frames,
            ms(self.between.as_secs_f32() / frames),
            ms(self.longest.as_secs_f32()),
            ms(self.cpu / frames),
            ms(self.slowest),
        );
        *self = Self {
            last: self.last,
            ..Self::new(now)
        };
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_times_are_printed_every_five_seconds() {
        let start = Instant::now();
        let mut times = FrameTimes::new(start);
        let at = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(times.frame_at(at(0), None, 1), None);
        // Frames 7 ms apart but for one 21 ms after the one before, taking
        // 3 ms on the CPU but for one taking 9, with three windows open,
        // then two.
        for frame in 1..=712 {
            let ms = frame * 7 + if frame < 300 { 0 } else { 14 };
            let cpu = if frame == 500 { 0.009 } else { 0.003 };
            let windows = if frame < 600 { 3 } else { 2 };
            assert_eq!(times.frame_at(at(ms), Some(cpu), windows), None);
        }
        // Five seconds on.
        assert_eq!(
            times.frame_at(at(5000), Some(0.003), 2).as_deref(),
            Some(
                "713 frames, 3 windows: 7.01 ms apart, at most 21.00; \
                 3.01 ms on the CPU, at most 9.00"
            )
        );
        // And five seconds after that.
        assert_eq!(times.frame_at(at(5007), Some(0.003), 2), None);
        let line = times.frame_at(at(10_000), Some(0.003), 2).unwrap();
        assert!(
            line.starts_with("2 frames, 2 windows: 2500.00 ms apart"),
            "{line}"
        );
    }
}
