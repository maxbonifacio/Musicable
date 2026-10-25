//! Parameters: what automation points at, and modulation, MIDI mapping and
//! extensions will. A [`ParamAddress`] names one anywhere in the project,
//! and a [`ParamSpec`] says what it is: its name, its range, its default
//! and its [`Scale`].

use crate::clip::{ClipContent, ClipId};
use crate::native::ParamInfo;
use crate::warp::{Preserve, SliceSettings, SpectralSettings, TransientLoop, Warp, WarpMode};
use crate::{Chain, DeviceId, DeviceKind, Project, SendId};

/// A parameter anywhere in the project: one address that automation,
/// modulation, MIDI mapping and extensions all use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ParamAddress {
    /// A channel's fader, as linear gain: 1.0 is unity.
    Volume(Chain),
    /// A channel's balance, from -1.0 (left) to 1.0 (right).
    Pan(Chain),
    /// A send's level, as linear gain.
    Send(SendId),
    /// Parameter `n` of a device: a plugin's own id for it, or the number
    /// of one of Musicable's own devices' parameters, never reused. The
    /// device's node has it as `ParamId(n)`.
    Device(DeviceId, u32),
    /// One of an audio clip's own parameters. Its curve is in the clip, in
    /// the clip's own time, rather than in the project's lanes, so it
    /// moves, copies and splits with the clip.
    Clip(ClipId, ClipParam),
}

/// One of an audio clip's own parameters. All but Gain belong to its warp,
/// and play while it's warped in a mode that has them (see
/// [`plays_in`](Self::plays_in)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClipParam {
    /// The clip's gain, as linear gain: 1.0 is unity.
    Gain,
    /// The warp's transpose, in semitones.
    Transpose,
    /// The warp's detune, in cents.
    Detune,
    /// Slice's Transient Envelope, from 0 to 100.
    TransientEnvelope,
    /// Slice's Preserve, in steps: the place of a
    /// [`Preserve`](crate::warp::Preserve) in its list.
    Preserve,
    /// Slice's Transient Loop Mode, in steps: the place of a
    /// [`TransientLoop`](crate::warp::TransientLoop) in its list.
    TransientLoop,
    /// Spectral's Formants, from 0 to 100.
    Formants,
    /// Spectral's Envelope, while High quality is on, from 8 to 512.
    Envelope,
}

impl ClipParam {
    /// Every one, in order.
    pub const ALL: [Self; 8] = [
        Self::Gain,
        Self::Transpose,
        Self::Detune,
        Self::TransientEnvelope,
        Self::Preserve,
        Self::TransientLoop,
        Self::Formants,
        Self::Envelope,
    ];

    /// What it is.
    pub fn spec(self) -> ParamSpec {
        let linear = |name: &str, min: f64, max: f64, default: f64| ParamSpec {
            name: name.into(),
            min,
            max,
            default,
            scale: Scale::Linear,
        };
        let steps = |name: &str, count: usize, default: usize| {
            linear(name, 0.0, (count - 1) as f64, default as f64)
        };
        match self {
            Self::Gain => ParamSpec {
                name: "Gain".into(),
                min: 0.0,
                max: ParamSpec::CLIP_GAIN_MAX,
                default: 1.0,
                scale: Scale::Fader,
            },
            Self::Transpose => {
                let max = f64::from(Warp::TRANSPOSE_MAX);
                linear("Transpose", -max, max, 0.0)
            }
            Self::Detune => {
                let max = f64::from(Warp::DETUNE_MAX);
                linear("Detune", -max, max, 0.0)
            }
            Self::TransientEnvelope => linear(
                "Transient Envelope",
                0.0,
                SliceSettings::ENVELOPE_MAX,
                SliceSettings::ENVELOPE_MAX,
            ),
            Self::Preserve => steps("Preserve", Preserve::ALL.len(), 0),
            Self::TransientLoop => {
                let default = TransientLoop::default().step();
                steps("Transient Loop Mode", TransientLoop::ALL.len(), default)
            }
            Self::Formants => linear(
                "Formants",
                0.0,
                SpectralSettings::FORMANTS_MAX,
                SpectralSettings::FORMANTS_MAX,
            ),
            Self::Envelope => ParamSpec {
                name: "Envelope".into(),
                min: SpectralSettings::ENVELOPE_MIN,
                max: SpectralSettings::ENVELOPE_MAX,
                default: SpectralSettings::ENVELOPE_DEFAULT,
                scale: Scale::Log,
            },
        }
    }

    /// Whether its values are steps, whole numbers that its curve jumps
    /// between rather than moving smoothly.
    pub fn stepped(self) -> bool {
        matches!(self, Self::Preserve | Self::TransientLoop)
    }

    /// Whether it plays in a clip warped as `warp`, or unwarped: Gain
    /// always, Transpose and Detune in every mode but Re-Pitch, each mode's
    /// own in it, and Envelope in Spectral while High quality is on.
    pub fn plays_in(self, warp: Option<&Warp>) -> bool {
        let mode = warp.map(|warp| warp.mode);
        match self {
            Self::Gain => true,
            Self::Transpose | Self::Detune => mode.is_some_and(WarpMode::transposes),
            Self::TransientEnvelope | Self::Preserve | Self::TransientLoop => {
                mode == Some(WarpMode::Slice)
            }
            Self::Formants => mode == Some(WarpMode::Spectral),
            Self::Envelope => {
                warp.is_some_and(|w| w.mode == WarpMode::Spectral && w.spectral.high_quality)
            }
        }
    }

    /// What scripts call it, as in "clip 3 gain".
    pub fn key(self) -> &'static str {
        match self {
            Self::Gain => "gain",
            Self::Transpose => "transpose",
            Self::Detune => "detune",
            Self::TransientEnvelope => "transient_envelope",
            Self::Preserve => "preserve",
            Self::TransientLoop => "transient_loop",
            Self::Formants => "formants",
            Self::Envelope => "envelope",
        }
    }

    /// The parameter scripts call `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }
}

/// How a parameter's values spread along its control and its automation
/// lane. Automation moves in a straight line in the scale from one point to
/// the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Scale {
    /// Evenly over the range.
    #[default]
    Linear,
    /// Evenly in ratios, for frequencies and times: each step along the
    /// range multiplies the value by the same amount. The range is above 0.
    Log,
    /// A fader's taper over linear gain: the fourth root of the value
    /// spreads evenly, so the loud end spreads out, as on the mixer's
    /// faders. With the range from 0, 0 dB sits 84% of the way up a fader
    /// that goes to +6 dB, -20 dB at 47% and -60 dB at 15%. It suits times
    /// that can be 0 too, giving short ones room.
    Fader,
}

impl Scale {
    /// Where `value` falls on the scale's even axis, which needs no range:
    /// the value itself, its natural logarithm, or its fourth root.
    /// Automation moves along it at a steady rate between two points.
    pub fn position(self, value: f64) -> f64 {
        match self {
            Self::Linear => value,
            Self::Log => value.max(f64::MIN_POSITIVE).ln(),
            Self::Fader => value.max(0.0).sqrt().sqrt(),
        }
    }

    /// The value at `position` on the scale's even axis: the inverse of
    /// [`position`](Self::position).
    pub fn value_at(self, position: f64) -> f64 {
        match self {
            Self::Linear => position,
            Self::Log => position.exp(),
            Self::Fader => {
                let squared = position.max(0.0) * position.max(0.0);
                squared * squared
            }
        }
    }

    /// Where `value` is along `min..=max`, from 0.0 to 1.0: how far up its
    /// control or its lane it sits.
    pub fn to_unit(self, value: f64, min: f64, max: f64) -> f64 {
        let (bottom, top) = (self.position(min), self.position(max));
        if top == bottom || value.is_nan() {
            return 0.0;
        }
        ((self.position(value) - bottom) / (top - bottom)).clamp(0.0, 1.0)
    }

    /// The value at `unit` along `min..=max`: the inverse of
    /// [`to_unit`](Self::to_unit).
    pub fn from_unit(self, unit: f64, min: f64, max: f64) -> f64 {
        let (bottom, top) = (self.position(min), self.position(max));
        let unit = if unit.is_nan() {
            0.0
        } else {
            unit.clamp(0.0, 1.0)
        };
        match unit {
            // Exactly the ends, whatever the rounding on the way.
            0.0 => min,
            1.0 => max,
            _ => self.value_at(bottom + (top - bottom) * unit),
        }
    }

    /// The value `t` of the way from `from` to `to`, moving evenly in the
    /// scale: what automation does between two points.
    pub fn mix(self, from: f64, to: f64, t: f64) -> f64 {
        // Exactly the ends, and the value between two the same, whatever
        // the rounding.
        if from == to || t <= 0.0 {
            return from;
        }
        if t >= 1.0 {
            return to;
        }
        if self == Self::Linear {
            return from + (to - from) * t;
        }
        let (from, to) = (self.position(from), self.position(to));
        self.value_at(from + (to - from) * t)
    }
}

/// What a parameter is, for showing it and automating it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub scale: Scale,
}

impl ParamSpec {
    /// The loudest a channel's fader goes: +6 dB, as linear gain.
    pub const VOLUME_MAX: f64 = 1.995_262_314_968_879_5;

    /// The loudest an audio clip's gain goes: +24 dB, as linear gain.
    pub const CLIP_GAIN_MAX: f64 = 15.848_931_924_611_133;

    /// A channel's fader: from silence to +6 dB.
    pub fn volume() -> Self {
        Self {
            name: "Volume".into(),
            min: 0.0,
            max: Self::VOLUME_MAX,
            default: 1.0,
            scale: Scale::Fader,
        }
    }

    /// A channel's balance.
    pub fn pan() -> Self {
        Self {
            name: "Pan".into(),
            min: -1.0,
            max: 1.0,
            default: 0.0,
            scale: Scale::Linear,
        }
    }

    /// The level of a send to the return track `to`: from silence to 0 dB.
    pub fn send(to: &str) -> Self {
        Self {
            name: format!("Send to {to}"),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            scale: Scale::Fader,
        }
    }

    /// `value` within the range.
    pub fn clamp(&self, value: f64) -> f64 {
        value.clamp(self.min.min(self.max), self.max.max(self.min))
    }
}

impl Project {
    /// What the parameter at `address` is, where the project knows: the
    /// mixer's parameters, the built-in devices' from their kind's table,
    /// and the test devices'. A plugin's parameters are the plugin's to
    /// say, so they have none here. `None` too where the project hasn't the
    /// parameter's channel, send or device.
    pub fn param_spec(&self, address: ParamAddress) -> Option<ParamSpec> {
        if !self.has_param(address) {
            return None;
        }
        match address {
            ParamAddress::Volume(_) => Some(ParamSpec::volume()),
            ParamAddress::Pan(_) => Some(ParamSpec::pan()),
            ParamAddress::Send(send) => {
                let to = self
                    .tracks
                    .iter()
                    .flat_map(|t| &t.sends)
                    .find(|s| s.id == send)?
                    .to;
                Some(ParamSpec::send(&self.track(to)?.name))
            }
            ParamAddress::Clip(_, param) => Some(param.spec()),
            ParamAddress::Device(device, n) => {
                if let Some(info) = self.native_param(address) {
                    return Some(ParamSpec {
                        name: info.full_name(),
                        min: info.min,
                        max: info.max,
                        default: info.default,
                        scale: info.scale,
                    });
                }
                let (_, _, device) = self.find_device(device)?;
                match (&device.kind, n) {
                    (DeviceKind::TestGain { .. }, 0) => Some(ParamSpec {
                        name: "Gain".into(),
                        ..ParamSpec::volume()
                    }),
                    (DeviceKind::TestInstrument { .. }, 0) => Some(ParamSpec {
                        name: "Level".into(),
                        min: 0.0,
                        max: 1.0,
                        default: 1.0,
                        scale: Scale::Fader,
                    }),
                    _ => None,
                }
            }
        }
    }

    /// The row in its kind's table of the built-in device's parameter at
    /// `address`, if it's one.
    pub fn native_param(&self, address: ParamAddress) -> Option<&'static ParamInfo> {
        let ParamAddress::Device(device, n) = address else {
            return None;
        };
        let (_, _, device) = self.find_device(device)?;
        device.kind.native()?.params().get(n as usize)
    }

    /// Whether the project has the parameter at `address`: the channel,
    /// send or device it belongs to, and for a built-in device, a parameter
    /// in its kind's table, and for a test device, parameter 0, its only
    /// one. Which parameters a plugin has is the plugin's to say, so any of
    /// a plugin's counts.
    pub fn has_param(&self, address: ParamAddress) -> bool {
        self.param_chain(address).is_some()
    }

    /// The channel the parameter at `address` is on: the track or the
    /// master whose fader, send or device it belongs to.
    pub fn param_chain(&self, address: ParamAddress) -> Option<Chain> {
        match address {
            ParamAddress::Volume(Chain::Master) | ParamAddress::Pan(Chain::Master) => {
                Some(Chain::Master)
            }
            ParamAddress::Volume(Chain::Track(id)) | ParamAddress::Pan(Chain::Track(id)) => {
                self.track(id).map(|t| Chain::Track(t.id))
            }
            ParamAddress::Send(send) => self
                .tracks
                .iter()
                .find(|t| t.sends.iter().any(|s| s.id == send))
                .map(|t| Chain::Track(t.id)),
            ParamAddress::Clip(clip, _) => {
                let (track, clip) = self.find_clip(clip)?;
                matches!(clip.content, ClipContent::Audio(_)).then_some(Chain::Track(track.id))
            }
            ParamAddress::Device(device, n) => {
                let (chain, _, device) = self.find_device(device)?;
                let known = match &device.kind {
                    DeviceKind::Plugin(_) => true,
                    DeviceKind::Native(native) => (n as usize) < native.params().len(),
                    DeviceKind::TestGain { .. } | DeviceKind::TestInstrument { .. } => n == 0,
                };
                known.then_some(chain)
            }
        }
    }

    /// The parameter's own value, which applies where automation doesn't:
    /// a channel's gain or pan, a send's level, a built-in or test device's
    /// setting, or a plugin parameter set since the plugin last saved its
    /// state.
    /// `None` for a plugin parameter that keeps the value its state gives
    /// it, and for parameters the project hasn't.
    pub fn param_value(&self, address: ParamAddress) -> Option<f64> {
        match address {
            ParamAddress::Volume(Chain::Master) => Some(f64::from(self.master.gain)),
            ParamAddress::Pan(Chain::Master) => Some(f64::from(self.master.pan)),
            ParamAddress::Volume(Chain::Track(id)) => self.track(id).map(|t| f64::from(t.gain)),
            ParamAddress::Pan(Chain::Track(id)) => self.track(id).map(|t| f64::from(t.pan)),
            ParamAddress::Send(send) => self
                .tracks
                .iter()
                .flat_map(|t| &t.sends)
                .find(|s| s.id == send)
                .map(|s| f64::from(s.level)),
            ParamAddress::Clip(clip, param) => {
                let ClipContent::Audio(audio) = &self.find_clip(clip)?.1.content else {
                    return None;
                };
                match param {
                    ClipParam::Gain => Some(f64::from(audio.gain)),
                    _ => audio.warp.as_ref()?.param(param),
                }
            }
            ParamAddress::Device(device, n) => {
                let (_, _, device) = self.find_device(device)?;
                match (&device.kind, n) {
                    (DeviceKind::Native(native), n) => native.value(n as usize),
                    (DeviceKind::TestGain { gain }, 0) => Some(f64::from(*gain)),
                    (DeviceKind::TestInstrument { level }, 0) => Some(f64::from(*level)),
                    (DeviceKind::Plugin(plugin), n) => plugin.settings.param(n),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TrackId, TrackKind};

    #[test]
    fn the_fader_scale_is_the_mixers_taper() {
        let volume = ParamSpec::volume();
        let height = |gain: f64| Scale::Fader.to_unit(gain, volume.min, volume.max);
        assert!((height(1.0) - 0.841).abs() < 0.001, "{}", height(1.0));
        let db = |db: f64| 10_f64.powf(db / 20.0);
        assert!((height(db(-20.0)) - 0.473).abs() < 0.001);
        assert!((height(db(-60.0)) - 0.150).abs() < 0.001);
        assert_eq!(height(0.0), 0.0);
        assert_eq!(height(volume.max), 1.0);
        assert!((ParamSpec::VOLUME_MAX - db(6.0)).abs() < 1e-12);
        for unit in [0.0, 0.25, 0.5, 0.841, 1.0] {
            let gain = Scale::Fader.from_unit(unit, volume.min, volume.max);
            assert!((height(gain) - unit).abs() < 1e-12);
        }
    }

    #[test]
    fn values_mix_evenly_in_their_scale() {
        assert_eq!(Scale::Linear.mix(-1.0, 1.0, 0.25), -0.5);
        // Halfway from 100 Hz to 10 kHz is 1 kHz.
        assert!((Scale::Log.mix(100.0, 10_000.0, 0.5) - 1_000.0).abs() < 1e-9);
        // Halfway up the fader's axis from silence to unity.
        assert!((Scale::Fader.mix(0.0, 1.0, 0.5) - 0.0625).abs() < 1e-12);
        for scale in [Scale::Linear, Scale::Log, Scale::Fader] {
            assert!((scale.mix(0.5, 2.0, 0.0) - 0.5).abs() < 1e-12);
            assert!((scale.mix(0.5, 2.0, 1.0) - 2.0).abs() < 1e-12);
            let middle = scale.from_unit(0.5, 0.5, 2.0);
            assert!((scale.mix(0.5, 2.0, 0.5) - middle).abs() < 1e-12);
        }
        assert_eq!(Scale::Log.from_unit(0.0, 20.0, 20_000.0), 20.0);
        assert_eq!(Scale::Log.from_unit(1.0, 20.0, 20_000.0), 20_000.0);
        assert_eq!(Scale::Linear.to_unit(f64::NAN, 0.0, 1.0), 0.0);
    }

    #[test]
    fn parameters_belong_to_their_channel_while_it_has_them() {
        let mut p = Project::new();
        let bass = p.add_track("Bass", TrackKind::Audio);
        let verb = p.add_track("Verb", TrackKind::Return);
        let send = p.new_send(verb);
        let send_id = send.id;
        let gain = p.new_device(DeviceKind::TestGain { gain: 0.5 });
        let gain_id = gain.id;
        let track = p.track_mut(bass).unwrap();
        track.sends.push(send);
        track.devices.push(gain);
        track.gain = 0.25;

        let on_bass = Some(Chain::Track(bass));
        assert_eq!(
            p.param_chain(ParamAddress::Volume(Chain::Track(bass))),
            on_bass
        );
        assert_eq!(p.param_chain(ParamAddress::Send(send_id)), on_bass);
        assert_eq!(p.param_chain(ParamAddress::Device(gain_id, 0)), on_bass);
        let master = ParamAddress::Pan(Chain::Master);
        assert_eq!(p.param_chain(master), Some(Chain::Master));
        assert!(!p.has_param(ParamAddress::Volume(Chain::Track(TrackId(9)))));

        let volume = ParamAddress::Volume(Chain::Track(bass));
        assert_eq!(p.param_value(volume), Some(0.25));
        assert_eq!(p.param_value(ParamAddress::Device(gain_id, 0)), Some(0.5));
        let spec = p.param_spec(ParamAddress::Send(send_id)).unwrap();
        assert_eq!((spec.name.as_str(), spec.max), ("Send to Verb", 1.0));
        assert_eq!(p.param_spec(ParamAddress::Device(gain_id, 1)), None);
        assert!(!p.has_param(ParamAddress::Device(gain_id, 1)));
    }

    #[test]
    fn a_clip_s_warp_parameters_are_its_warp_s_settings() {
        use crate::clip::{AudioClip, Offset};
        use crate::time::{BeatRange, Beats};
        use crate::warp::{Preserve, TransientLoop, Warp};

        for param in ClipParam::ALL {
            assert_eq!(ClipParam::from_key(param.key()), Some(param));
            let spec = param.spec();
            assert!(spec.min <= spec.default && spec.default <= spec.max);
            if param.stepped() {
                assert_eq!(spec.min.fract() + spec.max.fract(), 0.0, "{param:?}");
            }
        }
        let steps = |param: ClipParam| param.spec().max as usize + 1;
        assert_eq!(steps(ClipParam::Preserve), Preserve::ALL.len());
        assert_eq!(steps(ClipParam::TransientLoop), TransientLoop::ALL.len());
        let in_mode = |mode, high_quality| {
            let mut warp = Warp::new(0, Beats::ZERO, 120.0);
            (warp.mode, warp.spectral.high_quality) = (mode, high_quality);
            warp
        };
        assert!(ClipParam::Gain.plays_in(None));
        assert!(!ClipParam::Transpose.plays_in(Some(&in_mode(WarpMode::RePitch, false))));
        assert!(ClipParam::Detune.plays_in(Some(&in_mode(WarpMode::Slice, false))));
        assert!(!ClipParam::Preserve.plays_in(None));
        assert!(ClipParam::Formants.plays_in(Some(&in_mode(WarpMode::Spectral, false))));
        assert!(!ClipParam::Envelope.plays_in(Some(&in_mode(WarpMode::Spectral, false))));
        assert!(ClipParam::Envelope.plays_in(Some(&in_mode(WarpMode::Spectral, true))));
        assert!(!ClipParam::Envelope.plays_in(Some(&in_mode(WarpMode::Slice, true))));

        let mut p = Project::new();
        let drums = p.add_track("Drums", TrackKind::Audio);
        let mut audio = AudioClip::new("loop.wav");
        audio.offset = Offset::Beats(Beats::ZERO);
        let mut warp = Warp::new(0, Beats::ZERO, 120.0);
        (warp.transpose, warp.detune) = (-12, 25);
        warp.slice.envelope = 40.0;
        warp.slice.preserve = Preserve::Eighth;
        warp.slice.transient_loop = TransientLoop::BackAndForth;
        audio.warp = Some(warp);
        let range = BeatRange::new(Beats::ZERO, Beats::new(4));
        let warped = p.new_clip("Loop", range, ClipContent::Audio(audio.clone()));
        let unwarped = p.new_clip("Loop", range, ClipContent::Audio(AudioClip::new("a.wav")));
        let (warped_id, unwarped_id) = (warped.id, unwarped.id);
        p.track_mut(drums).unwrap().clips = vec![warped, unwarped];
        let value = |clip, param| p.param_value(ParamAddress::Clip(clip, param));
        assert_eq!(value(warped_id, ClipParam::Transpose), Some(-12.0));
        assert_eq!(value(warped_id, ClipParam::Detune), Some(25.0));
        assert_eq!(value(warped_id, ClipParam::TransientEnvelope), Some(40.0));
        assert_eq!(value(warped_id, ClipParam::Preserve), Some(4.0));
        assert_eq!(value(warped_id, ClipParam::TransientLoop), Some(2.0));
        assert_eq!(value(unwarped_id, ClipParam::Gain), Some(1.0));
        assert_eq!(value(unwarped_id, ClipParam::Transpose), None);
        let spec = p.param_spec(ParamAddress::Clip(warped_id, ClipParam::TransientLoop));
        assert_eq!(spec.unwrap().name, "Transient Loop Mode");
    }
}
