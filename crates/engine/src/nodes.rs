//! Built-in nodes.

use std::f64::consts::TAU;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use musicable_model::clip::{Controller, Dimension};
use musicable_model::mixer::pan_gains;
use musicable_model::param::{ParamSpec, Scale};
use musicable_model::time::{BeatRange, Beats};
use musicable_model::{ClipType, Monitor};

use crate::automation::Param;
use crate::meter::Meter;
use crate::{
    AudioMut, AudioRef, Automation, EngineConfig, Inputs, Node, NodeEvent, NodeEventKind,
    NoteEvent, NoteId, ParamId, ProcessContext, TransportState,
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

/// Plays what comes in a whole number of frames later. Delay compensation
/// puts one wherever a path meets another with more latency, to line them
/// up (see [`Graph::prepare`](crate::Graph::prepare)).
///
/// It sums its inputs, as [`Sum`] does, and delays the sum. Taking over from
/// another delay in a swap, it carries on with the frames that one holds,
/// so nothing drops out. If the length changed, it takes the newest frames
/// it has room for, and starts with silence where the old one had too few.
#[derive(Debug, Clone)]
pub struct Delay {
    frames: usize,
    channels: usize,
    /// The last `frames` frames that came in, channel after channel. Each
    /// channel's are a ring, the oldest at `next`.
    held: Vec<f32>,
    next: usize,
}

impl Delay {
    /// Delays a stereo signal by `frames`.
    pub fn new(frames: usize) -> Self {
        Self {
            frames,
            channels: 2,
            held: Vec::new(),
            next: 0,
        }
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    /// How many frames late it plays what comes in.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Copies the newest `out.len()` frames channel `c` holds into `out`,
    /// oldest first.
    fn newest(&self, c: usize, out: &mut [f32]) {
        let ring = &self.held[c * self.frames..][..self.frames];
        let k = out.len();
        if k <= self.next {
            out.copy_from_slice(&ring[self.next - k..self.next]);
        } else {
            let wrapped = k - self.next;
            out[..wrapped].copy_from_slice(&ring[self.frames - wrapped..]);
            out[wrapped..].copy_from_slice(&ring[..self.next]);
        }
    }
}

impl Node for Delay {
    fn channels(&self) -> usize {
        self.channels
    }

    fn prepare(&mut self, _config: &EngineConfig) {
        self.held = vec![0.0; self.channels * self.frames];
        self.next = 0;
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        let Some(previous) = previous.downcast_mut::<Delay>() else {
            return;
        };
        if (previous.frames, previous.channels) == (self.frames, self.channels) {
            std::mem::swap(&mut self.held, &mut previous.held);
            self.next = previous.next;
            return;
        }
        // Silence first, then the newest frames the previous one holds, so
        // what it held plays out without a gap.
        let k = self.frames.min(previous.frames);
        for c in 0..self.channels.min(previous.channels) {
            let start = c * self.frames + self.frames - k;
            previous.newest(c, &mut self.held[start..][..k]);
        }
    }

    fn reset(&mut self) {
        self.held.fill(0.0);
        self.next = 0;
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
        let frames = self.frames;
        if frames == 0 {
            return;
        }
        // Each frame of the block swaps places with the frame `frames`
        // earlier, which the ring holds.
        let mut next = self.next;
        for (c, out) in output.channels_mut().enumerate().take(self.channels) {
            let ring = &mut self.held[c * frames..][..frames];
            next = self.next;
            let mut done = 0;
            while done < out.len() {
                let n = (frames - next).min(out.len() - done);
                out[done..done + n].swap_with_slice(&mut ring[next..next + n]);
                done += n;
                next = (next + n) % frames;
            }
        }
        self.next = next;
    }
}

/// A node with latency, for tests: it plays what comes in late, as a
/// [`Delay`] does, and reports the delay as its latency, as a plugin that
/// looks ahead does.
#[cfg(test)]
pub(crate) struct Latent(pub Delay);

#[cfg(test)]
impl Node for Latent {
    fn channels(&self) -> usize {
        self.0.channels()
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.0.prepare(config);
    }

    fn latency(&self) -> usize {
        self.0.frames()
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Latent>() {
            self.0.adopt(&mut previous.0);
        }
    }

    fn reset(&mut self) {
        self.0.reset();
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        self.0.process(ctx, inputs, output);
    }
}

/// A single 1.0 on every channel at engine time `at`, and silence
/// otherwise, for tests. It ignores what comes in.
#[cfg(test)]
pub(crate) struct Impulse {
    pub at: u64,
}

#[cfg(test)]
impl Node for Impulse {
    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        if let Some(offset) = self.at.checked_sub(ctx.time)
            && offset < ctx.frames as u64
        {
            for channel in output.channels_mut() {
                channel[offset as usize] = 1.0;
            }
        }
    }
}

/// Spins for a while, for tests of the loads: for `render` in each block it
/// renders, split into `tasks` tasks ([`tasks::run`](crate::tasks::run))
/// when there's more than one, and for `before` as it plays its notes or
/// modulates, if it does. It adds the time it spun, as it measured it, to
/// `spun`, in nanoseconds, and outputs silence.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Spin {
    pub render: std::time::Duration,
    pub tasks: u32,
    pub before: std::time::Duration,
    pub spun: Arc<std::sync::atomic::AtomicU64>,
}

#[cfg(test)]
impl Spin {
    fn spin(spun: &std::sync::atomic::AtomicU64, time: std::time::Duration) {
        let started = std::time::Instant::now();
        let mut elapsed = started.elapsed();
        while elapsed < time {
            std::hint::spin_loop();
            elapsed = started.elapsed();
        }
        spun.fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Node for Spin {
    fn play_notes(&mut self, _: &ProcessContext<'_>, _: &mut crate::Notes<'_>) {
        Self::spin(&self.spun, self.before);
    }

    fn control(&mut self, _: &ProcessContext<'_>, _: &mut crate::Controls<'_>) {
        Self::spin(&self.spun, self.before);
    }

    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        let tasks = self.tasks.max(1);
        let (spun, time) = (&*self.spun, self.render / tasks);
        crate::tasks::run(tasks, &|_| Self::spin(spun, time));
        output.clear();
    }
}

/// A parameter that ramps to each new value instead of jumping to it, so
/// changing a gain doesn't click.
///
/// A ramp is linear and takes [`RAMP_SECONDS`](Self::RAMP_SECONDS). It
/// counts frames, so a change on a given frame ramps the same way however
/// the blocks fall, and on whichever thread the node runs. A node calls
/// [`next_value`](Self::next_value) once per frame, or [`skip`](Self::skip)
/// while [settled](Self::is_settled).
///
/// Until [`prepare`](Self::prepare) gives it a sample rate, and until its
/// node has rendered a frame, new values apply at once: a node that hasn't
/// made a sound yet has nothing to click.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Smoothed {
    /// Where the ramp started.
    from: f32,
    target: f32,
    /// Frames into the ramp.
    frame: u32,
    /// Frames a ramp takes. 0 jumps.
    frames: u32,
    /// `1 / frames`.
    step: f32,
    /// Whether its node has rendered a frame.
    started: bool,
}

impl Smoothed {
    /// How long a ramp takes: 10 ms, which is short enough to feel instant
    /// and long enough not to click.
    pub const RAMP_SECONDS: f64 = 0.01;

    /// Settled at `value`, jumping to new values until prepared.
    pub fn new(value: f32) -> Self {
        Self {
            from: value,
            target: value,
            frame: 0,
            frames: 0,
            step: 0.0,
            started: false,
        }
    }

    /// Sets how many frames a ramp takes at `sample_rate`. Call it from the
    /// node's [`Node::prepare`].
    pub fn prepare(&mut self, sample_rate: u32) {
        self.prepare_frames((Self::RAMP_SECONDS * f64::from(sample_rate)).round() as u32);
    }

    /// Has ramps take `frames` frames, whatever the sample rate: for what
    /// moves in steps of engine time, as modulation does.
    pub fn prepare_frames(&mut self, frames: u32) {
        self.frames = frames;
        self.step = 1.0 / self.frames.max(1) as f32;
        self.settle();
    }

    /// Ramps from the value now to `target`. Setting the target it already
    /// has changes nothing, even mid-ramp.
    pub fn set(&mut self, target: f32) {
        if target == self.target {
            return;
        }
        self.from = self.value();
        self.target = target;
        self.frame = 0;
        if !self.started || self.from == target {
            self.settle();
        }
    }

    /// The value on the frame last rendered.
    pub fn value(&self) -> f32 {
        if self.frame >= self.frames {
            self.target
        } else {
            self.from + (self.target - self.from) * (self.frame as f32 * self.step)
        }
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Whether it's at its target, not ramping.
    pub fn is_settled(&self) -> bool {
        self.frame >= self.frames
    }

    /// The value for the next frame. A ramp reaches its target on its last
    /// frame.
    #[inline]
    pub fn next_value(&mut self) -> f32 {
        self.started = true;
        if self.frame < self.frames {
            self.frame += 1;
        }
        self.value()
    }

    /// Moves `frames` frames on, as if [`next_value`](Self::next_value) were
    /// called that many times.
    pub fn skip(&mut self, frames: usize) {
        if frames > 0 {
            self.started = true;
            let frame = (self.frame as usize).saturating_add(frames);
            self.frame = frame.min(self.frames as usize) as u32;
        }
    }

    /// Jumps to the target.
    pub fn settle(&mut self) {
        self.from = self.target;
        self.frame = self.frames;
    }

    /// Jumps to the target, as if its node had rendered nothing yet, so new
    /// values apply at once again until it renders a frame: for a node
    /// starting afresh ([`Node::reset`]).
    pub fn reset(&mut self) {
        self.settle();
        self.started = false;
    }

    /// Jumps to `value` and holds it there, as if it had ramped there: for
    /// carrying on from where something else, such as automation, left a
    /// parameter.
    pub fn hold(&mut self, value: f32) {
        self.from = value;
        self.target = value;
        self.frame = self.frames;
    }

    /// Carries on from where `previous`, the same parameter in the node this
    /// one replaces, has got to: from its value, ramping on to this one's
    /// target. Call it from [`Node::adopt`].
    pub fn take_over(&mut self, previous: &Smoothed) {
        let target = self.target;
        *self = Smoothed {
            frame: previous.frame.min(self.frames),
            frames: self.frames,
            step: self.step,
            ..*previous
        };
        self.set(target);
    }
}

/// Sums its inputs and applies a gain and a balance: a fader, a send, or a
/// channel's output stage. It can measure its output into a [`Meter`].
///
/// Gain, mute and balance changes ramp ([`Smoothed`]), so they don't click.
/// The gain and the balance can follow automation and take modulation
/// ([`Param`]).
#[derive(Debug, Clone)]
pub struct Gain {
    gain: Param,
    /// What the output is multiplied by on top of the gain: 1.0, or 0.0
    /// while muted.
    mute: Smoothed,
    /// The balance, from -1.0 to 1.0.
    pan: Param,
    channels: usize,
    meter: Option<Arc<Meter>>,
}

impl Gain {
    /// Linear gain, 1.0 is unity. Automatable, and modulation keeps it from
    /// 0.0 to +6 dB.
    pub const GAIN: ParamId = ParamId(0);
    /// 1.0 mutes, 0.0 unmutes.
    pub const MUTE: ParamId = ParamId(1);
    /// Balance, from -1.0 (left) to 1.0 (right), by
    /// [`pan_gains`](musicable_model::mixer::pan_gains). Only stereo gain
    /// stages have it. At 0.0, the default, it changes nothing.
    /// Automatable, and modulation keeps it within its range.
    pub const PAN: ParamId = ParamId(2);

    /// How many frames it works its parameters out for at a time.
    const CHUNK: usize = 64;

    /// A stereo gain stage.
    pub fn new(gain: f32) -> Self {
        Self {
            gain: Param::new(gain, 0.0, ParamSpec::VOLUME_MAX as f32).with_scale(Scale::Fader),
            mute: Smoothed::new(1.0),
            pan: Param::new(0.0, -1.0, 1.0),
            channels: 2,
            meter: None,
        }
    }

    pub fn with_pan(mut self, pan: f32) -> Self {
        self.pan = Param::new(pan, -1.0, 1.0);
        self
    }

    /// Keeps the gain from 0.0 to `max` under modulation, rather than to
    /// +6 dB: a send's goes as far as unity.
    pub fn with_max_gain(mut self, max: f32) -> Self {
        self.gain = Param::new(self.gain.target(), 0.0, max).with_scale(Scale::Fader);
        self
    }

    pub fn with_muted(mut self, muted: bool) -> Self {
        self.mute = Smoothed::new(if muted { 0.0 } else { 1.0 });
        self
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    /// Measures each block of output, after the gain, into `meter`.
    pub fn with_meter(mut self, meter: Arc<Meter>) -> Self {
        self.meter = Some(meter);
        self
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param {
                param: Self::GAIN,
                value,
            } => self.gain.set(value as f32),
            NodeEventKind::Param {
                param: Self::MUTE,
                value,
            } => self.mute.set(if value >= 0.5 { 0.0 } else { 1.0 }),
            NodeEventKind::Param {
                param: Self::PAN,
                value,
            } => self.pan.set(value as f32),
            NodeEventKind::Modulation {
                param: Self::GAIN,
                amount,
                note: None,
            } => self.gain.modulate(amount as f32),
            NodeEventKind::Modulation {
                param: Self::PAN,
                amount,
                note: None,
            } => self.pan.modulate(amount as f32),
            _ => {}
        }
    }

    fn scale(&mut self, ctx: &ProcessContext<'_>, output: &mut AudioMut<'_>, frames: Range<usize>) {
        let mut start = frames.start;
        while start < frames.end {
            let end = frames.end.min(start + Self::CHUNK);
            self.scale_chunk(ctx, output, start..end);
            start = end;
        }
    }

    fn scale_chunk(
        &mut self,
        ctx: &ProcessContext<'_>,
        output: &mut AudioMut<'_>,
        frames: Range<usize>,
    ) {
        let len = frames.len();
        let mut gains = [0.0; Self::CHUNK];
        let mut pans = [0.0; Self::CHUNK];
        let (gains, pans) = (&mut gains[..len], &mut pans[..len]);
        let steady_gain = self.gain.render(ctx, frames.clone(), gains);
        let steady_pan = self.pan.render(ctx, frames.clone(), pans);
        let mut part = output.range_mut(frames);
        // Mono stages have no balance.
        let stereo = self.channels == 2;
        if steady_gain && (steady_pan || !stereo) && self.mute.is_settled() {
            let level = gains[0] * self.mute.value();
            let balance = if stereo {
                pan_gains(pans[0])
            } else {
                [1.0, 1.0]
            };
            self.mute.skip(len);
            for (c, channel) in part.channels_mut().enumerate() {
                // Exactly the level at the centre, so centred channels sound
                // as they did before there was a balance control.
                let gain = level * balance.get(c).copied().unwrap_or(1.0);
                if gain == 0.0 {
                    channel.fill(0.0);
                } else if gain != 1.0 {
                    for sample in channel {
                        *sample *= gain;
                    }
                }
            }
            return;
        }
        // Each channel ramps through the same values.
        for (c, channel) in part.channels_mut().enumerate() {
            let mut mute = self.mute;
            for (i, sample) in channel.iter_mut().enumerate() {
                let level = gains[i] * mute.next_value();
                let balance = if stereo { pan_gains(pans[i])[c] } else { 1.0 };
                *sample *= level * balance;
            }
        }
        self.mute.skip(len);
    }
}

impl Node for Gain {
    fn channels(&self) -> usize {
        self.channels
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.gain.prepare(config.sample_rate);
        self.mute.prepare(config.sample_rate);
        self.pan.prepare(config.sample_rate);
    }

    fn automate(&mut self, param: ParamId, automation: Automation) -> Option<Automation> {
        match param {
            Self::GAIN => self.gain.automate(automation),
            Self::PAN => self.pan.automate(automation),
            _ => return Some(automation),
        }
        None
    }

    fn modulated(&mut self, param: ParamId) {
        match param {
            Self::GAIN => self.gain.set_modulated(),
            Self::PAN => self.pan.set_modulated(),
            _ => {}
        }
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Gain>() {
            self.gain.take_over(&previous.gain);
            self.mute.take_over(&previous.mute);
            self.pan.take_over(&previous.pan);
        }
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        for event in ctx.events {
            self.apply(event);
        }
    }

    fn reset(&mut self) {
        self.gain.reset();
        self.mute.reset();
        self.pan.reset();
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let mut start = 0;
        for event in ctx.events {
            self.scale(ctx, output, start..event.offset);
            self.apply(event);
            start = event.offset;
        }
        self.scale(ctx, output, start..ctx.frames);
        if let Some(meter) = &self.meter {
            meter.measure(output.view(), ctx.sample_rate);
        }
    }
}

/// Plays what comes into an audio track's input in place of its clips,
/// while the track monitors its input: always with [`Monitor::In`], with
/// [`Monitor::Auto`] while the track is armed and the transport is stopped
/// or recording, and never with [`Monitor::Off`]. It goes over from one to
/// the other in 10 ms ([`Smoothed`]), so switching doesn't click.
///
/// The input is one or two of the engine's device inputs
/// ([`ProcessContext::input`]), or another track's tap, which comes into
/// its side input 0. A mono input plays in both channels. A track whose
/// input isn't open monitors silence. A track resampling the master never
/// plays it, whatever its monitoring: it plays its clips but while it's
/// armed and the transport records, when it plays nothing, from the frame
/// recording starts, so it isn't in the mix it records. Its
/// [`ResamplingCut`] does the same after its devices.
#[derive(Debug, Clone)]
pub struct InputMonitor {
    monitor: Monitor,
    armed: bool,
    input: Source,
    /// How much of the input plays: 0.0 the clips, 1.0 the input.
    mix: Smoothed,
    /// Where it says whether the track is armed, for its cut.
    cut: Option<Arc<AtomicBool>>,
}

/// Where an [`InputMonitor`]'s input comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// Nowhere: the input isn't open, so it's silent.
    None,
    /// The engine's device input channels the track's input takes: the
    /// first, and how many.
    Device(usize, usize),
    /// Side input 0: another track's tap.
    Side,
    /// The master, which it records and never plays.
    Resampling,
}

impl InputMonitor {
    /// 1.0 arms the track, 0.0 disarms it.
    pub const ARMED: ParamId = ParamId(0);
    /// The track's monitoring, as [`monitor_value`](Self::monitor_value)
    /// gives it.
    pub const MONITOR: ParamId = ParamId(1);

    /// A track's monitor, without an input.
    pub fn new(monitor: Monitor, armed: bool) -> Self {
        Self {
            monitor,
            armed,
            input: Source::None,
            mix: Smoothed::new(0.0),
            cut: None,
        }
    }

    /// Takes `channels` device input channels from channel `first`: one
    /// or two.
    pub fn with_input(mut self, first: usize, channels: usize) -> Self {
        self.input = Source::Device(first, channels.clamp(1, 2));
        self
    }

    /// Takes what comes into its side input 0: another track's tap.
    pub fn with_side_input(mut self) -> Self {
        self.input = Source::Side;
        self
    }

    /// Resamples the master: it never plays its input, and plays nothing
    /// while it's armed and the transport records.
    pub fn resampling(mut self) -> Self {
        self.input = Source::Resampling;
        self
    }

    /// The node that cuts what comes out of the track's devices while
    /// this one cuts what comes into them, for a track resampling the
    /// master, so what they ring on with is cut too. It follows this one's
    /// arming.
    pub fn cut(&mut self) -> ResamplingCut {
        let armed = Arc::new(AtomicBool::new(self.armed));
        self.cut = Some(Arc::clone(&armed));
        ResamplingCut {
            armed,
            cut: Smoothed::new(0.0),
        }
    }

    /// [`MONITOR`](Self::MONITOR)'s value for `monitor`.
    pub fn monitor_value(monitor: Monitor) -> f64 {
        match monitor {
            Monitor::In => 0.0,
            Monitor::Auto => 1.0,
            Monitor::Off => 2.0,
        }
    }

    /// Whether it plays its input in place of the clips: for a track
    /// resampling the master, whether it plays silence.
    fn monitors(&self, transport: &TransportState) -> bool {
        match self.monitor {
            _ if self.input == Source::Resampling => self.armed && transport.recording,
            Monitor::In => true,
            Monitor::Auto => self.armed && (!transport.playing || transport.recording),
            Monitor::Off => false,
        }
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param {
                param: Self::ARMED,
                value,
            } => self.armed = value >= 0.5,
            NodeEventKind::Param {
                param: Self::MONITOR,
                value,
            } => {
                self.monitor = match value.round() as i64 {
                    0 => Monitor::In,
                    2 => Monitor::Off,
                    _ => Monitor::Auto,
                }
            }
            _ => {}
        }
    }

    /// Plays the input, from the device inputs or `side`, over `output`'s
    /// `frames` as far as the monitoring says.
    fn mix(
        &mut self,
        ctx: &ProcessContext<'_>,
        side: Option<AudioRef<'_>>,
        output: &mut AudioMut<'_>,
        frames: Range<usize>,
    ) {
        let monitors = self.monitors(&ctx.transport);
        self.mix.set(if monitors { 1.0 } else { 0.0 });
        // A track resampling the master is out of it from the frame it
        // starts recording, so none of it is in what it records. It comes
        // back in without a click.
        if monitors && self.input == Source::Resampling {
            self.mix.settle();
        }
        let len = frames.len();
        if self.mix.is_settled() && self.mix.value() == 0.0 {
            self.mix.skip(len);
            return;
        }
        // An input the stream didn't open is silent, and so is a tap that
        // feeds nothing in.
        let input = match self.input {
            Source::Device(first, channels) if first + channels <= ctx.input.channels() => {
                Some((ctx.input, first, channels))
            }
            Source::Side => side
                .filter(|side| side.channels() > 0)
                .map(|side| (side, 0, side.channels())),
            Source::Device(..) | Source::None | Source::Resampling => None,
        };
        let settled = self.mix.is_settled();
        let mut part = output.range_mut(frames.clone());
        for (c, channel) in part.channels_mut().enumerate() {
            let source = input.map(|(input, first, channels)| {
                &input.channel(first + c.min(channels - 1))[frames.clone()]
            });
            match source {
                Some(source) if settled => channel.copy_from_slice(source),
                None if settled => channel.fill(0.0),
                _ => {
                    let mut mix = self.mix;
                    for (i, sample) in channel.iter_mut().enumerate() {
                        let m = mix.next_value();
                        let x = source.map_or(0.0, |source| source[i]);
                        *sample = *sample * (1.0 - m) + x * m;
                    }
                }
            }
        }
        self.mix.skip(len);
    }
}

impl Node for InputMonitor {
    fn prepare(&mut self, config: &EngineConfig) {
        self.mix.prepare(config.sample_rate);
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<InputMonitor>() {
            self.mix.take_over(&previous.mix);
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let side = inputs.side(0).next();
        let mut start = 0;
        for event in ctx.events {
            self.mix(ctx, side, output, start..event.offset);
            self.apply(event);
            start = event.offset;
        }
        self.mix(ctx, side, output, start..ctx.frames);
        // The cut comes after it in the graph, so it processes the block
        // after this, armed as this ends it.
        if let Some(cut) = &self.cut {
            cut.store(self.armed, Ordering::Release);
        }
    }

    /// A track whose input is the audio devices' is live while it can
    /// monitor them, whether the transport lets it now or not, and while
    /// it's still fading them out.
    fn live(&self) -> Option<bool> {
        let Source::Device(..) = self.input else {
            return None;
        };
        let can = match self.monitor {
            Monitor::In => true,
            Monitor::Auto => self.armed,
            Monitor::Off => false,
        };
        let silent = self.mix.is_settled() && self.mix.value() == 0.0;
        Some(can || !silent)
    }
}

/// Cuts what comes out of the devices of a track resampling the master
/// while the track's [`InputMonitor`] cuts what comes into them: while
/// the track is armed and the transport records, from the frame recording
/// starts, so what they ring on with then isn't in the mix the track
/// records either. It comes back on a 10 ms ramp, as the monitor does.
/// [`InputMonitor::cut`] makes it.
#[derive(Debug, Clone)]
pub struct ResamplingCut {
    /// Whether the track is armed, as its monitor says.
    armed: Arc<AtomicBool>,
    /// How much is cut: 0.0 nothing, 1.0 everything.
    cut: Smoothed,
}

impl Node for ResamplingCut {
    fn prepare(&mut self, config: &EngineConfig) {
        self.cut.prepare(config.sample_rate);
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<ResamplingCut>() {
            self.cut.take_over(&previous.cut);
        }
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let cuts = self.armed.load(Ordering::Acquire) && ctx.transport.recording;
        self.cut.set(if cuts { 1.0 } else { 0.0 });
        if cuts {
            self.cut.settle();
        }
        if self.cut.is_settled() {
            if self.cut.value() == 1.0 {
                output.clear();
            }
            self.cut.skip(ctx.frames);
            return;
        }
        for channel in output.channels_mut() {
            let mut cut = self.cut;
            for sample in channel.iter_mut() {
                *sample *= 1.0 - cut.next_value();
            }
        }
        self.cut.skip(ctx.frames);
    }
}

/// Sums its inputs and clips them as its [`ClipType`] says: a channel's
/// clip stage. (Clipping, not clips: those are what
/// [`AudioClips`](crate::clips::AudioClips) and
/// [`MidiClips`](crate::clips::MidiClips) play.)
#[derive(Debug, Clone)]
pub struct Clipper {
    clip_type: ClipType,
    channels: usize,
}

impl Clipper {
    /// Stereo.
    pub fn new(clip_type: ClipType) -> Self {
        Self {
            clip_type,
            channels: 2,
        }
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    /// One sample through the clip type's curve. Soft clipping leaves
    /// everything up to half scale alone, then bends smoothly towards full
    /// scale: `1 - 0.25 / |x|`, which meets the straight line at 0.5 at the
    /// same slope.
    fn clip(clip_type: ClipType, sample: f32) -> f32 {
        match clip_type {
            ClipType::Off => sample,
            ClipType::Hard => sample.clamp(-1.0, 1.0),
            ClipType::Soft => {
                let magnitude = sample.abs();
                if magnitude <= 0.5 {
                    sample
                } else {
                    (1.0 - 0.25 / magnitude).copysign(sample)
                }
            }
        }
    }
}

impl Node for Clipper {
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
        if self.clip_type == ClipType::Off {
            return;
        }
        for channel in output.channels_mut() {
            for sample in channel {
                *sample = Self::clip(self.clip_type, *sample);
            }
        }
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
    level: Smoothed,
    channels: usize,
    sample_rate: f64,
}

impl Sine {
    /// Frequency in Hz. The phase jumps when it changes during playback.
    pub const FREQUENCY: ParamId = ParamId(0);
    /// Linear amplitude. Changes ramp ([`Smoothed`]).
    pub const LEVEL: ParamId = ParamId(1);

    /// Stereo, the same signal in both channels.
    pub fn new(frequency: f64, level: f32) -> Self {
        Self {
            frequency,
            level: Smoothed::new(level),
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
            } => self.level.set(value as f32),
            _ => {}
        }
    }

    fn render(
        &mut self,
        ctx: &ProcessContext<'_>,
        output: &mut AudioMut<'_>,
        frames: Range<usize>,
    ) {
        if !ctx.transport.playing {
            // Silent, so a new level can apply at once.
            self.level.settle();
            output.range_mut(frames).clear();
            return;
        }
        let cycles_per_frame = self.frequency / self.sample_rate;
        for i in frames {
            let position = ctx.transport.position + i as u64;
            let phase = (position as f64 * cycles_per_frame).fract();
            let sample = (phase * TAU).sin() as f32 * self.level.next_value();
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
        self.level.prepare(config.sample_rate);
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<Sine>() {
            self.level.take_over(&previous.level);
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

/// Plays the notes sent to it with a plain tone of its own, added to what
/// comes into it: a test instrument, standing in for instruments until
/// there are plugins.
///
/// Each note is a sine at the note's pitch, bent by its pitch bend, with
/// its second harmonic blended in by its timbre, and louder with velocity
/// and pressure: at full velocity and no pressure it peaks at the
/// instrument's level, and pressure can double that. A note rises over
/// 2 ms and rings out over 30 ms once let go, transport or not, and while
/// the sustain pedal is down, notes let go sound on until it's let go. It
/// plays [`VOICES`](Self::VOICES) notes at once, and a note beyond them
/// takes the place of the one that started longest ago.
#[derive(Debug, Clone)]
pub struct TestInstrument {
    level: Smoothed,
    voices: [Voice; TestInstrument::VOICES],
    /// Whether the sustain pedal is down.
    sustain: bool,
    /// Notes started so far, to tell which voice is oldest.
    started: u64,
    channels: usize,
    sample_rate: f64,
    /// How far a voice's envelope moves in a frame, rising and ringing out.
    attack: f32,
    release: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    active: bool,
    released: bool,
    /// Let go while the sustain pedal was down, so still sounding.
    held: bool,
    note: NoteId,
    /// Which note it was, counting from the first.
    started: u64,
    key: u8,
    velocity: f32,
    bend: f32,
    pressure: f32,
    timbre: f32,
    /// In cycles, from 0 to 1.
    phase: f64,
    /// Cycles per frame.
    increment: f64,
    envelope: f32,
}

impl TestInstrument {
    /// Linear amplitude of a note at full velocity. Changes ramp
    /// ([`Smoothed`]).
    pub const LEVEL: ParamId = ParamId(0);
    /// Notes it plays at once.
    pub const VOICES: usize = 32;

    const ATTACK_SECONDS: f64 = 0.002;
    const RELEASE_SECONDS: f64 = 0.03;

    /// Stereo, the same signal in both channels.
    pub fn new(level: f32) -> Self {
        Self {
            level: Smoothed::new(level),
            voices: [Voice::default(); Self::VOICES],
            sustain: false,
            started: 0,
            channels: 2,
            sample_rate: 48_000.0,
            attack: 1.0,
            release: 1.0,
        }
    }

    pub fn with_channels(mut self, channels: usize) -> Self {
        self.channels = channels;
        self
    }

    /// The notes sounding or ringing out.
    pub fn sounding(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    fn apply(&mut self, event: &NodeEvent) {
        match event.kind {
            NodeEventKind::Param {
                param: Self::LEVEL,
                value,
            } => self.level.set(value as f32),
            NodeEventKind::Note(NoteEvent::On {
                note,
                key,
                velocity,
            }) => {
                let voice = match self.voices.iter().position(|v| !v.active) {
                    Some(free) => free,
                    None => (0..Self::VOICES)
                        .min_by_key(|&v| self.voices[v].started)
                        .expect("an instrument has voices"),
                };
                self.started += 1;
                self.voices[voice] = Voice {
                    active: true,
                    released: false,
                    note,
                    started: self.started,
                    key,
                    velocity,
                    ..Voice::default()
                };
                self.tune(voice);
            }
            NodeEventKind::Note(NoteEvent::Expression {
                note,
                dimension,
                value,
            }) => {
                for v in 0..Self::VOICES {
                    let voice = &mut self.voices[v];
                    if !voice.active || voice.note != note {
                        continue;
                    }
                    match dimension {
                        Dimension::PitchBend => {
                            voice.bend = value;
                            self.tune(v);
                        }
                        Dimension::Pressure => voice.pressure = value,
                        Dimension::Timbre => voice.timbre = value,
                    }
                }
            }
            NodeEventKind::Note(NoteEvent::Off { note, .. }) => {
                for voice in &mut self.voices {
                    if voice.active && voice.note == note {
                        voice.held = self.sustain;
                        voice.released = !self.sustain;
                    }
                }
            }
            NodeEventKind::Controller {
                controller: Controller::SUSTAIN,
                value,
            } => {
                self.sustain = Controller::is_on(value);
                if !self.sustain {
                    for voice in self.voices.iter_mut().filter(|v| v.held) {
                        voice.held = false;
                        voice.released = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// Sets a voice's frequency from its key and pitch bend.
    fn tune(&mut self, voice: usize) {
        let voice = &mut self.voices[voice];
        let semitones = f64::from(voice.key) + f64::from(voice.bend) - 69.0;
        voice.increment = 440.0 * (semitones / 12.0).exp2() / self.sample_rate;
    }

    fn render(&mut self, output: &mut AudioMut<'_>, frames: Range<usize>) {
        let len = frames.len();
        if !self.voices.iter().any(|v| v.active) {
            self.level.skip(len);
            return;
        }
        for voice in self.voices.iter_mut().filter(|v| v.active) {
            // Each voice ramps through the same levels.
            let mut level = self.level;
            for i in frames.clone() {
                let level = level.next_value();
                if voice.released {
                    voice.envelope -= self.release;
                    if voice.envelope <= 0.0 {
                        voice.active = false;
                        break;
                    }
                } else if voice.envelope < 1.0 {
                    voice.envelope = (voice.envelope + self.attack).min(1.0);
                }
                let phase = voice.phase * TAU;
                let timbre = f64::from(voice.timbre);
                let tone = phase.sin() * (1.0 - 0.5 * timbre) + (2.0 * phase).sin() * 0.5 * timbre;
                let loudness = voice.velocity * (1.0 + voice.pressure) * voice.envelope * level;
                let sample = tone as f32 * loudness;
                voice.phase = (voice.phase + voice.increment).fract();
                for channel in output.channels_mut() {
                    channel[i] += sample;
                }
            }
        }
        self.level.skip(len);
    }
}

impl Node for TestInstrument {
    fn channels(&self) -> usize {
        self.channels
    }

    fn prepare(&mut self, config: &EngineConfig) {
        self.sample_rate = f64::from(config.sample_rate);
        self.level.prepare(config.sample_rate);
        self.attack = (1.0 / (Self::ATTACK_SECONDS * self.sample_rate)) as f32;
        self.release = (1.0 / (Self::RELEASE_SECONDS * self.sample_rate)) as f32;
    }

    fn adopt(&mut self, previous: &mut dyn Node) {
        if let Some(previous) = previous.downcast_mut::<TestInstrument>() {
            self.level.take_over(&previous.level);
            self.voices = previous.voices;
            self.sustain = previous.sustain;
            self.started = previous.started;
        }
    }

    fn flush(&mut self, ctx: &ProcessContext<'_>) {
        for event in ctx.events {
            if let NodeEventKind::Param { .. } = event.kind {
                self.apply(event);
            }
        }
    }

    fn reset(&mut self) {
        self.level.reset();
        self.voices = [Voice::default(); Self::VOICES];
        self.sustain = false;
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, output: &mut AudioMut<'_>) {
        output.clear();
        for input in inputs.iter() {
            output.add_from(input);
        }
        let mut start = 0;
        for event in ctx.events {
            self.render(output, start..event.offset);
            self.apply(event);
            start = event.offset;
        }
        self.render(output, start..ctx.frames);
    }
}

#[cfg(test)]
mod tests {
    use musicable_model::time::{TempoMap, TimeSignature};

    use super::*;
    use crate::{AudioBuffer, InputPort, TransportState, rt};

    const CONFIG: EngineConfig = EngineConfig {
        sample_rate: 48_000,
        max_block: 1024,
    };

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
            input: AudioRef::empty(frames),
            midi: &[],
        }
    }

    /// Runs `monitor` over a block of `frames`, with the transport
    /// `transport`, on clips playing 0.5, with device inputs of 0.1, 0.2
    /// and 0.3.
    fn monitored(
        monitor: &mut InputMonitor,
        frames: usize,
        transport: TransportState,
        events: &[NodeEvent],
    ) -> [Vec<f32>; 2] {
        let input: Vec<f32> = [0.1, 0.2, 0.3]
            .into_iter()
            .flat_map(|level| std::iter::repeat_n(level, frames))
            .collect();
        let clips = vec![0.5; 2 * frames];
        let tempo = TempoMap::default();
        let ctx = ProcessContext {
            transport,
            input: AudioRef::from_planar(&input, 3),
            midi: &[],
            ..context(frames, events, transport.playing, &tempo)
        };
        let views = [AudioRef::from_planar(&clips, 2)];
        let mut out = vec![0.0; 2 * frames];
        let mut output = AudioMut::from_planar(&mut out, 2);
        monitor.process(&ctx, Inputs::from_views(&views), &mut output);
        let (left, right) = out.split_at(frames);
        [left.to_vec(), right.to_vec()]
    }

    fn param(offset: usize, param: ParamId, value: f64) -> NodeEvent {
        NodeEvent {
            offset,
            kind: NodeEventKind::Param { param, value },
        }
    }

    /// Runs `delay` over the counting numbers from `from`, `block` frames at a
    /// time, for `frames` frames, in stereo with the right channel negated,
    /// and returns the left channel.
    fn delayed(delay: &mut Delay, from: usize, frames: usize, block: usize) -> Vec<f32> {
        let mut left = Vec::new();
        for start in (from..from + frames).step_by(block) {
            let n = block.min(from + frames - start);
            let mut input: Vec<f32> = (start..start + n).map(|i| i as f32).collect();
            input.extend((start..start + n).map(|i| -(i as f32)));
            let inputs = [AudioRef::from_planar(&input, 2)];
            let mut out = AudioBuffer::new(2, n);
            let tempo = TempoMap::default();
            let ctx = context(n, &[], true, &tempo);
            let mut view = out.view_mut(n);
            rt::scope(|| delay.process(&ctx, Inputs::from_views(&inputs), &mut view));
            let out = out.view(n);
            let right: Vec<f32> = out.channel(1).iter().map(|s| -s).collect();
            assert_eq!(out.channel(0), right, "the channels are delayed alike");
            left.extend_from_slice(out.channel(0));
        }
        left
    }

    /// The counting numbers from `from`, `by` frames late, with silence
    /// before frame `silent_until`.
    fn late(from: usize, frames: usize, by: usize, silent_until: usize) -> Vec<f32> {
        let sample = |i: usize| match i.checked_sub(by) {
            Some(source) if i >= silent_until => source as f32,
            _ => 0.0,
        };
        (from..from + frames).map(sample).collect()
    }

    #[test]
    fn input_monitors_play_the_input_as_the_tracks_monitoring_says() {
        let stopped = TransportState::default();
        let playing = TransportState {
            playing: true,
            ..stopped
        };
        let recording = TransportState {
            record: true,
            recording: true,
            ..playing
        };
        let cases = [
            (Monitor::In, false, playing, true),
            (Monitor::Auto, true, stopped, true),
            (Monitor::Auto, true, playing, false),
            (Monitor::Auto, true, recording, true),
            (Monitor::Auto, false, stopped, false),
            (Monitor::Off, true, recording, false),
        ];
        for (monitor, armed, transport, monitors) in cases {
            let mut node = InputMonitor::new(monitor, armed).with_input(1, 2);
            node.prepare(&CONFIG);
            let [left, right] = monitored(&mut node, 64, transport, &[]);
            let (l, r) = if monitors { (0.2, 0.3) } else { (0.5, 0.5) };
            assert!(
                left.iter().all(|&s| s == l) && right.iter().all(|&s| s == r),
                "{monitor:?}, armed {armed}, {transport:?}"
            );
        }
        // A mono input plays in both channels, and one the stream didn't
        // open is silent, as is a track without an input.
        let mut mono = InputMonitor::new(Monitor::In, false).with_input(2, 1);
        assert_eq!(
            monitored(&mut mono, 4, stopped, &[]),
            [vec![0.3; 4], vec![0.3; 4]]
        );
        for mut silent in [
            InputMonitor::new(Monitor::In, false).with_input(2, 2),
            InputMonitor::new(Monitor::In, false),
        ] {
            assert_eq!(
                monitored(&mut silent, 4, stopped, &[]),
                [vec![0.0; 4], vec![0.0; 4]]
            );
        }
    }

    #[test]
    fn input_monitors_play_another_tracks_tap_and_never_the_master_they_resample() {
        let stopped = TransportState::default();
        let playing = TransportState {
            playing: true,
            ..stopped
        };
        let recording = TransportState {
            record: true,
            recording: true,
            ..playing
        };
        let tempo = TempoMap::default();
        // On clips playing 0.5, with a tap of 0.25 and -0.25 into side
        // input 0 if `tapped`.
        let run = |node: &mut InputMonitor, transport: TransportState, tapped: bool| {
            let clips = vec![0.5; 8];
            let tap = [[0.25; 4], [-0.25; 4]].concat();
            let views = [
                AudioRef::from_planar(&clips, 2),
                AudioRef::from_planar(&tap, 2),
            ];
            let ports = [InputPort::Main, InputPort::Side(0)];
            let inputs = match tapped {
                true => Inputs::from_views_on(&views, &ports),
                false => Inputs::from_views(&views[..1]),
            };
            let ctx = ProcessContext {
                transport,
                ..context(4, &[], transport.playing, &tempo)
            };
            let mut out = vec![0.0; 8];
            node.process(&ctx, inputs, &mut AudioMut::from_planar(&mut out, 2));
            out
        };
        let mut tap = InputMonitor::new(Monitor::In, false).with_side_input();
        tap.prepare(&CONFIG);
        let tapped = [[0.25; 4], [-0.25; 4]].concat();
        assert_eq!(run(&mut tap, playing, true), tapped);
        assert_eq!(run(&mut tap, playing, false), [0.0; 8], "nothing tapped");
        // Resampling plays the clips whatever the monitoring, and nothing
        // while it records.
        let cases = [
            (Monitor::In, false, playing, true),
            (Monitor::In, true, stopped, true),
            (Monitor::Auto, true, stopped, true),
            (Monitor::In, false, recording, true),
            (Monitor::Off, true, recording, false),
            (Monitor::In, true, recording, false),
        ];
        for (monitor, armed, transport, plays) in cases {
            let mut node = InputMonitor::new(monitor, armed).resampling();
            node.prepare(&CONFIG);
            let expected = if plays { [0.5; 8] } else { [0.0; 8] };
            assert_eq!(
                run(&mut node, transport, true),
                expected,
                "{monitor:?}, armed {armed}, {transport:?}"
            );
        }
        // Out on the frame it starts recording, and back in on a ramp.
        let mut node = InputMonitor::new(Monitor::Auto, true).resampling();
        node.prepare(&CONFIG);
        assert_eq!(run(&mut node, playing, true), [0.5; 8]);
        assert_eq!(run(&mut node, recording, true), [0.0; 8]);
        let back = run(&mut node, playing, true);
        assert!(back.iter().all(|&s| s > 0.0 && s < 0.1), "{back:?}");
    }

    #[test]
    fn resampling_cuts_follow_their_monitors_arming() {
        let tempo = TempoMap::default();
        let recording = TransportState {
            playing: true,
            record: true,
            recording: true,
            ..TransportState::default()
        };
        let mut monitor = InputMonitor::new(Monitor::Auto, true).resampling();
        let mut cut = monitor.cut();
        monitor.prepare(&CONFIG);
        cut.prepare(&CONFIG);
        // On clips playing 0.5, through devices ringing on at 0.25.
        let mut run = |events: &[NodeEvent]| {
            let ctx = ProcessContext {
                transport: recording,
                ..context(4, events, true, &tempo)
            };
            let clips = vec![0.5; 8];
            let mut monitored = vec![0.0; 8];
            let inputs = [AudioRef::from_planar(&clips, 2)];
            let output = &mut AudioMut::from_planar(&mut monitored, 2);
            monitor.process(&ctx, Inputs::from_views(&inputs), output);
            let ringing: Vec<f32> = monitored.iter().map(|s| s + 0.25).collect();
            let ctx = ProcessContext { events: &[], ..ctx };
            let mut out = vec![0.0; 8];
            let inputs = [AudioRef::from_planar(&ringing, 2)];
            cut.process(
                &ctx,
                Inputs::from_views(&inputs),
                &mut AudioMut::from_planar(&mut out, 2),
            );
            out
        };
        assert_eq!(run(&[]), [0.0; 8]);
        // Disarmed, both come back on their ramps.
        let back = run(&[param(0, InputMonitor::ARMED, 0.0)]);
        assert!(back.iter().all(|&s| s > 0.0 && s < 0.1), "{back:?}");
    }

    #[test]
    fn input_monitors_switch_over_without_a_click() {
        let mut node = InputMonitor::new(Monitor::Auto, true).with_input(0, 1);
        node.prepare(&CONFIG);
        let stopped = TransportState::default();
        // Monitoring from the first frame: nothing to ramp from.
        let [left, _] = monitored(&mut node, 16, stopped, &[]);
        assert_eq!(left, [0.1; 16]);
        // Disarmed 8 frames in: 10 ms back to the clips.
        let disarm = [param(8, InputMonitor::ARMED, 0.0)];
        let [left, _] = monitored(&mut node, 1000, stopped, &disarm);
        assert_eq!(left[..8], [0.1; 8]);
        assert!(left[8..488].windows(2).all(|w| w[0] < w[1]));
        assert!(left[487..].iter().all(|&s| s == 0.5));
        // Monitoring always, from the next block.
        let value = InputMonitor::monitor_value(Monitor::In);
        let always = [param(0, InputMonitor::MONITOR, value)];
        let [left, _] = monitored(&mut node, 1000, stopped, &always);
        assert!(left[..480].windows(2).all(|w| w[0] > w[1]));
        assert!(left[479..].iter().all(|&s| s == 0.1));
    }

    #[test]
    fn delays_play_what_comes_in_late_whatever_the_blocks() {
        for frames in [0, 1, 5, 16, 37] {
            for block in [1, 3, 16, 64] {
                let mut delay = Delay::new(frames);
                delay.prepare(&SLOW);
                assert_eq!(
                    delayed(&mut delay, 0, 200, block),
                    late(0, 200, frames, frames),
                    "{frames} frames in blocks of {block}"
                );
                assert_eq!(delay.latency(), 0, "a delay isn't latency");
            }
        }
    }

    #[test]
    fn delays_take_over_what_the_delay_before_them_holds() {
        // Swapped 50 frames in, for a delay as long, shorter and longer.
        for frames in [20, 7, 33] {
            let mut first = Delay::new(20);
            first.prepare(&SLOW);
            delayed(&mut first, 0, 50, 6);
            let mut second = Delay::new(frames);
            second.prepare(&SLOW);
            rt::scope(|| second.adopt(&mut first));
            // A longer delay is silent at first, where it goes back further
            // than the first one did.
            let silent_until = 50 + frames.saturating_sub(20);
            assert_eq!(
                delayed(&mut second, 50, 60, 6),
                late(50, 60, frames, silent_until),
                "{frames}"
            );
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

    /// 10 frames to a ramp.
    const SLOW: EngineConfig = EngineConfig {
        sample_rate: 1000,
        max_block: 64,
    };

    /// Runs a mono gain stage over ones, `frames` at a time, with `events`
    /// in the first block.
    fn gain_out(gain: &mut Gain, frames: usize, events: &[NodeEvent]) -> Vec<f32> {
        let ones = vec![1.0; frames];
        let inputs = [AudioRef::from_planar(&ones, 1)];
        let mut out = AudioBuffer::new(1, frames);
        gain.process(
            &context(frames, events, false, &TempoMap::default()),
            Inputs::from_views(&inputs),
            &mut out.view_mut(frames),
        );
        out.view(frames).channel(0).to_vec()
    }

    fn assert_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (a - e).abs() < 1e-6,
                "frame {i}: {actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn gain_ramps_to_new_values() {
        let mut gain = Gain::new(1.0).with_channels(1);
        gain.prepare(&SLOW);
        assert_eq!(gain_out(&mut gain, 4, &[]), [1.0; 4]);
        let out = gain_out(&mut gain, 14, &[param(2, Gain::GAIN, 0.0)]);
        let ramp = [
            1.0, 1.0, 0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2, 0.1, 0.0, 0.0, 0.0,
        ];
        assert_close(&out, &ramp);
        // It lands exactly, and stays there.
        assert_eq!(out[11..], [0.0; 3]);

        // Muting ramps down, and unmuting back up to the gain.
        let mut gain = Gain::new(0.5).with_channels(1);
        gain.prepare(&SLOW);
        gain_out(&mut gain, 1, &[]);
        let out = gain_out(&mut gain, 10, &[param(0, Gain::MUTE, 1.0)]);
        assert_close(&out[..2], &[0.45, 0.4]);
        assert_eq!(out[9], 0.0);
        let out = gain_out(&mut gain, 10, &[param(0, Gain::MUTE, 0.0)]);
        assert_close(&out[..2], &[0.05, 0.1]);
        assert_eq!(out[9], 0.5);
    }

    #[test]
    fn gain_ramps_the_same_however_the_blocks_fall() {
        let ramp = |sizes: &[usize]| {
            let mut gain = Gain::new(1.0).with_channels(1);
            gain.prepare(&SLOW);
            gain_out(&mut gain, 3, &[]);
            let mut out = Vec::new();
            for (i, &size) in sizes.iter().enumerate() {
                let events = if i == 0 {
                    vec![param(1, Gain::GAIN, 0.25), param(4, Gain::GAIN, 2.0)]
                } else {
                    vec![]
                };
                out.extend(gain_out(&mut gain, size, &events));
            }
            out
        };
        let whole = ramp(&[30]);
        assert_eq!(ramp(&[5, 1, 1, 7, 16]), whole);
        assert_eq!(ramp(&[5, 25]), whole);
        assert_eq!(whole[29], 2.0);
    }

    #[test]
    fn a_gain_jumps_until_it_has_made_a_sound() {
        let mut gain = Gain::new(1.0).with_channels(1);
        gain.prepare(&SLOW);
        let out = gain_out(&mut gain, 4, &[param(0, Gain::GAIN, 0.5)]);
        assert_eq!(out, [0.5; 4]);
    }

    #[test]
    fn a_new_gain_carries_on_where_the_old_one_was() {
        let mut old = Gain::new(1.0).with_channels(1);
        old.prepare(&SLOW);
        gain_out(&mut old, 1, &[]);
        let start = gain_out(&mut old, 4, &[param(0, Gain::GAIN, 0.0)]);
        assert_close(&start, &[0.9, 0.8, 0.7, 0.6]);

        // Compiled with the new value: the ramp carries on.
        let mut new = Gain::new(0.0).with_channels(1);
        new.prepare(&SLOW);
        new.adopt(&mut old);
        assert_close(&gain_out(&mut new, 2, &[]), &[0.5, 0.4]);

        // Compiled before the change: it ramps back from where the old one
        // was, until the new value comes again.
        let mut stale = Gain::new(1.0).with_channels(1);
        stale.prepare(&SLOW);
        stale.adopt(&mut new);
        let out = gain_out(&mut stale, 1, &[]);
        assert!(out[0] > 0.4 && out[0] < 1.0, "{out:?}");
    }

    #[test]
    fn smoothed_values_keep_their_ramp_for_the_same_target() {
        let mut level = Smoothed::new(0.0);
        level.prepare(1000);
        level.next_value();
        level.set(1.0);
        assert!(!level.is_settled());
        assert!((level.next_value() - 0.1).abs() < 1e-6);
        level.set(1.0);
        assert!((level.next_value() - 0.2).abs() < 1e-6);
        level.skip(100);
        assert!(level.is_settled());
        assert_eq!(level.value(), 1.0);
        level.set(0.5);
        level.settle();
        assert_eq!((level.value(), level.next_value()), (0.5, 0.5));
        // Unprepared, it jumps.
        let mut instant = Smoothed::new(1.0);
        instant.next_value();
        instant.set(0.0);
        assert!(instant.is_settled());
        assert_eq!(instant.next_value(), 0.0);
    }

    #[test]
    fn sine_levels_ramp_while_playing() {
        let tempo = TempoMap::default();
        let mut sine = Sine::new(250.0, 1.0).with_channels(1);
        sine.prepare(&SLOW);
        let mut out = AudioBuffer::new(1, 8);
        // 250 Hz at 1 kHz: 0, 1, 0, -1, ...
        let mut play = |sine: &mut Sine, events: &[NodeEvent], playing: bool| {
            sine.process(
                &context(8, events, playing, &tempo),
                Inputs::NONE,
                &mut out.view_mut(8),
            );
            out.view(8).channel(0).to_vec()
        };
        let first = play(&mut sine, &[], true);
        assert!((first[1] - 1.0).abs() < 1e-6);
        let ramped = play(&mut sine, &[param(0, Sine::LEVEL, 0.0)], true);
        assert!((ramped[1] - 0.8).abs() < 1e-6, "{ramped:?}");
        // While stopped, a new level applies at once.
        play(&mut sine, &[param(0, Sine::LEVEL, 0.5)], false);
        let after = play(&mut sine, &[], true);
        assert!((after[1] - 0.5).abs() < 1e-6, "{after:?}");
    }

    /// Runs `node` over one block of a stereo input, `left` and `right`.
    fn stereo(node: &mut dyn Node, left: &[f32], right: &[f32], events: &[NodeEvent]) -> Vec<f32> {
        let frames = left.len();
        let input = [left, right].concat();
        let inputs = [AudioRef::from_planar(&input, 2)];
        let mut out = AudioBuffer::new(2, frames);
        node.process(
            &context(frames, events, false, &TempoMap::default()),
            Inputs::from_views(&inputs),
            &mut out.view_mut(frames),
        );
        let view = out.view(frames);
        [view.channel(0), view.channel(1)].concat()
    }

    #[test]
    fn the_balance_turns_one_channel_down() {
        let ones = [1.0; 4];
        let events = [param(1, Gain::PAN, -0.5), param(2, Gain::PAN, 1.0)];
        let out = stereo(&mut Gain::new(0.5).with_pan(0.25), &ones, &ones, &events);
        assert_eq!(out, [0.375, 0.5, 0.0, 0.0, 0.5, 0.25, 0.5, 0.5]);
        // Mono stages have no balance.
        let mono = [1.0; 2];
        let inputs = [AudioRef::from_planar(&mono, 1)];
        let mut out = AudioBuffer::new(1, 2);
        Gain::new(0.5).with_pan(1.0).with_channels(1).process(
            &context(2, &[], false, &TempoMap::default()),
            Inputs::from_views(&inputs),
            &mut out.view_mut(2),
        );
        assert_eq!(out.view(2).channel(0), [0.5, 0.5]);
    }

    #[test]
    fn a_centred_gain_stage_only_multiplies_by_the_gain() {
        let left = [0.1, -0.3, f32::MIN_POSITIVE, 1e30];
        let right = [0.7, 0.0, -1.0, f32::from_bits(1)];
        let gain = 0.8;
        let out = stereo(&mut Gain::new(gain), &left, &right, &[]);
        let expected: Vec<f32> = left.iter().chain(&right).map(|s| s * gain).collect();
        assert!(out == expected);
    }

    #[test]
    fn the_balance_ramps_too() {
        let mut gain = Gain::new(1.0);
        gain.prepare(&SLOW);
        let ones = [1.0; 12];
        stereo(&mut gain, &ones[..1], &ones[..1], &[]);
        let out = stereo(&mut gain, &ones, &ones, &[param(0, Gain::PAN, 1.0)]);
        let (left, right) = out.split_at(12);
        let ramp = [0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 0.2, 0.1, 0.0, 0.0, 0.0];
        assert_close(left, &ramp);
        assert_eq!(right, [1.0; 12]);
        // A new stage carries on from where the old one's balance was.
        let mut old = Gain::new(1.0);
        old.prepare(&SLOW);
        stereo(
            &mut old,
            &ones[..1],
            &ones[..1],
            &[param(0, Gain::PAN, -1.0)],
        );
        stereo(
            &mut old,
            &ones[..4],
            &ones[..4],
            &[param(0, Gain::PAN, 0.0)],
        );
        let mut new = Gain::new(1.0);
        new.prepare(&SLOW);
        new.adopt(&mut old);
        let out = stereo(&mut new, &ones[..2], &ones[..2], &[]);
        assert_close(&out, &[1.0, 1.0, 0.5, 0.6]);
    }

    #[test]
    fn a_gain_stage_meters_what_it_outputs() {
        let meter = Arc::new(Meter::new());
        let mut gain = Gain::new(0.5).with_meter(Arc::clone(&meter));
        stereo(&mut gain, &[1.0, -0.5], &[0.25, 0.0], &[]);
        assert_eq!(meter.level().peak, [0.5, 0.125]);
    }

    #[test]
    fn clip_types_shape_samples() {
        let samples = [0.0, 0.25, 0.5, 0.75, 1.0, 2.0, 1e30, -0.75, -4.0];
        let hard = stereo(&mut Clipper::new(ClipType::Hard), &samples, &samples, &[]);
        assert_eq!(
            hard[..samples.len()],
            [0.0, 0.25, 0.5, 0.75, 1.0, 1.0, 1.0, -0.75, -1.0]
        );
        let soft = stereo(&mut Clipper::new(ClipType::Soft), &samples, &samples, &[]);
        let expected = [
            0.0,
            0.25,
            0.5,
            1.0 - 0.25 / 0.75,
            0.75,
            0.875,
            1.0,
            -(1.0 - 0.25 / 0.75),
            -0.9375,
        ];
        assert_eq!(soft[..samples.len()], expected);
        let off = stereo(&mut Clipper::new(ClipType::Off), &samples, &samples, &[]);
        assert_eq!(off[..samples.len()], samples);
        // Soft clipping stays below full scale for any sample a mix is
        // likely to hold, and never boosts.
        for sample in [0.6_f32, 0.9, 10.0, 1e6] {
            let clipped = Clipper::clip(ClipType::Soft, sample);
            assert!(clipped < 1.0 && clipped <= sample, "{sample}");
        }
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
                    ..TransportState::default()
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

    fn note(offset: usize, note: NoteEvent) -> NodeEvent {
        NodeEvent {
            offset,
            kind: NodeEventKind::Note(note),
        }
    }

    fn on(offset: usize, id: u64, key: u8, velocity: f32) -> NodeEvent {
        let note_id = NoteId(id);
        note(
            offset,
            NoteEvent::On {
                note: note_id,
                key,
                velocity,
            },
        )
    }

    fn off(offset: usize, id: u64) -> NodeEvent {
        note(
            offset,
            NoteEvent::Off {
                note: NoteId(id),
                velocity: 0.5,
            },
        )
    }

    fn expression(offset: usize, id: u64, dimension: Dimension, value: f32) -> NodeEvent {
        note(
            offset,
            NoteEvent::Expression {
                note: NoteId(id),
                dimension,
                value,
            },
        )
    }

    /// Renders `frames` mono frames of `instrument`, in blocks of `block`,
    /// with `events` at their offsets from the first frame.
    fn play(
        instrument: &mut TestInstrument,
        frames: usize,
        block: usize,
        events: &[NodeEvent],
    ) -> Vec<f32> {
        let tempo = TempoMap::default();
        let mut buffer = AudioBuffer::new(1, block);
        let mut out = Vec::new();
        while out.len() < frames {
            let (start, n) = (out.len(), block.min(frames - out.len()));
            let events: Vec<NodeEvent> = events
                .iter()
                .filter(|e| (start..start + n).contains(&e.offset))
                .map(|&e| NodeEvent {
                    offset: e.offset - start,
                    ..e
                })
                .collect();
            let ctx = context(n, &events, true, &tempo);
            instrument.process(&ctx, Inputs::NONE, &mut buffer.view_mut(n));
            out.extend_from_slice(buffer.view(n).channel(0));
        }
        out
    }

    fn instrument() -> TestInstrument {
        let mut instrument = TestInstrument::new(0.5).with_channels(1);
        instrument.prepare(&EngineConfig {
            sample_rate: 48_000,
            max_block: 4800,
        });
        instrument
    }

    /// Times the signal crosses zero going up.
    fn cycles(signal: &[f32]) -> usize {
        signal
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count()
    }

    fn peak(signal: &[f32]) -> f32 {
        signal.iter().fold(0.0, |m, s| m.max(s.abs()))
    }

    #[test]
    fn the_test_instrument_plays_notes_at_their_pitch_and_velocity() {
        let mut keys = instrument();
        // The A at 440 Hz, and an octave above it, a tenth of a second each.
        let a = play(&mut keys, 4800, 4800, &[on(0, 1, 69, 1.0)]);
        // 44 cycles, the first starting on the first frame.
        assert_eq!(cycles(&a), 43);
        assert!((peak(&a) - 0.5).abs() < 1e-3, "{}", peak(&a));
        let mut keys = instrument();
        let high = play(&mut keys, 4800, 4800, &[on(0, 1, 81, 0.5)]);
        assert_eq!(cycles(&high), 87);
        assert!((peak(&high) - 0.25).abs() < 1e-3);
        // It rises from silence.
        assert_eq!(a[0], 0.0);
    }

    #[test]
    fn notes_ring_out_once_let_go() {
        let mut keys = instrument();
        let out = play(&mut keys, 4800, 64, &[on(0, 1, 60, 1.0), off(1000, 1)]);
        // 30 ms at 48 kHz.
        assert!(out[1000..2439].iter().any(|&s| s != 0.0));
        assert!(out[2440..].iter().all(|&s| s == 0.0));
        assert_eq!(keys.sounding(), 0);
        // A note it doesn't know changes nothing.
        let mut keys = instrument();
        let out = play(&mut keys, 1000, 64, &[on(0, 1, 60, 1.0), off(10, 2)]);
        assert!(out[900..].iter().any(|&s| s != 0.0));
    }

    #[test]
    fn the_sustain_pedal_holds_notes_let_go_until_it_comes_up() {
        let pedal = |offset, down: bool| NodeEvent {
            offset,
            kind: NodeEventKind::Controller {
                controller: Controller::SUSTAIN,
                value: if down { 1.0 } else { 0.0 },
            },
        };
        let mut keys = instrument();
        let events = [
            pedal(0, true),
            on(0, 1, 60, 1.0),
            off(1000, 1),
            // Other controllers don't hold notes.
            NodeEvent {
                offset: 2000,
                kind: NodeEventKind::Controller {
                    controller: Controller::MOD_WHEEL,
                    value: 0.0,
                },
            },
            pedal(3000, false),
        ];
        let out = play(&mut keys, 6000, 64, &events);
        assert!(out[2900..3000].iter().any(|&s| s != 0.0), "held");
        // Then it rings out over 30 ms.
        assert!(out[4440..].iter().all(|&s| s == 0.0));
        assert_eq!(keys.sounding(), 0);
        // Notes let go with the pedal up ring out at once.
        let mut keys = instrument();
        let events = [pedal(0, false), on(0, 1, 60, 1.0), off(1000, 1)];
        let out = play(&mut keys, 3000, 64, &events);
        assert!(out[2440..].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn expression_bends_swells_and_brightens_a_note() {
        let mut keys = instrument();
        let events = [
            on(0, 7, 69, 1.0),
            expression(0, 7, Dimension::PitchBend, 12.0),
        ];
        assert_eq!(cycles(&play(&mut keys, 4800, 4800, &events)), 87);
        let mut keys = instrument();
        let events = [
            on(0, 7, 69, 1.0),
            expression(0, 7, Dimension::Pressure, 1.0),
        ];
        assert!((peak(&play(&mut keys, 4800, 4800, &events)) - 1.0).abs() < 1e-3);
        // Timbre blends in the second harmonic, and leaves the other notes
        // alone.
        let mut keys = instrument();
        let plain = play(&mut keys, 4800, 4800, &[on(0, 1, 69, 1.0)]);
        let mut keys = instrument();
        let events = [
            on(0, 1, 69, 1.0),
            on(0, 2, 40, 0.0),
            expression(0, 2, Dimension::Timbre, 1.0),
        ];
        assert_eq!(play(&mut keys, 4800, 4800, &events), plain);
        let mut keys = instrument();
        let events = [on(0, 1, 69, 1.0), expression(0, 1, Dimension::Timbre, 1.0)];
        assert_ne!(play(&mut keys, 4800, 4800, &events), plain);
    }

    #[test]
    fn notes_play_the_same_however_the_blocks_fall() {
        let events = [
            on(0, 1, 60, 0.8),
            expression(100, 1, Dimension::PitchBend, -0.5),
            on(200, 2, 64, 0.6),
            off(3000, 1),
        ];
        let whole = play(&mut instrument(), 6000, 4800, &events);
        let mut keys = instrument();
        let mut parts = play(&mut keys, 300, 300, &events);
        // The rest, from a new instrument taking over.
        let mut next = instrument();
        next.adopt(&mut keys);
        let rest = [off(2700, 1)];
        parts.extend(play(&mut next, 5700, 64, &rest));
        assert!(parts == whole);
    }

    #[test]
    fn a_note_beyond_the_voices_takes_the_oldest_ones_place() {
        let mut keys = instrument();
        let events: Vec<NodeEvent> = (0..=TestInstrument::VOICES as u64)
            .map(|id| on(id as usize, id, 60, 1.0))
            .collect();
        play(&mut keys, 100, 100, &events);
        assert_eq!(keys.sounding(), TestInstrument::VOICES);
        // The first note's voice went to the last, so letting it go does
        // nothing.
        play(&mut keys, 2000, 100, &[off(0, 0)]);
        assert_eq!(keys.sounding(), TestInstrument::VOICES);
        play(&mut keys, 2000, 100, &[off(0, 1)]);
        assert_eq!(keys.sounding(), TestInstrument::VOICES - 1);
    }
}
