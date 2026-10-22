//! Musicable's own devices: the DSP of each kind in
//! [`musicable_model::native`], and what they share.
//!
//! A built-in device's node keeps its parameters in [`Params`], which works
//! out each one's value for every frame with an
//! [`automation::Param`](crate::automation::Param): its automation at the
//! song position, or else the value set, ramping to each new one over 10
//! ms, plus any modulation, which applies at once. The node hands its
//! automation to its `Params` ([`Params::automate`]), so it follows lanes
//! frame by frame rather than as parameter events. It renders a block in spans
//! that end where its events fall ([`Params::run`]), so a parameter change,
//! a modulation or a note lands on its frame however the blocks fall, and
//! a device renders the same sample for sample whatever their length.
//!
//! A kind whose DSP isn't built yet has no node, so it passes the signal
//! through unchanged. [`BUILT`] lists the kinds that have one.

mod compressor;
mod delay;
mod eq;
mod gate;
mod params;
mod reverb;
mod svf;
mod utility;

use musicable_model::native::{NativeDevice, NativeKind};

pub use compressor::Compressor;
pub use delay::Delay;
pub use eq::Equalizer;
pub use gate::Gate;
pub use params::{Params, Span, Values};
pub use reverb::Reverb;
pub use utility::Utility;

use crate::Node;

/// The kinds with DSP so far, in the browser's order.
pub const BUILT: &[NativeKind] = &[
    NativeKind::Eq,
    NativeKind::Compressor,
    NativeKind::Gate,
    NativeKind::Delay,
    NativeKind::Reverb,
    NativeKind::Utility,
];

/// The node that plays `device`, if its kind has DSP.
pub fn node(device: &NativeDevice) -> Option<Box<dyn Node>> {
    match device.kind {
        NativeKind::Eq => Some(Box::new(Equalizer::new(device))),
        NativeKind::Compressor => Some(Box::new(Compressor::new(device))),
        NativeKind::Gate => Some(Box::new(Gate::new(device))),
        NativeKind::Delay => Some(Box::new(Delay::new(device))),
        NativeKind::Reverb => Some(Box::new(Reverb::new(device))),
        NativeKind::Utility => Some(Box::new(Utility::new(device))),
        NativeKind::Sampler | NativeKind::Synth => None,
    }
}

/// Running a device's node over a signal, for the devices' tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::OnceLock;

    use musicable_model::time::{TempoMap, TimeSignature};

    use crate::event::{NodeEvent, NodeEventKind, ParamId};
    use crate::{
        AudioBuffer, AudioRef, EngineConfig, Inputs, Node, ProcessContext, TransportState, rt,
    };

    /// 120 bpm in 4/4.
    pub(crate) fn tempo() -> &'static TempoMap {
        static TEMPO: OnceLock<TempoMap> = OnceLock::new();
        TEMPO.get_or_init(|| TempoMap::new(120.0, TimeSignature::new(4, 4).unwrap()))
    }

    /// A stopped block of `frames` at 48 kHz, with `events`.
    pub(crate) fn context<'a>(frames: usize, events: &'a [NodeEvent]) -> ProcessContext<'a> {
        ProcessContext {
            frames,
            sample_rate: 48_000,
            time: 0,
            transport: Default::default(),
            tempo: tempo(),
            events,
            input: AudioRef::empty(frames),
            midi: &[],
        }
    }

    /// The gain of `impulse` at `freq` Hz, as a linear magnitude: its
    /// discrete-time Fourier transform there.
    pub(crate) fn gain_at(impulse: &[f32], freq: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / sample_rate;
        let (mut re, mut im) = (0.0, 0.0);
        for (n, &h) in impulse.iter().enumerate() {
            let phase = w * n as f64;
            re += f64::from(h) * phase.cos();
            im -= f64::from(h) * phase.sin();
        }
        re.hypot(im)
    }

    pub(crate) fn db(gain: f64) -> f64 {
        20.0 * gain.log10()
    }

    /// Sets parameter `param` to `value` on frame `at` of a [`Run`].
    pub(crate) fn param(at: usize, param: usize, value: f64) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Param {
                param: ParamId(param as u32),
                value,
            },
        }
    }

    /// Modulates parameter `param` by `amount` from frame `at` of a [`Run`].
    pub(crate) fn modulation(at: usize, param: usize, amount: f64) -> NodeEvent {
        NodeEvent {
            offset: at,
            kind: NodeEventKind::Modulation {
                param: ParamId(param as u32),
                amount,
                note: None,
            },
        }
    }

    /// A node rendering a signal a block at a time, as the engine would,
    /// checking that it doesn't allocate.
    pub(crate) struct Run {
        node: Box<dyn Node>,
        sample_rate: u32,
        block: usize,
        tempo: TempoMap,
        /// Whether the transport plays, from the start of the song.
        playing: bool,
        /// Frames rendered so far.
        rendered: u64,
        /// Whether the node is prepared, which the first render does.
        prepared: bool,
    }

    impl Run {
        /// At 48 kHz, in blocks of 512, at 120 bpm with the transport
        /// stopped at the start of the song.
        pub(crate) fn new(node: Box<dyn Node>) -> Self {
            Self {
                node,
                sample_rate: 48_000,
                block: 512,
                tempo: tempo().clone(),
                playing: false,
                rendered: 0,
                prepared: false,
            }
        }

        pub(crate) fn with_tempo(mut self, tempo: TempoMap) -> Self {
            self.tempo = tempo;
            self
        }

        /// Plays the song from its start as the node renders.
        pub(crate) fn playing(mut self) -> Self {
            self.playing = true;
            self
        }

        pub(crate) fn with_sample_rate(mut self, sample_rate: u32) -> Self {
            self.sample_rate = sample_rate;
            self
        }

        pub(crate) fn with_block(mut self, block: usize) -> Self {
            self.block = block;
            self
        }

        /// Puts `node` in place of the node, taking over its state, as a
        /// graph swap does.
        pub(crate) fn swap(&mut self, mut node: Box<dyn Node>) {
            node.prepare(&self.config());
            node.adopt(&mut *self.node);
            self.node = node;
            self.prepared = true;
        }

        fn config(&self) -> EngineConfig {
            EngineConfig {
                sample_rate: self.sample_rate,
                max_block: self.block,
            }
        }

        /// Renders `input`, one `Vec` for each channel, with `events` on
        /// the frames their offsets give, counted from the start. Returns
        /// the output, a `Vec` for each of the node's channels. Another
        /// call carries on from where this one ends.
        pub(crate) fn render(&mut self, input: &[Vec<f32>], events: &[NodeEvent]) -> Vec<Vec<f32>> {
            let frames = input.first().map_or(0, Vec::len);
            let channels = self.node.channels();
            if !self.prepared {
                self.node.prepare(&self.config());
                self.prepared = true;
            }
            let mut output = vec![Vec::with_capacity(frames); channels];
            let mut planar = vec![0.0; input.len() * self.block];
            let mut buffer = AudioBuffer::new(channels, self.block);
            let mut start = 0;
            while start < frames {
                let len = self.block.min(frames - start);
                for (c, channel) in input.iter().enumerate() {
                    planar[c * len..][..len].copy_from_slice(&channel[start..start + len]);
                }
                let block_events: Vec<NodeEvent> = events
                    .iter()
                    .filter(|e| (start..start + len).contains(&e.offset))
                    .map(|e| NodeEvent {
                        offset: e.offset - start,
                        ..*e
                    })
                    .collect();
                let ctx = ProcessContext {
                    frames: len,
                    sample_rate: self.sample_rate,
                    time: start as u64,
                    transport: TransportState {
                        playing: self.playing,
                        position: if self.playing { self.rendered } else { 0 },
                        ..TransportState::default()
                    },
                    tempo: &self.tempo,
                    events: &block_events,
                    ..context(len, &[])
                };
                let views = [AudioRef::from_planar(
                    &planar[..input.len() * len],
                    input.len(),
                )];
                let mut out = buffer.view_mut(len);
                let node = &mut self.node;
                rt::scope(|| {
                    rt::flush_denormals(|| node.process(&ctx, Inputs::from_views(&views), &mut out))
                });
                for (c, channel) in output.iter_mut().enumerate() {
                    channel.extend_from_slice(out.channel(c));
                }
                start += len;
                self.rendered += len as u64;
            }
            output
        }
    }
}
