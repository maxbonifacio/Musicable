//! Planar audio buffers and the views nodes work with.
//!
//! Audio is `f32`, one contiguous run of samples per channel. An
//! [`AudioBuffer`] owns room for up to its capacity in frames. Nodes see
//! [`AudioRef`] and [`AudioMut`] views that are exactly one block long, so
//! block sizes can change from one block to the next.
//!
//! Where channel counts differ, a mono source feeds every channel. Otherwise
//! channels map one to one, extra source channels are dropped and destination
//! channels with no source get silence.

use std::ops::Range;

use crate::graph::NodeId;

/// Owned planar audio: `channels` channels with room for `capacity` frames
/// each. Allocate these off the audio thread.
#[derive(Debug, Clone, Default)]
pub struct AudioBuffer {
    data: Vec<f32>,
    channels: usize,
    capacity: usize,
}

impl AudioBuffer {
    pub fn new(channels: usize, capacity: usize) -> Self {
        Self {
            data: vec![0.0; channels * capacity],
            channels,
            capacity,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames each channel has room for.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The first `frames` frames of every channel.
    ///
    /// # Panics
    ///
    /// If `frames` is more than the capacity.
    pub fn view(&self, frames: usize) -> AudioRef<'_> {
        assert!(
            frames <= self.capacity,
            "{frames} frames exceed the capacity of {}",
            self.capacity
        );
        AudioRef {
            data: &self.data,
            stride: self.capacity,
            channels: self.channels,
            frames,
        }
    }

    /// The first `frames` frames of every channel, writable.
    ///
    /// # Panics
    ///
    /// If `frames` is more than the capacity.
    pub fn view_mut(&mut self, frames: usize) -> AudioMut<'_> {
        assert!(
            frames <= self.capacity,
            "{frames} frames exceed the capacity of {}",
            self.capacity
        );
        AudioMut {
            data: &mut self.data,
            stride: self.capacity,
            channels: self.channels,
            frames,
        }
    }

    /// Where the samples are, for a renderer that lets several threads view
    /// buffers at once. A buffer's samples never move, so this stays valid as
    /// long as the buffer lives.
    pub(crate) fn raw(&mut self) -> RawBuffer {
        RawBuffer {
            data: self.data.as_mut_ptr(),
            channels: self.channels,
            capacity: self.capacity,
        }
    }
}

/// An [`AudioBuffer`]'s samples, by address. The parallel renderer gives each
/// thread views of the buffers it may use: one node's output to write, and its
/// inputs to read. Who may use which buffer when is up to the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RawBuffer {
    data: *mut f32,
    channels: usize,
    capacity: usize,
}

// SAFETY: a `RawBuffer` is an address. Creating views from it is unsafe, and
// the renderer that does so ensures no buffer is written while it is read.
unsafe impl Send for RawBuffer {}
// SAFETY: as above.
unsafe impl Sync for RawBuffer {}

impl RawBuffer {
    /// Points nowhere. Not to be viewed.
    pub(crate) const NULL: RawBuffer = RawBuffer {
        data: std::ptr::null_mut(),
        channels: 0,
        capacity: 0,
    };

    /// The first `frames` frames of every channel.
    ///
    /// # Safety
    ///
    /// The buffer is alive and nothing writes to it while the view lives.
    /// `frames` is at most its capacity.
    pub(crate) unsafe fn view<'a>(self, frames: usize) -> AudioRef<'a> {
        debug_assert!(frames <= self.capacity);
        AudioRef {
            // SAFETY: `data` points to `channels * capacity` samples, which
            // the caller ensures are alive and not being written.
            data: unsafe { std::slice::from_raw_parts(self.data, self.channels * self.capacity) },
            stride: self.capacity,
            channels: self.channels,
            frames,
        }
    }

    /// The first `frames` frames of every channel, writable.
    ///
    /// # Safety
    ///
    /// The buffer is alive and nothing else reads or writes it while the view
    /// lives. `frames` is at most its capacity.
    pub(crate) unsafe fn view_mut<'a>(self, frames: usize) -> AudioMut<'a> {
        debug_assert!(frames <= self.capacity);
        AudioMut {
            // SAFETY: as in `view`, and the caller ensures this is the only
            // access.
            data: unsafe {
                std::slice::from_raw_parts_mut(self.data, self.channels * self.capacity)
            },
            stride: self.capacity,
            channels: self.channels,
            frames,
        }
    }
}

/// A read-only view of planar audio: `channels` channels of `frames` frames.
#[derive(Debug, Clone, Copy)]
pub struct AudioRef<'a> {
    data: &'a [f32],
    stride: usize,
    channels: usize,
    frames: usize,
}

impl<'a> AudioRef<'a> {
    /// No channels, `frames` long: the device inputs of a stream that has
    /// none.
    pub const fn empty(frames: usize) -> AudioRef<'static> {
        AudioRef {
            data: &[],
            stride: 0,
            channels: 0,
            frames,
        }
    }

    /// Views `data` as `channels` channels stored one after another.
    ///
    /// # Panics
    ///
    /// If `channels` is zero or doesn't divide `data.len()`.
    pub fn from_planar(data: &'a [f32], channels: usize) -> Self {
        let frames = planar_frames(data.len(), channels);
        Self {
            data,
            stride: frames,
            channels,
            frames,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// # Panics
    ///
    /// If there is no channel `index`.
    pub fn channel(&self, index: usize) -> &'a [f32] {
        assert!(
            index < self.channels,
            "no channel {index} in {}",
            self.channels
        );
        &self.data[index * self.stride..][..self.frames]
    }

    /// The channels in order.
    pub fn iter(&self) -> impl Iterator<Item = &'a [f32]> + 'a {
        let this = *self;
        (0..this.channels).map(move |c| this.channel(c))
    }

    /// The frames in `range`, as a shorter view.
    pub fn range(&self, range: Range<usize>) -> AudioRef<'a> {
        assert!(
            range.start <= range.end && range.end <= self.frames,
            "{range:?} is outside {} frames",
            self.frames
        );
        AudioRef {
            data: self.data.get(range.start..).unwrap_or_default(),
            stride: self.stride,
            channels: self.channels,
            frames: range.len(),
        }
    }

    /// Writes the frames to `out` interleaved for a device with `channels`
    /// channels.
    ///
    /// # Panics
    ///
    /// If `out` isn't exactly `frames * channels` samples long.
    pub fn write_interleaved(&self, out: &mut [f32], channels: usize) {
        assert_eq!(
            out.len(),
            self.frames * channels,
            "interleaved buffer length"
        );
        for c in 0..channels {
            let samples = out[c..].iter_mut().step_by(channels);
            match source_channel(self.channels, c) {
                Some(source) => {
                    for (o, s) in samples.zip(self.channel(source)) {
                        *o = *s;
                    }
                }
                None => samples.for_each(|o| *o = 0.0),
            }
        }
    }

    /// Adds the frames to `out`, interleaved for a device with `channels`
    /// channels, the way [`write_interleaved`](Self::write_interleaved)
    /// writes them.
    ///
    /// # Panics
    ///
    /// If `out` isn't exactly `frames * channels` samples long.
    pub fn add_interleaved(&self, out: &mut [f32], channels: usize) {
        assert_eq!(
            out.len(),
            self.frames * channels,
            "interleaved buffer length"
        );
        for c in 0..channels {
            if let Some(source) = source_channel(self.channels, c) {
                let samples = out[c..].iter_mut().step_by(channels);
                for (o, s) in samples.zip(self.channel(source)) {
                    *o += *s;
                }
            }
        }
    }
}

/// A writable view of planar audio: `channels` channels of `frames` frames.
#[derive(Debug)]
pub struct AudioMut<'a> {
    data: &'a mut [f32],
    stride: usize,
    channels: usize,
    frames: usize,
}

impl<'a> AudioMut<'a> {
    /// Views `data` as `channels` channels stored one after another.
    ///
    /// # Panics
    ///
    /// If `channels` is zero or doesn't divide `data.len()`.
    pub fn from_planar(data: &'a mut [f32], channels: usize) -> Self {
        let frames = planar_frames(data.len(), channels);
        Self {
            data,
            stride: frames,
            channels,
            frames,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// # Panics
    ///
    /// If there is no channel `index`.
    pub fn channel(&self, index: usize) -> &[f32] {
        assert!(
            index < self.channels,
            "no channel {index} in {}",
            self.channels
        );
        &self.data[index * self.stride..][..self.frames]
    }

    /// # Panics
    ///
    /// If there is no channel `index`.
    pub fn channel_mut(&mut self, index: usize) -> &mut [f32] {
        assert!(
            index < self.channels,
            "no channel {index} in {}",
            self.channels
        );
        &mut self.data[index * self.stride..][..self.frames]
    }

    /// The channels in order, writable.
    pub fn channels_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        let frames = self.frames;
        self.data
            .chunks_mut(self.stride.max(1))
            .take(self.channels)
            .map(move |channel| &mut channel[..frames])
    }

    /// A read-only view of the same frames.
    pub fn view(&self) -> AudioRef<'_> {
        AudioRef {
            data: self.data,
            stride: self.stride,
            channels: self.channels,
            frames: self.frames,
        }
    }

    /// The frames in `range`, as a shorter view.
    pub fn range_mut(&mut self, range: Range<usize>) -> AudioMut<'_> {
        assert!(
            range.start <= range.end && range.end <= self.frames,
            "{range:?} is outside {} frames",
            self.frames
        );
        AudioMut {
            data: self.data.get_mut(range.start..).unwrap_or_default(),
            stride: self.stride,
            channels: self.channels,
            frames: range.len(),
        }
    }

    pub fn fill(&mut self, value: f32) {
        for channel in self.channels_mut() {
            channel.fill(value);
        }
    }

    pub fn clear(&mut self) {
        self.fill(0.0);
    }

    /// Adds `source` to these frames.
    ///
    /// # Panics
    ///
    /// If the lengths differ.
    pub fn add_from(&mut self, source: AudioRef<'_>) {
        assert_eq!(
            source.frames, self.frames,
            "mixing blocks of different lengths"
        );
        for c in 0..self.channels {
            if let Some(s) = source_channel(source.channels, c) {
                for (o, i) in self.channel_mut(c).iter_mut().zip(source.channel(s)) {
                    *o += *i;
                }
            }
        }
    }

    /// Overwrites these frames with `source`.
    ///
    /// # Panics
    ///
    /// If the lengths differ.
    pub fn copy_from(&mut self, source: AudioRef<'_>) {
        assert_eq!(
            source.frames, self.frames,
            "copying blocks of different lengths"
        );
        for c in 0..self.channels {
            match source_channel(source.channels, c) {
                Some(s) => self.channel_mut(c).copy_from_slice(source.channel(s)),
                None => self.channel_mut(c).fill(0.0),
            }
        }
    }
}

/// The source channel that feeds destination channel `channel`.
fn source_channel(source_channels: usize, channel: usize) -> Option<usize> {
    if source_channels == 1 {
        Some(0)
    } else {
        (channel < source_channels).then_some(channel)
    }
}

fn planar_frames(len: usize, channels: usize) -> usize {
    assert!(
        channels > 0 && len.is_multiple_of(channels),
        "{len} samples don't split into {channels} channels"
    );
    len / channels
}

/// The blocks a node's inputs rendered, in the order they were connected.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a>(InputsRepr<'a>);

#[derive(Debug, Clone, Copy)]
enum InputsRepr<'a> {
    Graph {
        buffers: &'a [AudioBuffer],
        ids: &'a [NodeId],
        frames: usize,
    },
    Shared {
        buffers: &'a [RawBuffer],
        ids: &'a [NodeId],
        frames: usize,
    },
    Views(&'a [AudioRef<'a>]),
}

impl<'a> Inputs<'a> {
    /// No inputs, as a source node gets.
    pub const NONE: Inputs<'static> = Inputs(InputsRepr::Views(&[]));

    /// Inputs from views the caller owns, for running a node outside a graph.
    pub fn from_views(views: &'a [AudioRef<'a>]) -> Self {
        Self(InputsRepr::Views(views))
    }

    /// The outputs of nodes `ids` in a renderer's buffers.
    pub(crate) fn graph(buffers: &'a [AudioBuffer], ids: &'a [NodeId], frames: usize) -> Self {
        Self(InputsRepr::Graph {
            buffers,
            ids,
            frames,
        })
    }

    /// The outputs of nodes `ids`, in buffers other threads are using too.
    ///
    /// # Safety
    ///
    /// While the inputs live, the buffers of `ids` stay alive and nothing
    /// writes to them. `frames` is at most their capacity.
    pub(crate) unsafe fn shared(
        buffers: &'a [RawBuffer],
        ids: &'a [NodeId],
        frames: usize,
    ) -> Self {
        Self(InputsRepr::Shared {
            buffers,
            ids,
            frames,
        })
    }

    pub fn len(&self) -> usize {
        match self.0 {
            InputsRepr::Graph { ids, .. } | InputsRepr::Shared { ids, .. } => ids.len(),
            InputsRepr::Views(views) => views.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, index: usize) -> Option<AudioRef<'a>> {
        match self.0 {
            InputsRepr::Graph {
                buffers,
                ids,
                frames,
            } => ids.get(index).map(|id| buffers[id.0].view(frames)),
            InputsRepr::Shared {
                buffers,
                ids,
                frames,
            } => ids.get(index).map(|id| {
                // SAFETY: `Inputs::shared` requires the buffers to stay
                // alive and unwritten while these inputs live.
                unsafe { buffers[id.0].view(frames) }
            }),
            InputsRepr::Views(views) => views.get(index).copied(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = AudioRef<'a>> + 'a {
        let this = *self;
        (0..this.len()).filter_map(move |i| this.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_cover_only_the_block() {
        let mut buffer = AudioBuffer::new(2, 8);
        let mut block = buffer.view_mut(3);
        block.channel_mut(0).fill(1.0);
        block.channel_mut(1).fill(2.0);
        assert_eq!(buffer.view(4).channel(0), [1.0, 1.0, 1.0, 0.0]);
        assert_eq!(buffer.view(4).channel(1), [2.0, 2.0, 2.0, 0.0]);
    }

    #[test]
    fn mono_feeds_every_channel() {
        let mono = [1.0, 2.0];
        let mut data = [10.0; 4];
        let mut stereo = AudioMut::from_planar(&mut data, 2);
        stereo.add_from(AudioRef::from_planar(&mono, 1));
        assert_eq!(data, [11.0, 12.0, 11.0, 12.0]);
    }

    #[test]
    fn extra_channels_are_dropped_or_silent() {
        let stereo = [1.0, 2.0, 3.0, 4.0];
        let mut quad = [9.0; 8];
        AudioMut::from_planar(&mut quad, 4).copy_from(AudioRef::from_planar(&stereo, 2));
        assert_eq!(quad, [1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0]);

        let mut mono = [0.0; 2];
        AudioMut::from_planar(&mut mono, 1).copy_from(AudioRef::from_planar(&stereo, 2));
        assert_eq!(mono, [1.0, 2.0]);
    }

    #[test]
    fn ranges_keep_channels_apart() {
        let mut buffer = AudioBuffer::new(2, 4);
        buffer.view_mut(4).range_mut(1..3).fill(1.0);
        let block = buffer.view(4);
        assert_eq!(block.channel(0), [0.0, 1.0, 1.0, 0.0]);
        assert_eq!(block.channel(1), [0.0, 1.0, 1.0, 0.0]);
        assert_eq!(block.range(2..4).channel(1), [1.0, 0.0]);
    }

    #[test]
    fn interleaving_maps_channels() {
        let stereo = [1.0, 2.0, 3.0, 4.0];
        let mut out = [9.0; 6];
        AudioRef::from_planar(&stereo, 2).write_interleaved(&mut out, 3);
        assert_eq!(out, [1.0, 3.0, 0.0, 2.0, 4.0, 0.0]);
    }

    #[test]
    fn inputs_read_graph_buffers() {
        let mut buffers = vec![AudioBuffer::new(1, 4), AudioBuffer::new(2, 4)];
        buffers[0].view_mut(4).fill(1.0);
        buffers[1].view_mut(4).fill(2.0);
        let ids = [NodeId(1), NodeId(0)];
        let inputs = Inputs::graph(&buffers, &ids, 2);
        let channels: Vec<usize> = inputs.iter().map(|b| b.channels()).collect();
        assert_eq!(channels, [2, 1]);
        assert_eq!(inputs.get(1).unwrap().channel(0), [1.0, 1.0]);
        assert!(inputs.get(2).is_none());
        assert!(Inputs::NONE.is_empty());
    }
}
