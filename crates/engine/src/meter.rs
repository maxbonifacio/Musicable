//! Level meters: what the audio thread measures, for any thread to read.
//!
//! A [`Meter`] sits in a node in the graph, which measures each block it
//! renders into it. The readings live in atomics, so reading them never
//! waits on the audio thread, and a meter carries on across graph swaps
//! when the new graph's node gets the same one. [`Meters`] keeps one for
//! each channel of a project, for [`compile_metered`](crate::compile::compile_metered)
//! to put in the graph and the UI to read, and a [`Reading`] of each
//! modulator's outputs and of what moves each parameter modulators move.
//!
//! Each reading keeps the engine time it's for, over the last second or
//! so, so that while the engine renders ahead of the device
//! ([`EngineHandle::set_lead`](crate::EngineHandle::set_lead)) a reader
//! can take the one for the frame the device plays
//! ([`Meter::level_at`], [`Reading::get_at`]), and a meter moves when its
//! sound is heard.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Acquire, Ordering::Relaxed};
use std::sync::atomic::{Ordering::Release, fence};

use musicable_model::modulation::Output;
use musicable_model::param::ParamAddress;
use musicable_model::{Project, TrackId};

use crate::AudioRef;

/// How many frames of engine time each of a [`Timed`]'s slots holds the
/// readings of.
const GRAIN: usize = 256;

/// How many slots a [`Timed`] has: 1.4 s of readings at 48 kHz, and at
/// 192 kHz the longest lead, 200 ms, with blocks to spare.
const SLOTS: usize = 256;

/// The time in a [`Timed`]'s slot while its values change, or before it
/// has any.
const WRITING: u64 = u64::MAX;

/// Readings of `N` numbers a node works out as it renders, with the engine
/// time each is for, as far back as [`SLOTS`] slots of [`GRAIN`] frames go.
///
/// Engine time `t`'s slot is `t / GRAIN`'s, round a ring, and holds the
/// readings set last for a time in its stretch. The time beside them says
/// which they are, and stands in for a sequence number: while it's
/// [`WRITING`], they're changing, so a reader that sees it, or sees it
/// change while it reads, goes to the slot before.
struct Timed<const N: usize> {
    slots: Box<[Slot<N>]>,
    /// The time of the first readings set, or [`WRITING`] before any are.
    first: AtomicU64,
}

struct Slot<const N: usize> {
    time: AtomicU64,
    values: [AtomicU32; N],
}

impl<const N: usize> Default for Timed<N> {
    fn default() -> Self {
        let slot = || Slot {
            time: AtomicU64::new(WRITING),
            values: std::array::from_fn(|_| AtomicU32::new(0)),
        };
        Self {
            slots: (0..SLOTS).map(|_| slot()).collect(),
            first: AtomicU64::new(WRITING),
        }
    }
}

impl<const N: usize> fmt::Debug for Timed<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Timed").finish_non_exhaustive()
    }
}

impl<const N: usize> Timed<N> {
    /// Sets the readings for engine time `time`. Real-time safe. One
    /// thread at a time sets them.
    fn set(&self, time: u64, values: [f32; N]) {
        if self.first.load(Relaxed) == WRITING {
            self.first.store(time, Relaxed);
        }
        let slot = &self.slots[(time / GRAIN as u64) as usize % SLOTS];
        slot.time.store(WRITING, Relaxed);
        // Whoever sees a value set after this sees the slot changing.
        fence(Release);
        for (to, value) in slot.values.iter().zip(values) {
            to.store(value.to_bits(), Relaxed);
        }
        slot.time.store(time, Release);
    }

    /// The readings set last for a time at or before `time`, if the ring
    /// has any: none before the first ([`before_first`](Self::before_first)),
    /// or once the ring has gone round since.
    fn at(&self, time: u64) -> Option<[f32; N]> {
        let newest = time / GRAIN as u64;
        let oldest = newest.saturating_sub(SLOTS as u64 - 1);
        for stretch in (oldest..=newest).rev() {
            let slot = &self.slots[stretch as usize % SLOTS];
            let set = slot.time.load(Acquire);
            // Changing, later than `time`, or another stretch's, from a
            // turn of the ring before or another engine.
            if set == WRITING || set > time || set / GRAIN as u64 != stretch {
                continue;
            }
            let values = slot
                .values
                .each_ref()
                .map(|v| f32::from_bits(v.load(Relaxed)));
            fence(Acquire);
            if slot.time.load(Relaxed) == set {
                return Some(values);
            }
        }
        None
    }

    /// Whether `time` comes before the first readings, as far as the ring
    /// can tell: up to the end of their stretch, whose slot later readings
    /// in it take.
    fn before_first(&self, time: u64) -> bool {
        time / GRAIN as u64 <= self.first.load(Relaxed) / GRAIN as u64
    }

    /// Forgets every reading, as if none had been set, while none are.
    fn clear(&self) {
        for slot in &self.slots {
            slot.time.store(WRITING, Relaxed);
        }
        self.first.store(WRITING, Relaxed);
    }
}

/// A stereo level meter.
///
/// The peak reading holds the highest sample and falls back at
/// [`PEAK_FALL`](Self::PEAK_FALL) once the signal drops. The RMS reading
/// averages the signal's power over about [`RMS_TIME`](Self::RMS_TIME).
#[derive(Debug, Default)]
pub struct Meter {
    /// Linear peak per channel, as f32 bits.
    peak: [AtomicU32; 2],
    /// Mean square per channel, as f32 bits.
    power: [AtomicU32; 2],
    /// The highest sample of either channel since the meter started or was
    /// cleared, as f32 bits. The UI thread clears it while the audio thread
    /// raises it, so it only ever goes up with `fetch_max`: non-negative
    /// floats order like their bits.
    highest: AtomicU32,
    /// The peaks and the powers, left and right, as of the end of each
    /// stretch measured.
    timed: Timed<4>,
}

/// A meter's readings, linear: 1.0 is full scale.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Level {
    /// Left and right.
    pub peak: [f32; 2],
    /// Left and right.
    pub rms: [f32; 2],
    /// The highest sample since the meter started or was last cleared. It
    /// doesn't fall back: above 1.0, the channel went over full scale.
    pub highest: f32,
}

impl Meter {
    /// How fast the peak reading falls, in dB per second.
    pub const PEAK_FALL: f32 = 20.0;
    /// The RMS reading's time constant, in seconds.
    pub const RMS_TIME: f32 = 0.3;

    pub fn new() -> Self {
        Self::default()
    }

    /// The latest readings.
    pub fn level(&self) -> Level {
        let read = |value: &AtomicU32| f32::from_bits(value.load(Relaxed));
        Level {
            peak: self.peak.each_ref().map(read),
            rms: self.power.each_ref().map(|power| read(power).sqrt()),
            highest: read(&self.highest),
        }
    }

    /// The readings as of engine time `time`, as the device plays it: those
    /// of the last stretch measured that ended by then, at most a few
    /// milliseconds before. While the engine renders ahead, the latest are
    /// up to the lead later than that. Until the first stretch the meter
    /// measured ended, it reads silence, as it starts, and it reads the
    /// latest if it has none from by then in the last second or so. The
    /// highest reading is the latest, whatever the time.
    pub fn level_at(&self, time: u64) -> Level {
        let latest = self.level();
        match self.timed.at(time) {
            Some([left, right, power_left, power_right]) => Level {
                peak: [left, right],
                rms: [power_left.sqrt(), power_right.sqrt()],
                highest: latest.highest,
            },
            None if self.timed.before_first(time) => Level {
                highest: latest.highest,
                ..Level::default()
            },
            None => latest,
        }
    }

    /// Starts the highest reading again from silence. Any thread can call
    /// it.
    pub fn clear_highest(&self) {
        self.highest.store(0, Relaxed);
    }

    /// Starts the meter again, as new but for its highest reading, for an
    /// engine that starts its time again: see [`Meters::restart`].
    fn restart(&self) {
        for reading in self.peak.iter().chain(&self.power) {
            reading.store(0, Relaxed);
        }
        self.timed.clear();
    }

    /// Measures a block of `signal` that starts at engine time `time`,
    /// played at `sample_rate`, in stretches of up to 256 frames, each of
    /// whose readings [`level_at`](Self::level_at) has from its end. A mono
    /// signal reads the same in both channels.
    ///
    /// Real-time safe. One node at a time measures into a meter, so the
    /// readings it stores aren't raced.
    pub fn measure(&self, time: u64, signal: AudioRef<'_>, sample_rate: u32) {
        let frames = signal.frames();
        if frames == 0 || signal.channels() == 0 {
            return;
        }
        let mut start = 0;
        while start < frames {
            let end = (start + GRAIN).min(frames);
            let seconds = (end - start) as f32 / sample_rate as f32;
            let fall = 10_f32.powf(-Self::PEAK_FALL * seconds / 20.0);
            let keep = (-seconds / Self::RMS_TIME).exp();
            let mut readings = [0.0; 4];
            for c in 0..2 {
                let channel = &signal.channel(c.min(signal.channels() - 1))[start..end];
                let (mut peak, mut sum) = (0.0_f32, 0.0_f32);
                for &sample in channel {
                    peak = peak.max(sample.abs());
                    sum += sample * sample;
                }
                let power = sum / (end - start) as f32;
                // A reading that went infinite or NaN starts again, rather
                // than staying there.
                let held = f32::from_bits(self.peak[c].load(Relaxed));
                let held = if held.is_finite() { held * fall } else { 0.0 };
                let average = f32::from_bits(self.power[c].load(Relaxed));
                let average = if average.is_finite() {
                    power + (average - power) * keep
                } else {
                    power
                };
                let peak_held = peak.max(held);
                self.peak[c].store(peak_held.to_bits(), Relaxed);
                self.power[c].store(average.to_bits(), Relaxed);
                // Never NaN: `max` skips NaN samples.
                self.highest.fetch_max(peak.to_bits(), Relaxed);
                (readings[c], readings[2 + c]) = (peak_held, average);
            }
            self.timed.set(time + end as u64, readings);
            start = end;
        }
    }
}

/// A number a node works out as it plays, for any thread to read, such as
/// where a modulator's output has got to. It starts at 0.
#[derive(Debug, Default)]
pub struct Reading {
    latest: AtomicU32,
    timed: Timed<1>,
}

impl Reading {
    pub fn new() -> Self {
        Self::default()
    }

    /// The latest value.
    pub fn get(&self) -> f32 {
        f32::from_bits(self.latest.load(Relaxed))
    }

    /// The value as of engine time `time`, as the device plays it: the last
    /// set for a time by then, at most a few milliseconds before, or the
    /// latest if there's none from by then in the last second or so. None
    /// before the first was set, and for up to a few milliseconds after:
    /// what's heard then hasn't the reading in it yet. See
    /// [`Meter::level_at`].
    pub fn get_at(&self, time: u64) -> Option<f32> {
        match self.timed.at(time) {
            Some([value]) => Some(value),
            None if self.timed.before_first(time) => None,
            None => Some(self.get()),
        }
    }

    /// Sets the value for engine time `time`. Real-time safe. One node at a
    /// time sets a reading, so what it sets isn't raced.
    pub fn set(&self, time: u64, value: f32) {
        self.latest.store(value.to_bits(), Relaxed);
        self.timed.set(time, [value]);
    }

    /// Starts the reading again from 0, as new: see [`Meters::restart`].
    fn restart(&self) {
        self.latest.store(0, Relaxed);
        self.timed.clear();
    }
}

/// The meters of a project's channels: one for each track, and one for the
/// master. And the readings of its modulators, which their channels'
/// [`Modulation`](crate::modulation::Modulation) nodes set at each tick:
/// where each output is, and how far the routings that play move each
/// parameter they move, as a share of its range
/// ([`Routing::amount`](musicable_model::modulation::Routing::amount)), or
/// in a plugin's units for a plugin's parameter.
#[derive(Debug, Clone, Default)]
pub struct Meters {
    tracks: BTreeMap<TrackId, Arc<Meter>>,
    master: Arc<Meter>,
    outputs: BTreeMap<Output, Arc<Reading>>,
    moved: BTreeMap<ParamAddress, Arc<Reading>>,
}

impl Meters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Meters for every track of `project`, and readings for every output
    /// of its modulators and every parameter its routings move: those that
    /// had one keep it, new ones get one, and the rest go.
    pub fn follow(&mut self, project: &Project) {
        let mut tracks = BTreeMap::new();
        for track in &project.tracks {
            let meter = self.tracks.remove(&track.id).unwrap_or_default();
            tracks.insert(track.id, meter);
        }
        self.tracks = tracks;
        let (mut outputs, mut moved) = (BTreeMap::new(), BTreeMap::new());
        for (_, modulation) in project.modulations() {
            for modulator in &modulation.modulators {
                for n in modulator.outputs() {
                    let output = Output::new(modulator.id, n);
                    outputs.insert(output, self.outputs.remove(&output).unwrap_or_default());
                }
            }
            for routing in &modulation.routings {
                let param = routing.to;
                (moved.entry(param))
                    .or_insert_with(|| self.moved.remove(&param).unwrap_or_default());
            }
        }
        self.outputs = outputs;
        self.moved = moved;
    }

    /// The meter of the track with `id`, if it has one.
    pub fn track(&self, id: TrackId) -> Option<&Arc<Meter>> {
        self.tracks.get(&id)
    }

    pub fn master(&self) -> &Arc<Meter> {
        &self.master
    }

    /// The reading of `output`, a modulator's, if it has one: from -1 to 1
    /// for a bipolar output, and from 0 to 1 for the others.
    pub fn output(&self, output: Output) -> Option<&Arc<Reading>> {
        self.outputs.get(&output)
    }

    /// The reading of how far modulators move the parameter at `param`, if
    /// it has one.
    pub fn moved(&self, param: ParamAddress) -> Option<&Arc<Reading>> {
        self.moved.get(&param)
    }

    /// Clears the highest reading of every meter.
    pub fn clear_highest(&self) {
        for meter in self.tracks.values().chain([&self.master]) {
            meter.clear_highest();
        }
    }

    /// Starts every meter and reading again, as new, but for the meters'
    /// highest readings, for an engine whose time starts again from 0 to
    /// measure into them: the readings with the old engine's times would
    /// read as the new one's. Call it while no engine measures into them.
    pub fn restart(&self) {
        for meter in self.tracks.values().chain([&self.master]) {
            meter.restart();
        }
        for reading in self.outputs.values().chain(self.moved.values()) {
            reading.restart();
        }
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::Chain;

    use super::*;

    const RATE: u32 = 48_000;

    /// Measures `blocks` blocks of `frames` frames of a stereo signal, the
    /// left channel at `left` and the right at `right`, from engine time 0.
    fn measure(meter: &Meter, left: f32, right: f32, frames: usize, blocks: usize) {
        let data: Vec<f32> = [vec![left; frames], vec![right; frames]].concat();
        for block in 0..blocks {
            let time = (block * frames) as u64;
            meter.measure(time, AudioRef::from_planar(&data, 2), RATE);
        }
    }

    #[test]
    fn peaks_hold_and_fall_back() {
        let meter = Meter::new();
        assert_eq!(meter.level(), Level::default());
        measure(&meter, 0.5, -1.0, 64, 1);
        assert_eq!(meter.level().peak, [0.5, 1.0]);
        // A second of silence: 20 dB down, a tenth.
        measure(&meter, 0.0, 0.0, 480, 100);
        let [left, right] = meter.level().peak;
        assert!((left - 0.05).abs() < 1e-4, "{left}");
        assert!((right - 0.1).abs() < 1e-4, "{right}");
    }

    #[test]
    fn rms_settles_on_the_signals_power() {
        let meter = Meter::new();
        // A square wave at ±0.5 has an RMS of 0.5.
        let wave: Vec<f32> = (0..128)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        let stereo = [wave.clone(), wave].concat();
        // After one time constant, 1 - 1/e of the way there, in power.
        for _ in 0..(RATE as f32 * Meter::RMS_TIME / 128.0) as usize {
            meter.measure(0, AudioRef::from_planar(&stereo, 2), RATE);
        }
        let rms = meter.level().rms[0];
        let expected = (0.25 * (1.0 - (-1.0_f32).exp())).sqrt();
        assert!((rms - expected).abs() < 0.01, "{rms}");
        for _ in 0..1000 {
            meter.measure(0, AudioRef::from_planar(&stereo, 2), RATE);
        }
        assert!((meter.level().rms[1] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn the_highest_reading_holds_until_cleared() {
        let meter = Meter::new();
        measure(&meter, 0.5, -1.25, 64, 1);
        measure(&meter, 0.25, 0.0, 480, 1000);
        assert_eq!(meter.level().highest, 1.25);
        meter.clear_highest();
        assert_eq!(meter.level().highest, 0.0);
        measure(&meter, 0.25, -0.125, 64, 1);
        assert_eq!(meter.level().highest, 0.25);
        // An infinite sample is the highest there is, and NaN never is.
        measure(&meter, f32::NAN, f32::INFINITY, 64, 1);
        assert_eq!(meter.level().highest, f32::INFINITY);
        measure(&meter, f32::NAN, f32::NAN, 64, 1);
        assert_eq!(meter.level().highest, f32::INFINITY);
    }

    #[test]
    fn readings_recover_from_infinity_and_nan() {
        let meter = Meter::new();
        measure(&meter, f32::INFINITY, f32::NAN, 64, 1);
        assert_eq!(meter.level().peak[0], f32::INFINITY);
        assert!(meter.level().rms[1].is_nan());
        measure(&meter, 0.25, 0.25, 64, 2);
        assert_eq!(meter.level().peak, [0.25, 0.25]);
        assert_eq!(meter.level().rms[1], 0.25);
    }

    #[test]
    fn readings_keep_the_time_they_are_for() {
        let meter = Meter::new();
        // Four blocks of 1024 frames, each louder than the one before.
        for block in 0..4 {
            let level = 0.1 * (block + 1) as f32;
            let data = vec![level; 2 * 1024];
            meter.measure(block * 1024, AudioRef::from_planar(&data, 2), RATE);
        }
        let peak_at = |time| meter.level_at(time).peak[0];
        // What had been measured by each time: each block's stretches end
        // every 256 frames.
        assert_eq!(peak_at(1024), 0.1);
        assert_eq!(peak_at(2048 + 300), 0.3);
        assert_eq!(peak_at(4096), 0.4);
        assert_eq!(peak_at(5000), 0.4);
        assert_eq!(meter.level().peak[0], 0.4);
        // The RMS reading, rising, comes with the peaks, and the highest
        // reading is the latest.
        let rms = |time| meter.level_at(time).rms[1];
        assert!(rms(1024) < rms(2048 + 300) && rms(2048 + 300) < rms(4096));
        assert_eq!(meter.level_at(1024).highest, 0.4);
        // Before the first stretch had ended, the meter reads silence, as it
        // starts, but for the highest reading.
        let level = meter.level_at(100);
        assert_eq!(
            (level.peak, level.rms, level.highest),
            ([0.0; 2], [0.0; 2], 0.4)
        );
        // Once the ring has gone round, the early stretches' slots have
        // later readings, so for their times there are only the latest.
        let data = vec![0.5; 2 * 1024];
        meter.measure(65_536, AudioRef::from_planar(&data, 2), RATE);
        assert_eq!(peak_at(1024), 0.5);
        assert_eq!(meter.level_at(1024).rms, meter.level().rms);
    }

    #[test]
    fn readings_keep_their_times_too() {
        let reading = Reading::new();
        for tick in 0..64 {
            reading.set(tick * 32, tick as f32);
        }
        assert_eq!(reading.get(), 63.0);
        // The last set for each stretch of 256 frames, and by the time.
        assert_eq!(reading.get_at(32 * 20), Some(15.0));
        assert_eq!(reading.get_at(32 * 23 + 7), Some(23.0));
        assert_eq!(reading.get_at(10_000), Some(63.0));
        // The first stretch's slot has the last set in it, at 224, so before
        // then there's none.
        assert_eq!(reading.get_at(100), None);
        assert_eq!(reading.get_at(224), Some(7.0));
    }

    #[test]
    fn readings_new_to_an_engine_rendering_ahead_wait_for_their_time() {
        // A meter and a reading new to a graph that went in at engine time
        // 48 000, while the device played 9600 frames, 200 ms, behind.
        let (meter, reading) = (Meter::new(), Reading::new());
        let data = vec![0.5; 2 * 1024];
        for block in 0..10 {
            let time = 48_000 + block * 1024;
            meter.measure(time, AudioRef::from_planar(&data, 2), RATE);
            reading.set(time, 0.75);
        }
        // What the device plays is from before they had any.
        assert_eq!(meter.level_at(48_000 - 9600).peak, [0.0; 2]);
        assert_eq!(meter.level_at(48_000 + 100).rms, [0.0; 2]);
        assert_eq!(reading.get_at(48_000 - 1), None);
        // Then from when they did.
        assert_eq!(meter.level_at(48_000 + 300).peak, [0.5; 2]);
        assert_eq!(reading.get_at(48_000), Some(0.75));
    }

    #[test]
    fn restarted_meters_forget_the_old_engines_times() {
        let mut project = Project::new();
        let a = project.add_track("A", musicable_model::TrackKind::Audio);
        let mut meters = Meters::new();
        meters.follow(&project);
        // An engine that played for a third of a second, loud.
        let meter = Arc::clone(meters.track(a).unwrap());
        measure(&meter, 1.25, 0.5, 512, 30);
        assert_eq!(meter.level_at(1024).peak, [1.25, 0.5]);
        meters.restart();
        // A new one starts again from 0, with nothing measured yet.
        let level = meter.level_at(1024);
        assert_eq!(
            (level.peak, level.rms, level.highest),
            ([0.0; 2], [0.0; 2], 1.25)
        );
        assert_eq!(
            meter.level(),
            Level {
                highest: 1.25,
                ..Level::default()
            }
        );
        measure(&meter, 0.25, 0.25, 512, 4);
        assert_eq!(meter.level_at(1024).peak, [0.25; 2]);
    }

    #[test]
    fn readings_are_never_read_while_they_change() {
        use std::sync::atomic::AtomicBool;
        let timed = Arc::new(Timed::<4>::default());
        let started = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let writer = {
            let timed = Arc::clone(&timed);
            let (started, done) = (Arc::clone(&started), Arc::clone(&done));
            std::thread::spawn(move || {
                let mut time = 0;
                while !done.load(Relaxed) {
                    // Each reading is its time, four times over.
                    timed.set(time, [time as f32; 4]);
                    started.store(true, Relaxed);
                    time = (time + 37) % (1 << 17);
                }
            })
        };
        // Once it's set some.
        while !started.load(Relaxed) {
            std::thread::yield_now();
        }
        let mut read = 0;
        for round in 0..200_000_u64 {
            let time = round * 41 % (1 << 17);
            if let Some(values) = timed.at(time) {
                assert!(values.iter().all(|&v| v == values[0]), "{values:?}");
                assert!(values[0] as u64 <= time, "{values:?} at {time}");
                read += 1;
            }
        }
        done.store(true, Relaxed);
        writer.join().unwrap();
        assert!(read > 0);
    }

    #[test]
    fn mono_reads_the_same_in_both_channels() {
        let meter = Meter::new();
        let mono = [0.75; 32];
        meter.measure(0, AudioRef::from_planar(&mono, 1), RATE);
        let level = meter.level();
        assert_eq!(level.peak, [0.75, 0.75]);
        assert_eq!(level.rms[0], level.rms[1]);
    }

    #[test]
    fn tracks_keep_their_meters() {
        let mut project = Project::new();
        let a = project.add_track("A", musicable_model::TrackKind::Audio);
        let mut meters = Meters::new();
        assert!(meters.track(a).is_none());
        meters.follow(&project);
        let meter = Arc::clone(meters.track(a).unwrap());
        let b = project.add_track("B", musicable_model::TrackKind::Audio);
        meters.follow(&project);
        assert!(Arc::ptr_eq(meters.track(a).unwrap(), &meter));
        assert!(meters.track(b).is_some());
        project.tracks.remove(0);
        meters.follow(&project);
        assert!(meters.track(a).is_none());
    }

    #[test]
    fn modulators_outputs_and_the_parameters_they_move_keep_their_readings() {
        use musicable_model::modulation::ModulatorKind;
        let mut project = Project::new();
        let a = project.add_track("A", musicable_model::TrackKind::Audio);
        let lfo = project.new_modulator(ModulatorKind::Lfo, "LFO 1");
        let (lfo_out, volume) = (
            Output::new(lfo.id, 0),
            ParamAddress::Volume(Chain::Track(a)),
        );
        let routing = project.new_routing(lfo_out, volume, 0.5);
        let modulation = &mut project.track_mut(a).unwrap().modulation;
        modulation.modulators.push(lfo);
        modulation.routings.push(routing);
        let mut meters = Meters::new();
        meters.follow(&project);
        let (output, moved) = (
            meters.output(lfo_out).unwrap(),
            meters.moved(volume).unwrap(),
        );
        let (output, moved) = (Arc::clone(output), Arc::clone(moved));
        output.set(0, -0.5);
        assert_eq!(meters.output(lfo_out).unwrap().get(), -0.5);

        // A second routing to the same parameter shares its reading, and a
        // second modulator gets its own.
        let macros = project.new_modulator(ModulatorKind::Macro, "Macro 1");
        let knob = Output::new(macros.id, 0);
        let second = project.new_routing(knob, volume, 0.25);
        let modulation = &mut project.track_mut(a).unwrap().modulation;
        modulation.modulators.push(macros);
        modulation.routings.push(second);
        meters.follow(&project);
        assert!(Arc::ptr_eq(meters.output(lfo_out).unwrap(), &output));
        assert!(Arc::ptr_eq(meters.moved(volume).unwrap(), &moved));
        assert_eq!(meters.output(knob).unwrap().get(), 0.0);

        // Without routings to it, the parameter's reading goes.
        project.track_mut(a).unwrap().modulation.routings.clear();
        meters.follow(&project);
        assert!(meters.moved(volume).is_none());
        assert!(meters.output(knob).is_some());
        project.tracks.clear();
        meters.follow(&project);
        assert!(meters.output(lfo_out).is_none());
    }

    #[test]
    fn clearing_clears_every_meter() {
        let mut project = Project::new();
        let a = project.add_track("A", musicable_model::TrackKind::Audio);
        let mut meters = Meters::new();
        meters.follow(&project);
        measure(meters.track(a).unwrap(), 0.5, 0.5, 64, 1);
        measure(meters.master(), 2.0, 0.5, 64, 1);
        meters.clear_highest();
        assert_eq!(meters.track(a).unwrap().level().highest, 0.0);
        assert_eq!(meters.master().level().highest, 0.0);
    }
}
