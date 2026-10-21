//! Clip players: the nodes at the head of audio and MIDI tracks' channel
//! strips, playing the tracks' clips from the song position like everything
//! else, so a position always renders the same, however the blocks fall and
//! however often the graph is rebuilt.

use std::f64::consts::FRAC_PI_2;

use musicable_model::clip::{self, Clip, ClipContent, ClipId, Dimension};
use musicable_model::time::Beats;

use crate::media::Source;
use crate::resample::Resampler;
use crate::{AudioMut, EngineConfig, Inputs, Node, NoteEvent, NoteId, Notes, ProcessContext};

/// Plays a track's audio clips, added to what comes into it, such as the
/// track's test tone.
///
/// A clip plays its file at the file's own speed, from the frame it starts
/// at in the file, for as long as the clip lasts. A file at the device's
/// sample rate plays sample for sample; one at another rate is resampled
/// (see `resample.rs`). Mono files play in both channels, and files with
/// more than two play their first two. Fades are equal-power, so where one
/// clip fades out over the same frames another fades in, they crossfade at
/// constant power. A clip without a source, such as one whose file is
/// missing, is silent.
///
/// Clips that stream tell their [`Stream`](crate::media::Stream) where
/// they'll read next every block, playing or not, and where they'll read
/// after the loop jumps back.
#[derive(Debug, Default)]
pub struct AudioClips {
    /// In order of start, then id.
    clips: Vec<Placed>,
    /// The latest end of each clip and the clips before it, for finding the
    /// clips a block overlaps.
    reach: Vec<Beats>,
    /// The clips that stream.
    streams: Vec<usize>,
    /// One clip's input for one block, a channel after another.
    input: Vec<f32>,
    /// The resampler's weights for one frame.
    weights: Vec<f32>,
}

#[derive(Debug)]
struct Placed {
    id: ClipId,
    start: Beats,
    end: Beats,
    /// Where the fade in ends and where the fade out starts.
    fade_in: Beats,
    fade_out: Beats,
    /// The file frame the clip starts at.
    offset: u64,
    gain: f32,
    source: Option<Source>,
    /// For a file that isn't at the device's rate, once prepared.
    resampler: Option<Resampler>,
}

impl AudioClips {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `clip`, playing `source`. MIDI clips are ignored.
    pub fn add(&mut self, clip: &Clip, source: Option<Source>) {
        let ClipContent::Audio(audio) = &clip.content else {
            return;
        };
        self.clips.push(Placed {
            id: clip.id,
            start: clip.start,
            end: clip.end(),
            fade_in: clip.start + audio.fade_in,
            fade_out: clip.end() - audio.fade_out,
            offset: audio.offset,
            gain: audio.gain,
            source,
            resampler: None,
        });
    }

    pub fn with_clip(mut self, clip: &Clip, source: Option<Source>) -> Self {
        self.add(clip, source);
        self
    }

    /// Tells each stream where its clip reads next, from where the block
    /// starts.
    fn hint(&self, ctx: &ProcessContext<'_>) {
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let first = ctx.transport.position as i64;
        let looping = ctx.transport.looping.map(|region| {
            let start = tempo.frame_at(region.start, rate);
            (start, tempo.frame_at(region.end, rate))
        });
        for &i in &self.streams {
            let clip = &self.clips[i];
            let Some(Source::Stream(stream)) = &clip.source else {
                continue;
            };
            let (start, end) = (
                tempo.frame_at(clip.start, rate),
                tempo.frame_at(clip.end, rate),
            );
            // Where the clip's first read for a frame starts.
            let read_at = |frame: i64| {
                let (position, reach) = match &clip.resampler {
                    None => (clip.offset as i64 + (frame - start), 1),
                    Some(r) => {
                        let x = clip.offset as f64 + (frame - start) as f64 * r.step();
                        (x.floor() as i64, r.reach() as i64)
                    }
                };
                (position - reach + 1).max(0) as u64
            };
            let next = match first {
                f if start <= f && f < end => read_at(f),
                _ => read_at(start),
            };
            let after_loop = looping
                .filter(|&(from, to)| from < to && from < end && start < to)
                .map(|(from, _)| read_at(from.max(start)));
            stream.hint(next, after_loop);
        }
    }

    /// Adds clip `i`'s frames in this block to `output`.
    fn play(&mut self, i: usize, ctx: &ProcessContext<'_>, output: &mut AudioMut<'_>) {
        let clip = &self.clips[i];
        let Some(source) = &clip.source else {
            return;
        };
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let first = ctx.transport.position as i64;
        let (start, end) = (
            tempo.frame_at(clip.start, rate),
            tempo.frame_at(clip.end, rate),
        );
        let from = first.max(start);
        let to = (first + ctx.frames as i64).min(end);
        if from >= to {
            return;
        }
        let fades = Fades {
            gain: clip.gain,
            fade_in: (start, tempo.frame_at(clip.fade_in, rate)),
            fade_out: (tempo.frame_at(clip.fade_out, rate), end),
        };
        let frames = (to - from) as usize;
        let at = (from - first) as usize;
        let channels = source.channels().min(2);
        match &clip.resampler {
            None => {
                let input = &mut self.input[..channels * frames];
                read(source, clip.offset as i64 + (from - start), input, channels);
                let flat = fades.flat(from, to);
                for c in 0..output.channels() {
                    let input = &input[c.min(channels - 1) * frames..][..frames];
                    let out = &mut output.channel_mut(c)[at..at + frames];
                    match flat {
                        Some(gain) => out.iter_mut().zip(input).for_each(|(o, &x)| *o += x * gain),
                        None => {
                            for (k, (o, &x)) in out.iter_mut().zip(input).enumerate() {
                                *o += x * fades.at(from + k as i64);
                            }
                        }
                    }
                }
            }
            Some(resampler) => {
                let position =
                    |frame: i64| clip.offset as f64 + (frame - start) as f64 * resampler.step();
                let reach = resampler.reach() as i64;
                let first_in = position(from).floor() as i64 - reach + 1;
                let span = (position(to - 1).floor() as i64 + reach - first_in + 1) as usize;
                let input = &mut self.input[..channels * span];
                read(source, first_in, input, channels);
                let weights = &mut self.weights[..2 * reach as usize];
                for k in 0..frames {
                    let frame = from + k as i64;
                    let base = (resampler.weights(position(frame), weights) - first_in) as usize;
                    let gain = fades.at(frame);
                    let mut sums = [0.0f32; 2];
                    for (c, sum) in sums.iter_mut().enumerate().take(channels) {
                        let input = &input[c * span + base..][..weights.len()];
                        *sum = input.iter().zip(weights.iter()).map(|(x, w)| x * w).sum();
                    }
                    for c in 0..output.channels() {
                        output.channel_mut(c)[at + k] += sums[c.min(channels - 1)] * gain;
                    }
                }
            }
        }
    }
}

/// Reads `channels` channels of `source` from file frame `start` into
/// `out`, a channel after another, as many frames as fit. Frames before the
/// file starts or after it ends are silent.
fn read(source: &Source, start: i64, out: &mut [f32], channels: usize) {
    let frames = out.len() / channels;
    // Frames before the file.
    let skip = (-start).clamp(0, frames as i64) as usize;
    let at = start.max(0) as u64;
    let (left, right) = out.split_at_mut(frames);
    let mut parts: [&mut [f32]; 2] = [left, right];
    let parts = &mut parts[..channels];
    for part in parts.iter_mut() {
        part[..skip].fill(0.0);
    }
    match source {
        Source::Whole(data) => {
            for (c, part) in parts.iter_mut().enumerate() {
                let samples = data.channel(c);
                let available = &samples[(at as usize).min(samples.len())..];
                let wanted = &mut part[skip..];
                let n = available.len().min(wanted.len());
                wanted[..n].copy_from_slice(&available[..n]);
                wanted[n..].fill(0.0);
            }
        }
        Source::Stream(stream) => match parts {
            [left] => {
                stream.read(at, &mut [&mut left[skip..]]);
            }
            [left, right] => {
                stream.read(at, &mut [&mut left[skip..], &mut right[skip..]]);
            }
            _ => unreachable!("one or two channels"),
        },
    }
}

/// A clip's gain and fades, in song frames.
#[derive(Debug, Clone, Copy)]
struct Fades {
    gain: f32,
    /// The fade in's first frame, and the frame after its last.
    fade_in: (i64, i64),
    /// The same for the fade out.
    fade_out: (i64, i64),
}

impl Fades {
    /// The clip's gain on `frame`: a quarter sine from silence up to full
    /// over the fade in, and down over the fade out.
    fn at(&self, frame: i64) -> f32 {
        let mut level = 1.0;
        let (start, end) = self.fade_in;
        if frame < end && start < end {
            level *= (FRAC_PI_2 * (frame - start) as f64 / (end - start) as f64).sin();
        }
        let (start, end) = self.fade_out;
        if frame >= start && start < end {
            level *= (FRAC_PI_2 * (end - frame) as f64 / (end - start) as f64).sin();
        }
        self.gain * level as f32
    }

    /// The gain, if no fade touches the frames from `from` to `to`.
    fn flat(&self, from: i64, to: i64) -> Option<f32> {
        let fading_in = from < self.fade_in.1 && self.fade_in.0 < self.fade_in.1;
        let fading_out = to > self.fade_out.0 && self.fade_out.0 < self.fade_out.1;
        (!fading_in && !fading_out).then_some(self.gain)
    }
}

impl Node for AudioClips {
    fn prepare(&mut self, config: &EngineConfig) {
        self.clips.sort_by_key(|c| (c.start, c.id));
        self.reach = self
            .clips
            .iter()
            .scan(None, |latest: &mut Option<Beats>, c| {
                let end = latest.map_or(c.end, |l| l.max(c.end));
                *latest = Some(end);
                Some(end)
            })
            .collect();
        self.streams = (0..self.clips.len())
            .filter(|&i| matches!(self.clips[i].source, Some(Source::Stream(_))))
            .collect();
        let (mut input, mut weights) = (config.max_block, 0);
        for clip in &mut self.clips {
            let rate = clip.source.as_ref().map(Source::sample_rate);
            clip.resampler = rate
                .filter(|&rate| rate != config.sample_rate)
                .map(|rate| Resampler::new(rate, config.sample_rate));
            if let Some(r) = &clip.resampler {
                let span = ((config.max_block - 1) as f64 * r.step()).ceil() as usize;
                input = input.max(span + 2 * r.reach() + 2);
                weights = weights.max(2 * r.reach());
            }
        }
        self.input = vec![0.0; 2 * input];
        self.weights = vec![0.0; weights];
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        if self.clips.is_empty() {
            return;
        }
        self.hint(ctx);
        if !ctx.transport.playing {
            return;
        }
        // Every clip that can have a frame in the block, with a frame to
        // spare on each side, then the ones that do.
        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let first = ctx.transport.position as f64;
        let from = Beats::from_f64(tempo.beats_at(first - 1.0, rate));
        let to = Beats::from_f64(tempo.beats_at(first + ctx.frames as f64, rate));
        let after = self.reach.partition_point(|&end| end <= from);
        let before = self
            .clips
            .partition_point(|c| c.start <= to + Beats::from_ticks(1));
        for i in after..before {
            self.play(i, ctx, output);
        }
    }
}

/// The most notes a MIDI clip player keeps track of at once. It doesn't
/// start notes beyond them.
const MAX_SOUNDING: usize = 1024;

/// Plays a track's MIDI clips: sends their notes, with their expression, to
/// the track's instrument ([`Graph::send_notes`](crate::Graph::send_notes)).
/// Audio coming into it passes through.
///
/// A note plays from its start until it ends or its clip does. Notes that
/// started before the playhead reached them don't play, so a locate or a
/// loop that lands in the middle of a note leaves it out. When the playhead
/// jumps or the transport stops, the notes sounding are let go, and when a
/// recompiled player takes over, so are the ones it doesn't have sounding
/// as they are: a note deleted, moved, transposed, or shortened to end
/// before the playhead stops, and a note made longer plays on.
///
/// Each note's id is its clip's and its own ([`note_id`]), so a note keeps
/// its id however the notes around it are edited.
#[derive(Debug)]
pub struct MidiClips {
    /// What the clips play, in order of when.
    events: Vec<Scheduled>,
    /// The notes that play, by id.
    spans: Vec<Span>,
    /// The notes sounding now. Capacity for [`MAX_SOUNDING`].
    sounding: Vec<Sounding>,
    /// The next event to play, as of the last block.
    cursor: Option<usize>,
    /// The transport's jumps and whether it was playing, as of the last
    /// block.
    last: Option<(u64, bool)>,
    /// Where the last block ended, in beats.
    played_to: Beats,
}

#[derive(Debug, Clone, Copy)]
struct Scheduled {
    at: Beats,
    event: NoteEvent,
    /// For a note's start: its release velocity, in case it's cut short.
    release: f32,
}

/// Where a note plays, and on which key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Span {
    note: NoteId,
    on: Beats,
    key: u8,
    off: Beats,
}

#[derive(Debug, Clone, Copy)]
struct Sounding {
    note: NoteId,
    /// When it started, and on which key.
    on: Beats,
    key: u8,
    release: f32,
    /// A recompiled player no longer has it sounding: it stops at the start
    /// of the next block.
    cut: bool,
}

impl Default for MidiClips {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiClips {
    pub fn new() -> Self {
        Self {
            events: Vec::new(),
            spans: Vec::new(),
            sounding: Vec::with_capacity(MAX_SOUNDING),
            cursor: None,
            last: None,
            played_to: Beats::ZERO,
        }
    }

    /// Adds `clip`'s notes. Audio clips are ignored.
    pub fn add(&mut self, clip: &Clip) {
        let ClipContent::Midi(midi) = &clip.content else {
            return;
        };
        let window = midi.offset..midi.offset + clip.length;
        for note in &midi.notes {
            // Notes are always longer than nothing, but one that weren't
            // would stop before it started, and sound on.
            if !window.contains(&note.start) || note.length <= Beats::ZERO {
                continue;
            }
            let id = note_id(clip.id, note.id);
            let on = clip.start + (note.start - midi.offset);
            let off = (on + note.length).min(clip.end());
            let schedule = |at, event| Scheduled {
                at,
                event,
                release: note.release_velocity,
            };
            self.events.push(schedule(
                on,
                NoteEvent::On {
                    note: id,
                    key: note.key,
                    velocity: note.velocity,
                },
            ));
            for dimension in Dimension::ALL {
                for point in note.expression.curve(dimension) {
                    let at = on + point.at;
                    if at < off {
                        let value = point.value;
                        let change = NoteEvent::Expression {
                            note: id,
                            dimension,
                            value,
                        };
                        self.events.push(schedule(at, change));
                    }
                }
            }
            let velocity = note.release_velocity;
            self.events
                .push(schedule(off, NoteEvent::Off { note: id, velocity }));
            let key = note.key;
            self.spans.push(Span {
                note: id,
                on,
                key,
                off,
            });
        }
    }

    pub fn with_clip(mut self, clip: &Clip) -> Self {
        self.add(clip);
        self
    }

    fn release_all(&mut self, notes: &mut Notes<'_>) {
        for sounding in self.sounding.drain(..) {
            let off = NoteEvent::Off {
                note: sounding.note,
                velocity: sounding.release,
            };
            notes.send(0, off);
        }
    }
}

/// The id a note from a clip plays with: its clip's id in the top 32 bits,
/// and its own in the bottom 32, so whatever gets the note can tell which
/// one it is.
pub fn note_id(clip: ClipId, note: clip::NoteId) -> NoteId {
    NoteId((u64::from(clip.0) << 32) | u64::from(note.0))
}

/// Where an event comes among others at the same time: notes end before
/// others start, and a note's expression follows its start.
fn rank(event: &NoteEvent) -> u8 {
    match event {
        NoteEvent::Off { .. } => 0,
        NoteEvent::On { .. } => 1,
        NoteEvent::Expression { .. } => 2,
    }
}

impl Node for MidiClips {
    fn prepare(&mut self, _config: &EngineConfig) {
        // Stable, so events at the same time keep the clips' order.
        self.events.sort_by_key(|e| (e.at, rank(&e.event)));
        self.spans.sort_unstable();
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        let Some(previous) = previous.downcast_mut::<MidiClips>() else {
            return;
        };
        std::mem::swap(&mut self.sounding, &mut previous.sounding);
        self.last = previous.last;
        self.played_to = previous.played_to;
        for sounding in &mut self.sounding {
            let span = self.spans.binary_search_by_key(&sounding.note, |s| s.note);
            let still = span.is_ok_and(|i| {
                let span = self.spans[i];
                span.on == sounding.on && span.key == sounding.key && span.off > self.played_to
            });
            sounding.cut |= !still;
        }
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

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        self.sounding.retain(|sounding| {
            if sounding.cut {
                let off = NoteEvent::Off {
                    note: sounding.note,
                    velocity: sounding.release,
                };
                notes.send(0, off);
            }
            !sounding.cut
        });
        let transport = ctx.transport;
        if self.last != Some((transport.jumps, true)) || !transport.playing {
            self.release_all(notes);
        }
        self.last = Some((transport.jumps, transport.playing));
        if !transport.playing {
            return;
        }

        let (tempo, rate) = (ctx.tempo, ctx.sample_rate);
        let frame = |at: Beats| tempo.frame_at(at, rate);
        let first = transport.position as i64;
        let end = first + ctx.frames as i64;
        // Carry on from the last block if that's where its next event is,
        // and look for it otherwise: after a jump, a new tempo or a swap.
        let events = &self.events;
        let carries_on = |i: usize| {
            (i == 0 || frame(events[i - 1].at) < first)
                && events.get(i).is_none_or(|e| frame(e.at) >= first)
        };
        let mut i = match self.cursor {
            Some(i) if carries_on(i) => i,
            _ => events.partition_point(|e| frame(e.at) < first),
        };
        while let Some(scheduled) = events.get(i) {
            let at = frame(scheduled.at);
            if at >= end {
                break;
            }
            let offset = (at.max(first) - first) as usize;
            let note = scheduled.event.note();
            let sounding = self.sounding.iter().position(|s| s.note == note);
            match scheduled.event {
                NoteEvent::On { key, .. } => {
                    if sounding.is_none()
                        && self.sounding.len() < MAX_SOUNDING
                        && notes.send(offset, scheduled.event)
                    {
                        self.sounding.push(Sounding {
                            note,
                            on: scheduled.at,
                            key,
                            release: scheduled.release,
                            cut: false,
                        });
                    }
                }
                NoteEvent::Expression { .. } => {
                    if sounding.is_some() {
                        notes.send(offset, scheduled.event);
                    }
                }
                NoteEvent::Off { .. } => {
                    if let Some(s) = sounding {
                        notes.send(offset, scheduled.event);
                        self.sounding.swap_remove(s);
                    }
                }
            }
            i += 1;
        }
        self.cursor = Some(i);
        self.played_to = Beats::from_f64(tempo.beats_at(end as f64, rate));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use musicable_model::clip::{AudioClip, MidiClip, Note, Point};
    use musicable_model::time::{TempoMap, TimeSignature};

    use super::*;
    use crate::media::AudioData;
    use crate::{
        AudioBuffer, Block, Graph, NodeEventKind, NodeId, NodeKey, PreparedGraph,
        ReferenceRenderer, Renderer, TransportState, rt,
    };

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 64,
    };

    /// Frames in a beat at 120 BPM and 48 kHz.
    const BEAT: u64 = 24_000;

    fn beats(beats: f64) -> Beats {
        Beats::from_f64(beats)
    }

    /// Records the notes it gets, at the song position they land on.
    struct Recorder(Vec<(u64, NoteEvent)>);

    impl Node for Recorder {
        fn channels(&self) -> usize {
            1
        }

        fn process(
            &mut self,
            ctx: &ProcessContext<'_>,
            _inputs: Inputs<'_>,
            output: &mut AudioMut<'_>,
        ) {
            output.clear();
            for event in ctx.events {
                if let NodeEventKind::Note(note) = event.kind {
                    let at = ctx.transport.position + event.offset as u64;
                    self.0.push((at, note));
                }
            }
        }
    }

    /// A MIDI clip player feeding a [`Recorder`], and the transport they
    /// play with.
    struct Rig {
        graph: PreparedGraph,
        recorder: NodeId,
        transport: TransportState,
        tempo: TempoMap,
    }

    impl Rig {
        fn new(player: MidiClips) -> Self {
            let mut g = Graph::new();
            let player = g.add(NodeKey(0), player);
            let recorder = g.add(NodeKey(1), Recorder(Vec::with_capacity(4096)));
            g.send_notes(player, recorder).unwrap();
            g.set_output(recorder).unwrap();
            Self {
                graph: g.prepare(&CONFIG).unwrap(),
                recorder,
                transport: TransportState {
                    playing: true,
                    ..TransportState::default()
                },
                tempo: TempoMap::default(),
            }
        }

        /// Plays `frames` frames on from where the transport is, in blocks
        /// of up to `block`.
        fn play(&mut self, frames: u64, block: u64) {
            let end = self.transport.position + frames;
            while self.transport.position < end {
                let frames = block.min(end - self.transport.position) as usize;
                let block = Block {
                    frames,
                    time: 0,
                    transport: self.transport,
                    tempo: &self.tempo,
                };
                let graph = &mut self.graph;
                rt::scope(|| {
                    graph.play_notes(&block);
                    graph.load_routed();
                    ReferenceRenderer.render(graph, &block);
                    graph.clear_routed();
                });
                if self.transport.playing {
                    self.transport.position += frames as u64;
                } else {
                    break;
                }
            }
        }

        fn locate(&mut self, position: u64) {
            self.transport.position = position;
            self.transport.jumps += 1;
        }

        /// The notes recorded since last asked.
        fn notes(&mut self) -> Vec<(u64, NoteEvent)> {
            let node = self.graph.nodes[self.recorder.0].as_mut();
            // Drained, so the recorder keeps its room.
            node.downcast_mut::<Recorder>()
                .unwrap()
                .0
                .drain(..)
                .collect()
        }

        /// Swaps in a player for `clips`, as a recompile would.
        fn recompile(&mut self, player: MidiClips) {
            let mut next = Rig::new(player);
            next.graph.adopt_from(&mut self.graph);
            self.graph = next.graph;
        }
    }

    fn clip(id: u32, start: f64, length: f64, offset: f64, notes: Vec<Note>) -> Clip {
        Clip {
            id: ClipId(id),
            name: String::new(),
            start: beats(start),
            length: beats(length),
            content: ClipContent::Midi(MidiClip {
                offset: beats(offset),
                next_note_id: notes.iter().map(|n| n.id.0 + 1).max().unwrap_or(0),
                notes,
            }),
        }
    }

    fn note(id: u32, start: f64, length: f64, key: u8) -> Note {
        Note {
            id: clip::NoteId(id),
            start: beats(start),
            length: beats(length),
            key,
            velocity: 0.75,
            release_velocity: Note::RELEASE_VELOCITY,
            expression: Default::default(),
        }
    }

    fn on(key: u8) -> impl Fn(&NoteEvent) -> bool {
        move |e| matches!(*e, NoteEvent::On { key: k, .. } if k == key)
    }

    /// The kinds of the events, by name, with where they land.
    fn kinds(events: &[(u64, NoteEvent)]) -> Vec<(u64, &'static str)> {
        let kind = |e: &NoteEvent| match e {
            NoteEvent::On { .. } => "on",
            NoteEvent::Off { .. } => "off",
            NoteEvent::Expression { .. } => "expression",
        };
        events.iter().map(|(at, e)| (*at, kind(e))).collect()
    }

    #[test]
    fn notes_play_on_their_frames_with_their_expression() {
        let mut first = note(0, 0.0, 1.0, 60);
        first.expression.pitch_bend =
            vec![Point::new(beats(0.0), 0.5), Point::new(beats(0.5), 1.0)];
        first.expression.pressure = vec![Point::new(beats(0.25), 0.3)];
        first.release_velocity = 0.25;
        // Longer than the clip, so the clip's end cuts it.
        let second = note(1, 2.0, 4.0, 64);
        let clip = clip(7, 1.0, 4.0, 0.0, vec![first, second]);
        for block in [1, 7, 64] {
            let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
            rig.play(6 * BEAT, block);
            let notes = rig.notes();
            assert_eq!(
                kinds(&notes),
                [
                    (BEAT, "on"),
                    (BEAT, "expression"),
                    (BEAT + BEAT / 4, "expression"),
                    (BEAT + BEAT / 2, "expression"),
                    (2 * BEAT, "off"),
                    (3 * BEAT, "on"),
                    (5 * BEAT, "off"),
                ],
                "blocks of {block}"
            );
            let id = notes[0].1.note();
            assert_eq!(id, note_id(ClipId(7), clip::NoteId(0)));
            assert_eq!(
                notes[0].1,
                NoteEvent::On {
                    note: id,
                    key: 60,
                    velocity: 0.75
                }
            );
            assert_eq!(
                notes[1].1,
                NoteEvent::Expression {
                    note: id,
                    dimension: Dimension::PitchBend,
                    value: 0.5
                }
            );
            assert_eq!(
                notes[2].1,
                NoteEvent::Expression {
                    note: id,
                    dimension: Dimension::Pressure,
                    value: 0.3
                }
            );
            assert_eq!(
                notes[4].1,
                NoteEvent::Off {
                    note: id,
                    velocity: 0.25
                }
            );
            assert_ne!(notes[5].1.note(), id);
            assert_eq!(notes[6].1.note(), notes[5].1.note());
        }
    }

    #[test]
    fn a_clips_offset_skips_notes_before_it_and_moves_the_rest() {
        // The clip starts playing two beats into its notes.
        let notes = vec![
            note(0, 1.0, 0.5, 60),
            note(1, 2.5, 0.5, 62),
            note(2, 3.0, 2.0, 64),
        ];
        let clip = clip(0, 4.0, 2.0, 2.0, notes);
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(8 * BEAT, 64);
        let notes = rig.notes();
        assert!(!notes.iter().any(|(_, e)| on(60)(e)));
        assert_eq!(
            kinds(&notes),
            [
                (4 * BEAT + BEAT / 2, "on"),
                (5 * BEAT, "off"),
                (5 * BEAT, "on"),
                (6 * BEAT, "off"),
            ]
        );
    }

    #[test]
    fn jumps_let_go_of_sounding_notes_without_chasing_others() {
        let clip = clip(
            0,
            0.0,
            8.0,
            0.0,
            vec![note(0, 0.0, 2.0, 60), note(1, 3.0, 2.0, 62)],
        );
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(0, "on")]);
        // Into the middle of the second note: the first lets go where the
        // playhead lands, and the second doesn't start.
        rig.locate(4 * BEAT);
        rig.play(2 * BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(4 * BEAT, "off")]);
        // Back to the start, as a loop would jump.
        rig.locate(0);
        rig.play(BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(0, "on")]);
    }

    #[test]
    fn stopping_lets_go_and_a_new_tempo_doesnt() {
        let clip = clip(0, 0.0, 8.0, 0.0, vec![note(0, 0.0, 4.0, 60)]);
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(BEAT, 64);
        // Half the tempo: the playhead keeps its place in beats, so its
        // position in frames doubles, and the note goes on.
        rig.tempo = TempoMap::new(60.0, TimeSignature::FOUR_FOUR);
        rig.transport.position *= 2;
        rig.play(2 * BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(0, "on")]);
        rig.transport.playing = false;
        rig.play(64, 64);
        assert_eq!(kinds(&rig.notes()), [(4 * BEAT, "off")]);
        // Starting again from there doesn't start the note again.
        rig.transport.playing = true;
        rig.play(BEAT, 64);
        assert!(rig.notes().is_empty());
    }

    #[test]
    fn a_recompiled_player_lets_go_of_only_the_notes_it_lost() {
        let notes = vec![
            note(0, 0.0, 4.0, 60),
            note(1, 0.0, 4.0, 64),
            note(2, 0.0, 4.0, 67),
        ];
        let full = clip(0, 0.0, 8.0, 0.0, notes.clone());
        let mut rig = Rig::new(MidiClips::new().with_clip(&full));
        rig.play(BEAT, 64);
        assert_eq!(rig.notes().len(), 3);

        // Unchanged, with a note added before the others: nothing stops.
        let mut more = notes.clone();
        more.insert(0, note(3, 6.0, 1.0, 50));
        rig.recompile(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, more)));
        rig.play(BEAT, 64);
        assert!(rig.notes().is_empty());

        // The middle note deleted, and the top one shortened to end before
        // the playhead: both let go on the next frame.
        let edited = vec![note(0, 0.0, 4.0, 60), note(2, 0.0, 1.0, 67)];
        rig.recompile(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, edited)));
        rig.play(BEAT, 64);
        let notes = rig.notes();
        assert_eq!(kinds(&notes), [(2 * BEAT, "off"), (2 * BEAT, "off")]);
        // The bottom one plays on to its end.
        rig.play(2 * BEAT, 64);
        assert_eq!(kinds(&rig.notes()), [(4 * BEAT, "off")]);
    }

    #[test]
    fn notes_moved_or_transposed_as_they_sound_stop_and_longer_ones_play_on() {
        let notes = vec![
            note(0, 0.0, 4.0, 60),
            note(1, 0.0, 4.0, 64),
            note(2, 0.0, 4.0, 67),
        ];
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, notes)));
        rig.play(BEAT, 64);
        assert_eq!(rig.notes().len(), 3);
        // Longer, transposed, and moved to start where the playhead has
        // already been, so it doesn't start again.
        let edited = vec![
            note(0, 0.0, 6.0, 60),
            note(1, 0.0, 4.0, 65),
            note(2, 0.5, 4.0, 67),
        ];
        rig.recompile(MidiClips::new().with_clip(&clip(0, 0.0, 8.0, 0.0, edited)));
        rig.play(6 * BEAT, 64);
        let notes = rig.notes();
        assert_eq!(
            kinds(&notes),
            [(BEAT, "off"), (BEAT, "off"), (6 * BEAT, "off")]
        );
        assert_eq!(notes[2].1.note(), note_id(ClipId(0), clip::NoteId(0)));
    }

    #[test]
    fn notes_alike_play_as_two() {
        let clip = clip(
            0,
            0.0,
            4.0,
            0.0,
            vec![note(0, 0.0, 1.0, 60), note(1, 0.0, 2.0, 60)],
        );
        let mut rig = Rig::new(MidiClips::new().with_clip(&clip));
        rig.play(3 * BEAT, 64);
        let notes = rig.notes();
        assert_eq!(
            kinds(&notes),
            [(0, "on"), (0, "on"), (BEAT, "off"), (2 * BEAT, "off")]
        );
        assert_ne!(notes[0].1.note(), notes[1].1.note());
    }

    /// Plays `player` from frame `from` for `frames` frames, in blocks of
    /// up to `block`, and returns the left and right channels.
    fn audio(player: AudioClips, from: u64, frames: usize, block: usize) -> [Vec<f32>; 2] {
        let mut player = player;
        let config = EngineConfig {
            max_block: block,
            ..CONFIG
        };
        player.prepare(&config);
        let tempo = TempoMap::default();
        let mut buffer = AudioBuffer::new(2, block);
        let (mut left, mut right) = (Vec::new(), Vec::new());
        while left.len() < frames {
            let n = block.min(frames - left.len());
            let ctx = ProcessContext {
                frames: n,
                sample_rate: 48_000,
                time: 0,
                transport: TransportState {
                    playing: true,
                    position: from + left.len() as u64,
                    ..TransportState::default()
                },
                tempo: &tempo,
                events: &[],
            };
            rt::scope(|| player.process(&ctx, Inputs::NONE, &mut buffer.view_mut(n)));
            left.extend_from_slice(buffer.view(n).channel(0));
            right.extend_from_slice(buffer.view(n).channel(1));
        }
        [left, right]
    }

    fn audio_clip(start: f64, length: f64, audio: AudioClip) -> Clip {
        Clip {
            id: ClipId(0),
            name: String::new(),
            start: beats(start),
            length: beats(length),
            content: ClipContent::Audio(audio),
        }
    }

    /// A stereo file whose samples count up: frame `i` is `i` on the left
    /// and `-i` on the right.
    fn counting(frames: usize, sample_rate: u32) -> Source {
        let left = (0..frames).map(|i| i as f32).collect::<Vec<_>>();
        let right = left.iter().map(|s| -s).collect();
        Source::Whole(Arc::new(AudioData::new(sample_rate, vec![left, right])))
    }

    #[test]
    fn audio_clips_play_their_files_sample_for_sample() {
        let mut take = AudioClip::new("take.wav");
        take.offset = 1000;
        take.gain = 0.5;
        // From beat 1, for a beat and a half, but the file runs out first.
        let clip = audio_clip(1.0, 1.5, take);
        for block in [1, 64, 1000] {
            let player = AudioClips::new().with_clip(&clip, Some(counting(20_000, 48_000)));
            let [left, right] = audio(player, 0, 70_000, block);
            for (i, (&l, &r)) in left.iter().zip(&right).enumerate() {
                let frame = i as u64;
                let file = 1000 + frame as i64 - BEAT as i64;
                let expected = match frame {
                    _ if frame < BEAT => 0.0,
                    _ if file < 20_000 => file as f32 * 0.5,
                    _ => 0.0,
                };
                assert_eq!(
                    (l, r),
                    (expected, -expected),
                    "frame {frame}, blocks of {block}"
                );
            }
        }
    }

    #[test]
    fn mono_files_play_in_both_channels() {
        let data = AudioData::new(48_000, vec![vec![0.25; 100]]);
        let source = Source::Whole(Arc::new(data));
        let clip = audio_clip(0.0, 1.0, AudioClip::new("mono.wav"));
        let [left, right] = audio(AudioClips::new().with_clip(&clip, Some(source)), 0, 200, 64);
        assert_eq!(left[..100], [0.25; 100]);
        assert_eq!(left, right);
        assert_eq!(left[100..], [0.0; 100]);
    }

    #[test]
    fn fades_are_quarter_sines_that_crossfade_at_constant_power() {
        let dc = || Source::Whole(Arc::new(AudioData::new(48_000, vec![vec![1.0; 100_000]])));
        let mut fading = AudioClip::new("a.wav");
        fading.fade_in = beats(0.25);
        fading.fade_out = beats(0.5);
        let clip = audio_clip(0.0, 2.0, fading);
        let [out, _] = audio(
            AudioClips::new().with_clip(&clip, Some(dc())),
            0,
            50_000,
            64,
        );
        let fade_in = BEAT as usize / 4;
        let fade_out = BEAT as usize / 2;
        let quarter_sine = |t: f64| (FRAC_PI_2 * t).sin() as f32;
        assert_eq!(out[0], 0.0);
        assert_eq!(out[fade_in / 2], quarter_sine(0.5));
        assert_eq!(
            out[fade_in..48_000 - fade_out],
            vec![1.0; 48_000 - fade_out - fade_in][..]
        );
        assert_eq!(out[48_000 - fade_out / 2], quarter_sine(0.5));
        assert_eq!(out[47_999], quarter_sine(1.0 / fade_out as f64));
        assert_eq!(out[48_000..], [0.0; 2_000]);

        // One clip fades out over the frames the next fades in over.
        let mut a = AudioClip::new("a.wav");
        a.fade_out = beats(1.0);
        let mut b = AudioClip::new("b.wav");
        b.fade_in = beats(1.0);
        let (a, mut b) = (audio_clip(0.0, 2.0, a), audio_clip(1.0, 2.0, b));
        b.id = ClipId(1);
        let only = |clip: &Clip| {
            let [out, _] = audio(AudioClips::new().with_clip(clip, Some(dc())), 0, 72_000, 64);
            out
        };
        let (a, b) = (only(&a), only(&b));
        for frame in BEAT as usize..2 * BEAT as usize {
            let power = a[frame] * a[frame] + b[frame] * b[frame];
            assert!((power - 1.0).abs() < 1e-5, "frame {frame}: {power}");
        }
    }

    #[test]
    fn files_at_another_rate_are_resampled() {
        // A 1 kHz sine at 44.1 kHz, played at 48 kHz.
        let rate = 44_100;
        let sine = |i: usize, rate: u32| {
            (std::f64::consts::TAU * 1000.0 * i as f64 / f64::from(rate)).sin() as f32
        };
        let samples = (0..50_000).map(|i| sine(i, rate)).collect();
        let source = Source::Whole(Arc::new(AudioData::new(rate, vec![samples])));
        let clip = audio_clip(0.0, 2.0, AudioClip::new("44k.wav"));
        let player = || AudioClips::new().with_clip(&clip, Some(source.clone()));
        let [out, _] = audio(player(), 0, 40_000, 64);
        for (i, &s) in out.iter().enumerate().skip(100) {
            assert!((s - sine(i, 48_000)).abs() < 2e-3, "frame {i}: {s}");
        }
        // However the blocks fall.
        assert_eq!(audio(player(), 0, 40_000, 7)[0], out);
        assert_eq!(audio(player(), 1000, 5000, 64)[0], out[1000..6000]);
    }

    #[test]
    fn clips_without_a_source_are_silent() {
        let clip = audio_clip(0.0, 1.0, AudioClip::new("missing.wav"));
        let [left, right] = audio(AudioClips::new().with_clip(&clip, None), 0, 1000, 64);
        assert!(left.iter().chain(&right).all(|&s| s == 0.0));
    }
}
