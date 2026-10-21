//! Built-in nodes.

use std::f64::consts::TAU;
use std::ops::Range;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioBuffer, AudioRef, TransportState};

    fn context(frames: usize, events: &[NodeEvent], playing: bool) -> ProcessContext<'_> {
        ProcessContext {
            frames,
            sample_rate: 48_000,
            time: 0,
            transport: TransportState {
                playing,
                position: 0,
            },
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
            &context(6, &events, false),
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
        let mut out = AudioBuffer::new(2, 64);
        sine.process(
            &context(64, &[], false),
            Inputs::NONE,
            &mut out.view_mut(64),
        );
        assert!(out.view(64).iter().flatten().all(|&s| s == 0.0));

        sine.process(&context(64, &[], true), Inputs::NONE, &mut out.view_mut(64));
        let left = out.view(64).channel(0);
        // 1 kHz at 48 kHz peaks 12 frames in.
        assert_eq!(left[0], 0.0);
        assert!((left[12] - 1.0).abs() < 1e-6);
        assert_eq!(left, out.view(64).channel(1));
    }
}
