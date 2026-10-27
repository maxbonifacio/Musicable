//! Level meters: what the audio thread measures, for any thread to read.
//!
//! A [`Meter`] sits in a node in the graph, which measures each block it
//! renders into it. The readings live in atomics, so reading them never
//! waits on the audio thread, and a meter carries on across graph swaps
//! when the new graph's node gets the same one. [`Meters`] keeps one for
//! each channel of a project, for [`compile_metered`](crate::compile::compile_metered)
//! to put in the graph and the UI to read, and a [`Reading`] of each
//! modulator's outputs and of what moves each parameter modulators move.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use musicable_model::modulation::Output;
use musicable_model::param::ParamAddress;
use musicable_model::{Project, TrackId};

use crate::AudioRef;

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

    /// Starts the highest reading again from silence. Any thread can call
    /// it.
    pub fn clear_highest(&self) {
        self.highest.store(0, Relaxed);
    }

    /// Measures a block of `signal`, played at `sample_rate`. A mono signal
    /// reads the same in both channels.
    ///
    /// Real-time safe. One node at a time measures into a meter, so the
    /// readings it stores aren't raced.
    pub fn measure(&self, signal: AudioRef<'_>, sample_rate: u32) {
        let frames = signal.frames();
        if frames == 0 || signal.channels() == 0 {
            return;
        }
        let seconds = frames as f32 / sample_rate as f32;
        let fall = 10_f32.powf(-Self::PEAK_FALL * seconds / 20.0);
        let keep = (-seconds / Self::RMS_TIME).exp();
        for c in 0..2 {
            let channel = signal.channel(c.min(signal.channels() - 1));
            let (mut peak, mut sum) = (0.0_f32, 0.0_f32);
            for &sample in channel {
                peak = peak.max(sample.abs());
                sum += sample * sample;
            }
            let power = sum / frames as f32;
            // A reading that went infinite or NaN starts again, rather than
            // staying there.
            let held = f32::from_bits(self.peak[c].load(Relaxed));
            let held = if held.is_finite() { held * fall } else { 0.0 };
            let average = f32::from_bits(self.power[c].load(Relaxed));
            let average = if average.is_finite() {
                power + (average - power) * keep
            } else {
                power
            };
            self.peak[c].store(peak.max(held).to_bits(), Relaxed);
            self.power[c].store(average.to_bits(), Relaxed);
            // Never NaN: `max` skips NaN samples.
            self.highest.fetch_max(peak.to_bits(), Relaxed);
        }
    }
}

/// A number a node works out as it plays, for any thread to read, such as
/// where a modulator's output has got to. It starts at 0.
#[derive(Debug, Default)]
pub struct Reading(AtomicU32);

impl Reading {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }

    /// Real-time safe. One node at a time sets a reading, so what it sets
    /// isn't raced.
    pub fn set(&self, value: f32) {
        self.0.store(value.to_bits(), Relaxed);
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
}

#[cfg(test)]
mod tests {
    use musicable_model::Chain;

    use super::*;

    const RATE: u32 = 48_000;

    /// Measures `blocks` blocks of `frames` frames of a stereo signal, the
    /// left channel at `left` and the right at `right`.
    fn measure(meter: &Meter, left: f32, right: f32, frames: usize, blocks: usize) {
        let data: Vec<f32> = [vec![left; frames], vec![right; frames]].concat();
        for _ in 0..blocks {
            meter.measure(AudioRef::from_planar(&data, 2), RATE);
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
            meter.measure(AudioRef::from_planar(&stereo, 2), RATE);
        }
        let rms = meter.level().rms[0];
        let expected = (0.25 * (1.0 - (-1.0_f32).exp())).sqrt();
        assert!((rms - expected).abs() < 0.01, "{rms}");
        for _ in 0..1000 {
            meter.measure(AudioRef::from_planar(&stereo, 2), RATE);
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
    fn mono_reads_the_same_in_both_channels() {
        let meter = Meter::new();
        let mono = [0.75; 32];
        meter.measure(AudioRef::from_planar(&mono, 1), RATE);
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
        output.set(-0.5);
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
