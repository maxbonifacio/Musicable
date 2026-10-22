//! The callbacks' half of a stream: at each buffer switch, count any
//! dropout, then render the half the driver asked for and write it in each
//! channel's sample type. All of it real-time safe.

use std::slice;
use std::time::{Duration, Instant};

use musicable_engine::{AudioMut, Engine};

use super::context::Shared;
use super::convert::Encoding;
use super::detector::{self, Callback, Detector};
use super::driver::Handle;
use super::sys::Long;
use crate::frames_to_duration;

/// An output channel: its two buffer halves, and how it stores samples.
pub(super) struct Output {
    pub halves: [*mut u8; 2],
    pub encoding: Encoding,
}

pub(super) struct Render {
    driver: Handle,
    outputs: Vec<Output>,
    period: usize,
    period_duration: Duration,
    rate: u32,
    /// Whether to tell the driver when each half is written.
    output_ready: bool,
    /// The engine renders here, a channel after another, before the samples
    /// are converted.
    scratch: Vec<f32>,
    pub engine: Option<Engine>,
    detector: Detector,
    callbacks: u64,
    last_callback: Option<Instant>,
    /// Whether the last callback took longer than a buffer.
    overran: bool,
}

// SAFETY: the driver's buffers are for whichever thread has the render lock,
// and drivers take calls to their real-time methods from any thread.
unsafe impl Send for Render {}

impl Render {
    pub fn new(
        driver: Handle,
        outputs: Vec<Output>,
        period: u32,
        rate: u32,
        output_ready: bool,
        driver_reports: bool,
    ) -> Self {
        let channels = outputs.len();
        Self {
            driver,
            outputs,
            period: period as usize,
            period_duration: frames_to_duration(u64::from(period), rate),
            rate,
            output_ready,
            scratch: vec![0.0; period as usize * channels],
            engine: None,
            detector: Detector::new(period, driver_reports),
            callbacks: 0,
            last_callback: None,
            overran: false,
        }
    }

    /// One buffer switch: the driver asks for half `index`, which starts at
    /// sample `position` if the callback said. Real-time safe.
    pub fn serve(&mut self, shared: &Shared, index: Long, position: Option<u64>) {
        let woke = Instant::now();
        let counters = &shared.counters;
        self.callbacks += 1;
        let interval = self
            .last_callback
            .replace(woke)
            .map(|last| woke.saturating_duration_since(last));
        if let Some(interval) = interval
            && self.callbacks > detector::STARTUP
        {
            counters.interval(interval);
            if interval * 2 > self.period_duration * 3 {
                counters.late_wakeup();
            }
        }
        let half = (index & 1) as usize;
        let position = position.or_else(|| {
            // SAFETY: the host thread takes this state, with the handle,
            // before it releases the driver.
            unsafe { self.driver.sample_position() }
        });
        let outcome = self.detector.observe(Callback {
            index: half,
            position,
            interval: interval.map(|interval| frames(interval, self.rate)),
            latency: u64::from(shared.latency()),
            overran: self.overran,
            reported: shared.take_reported(),
        });
        if outcome.underrun {
            if let Some(engine) = &self.engine {
                engine.record_xrun();
            }
            counters.underrun();
        }
        counters.lost(outcome.lost);

        let channels = self.outputs.len();
        match &mut self.engine {
            Some(engine) => engine.process(&mut AudioMut::from_planar(&mut self.scratch, channels)),
            None => self.scratch.fill(0.0),
        }
        for (output, samples) in self
            .outputs
            .iter()
            .zip(self.scratch.chunks_exact(self.period))
        {
            let bytes = self.period * output.encoding.bytes();
            // SAFETY: the driver's buffer half, `period` samples in the
            // channel's encoding. It keeps it until the buffers are disposed
            // of, after the host thread has taken this state back.
            let out = unsafe { slice::from_raw_parts_mut(output.halves[half], bytes) };
            output.encoding.write(samples, out);
        }
        if self.output_ready {
            // SAFETY: as for the sample position.
            unsafe { self.driver.output_ready() };
        }
        counters.wrote(self.period as u32);
        counters.wakeup();
        let busy = woke.elapsed();
        counters.busy(busy);
        self.overran = busy > self.period_duration;
    }
}

/// `duration` in frames at `rate`, rounded down. Real-time safe.
fn frames(duration: Duration, rate: u32) -> u64 {
    let frames = duration.as_nanos() * u128::from(rate) / 1_000_000_000;
    u64::try_from(frames).unwrap_or(u64::MAX)
}
