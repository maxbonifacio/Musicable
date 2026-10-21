//! Built-in nodes.

use std::f64::consts::TAU;
use std::ops::Range;

use musicable_model::time::{BeatRange, Beats};

use crate::{
    AudioMut, EngineConfig, Inputs, Node, NodeEvent, NodeEventKind, ParamId, ProcessContext,
};

/// Sums its inputs. Used for buses and the master.
#[derive(Debug, Clone)]
pub struct Sum {
    channels: usize,
}

impl Sum {
    pub fn new(channels: usize) -> Self {
        Self { channels }
    }
}

impl Default for Sum {
    /// Stereo.
    fn default() -> Self {
        Self::new(2)
    }
}

impl Node for Sum {
    fn channels(&self) -> usize {
        self.channels
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext<'_>,
        inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
    }
}

/// Sums its inputs and applies a gain: a track's gain stage.
#[derive(Debug, Clone)]
pub struct Gain {
    gain: f32,
    muted: bool,
    channels: usize,
}

impl Gain {
    /// Linear gain, 1.0 is unity.
    pub const GAIN: ParamId = ParamId(0);
    /// 1.0 mutes, 0.0 unmutes.
    pub const MUTE: ParamId = ParamId(1);

    /// A stereo gain stage.
    pub fn new(gain: f32) -> Self {
        Self {
            gain,
            muted: false,
            channels: 2,
        }
    }

    pub fn with_muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param {
                param: Self::GAIN,
                value,
            } => self.gain = value as f32,
            NodeEventKind::Param {
                param: Self::MUTE,
                value,
            } => self.muted = value >= 0.5,
            _ => {}
        }
    }

    fn scale(&self, output: &mut AudioMut<'_>, frames: Range<usize>) {
        let mut part = output.range_mut(frames);
        if self.muted {
            part.clear();
        } else {
            for channel in part.channels_mut() {
                for sample in channel {
                    *sample *= self.gain;
                }
            }
        }
    }
}

impl Node for Gain {
    fn channels(&self) -> usize {
        self.channels
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let mut start = 0;
        for event in ctx.events {
            self.scale(output, start..event.offset);
            self.apply(event);
            start = event.offset;
        }
        self.scale(output, start..ctx.frames);
    }
}

/// Outputs a constant value. Mostly useful in tests.
#[derive(Debug, Clone)]
pub struct Constant {
    value: f32,
    channels: usize,
}

impl Constant {
    /// Stereo.
    pub fn new(value: f32) -> Self {
        Self { value, channels: 2 }
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }
}

impl Node for Constant {
    fn channels(&self) -> usize {
        self.channels
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        output.fill(self.value);
    }
}

/// A sine wave that sounds while the transport plays: Tier 0's test tone.
///
/// The phase follows the song position, so a position always renders the
/// same samples, however the blocks fall and however often the graph is
/// rebuilt.
#[derive(Debug, Clone)]
pub struct Sine {
    frequency: f64,
    level: f32,
    channels: usize,
    sample_rate: f64,
}

impl Sine {
    /// Frequency in Hz. The phase jumps when it changes during playback.
    pub const FREQUENCY: ParamId = ParamId(0);
    /// Linear amplitude.
    pub const LEVEL: ParamId = ParamId(1);

    /// Stereo, the same signal in both channels.
    pub fn new(frequency: f64, level: f32) -> Self {
        Self {
            frequency,
            level,
            channels: 2,
            sample_rate: 48_000.0,
        }
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param {
                param: Self::FREQUENCY,
                value,
            } => self.frequency = value,
            NodeEventKind::Param {
                param: Self::LEVEL,
                value,
            } => self.level = value as f32,
            _ => {}
        }
    }

    fn render(&self, ctx: &ProcessContext<'_>, output: &mut AudioMut<'_>, frames: Range<usize>) {
        if !ctx.transport.playing {
            output.range_mut(frames).clear();
            return;
        }
        let cycles_per_frame = self.frequency / self.sample_rate;
        for i in frames {
            let position = ctx.transport.position + i as u64;
            let phase = (position as f64 * cycles_per_frame).fract();
            let sample = (phase * TAU).sin() as f32 * self.level;
            for channel in output.channels_mut() {
                channel[i] = sample;
            }
        }
    }
}

impl Node for Sine {
    fn channels(&self) -> usize {
        self.channels
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.sample_rate = f64::from(config.sample_rate);
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        let mut start = 0;
        for event in ctx.events {
            self.render(ctx, output, start..event.offset);
            self.apply(event);
            start = event.offset;
        }
        self.render(ctx, output, start..ctx.frames);
    }
}

/// Clicks on each beat of the time signature while the transport plays, with
/// an accented downbeat.
///
/// A click starts on the frame its beat plays on (see
/// [`TempoMap::frame_at`](musicable_model::time::TempoMap::frame_at)) and
/// rings for 30 ms, even if the transport stops or the playhead jumps.
#[derive(Debug, Clone)]
pub struct Metronome {
    enabled: bool,
    level: f32,
    channels: usize,
    sample_rate: u32,
    /// The click sounding now.
    click: Option<Click>,
}

#[derive(Debug, Clone, Copy)]
struct Click {
    accent: bool,
    /// Frames since it started.
    frame: u32,
}

impl Metronome {
    /// 1.0 turns the clicks on, 0.0 off. A click already sounding rings out.
    pub const ENABLED: ParamId = ParamId(0);
    /// Linear amplitude of the accented click. The others are half as loud.
    pub const LEVEL: ParamId = ParamId(1);

    const CLICK_SECONDS: f64 = 0.03;
    const ACCENT_HZ: f64 = 1760.0;
    const BEAT_HZ: f64 = 880.0;

    /// Stereo, on, at level 0.5.
    pub fn new() -> Self {
        Self {
            enabled: true,
            level: 0.5,
            channels: 2,
            sample_rate: 48_000,
            click: None,
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn with_level(mut self, level: f32) -> Self {
        self.level = level;
        self
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param {
                param: Self::ENABLED,
                value,
            } => self.enabled = value >= 0.5,
            NodeEventKind::Param {
                param: Self::LEVEL,
                value,
            } => self.level = value as f32,
            _ => {}
        }
    }

    fn render(
        &mut self,
        ctx: &ProcessContext<'_>,
        output: &mut AudioMut<'_>,
        frames: Range<usize>,
    ) {
        // The song positions this part of the block plays, and the clicks
        // that start in it.
        let first = ctx.transport.position as i64 + frames.start as i64;
        let end = first + frames.len() as i64;
        let tempo = ctx.tempo;
        let rate = ctx.sample_rate;
        let mut onsets = (ctx.transport.playing && self.enabled)
            .then(|| {
                // Every beat that can round onto these frames, with a frame
                // to spare on each side, then the ones that do.
                let from = Beats::from_f64(tempo.beats_at(first as f64 - 1.0, rate));
                let to = Beats::from_f64(tempo.beats_at(end as f64, rate));
                tempo
                    .beats_in(BeatRange::new(from, to + Beats::from_ticks(1)))
                    .map(move |beat| (tempo.frame_at(beat.at, rate), beat.beat == 0))
                    .filter(move |&(frame, _)| first <= frame && frame < end)
            })
            .into_iter()
            .flatten()
            .peekable();
        if self.click.is_none() && onsets.peek().is_none() {
            output.range_mut(frames).clear();
            return;
        }

        let length = (Self::CLICK_SECONDS * f64::from(rate)).round() as u32;
        for (i, frame) in frames.zip(first..) {
            while let Some((_, accent)) = onsets.next_if(|&(at, _)| at == frame) {
                self.click = Some(Click { accent, frame: 0 });
            }
            let sample = match &mut self.click {
                Some(click) => {
                    let sample = self.level * click_sample(*click, rate);
                    click.frame += 1;
                    if click.frame >= length {
                        self.click = None;
                    }
                    sample
                }
                None => 0.0,
            };
            for channel in output.channels_mut() {
                channel[i] = sample;
            }
        }
    }
}

/// A click at full level: a decaying sine, starting at its peak so the
/// onset is sharp.
fn click_sample(click: Click, sample_rate: u32) -> f32 {
    let t = f64::from(click.frame) / f64::from(sample_rate);
    let (frequency, gain) = match click.accent {
        true => (Metronome::ACCENT_HZ, 1.0),
        false => (Metronome::BEAT_HZ, 0.5),
    };
    let envelope = (1.0 - t / Metronome::CLICK_SECONDS).powi(4);
    ((TAU * frequency * t).cos() * envelope * gain) as f32
}

impl Default for Metronome {
    fn default() -> Self {
        Self::new()
    }
}

impl Node for Metronome {
    fn channels(&self) -> usize {
        self.channels
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.sample_rate = config.sample_rate;
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Metronome>() {
            self.click = previous.click;
        }
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        let mut start = 0;
        for event in ctx.events {
            self.render(ctx, output, start..event.offset);
            self.apply(event);
            start = event.offset;
        }
        self.render(ctx, output, start..ctx.frames);
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::time::{TempoMap, TimeSignature};

    use super::*;
    use crate::{AudioBuffer, AudioRef, TransportState};

    fn context<'a>(
        frames: usize,
        events: &'a [NodeEvent],
        playing: bool,
        tempo: &'a TempoMap,
    ) -> ProcessContext<'a> {
        ProcessContext {
            frames,
            sample_rate: 48_000,
            time: 0,
            transport: TransportState {
                playing,
                ..TransportState::default()
            },
            tempo,
            events,
        }
    }

    fn param(offset: usize, param: ParamId, value: f64) -> NodeEvent {
        NodeEvent {
            offset,
            kind: NodeEventKind::Param { param, value },
        }
    }

    #[test]
    fn gain_changes_at_the_event_offset() {
        let ones = [1.0; 6];
        let inputs = [AudioRef::from_planar(&ones, 1)];
        let events = [
            param(2, Gain::GAIN, 0.5),
            param(4, Gain::MUTE, 1.0),
            param(5, Gain::MUTE, 0.0),
        ];
        let mut out = AudioBuffer::new(1, 6);
        Gain::new(2.0).with_channels(1).process(
            &context(6, &events, false, &TempoMap::default()),
            Inputs::from_views(&inputs),
            &mut out.view_mut(6),
        );
        assert_eq!(out.view(6).channel(0), [2.0, 2.0, 0.5, 0.5, 0.0, 0.5]);
    }

    #[test]
    fn sine_is_silent_until_the_transport_plays() {
        let mut sine = Sine::new(1000.0, 1.0);
        sine.prepare(&EngineConfig {
            sample_rate: 48_000,
            max_block: 64,
        });
        let tempo = TempoMap::default();
        let mut out = AudioBuffer::new(2, 64);
        sine.process(
            &context(64, &[], false, &tempo),
            Inputs::NONE,
            &mut out.view_mut(64),
        );
        assert!(out.view(64).iter().flatten().all(|&s| s == 0.0));

        sine.process(
            &context(64, &[], true, &tempo),
            Inputs::NONE,
            &mut out.view_mut(64),
        );
        let left = out.view(64).channel(0);
        // 1 kHz at 48 kHz peaks 12 frames in.
        assert_eq!(left[0], 0.0);
        assert!((left[12] - 1.0).abs() < 1e-6);
        assert_eq!(left, out.view(64).channel(1));
    }

    /// Runs a mono metronome over `frames` frames from song position `from`,
    /// in blocks of `block`, at 48 kHz.
    fn clicks(
        metronome: &mut Metronome,
        tempo: &TempoMap,
        playing: bool,
        from: u64,
        frames: usize,
        block: usize,
    ) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames);
        let mut buffer = AudioBuffer::new(1, block);
        while out.len() < frames {
            let n = block.min(frames - out.len());
            let ctx = ProcessContext {
                transport: TransportState {
                    playing,
                    position: from + out.len() as u64,
                    looping: None,
                },
                ..context(n, &[], playing, tempo)
            };
            metronome.process(&ctx, Inputs::NONE, &mut buffer.view_mut(n));
            out.extend_from_slice(buffer.view(n).channel(0));
        }
        out
    }

    /// Where clicks start, out of silence, and how loud they start.
    fn onsets(out: &[f32]) -> Vec<(usize, f32)> {
        (0..out.len())
            .filter(|&i| out[i] != 0.0 && (i == 0 || out[i - 1] == 0.0))
            .map(|i| (i, out[i]))
            .collect()
    }

    fn mono_metronome() -> Metronome {
        Metronome::new().with_channels(1)
    }

    #[test]
    fn the_metronome_accents_the_downbeat() {
        // 3/4 at 120 BPM: a beat every 24,000 frames.
        let tempo = TempoMap::new(120.0, TimeSignature::new(3, 4).unwrap());
        let out = clicks(&mut mono_metronome(), &tempo, true, 0, 96_000, 1000);
        assert_eq!(
            onsets(&out),
            [(0, 0.5), (24_000, 0.25), (48_000, 0.25), (72_000, 0.5)]
        );
        // However the blocks fall.
        for block in [1, 333, 4096] {
            let other = clicks(&mut mono_metronome(), &tempo, true, 0, 96_000, block);
            assert!(other == out, "blocks of {block}");
        }
        // Mid-song, from a jump.
        let out = clicks(&mut mono_metronome(), &tempo, true, 71_000, 2_000, 64);
        assert_eq!(onsets(&out), [(1_000, 0.5)]);
    }

    #[test]
    fn the_metronome_is_silent_when_stopped_or_off() {
        let tempo = TempoMap::default();
        let stopped = clicks(&mut mono_metronome(), &tempo, false, 0, 30_000, 512);
        assert!(stopped.iter().all(|&s| s == 0.0));
        let mut off = mono_metronome().with_enabled(false);
        assert!(
            clicks(&mut off, &tempo, true, 0, 30_000, 512)
                .iter()
                .all(|&s| s == 0.0)
        );
    }

    #[test]
    fn a_click_rings_out_after_the_transport_stops() {
        let tempo = TempoMap::default();
        let mut metronome = mono_metronome();
        let mut out = clicks(&mut metronome, &tempo, true, 0, 100, 100);
        out.extend(clicks(&mut metronome, &tempo, false, 100, 23_900, 100));
        // 30 ms at 48 kHz.
        assert_ne!(out[1439], 0.0);
        assert!(out[1440..].iter().all(|&s| s == 0.0));
        assert_eq!(onsets(&out), [(0, 0.5)]);
    }

    #[test]
    fn a_new_metronome_finishes_the_click() {
        let tempo = TempoMap::default();
        let whole = clicks(&mut mono_metronome(), &tempo, true, 0, 2000, 100);
        let mut first = mono_metronome();
        let mut out = clicks(&mut first, &tempo, true, 0, 1000, 100);
        let mut second = mono_metronome();
        second.adopt(&mut first);
        out.extend(clicks(&mut second, &tempo, true, 1000, 1000, 100));
        assert!(out == whole);
    }
}
