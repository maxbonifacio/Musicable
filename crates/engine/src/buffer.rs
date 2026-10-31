//! Planar audio buffers and the views nodes work with.
//!
//! Audio is `f32`, one contiguous run of samples per channel. An
//! [`AudioBuffer`] owns room for up to its capacity in frames. Nodes see
//! [`AudioRef`] and [`AudioMut`] views that are exactly one block long, so
//! block sizes can change from one block to the next. A view reaches only
//! its own frames of each channel, so threads can work on different frames
//! of one buffer at once: the parallel renderer's parts of a block.
//!
//! Where channel counts differ, a mono source feeds every channel. Otherwise
//! channels map one to one, extra source channels are dropped and destination
//! channels with no source get silence.

use std::marker::PhantomData;
use std::ops::Range;
use std::ptr::NonNull;

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
            data: NonNull::from(self.data.as_slice()).cast(),
            stride: self.capacity,
            channels: self.channels,
            frames,
            samples: PhantomData,
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
            data: NonNull::from(self.data.as_mut_slice()).cast(),
            stride: self.capacity,
            channels: self.channels,
            frames,
            samples: PhantomData,
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

    /// The first `frames` frames of every channel, writable.
    ///
    /// # Safety
    ///
    /// As for [`part_mut`](Self::part_mut), of `0..frames`.
    pub(crate) unsafe fn view_mut<'a>(self, frames: usize) -> AudioMut<'a> {
        // SAFETY: the caller's promise.
        unsafe { self.part_mut(0..frames) }
    }

    /// Frames `frames` of every channel.
    ///
    /// # Safety
    ///
    /// The buffer is alive and nothing writes to those frames while the
    /// view lives. They end at most at its capacity.
    pub(crate) unsafe fn part<'a>(self, frames: Range<usize>) -> AudioRef<'a> {
        AudioRef {
            data: self.at(&frames),
            stride: self.capacity,
            channels: self.channels,
            frames: frames.len(),
            samples: PhantomData,
        }
    }

    /// Frames `frames` of every channel, writable.
    ///
    /// # Safety
    ///
    /// The buffer is alive and nothing else reads or writes those frames
    /// while the view lives. They end at most at its capacity.
    pub(crate) unsafe fn part_mut<'a>(self, frames: Range<usize>) -> AudioMut<'a> {
        AudioMut {
            data: self.at(&frames),
            stride: self.capacity,
            channels: self.channels,
            frames: frames.len(),
            samples: PhantomData,
        }
    }

    /// Where `frames` start in the first channel.
    fn at(self, frames: &Range<usize>) -> NonNull<f32> {
        debug_assert!(!self.data.is_null(), "only a buffer is viewed");
        debug_assert!(frames.start <= frames.end && frames.end <= self.capacity);
        let data = match self.channels {
            // An empty buffer's address is only aligned, with nothing past it.
            0 => self.data,
            // SAFETY: the frames are within the capacity, so this is within
            // the first channel, or just past its end.
            _ => unsafe { self.data.add(frames.start) },
        };
        // SAFETY: a buffer's address isn't null, and neither is one in it.
        unsafe { NonNull::new_unchecked(data) }
    }
}

/// A read-only view of planar audio: `channels` channels of `frames` frames.
#[derive(Debug, Clone, Copy)]
pub struct AudioRef<'a> {
    /// The first channel's first frame. Each channel starts `stride`
    /// samples after the one before, and the view reads `frames` samples
    /// from there, and nothing between them.
    data: NonNull<f32>,
    stride: usize,
    channels: usize,
    frames: usize,
    samples: PhantomData<&'a [f32]>,
}

// SAFETY: it reads its samples as the `&[f32]` of each channel it gives out
// would, and those can be sent and shared.
unsafe impl Send for AudioRef<'_> {}
// SAFETY: as above.
unsafe impl Sync for AudioRef<'_> {}

impl<'a> AudioRef<'a> {
    /// No channels, `frames` long: the device inputs of a stream that has
    /// none.
    pub const fn empty(frames: usize) -> AudioRef<'static> {
        AudioRef {
            data: NonNull::dangling(),
            stride: 0,
            channels: 0,
            frames,
            samples: PhantomData,
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
            data: NonNull::from(data).cast(),
            stride: frames,
            channels,
            frames,
            samples: PhantomData,
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
        // SAFETY: whoever made the view lent it `frames` samples from the
        // start of each channel for `'a`.
        unsafe { std::slice::from_raw_parts(channel(self.data, index, self.stride), self.frames) }
    }

    /// The channels in order.
    pub fn iter(&self) -> impl Iterator<Item = &'a [f32]> + 'a {
        let this = *self;
        (0..this.channels).map(move |c| this.channel(c))
    }

    /// The first `channels` channels, or all of them if there are fewer.
    pub(crate) fn first(&self, channels: usize) -> AudioRef<'a> {
        AudioRef {
            channels: channels.min(self.channels),
            ..*self
        }
    }

    /// The frames in `range`, as a shorter view.
    pub fn range(&self, range: Range<usize>) -> AudioRef<'a> {
        assert!(
            range.start <= range.end && range.end <= self.frames,
            "{range:?} is outside {} frames",
            self.frames
        );
        AudioRef {
            data: start(self.data, self.channels, range.start),
            stride: self.stride,
            channels: self.channels,
            frames: range.len(),
            samples: PhantomData,
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
    /// As in [`AudioRef`]: it writes `frames` samples from the start of
    /// each channel, and nothing between them.
    data: NonNull<f32>,
    stride: usize,
    channels: usize,
    frames: usize,
    samples: PhantomData<&'a mut [f32]>,
}

// SAFETY: it reads and writes its samples as the `&mut [f32]` of each
// channel it gives out would, and those can be sent and shared.
unsafe impl Send for AudioMut<'_> {}
// SAFETY: as above.
unsafe impl Sync for AudioMut<'_> {}

impl<'a> AudioMut<'a> {
    /// Views `data` as `channels` channels stored one after another.
    ///
    /// # Panics
    ///
    /// If `channels` is zero or doesn't divide `data.len()`.
    pub fn from_planar(data: &'a mut [f32], channels: usize) -> Self {
        let frames = planar_frames(data.len(), channels);
        Self {
            data: NonNull::from(data).cast(),
            stride: frames,
            channels,
            frames,
            samples: PhantomData,
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
        self.view().channel(index)
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
        let data = channel(self.data, index, self.stride);
        // SAFETY: whoever made the view lent it `frames` samples from the
        // start of each channel, and borrowing the view lends them on.
        unsafe { std::slice::from_raw_parts_mut(data, self.frames) }
    }

    /// The channels in order, writable.
    pub fn channels_mut(&mut self) -> impl Iterator<Item = &mut [f32]> {
        let (data, stride, frames) = (self.data, self.stride, self.frames);
        // SAFETY: as in `channel_mut`, once for each channel, and the
        // channels don't overlap.
        (0..self.channels).map(move |index| unsafe {
            std::slice::from_raw_parts_mut(channel(data, index, stride), frames)
        })
    }

    /// A read-only view of the same frames.
    pub fn view(&self) -> AudioRef<'_> {
        AudioRef {
            data: self.data,
            stride: self.stride,
            channels: self.channels,
            frames: self.frames,
            samples: PhantomData,
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
            data: start(self.data, self.channels, range.start),
            stride: self.stride,
            channels: self.channels,
            frames: range.len(),
            samples: PhantomData,
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

/// Where channel `index` of a view starts.
fn channel(data: NonNull<f32>, index: usize, stride: usize) -> *mut f32 {
    // SAFETY: a view's channels are all in the samples it was made from.
    unsafe { data.as_ptr().add(index * stride) }
}

/// Where a view's frames from `frame` on start, for a shorter view.
fn start(data: NonNull<f32>, channels: usize, frame: usize) -> NonNull<f32> {
    match channels {
        // An empty view's address is only aligned, with nothing past it.
        0 => data,
        // SAFETY: the shorter view's frames are the view's, so this is
        // within its first channel, or just past its end.
        _ => unsafe { data.add(frame) },
    }
}

/// The source channel that feeds destination channel `channel`.
pub(crate) fn source_channel(source_channels: usize, channel: usize) -> Option<usize> {
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

/// Which of a node's inputs a connection feeds. Every node hears its main
/// input, the sum of what's connected to it. A side input is for a node
/// that hears something else too, such as a compressor's sidechain, and
/// nodes without one leave it alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum InputPort {
    #[default]
    Main,
    /// Side input `n`, from 0.
    Side(u32),
}

/// The blocks a node's inputs rendered: those into its main input, in the
/// order they were connected, which [`iter`](Self::iter) and
/// [`get`](Self::get) give, and those into each side input, which
/// [`side`](Self::side) gives.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    repr: InputsRepr<'a>,
    /// Each input's port, in order: the main input's first, then each side
    /// input's in turn. Empty where they're all the main input's.
    ports: &'a [InputPort],
    /// How many go into the main input.
    mains: usize,
    /// The frames of the inputs' blocks the node renders, where it renders
    /// part of a block.
    part: Option<(usize, usize)>,
}

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
    pub const NONE: Inputs<'static> = Inputs {
        repr: InputsRepr::Views(&[]),
        ports: &[],
        mains: 0,
        part: None,
    };

    /// Inputs from views the caller owns, all into the main input, for
    /// running a node outside a graph.
    pub fn from_views(views: &'a [AudioRef<'a>]) -> Self {
        Self::new(InputsRepr::Views(views), &[])
    }

    /// Inputs from views the caller owns, each into the port at its place in
    /// `ports`: the main input's first, then each side input's in turn.
    ///
    /// # Panics
    ///
    /// If `ports` isn't as long as `views`, or isn't in that order.
    pub fn from_views_on(views: &'a [AudioRef<'a>], ports: &'a [InputPort]) -> Self {
        assert_eq!(views.len(), ports.len(), "a port for each input");
        assert!(ports.is_sorted(), "{ports:?} are out of order");
        Self::new(InputsRepr::Views(views), ports)
    }

    /// The outputs of nodes `ids` in a renderer's buffers, into `ports`.
    pub(crate) fn graph(
        buffers: &'a [AudioBuffer],
        ids: &'a [NodeId],
        ports: &'a [InputPort],
        frames: usize,
    ) -> Self {
        let repr = InputsRepr::Graph {
            buffers,
            ids,
            frames,
        };
        Self::new(repr, ports)
    }

    /// The outputs of nodes `ids`, into `ports`, in buffers other threads
    /// are using too.
    ///
    /// # Safety
    ///
    /// While the inputs live, the buffers of `ids` stay alive and nothing
    /// writes to the frames they're read over: the first `frames`, or those
    /// of the [`range`](Self::range) they're narrowed to before they're
    /// read. `frames` is at most their capacity.
    pub(crate) unsafe fn shared(
        buffers: &'a [RawBuffer],
        ids: &'a [NodeId],
        ports: &'a [InputPort],
        frames: usize,
    ) -> Self {
        let repr = InputsRepr::Shared {
            buffers,
            ids,
            frames,
        };
        Self::new(repr, ports)
    }

    /// `ports` has a port for each input, or none where they're all the
    /// main input's.
    fn new(repr: InputsRepr<'a>, ports: &'a [InputPort]) -> Self {
        debug_assert!(ports.is_sorted(), "{ports:?} are out of order");
        let mains = match repr {
            _ if !ports.is_empty() => ports.partition_point(|&p| p == InputPort::Main),
            InputsRepr::Graph { ids, .. } | InputsRepr::Shared { ids, .. } => ids.len(),
            InputsRepr::Views(views) => views.len(),
        };
        Self {
            repr,
            ports,
            mains,
            part: None,
        }
    }

    /// How many go into the main input.
    pub fn len(&self) -> usize {
        self.mains
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The main input's input `index`, in the order they were connected.
    pub fn get(&self, index: usize) -> Option<AudioRef<'a>> {
        (index < self.mains).then(|| self.view(index))
    }

    /// The main input's inputs, in the order they were connected.
    pub fn iter(&self) -> impl Iterator<Item = AudioRef<'a>> + 'a {
        let this = *self;
        (0..this.mains).map(move |i| this.view(i))
    }

    /// Side input `port`'s inputs, in the order they were connected: none
    /// if nothing's connected to it.
    pub fn side(&self, port: u32) -> impl Iterator<Item = AudioRef<'a>> + 'a {
        let this = *self;
        let port = InputPort::Side(port);
        let first = self.ports.partition_point(|&p| p < port);
        let end = self.ports.partition_point(|&p| p <= port);
        (first..end).map(move |i| this.view(i))
    }

    /// The same inputs, over `frames` of the block, for a node rendering a
    /// part of it as a block of its own.
    pub fn range(&self, frames: Range<usize>) -> Self {
        let start = self.part.map_or(0, |(start, _)| start);
        Self {
            part: Some((start + frames.start, start + frames.end)),
            ..*self
        }
    }

    /// Input `index`, into any port.
    fn view(&self, index: usize) -> AudioRef<'a> {
        let whole = match self.repr {
            InputsRepr::Graph {
                buffers,
                ids,
                frames,
            } => buffers[ids[index].0].view(frames),
            InputsRepr::Shared {
                buffers,
                ids,
                frames,
            } => {
                // Only the part's frames, as other lanes may be writing the
                // rest of a pipelined block's buffers.
                let part = self.part.map_or(0..frames, |(start, end)| start..end);
                // SAFETY: `Inputs::shared` requires the buffers to stay
                // alive and those frames unwritten while these inputs live.
                return unsafe { buffers[ids[index].0].part(part) };
            }
            InputsRepr::Views(views) => views[index],
        };
        match self.part {
            Some((start, end)) => whole.range(start..end),
            None => whole,
        }
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
        let inputs = Inputs::graph(&buffers, &ids, &[InputPort::Main; 2], 2);
        let channels: Vec<usize> = inputs.iter().map(|b| b.channels()).collect();
        assert_eq!(channels, [2, 1]);
        assert_eq!(inputs.get(1).unwrap().channel(0), [1.0, 1.0]);
        assert!(inputs.get(2).is_none());
        assert_eq!(inputs.side(0).count(), 0);
        assert!(Inputs::NONE.is_empty());
    }

    #[test]
    fn side_inputs_stay_out_of_the_main_input() {
        let blocks: Vec<Vec<f32>> = (0..4).map(|i| vec![i as f32; 4]).collect();
        let views: Vec<AudioRef<'_>> = (blocks.iter())
            .map(|block| AudioRef::from_planar(block, 1))
            .collect();
        let ports = [
            InputPort::Main,
            InputPort::Side(0),
            InputPort::Side(0),
            InputPort::Side(2),
        ];
        let inputs = Inputs::from_views_on(&views, &ports);
        let first = |input: AudioRef<'_>| input.channel(0)[0];
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs.iter().map(first).collect::<Vec<_>>(), [0.0]);
        assert_eq!(inputs.side(0).map(first).collect::<Vec<_>>(), [1.0, 2.0]);
        assert_eq!(inputs.side(1).count(), 0);
        assert_eq!(inputs.side(2).map(first).collect::<Vec<_>>(), [3.0]);
        // A part of the block, and a part of that, for every port.
        let part = inputs.range(1..4).range(1..3);
        assert_eq!(part.get(0).unwrap().frames(), 2);
        assert!(part.side(0).all(|input| input.frames() == 2));
        assert_eq!(Inputs::from_views(&views).len(), 4);
    }
}
