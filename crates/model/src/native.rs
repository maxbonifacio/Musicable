//! Musicable's own devices: an EQ, a compressor, a gate, a delay, a reverb,
//! a utility, a sampler and a synth.
//!
//! A built-in device is its kind and a value for each of the kind's
//! parameters. Each kind has a table of them ([`NativeKind::params`]): what
//! each is called, its range and default, and how its control spreads its
//! values. A parameter's number is its place in the table. It's the
//! engine's `ParamId` for the parameter and what automation points at, and
//! project files keep values by key, so a table only ever grows at the end:
//! no parameter is removed, reordered or renamed.
//!
//! The engine plays the devices (`musicable_engine::devices`). Each kind's
//! module here says what its parameters do, which the engine's DSP follows.

pub mod compressor;
pub mod delay;
pub mod eq;
pub mod gate;
pub mod reverb;
pub mod sampler;
pub mod synth;
pub mod utility;

/// Which built-in device a device is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NativeKind {
    Eq,
    Compressor,
    Gate,
    Delay,
    Reverb,
    Utility,
    Sampler,
    Synth,
}

impl NativeKind {
    /// Every kind, in the order the browser lists them.
    pub const ALL: [NativeKind; 8] = [
        Self::Eq,
        Self::Compressor,
        Self::Gate,
        Self::Delay,
        Self::Reverb,
        Self::Utility,
        Self::Sampler,
        Self::Synth,
    ];

    /// What the browser and the device panel call it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Eq => "EQ",
            Self::Compressor => "Compressor",
            Self::Gate => "Gate",
            Self::Delay => "Delay",
            Self::Reverb => "Reverb",
            Self::Utility => "Utility",
            Self::Sampler => "Sampler",
            Self::Synth => "Synth",
        }
    }

    /// What project files and commands call it: "eq", "compressor".
    pub fn key(self) -> &'static str {
        match self {
            Self::Eq => "eq",
            Self::Compressor => "compressor",
            Self::Gate => "gate",
            Self::Delay => "delay",
            Self::Reverb => "reverb",
            Self::Utility => "utility",
            Self::Sampler => "sampler",
            Self::Synth => "synth",
        }
    }

    /// The kind whose [`key`](Self::key) is `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// Whether it plays notes: the sampler and the synth.
    pub fn is_instrument(self) -> bool {
        matches!(self, Self::Sampler | Self::Synth)
    }

    /// Its parameters, in order of number.
    pub fn params(self) -> &'static [ParamInfo] {
        match self {
            Self::Eq => &eq::PARAMS,
            Self::Compressor => &compressor::PARAMS,
            Self::Gate => &gate::PARAMS,
            Self::Delay => &delay::PARAMS,
            Self::Reverb => &reverb::PARAMS,
            Self::Utility => &utility::PARAMS,
            Self::Sampler => &sampler::PARAMS,
            Self::Synth => &synth::PARAMS,
        }
    }
}

/// A built-in device in a device chain.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeDevice {
    pub kind: NativeKind,
    /// The value of each of the kind's parameters, in order of number, in
    /// the parameter's units.
    pub values: Vec<f64>,
    /// The sampler's sample, if it has one: the file's path, relative to
    /// the folder the project file is in, with `/` between names, or
    /// absolute, as for audio clips. Always `None` for the other kinds.
    pub sample: Option<String>,
}

impl NativeDevice {
    /// A new device of `kind`: every parameter at its default, and no
    /// sample.
    pub fn new(kind: NativeKind) -> Self {
        Self {
            kind,
            values: kind.params().iter().map(|param| param.default).collect(),
            sample: None,
        }
    }

    pub fn params(&self) -> &'static [ParamInfo] {
        self.kind.params()
    }

    /// The value of parameter `index`, if the kind has it.
    pub fn value(&self, index: usize) -> Option<f64> {
        self.values.get(index).copied()
    }

    /// Sets parameter `index` to `value`, kept in its range (see
    /// [`ParamInfo::clamp`]), and returns what it was. `None`, changing
    /// nothing, if the kind has no such parameter.
    pub fn set(&mut self, index: usize, value: f64) -> Option<f64> {
        let param = self.params().get(index)?;
        let slot = self.values.get_mut(index)?;
        Some(std::mem::replace(slot, param.clamp(value)))
    }

    /// Whether a project file can hold the device: a value for each
    /// parameter, each one valid for it, and a sample only on a sampler,
    /// with a path that isn't empty.
    pub fn is_valid(&self) -> bool {
        let params = self.params();
        let sample = match &self.sample {
            None => true,
            Some(path) => self.kind == NativeKind::Sampler && !path.is_empty(),
        };
        sample
            && self.values.len() == params.len()
            && params.iter().zip(&self.values).all(|(p, &v)| p.is_valid(v))
    }
}

/// A built-in device's parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
    /// What project files call it: lowercase words joined by `_`, unique in
    /// its table.
    pub key: &'static str,
    /// What its control is labelled, under its group.
    pub name: &'static str,
    /// The part of the device it belongs to, which the device panel shows
    /// together, such as "Band 1". Empty for none.
    pub group: &'static str,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: Unit,
    pub scale: Scale,
    /// Whether it takes whole numbers only: switches, choices, notes and
    /// counts. Stepped parameters are linear, and aren't modulated.
    pub stepped: bool,
    /// Whether modulators can move it. The device works out its value
    /// frame by frame, so it follows modulation as fast as it comes.
    pub modulated: bool,
}

impl ParamInfo {
    /// The same parameter, with modulators kept off it.
    const fn fixed(self) -> Self {
        Self {
            modulated: false,
            ..self
        }
    }

    /// Its name with its group's: "Band 1 Freq".
    pub fn full_name(&self) -> String {
        if self.group.is_empty() {
            self.name.to_owned()
        } else {
            format!("{} {}", self.group, self.name)
        }
    }

    /// `value` kept in the range, and rounded to a whole number if the
    /// parameter is stepped. Not a number gives the default.
    pub fn clamp(&self, value: f64) -> f64 {
        if value.is_nan() {
            return self.default;
        }
        let value = value.clamp(self.min, self.max);
        if self.stepped { value.round() } else { value }
    }

    /// Whether the parameter can take `value`: a number in the range, and a
    /// whole one if the parameter is stepped.
    pub fn is_valid(&self, value: f64) -> bool {
        (self.min..=self.max).contains(&value) && (!self.stepped || value.fract() == 0.0)
    }

    /// Where `value` is along the control, from 0.0 to 1.0.
    pub fn to_unit(&self, value: f64) -> f64 {
        self.scale.to_unit(value, self.min, self.max)
    }

    /// The value at `unit` along the control, rounded if the parameter is
    /// stepped.
    pub fn from_unit(&self, unit: f64) -> f64 {
        self.clamp(self.scale.from_unit(unit, self.min, self.max))
    }

    /// `value` as the device panel shows it: "1.20 kHz", "-6.0 dB", "Bell".
    pub fn format(&self, value: f64) -> String {
        let bipolar = self.min < 0.0 && self.max > 0.0;
        let signed = |text: String, value: f64| {
            if bipolar && value > 0.0 {
                format!("+{text}")
            } else {
                text
            }
        };
        // No "-0.0" for values a hair under zero.
        let tidy = |value: f64, decimals: i32| {
            let scale = 10_f64.powi(decimals);
            if (value * scale).round() == 0.0 {
                0.0
            } else {
                value
            }
        };
        match self.unit {
            Unit::Number if self.stepped => signed(format!("{value:.0}"), value),
            Unit::Number if value.abs() < 10.0 => format!("{:.2}", tidy(value, 2)),
            Unit::Number => format!("{value:.1}"),
            Unit::Decibels => signed(format!("{:.1} dB", tidy(value, 1)), tidy(value, 1)),
            Unit::Gain => decibels(value),
            Unit::Hertz => hertz(value),
            Unit::Milliseconds => milliseconds(value),
            Unit::Seconds if value < 10.0 => format!("{value:.2} s"),
            Unit::Seconds => format!("{value:.1} s"),
            Unit::Percent => format!("{:.0}%", tidy(value, 0)),
            Unit::Semitones if self.stepped => signed(format!("{value:.0} st"), value),
            Unit::Semitones => signed(format!("{:.1} st", tidy(value, 1)), tidy(value, 1)),
            Unit::Cents => signed(format!("{:.0} ct", tidy(value, 0)), tidy(value, 0)),
            Unit::Ratio => format!("{value:.1}:1"),
            Unit::Pan => pan(value),
            Unit::Note => note_name(value),
            Unit::Switch => if value >= 0.5 { "On" } else { "Off" }.to_owned(),
            Unit::Choice(choices) => usize::try_from(value as i64)
                .ok()
                .and_then(|i| choices.get(i))
                .map_or_else(|| "?".to_owned(), |&name| name.to_owned()),
        }
    }

    /// The value typed as `text`, kept in the range: a number with or
    /// without its unit ("-6", "1.2k", "1.2 kHz", "250 ms"), a choice's
    /// name, "on" or "off", a note's name ("C#4") or a pan ("C", "20L").
    /// `None` if it's none of those.
    pub fn parse(&self, text: &str) -> Option<f64> {
        let text = text.trim().to_ascii_lowercase();
        let number = |text: &str, suffixes: &[(&str, f64)]| {
            suffixes.iter().find_map(|&(suffix, scale)| {
                let number = text.strip_suffix(suffix)?.trim();
                let value: f64 = number.parse().ok()?;
                value.is_finite().then_some(value * scale)
            })
        };
        let value = match self.unit {
            Unit::Choice(choices) => choices
                .iter()
                .position(|choice| choice.eq_ignore_ascii_case(&text))
                .map(|i| i as f64)?,
            Unit::Switch => match text.as_str() {
                "on" | "1" => 1.0,
                "off" | "0" => 0.0,
                _ => return None,
            },
            Unit::Note => parse_note(&text).or_else(|| number(&text, &[("", 1.0)]))?,
            Unit::Pan => parse_pan(&text)?,
            Unit::Gain => match text.as_str() {
                "-inf" | "inf" | "-inf db" => 0.0,
                _ => 10_f64.powf(number(&text, &[("db", 1.0), ("", 1.0)])? / 20.0),
            },
            Unit::Hertz => number(
                &text,
                &[("khz", 1000.0), ("k", 1000.0), ("hz", 1.0), ("", 1.0)],
            )?,
            Unit::Milliseconds => number(&text, &[("ms", 1.0), ("s", 1000.0), ("", 1.0)])?,
            Unit::Seconds => number(&text, &[("ms", 0.001), ("s", 1.0), ("", 1.0)])?,
            Unit::Decibels => number(&text, &[("db", 1.0), ("", 1.0)])?,
            Unit::Percent => number(&text, &[("%", 1.0), ("", 1.0)])?,
            Unit::Semitones => number(&text, &[("st", 1.0), ("", 1.0)])?,
            Unit::Cents => number(&text, &[("ct", 1.0), ("", 1.0)])?,
            Unit::Ratio => number(&text, &[(":1", 1.0), ("", 1.0)])?,
            Unit::Number => number(&text, &[("", 1.0)])?,
        };
        Some(self.clamp(value))
    }
}

/// A continuous parameter that modulators can move, for the tables.
const fn param(
    key: &'static str,
    name: &'static str,
    group: &'static str,
    [min, max, default]: [f64; 3],
    unit: Unit,
    scale: Scale,
) -> ParamInfo {
    ParamInfo {
        key,
        name,
        group,
        min,
        max,
        default,
        unit,
        scale,
        stepped: false,
        modulated: true,
    }
}

/// A parameter of whole numbers from `min` to `max`.
const fn whole(
    key: &'static str,
    name: &'static str,
    group: &'static str,
    [min, max, default]: [f64; 3],
    unit: Unit,
) -> ParamInfo {
    ParamInfo {
        stepped: true,
        modulated: false,
        ..param(key, name, group, [min, max, default], unit, Scale::Linear)
    }
}

/// A switch: 0.0 off, 1.0 on.
const fn switch(key: &'static str, name: &'static str, group: &'static str, on: bool) -> ParamInfo {
    let default = if on { 1.0 } else { 0.0 };
    whole(key, name, group, [0.0, 1.0, default], Unit::Switch)
}

/// One of `choices`, by its index.
const fn choice(
    key: &'static str,
    name: &'static str,
    group: &'static str,
    choices: &'static [&'static str],
    default: usize,
) -> ParamInfo {
    let max = (choices.len() - 1) as f64;
    whole(
        key,
        name,
        group,
        [0.0, max, default as f64],
        Unit::Choice(choices),
    )
}

/// What a parameter's values are, which says how to show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    /// A plain number, such as a filter's Q or a count of voices.
    Number,
    Decibels,
    /// A linear gain, 1.0 = unity, shown in decibels, on the
    /// [`Fader`](Scale::Fader) scale.
    Gain,
    Hertz,
    Milliseconds,
    Seconds,
    /// From 0 to 100, or more for something that can go past full, such as
    /// a stereo width.
    Percent,
    Semitones,
    Cents,
    /// A compressor's ratio, shown as "4.0:1".
    Ratio,
    /// A balance from -1.0 (left) to 1.0 (right), shown as the mixer shows
    /// pans: "C", "50L".
    Pan,
    /// A MIDI note number, shown by name: 60 is C4, middle C.
    Note,
    /// 0.0 off, 1.0 on.
    Switch,
    /// One of these, by its index from 0.
    Choice(&'static [&'static str]),
}

/// How a parameter's values spread along its control. The same as
/// automation's scales (`musicable_model::param::Scale`, which can replace
/// this one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Scale {
    /// Evenly over the range.
    #[default]
    Linear,
    /// Evenly in ratios, for frequencies and times. The range is above 0.
    Log,
    /// A fader's taper, as the mixer's faders spread linear gain: the
    /// fourth root of value / max spreads evenly, from 0 at the bottom. It
    /// suits times that can be 0 too, giving short ones room. The range
    /// starts at 0.
    Fader,
}

impl Scale {
    /// Where `value` is along `min..=max`, from 0.0 to 1.0.
    pub fn to_unit(self, value: f64, min: f64, max: f64) -> f64 {
        let unit = match self {
            Self::Linear => (value - min) / (max - min),
            Self::Log => (value / min).ln() / (max / min).ln(),
            Self::Fader => (value / max).max(0.0).sqrt().sqrt(),
        };
        if unit.is_nan() {
            0.0
        } else {
            unit.clamp(0.0, 1.0)
        }
    }

    /// The value at `unit` along `min..=max`.
    pub fn from_unit(self, unit: f64, min: f64, max: f64) -> f64 {
        let unit = unit.clamp(0.0, 1.0);
        match self {
            Self::Linear => min + (max - min) * unit,
            Self::Log => min * (max / min).powf(unit),
            Self::Fader => max * unit.powi(4),
        }
    }
}

/// The linear gain of `db` decibels.
pub fn decibels_to_gain(db: f64) -> f64 {
    10_f64.powf(db / 20.0)
}

/// A linear gain in decibels: "-6.0 dB", "+3.0 dB", "-inf dB".
fn decibels(gain: f64) -> String {
    if gain <= 0.0 {
        return "-inf dB".to_owned();
    }
    let db = 20.0 * gain.log10();
    let db = if db.abs() < 0.05 { 0.0 } else { db };
    if db > 0.0 {
        format!("+{db:.1} dB")
    } else {
        format!("{db:.1} dB")
    }
}

/// "31.5 Hz", "250 Hz", "1.20 kHz", "12.0 kHz".
fn hertz(hz: f64) -> String {
    if hz < 100.0 {
        format!("{hz:.1} Hz")
    } else if hz < 999.5 {
        format!("{hz:.0} Hz")
    } else if hz < 9995.0 {
        format!("{:.2} kHz", hz / 1000.0)
    } else {
        format!("{:.1} kHz", hz / 1000.0)
    }
}

/// "0.50 ms", "12.5 ms", "250 ms", "1.20 s".
fn milliseconds(ms: f64) -> String {
    if ms < 10.0 {
        format!("{ms:.2} ms")
    } else if ms < 100.0 {
        format!("{ms:.1} ms")
    } else if ms < 999.5 {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

/// A pan as the mixer shows it: "C", or from "50L" to "50R".
fn pan(pan: f64) -> String {
    let amount = (pan.abs() * 50.0).round();
    if amount == 0.0 {
        "C".to_owned()
    } else if pan < 0.0 {
        format!("{amount:.0}L")
    } else {
        format!("{amount:.0}R")
    }
}

/// "C", "20l", "20 r", or a number from -1 to 1.
fn parse_pan(text: &str) -> Option<f64> {
    if text == "c" || text == "center" || text == "centre" {
        return Some(0.0);
    }
    for (suffix, sign) in [("l", -1.0), ("r", 1.0)] {
        if let Some(amount) = text.strip_suffix(suffix) {
            let amount: f64 = amount.trim().parse().ok()?;
            return amount.is_finite().then_some(sign * amount / 50.0);
        }
    }
    text.parse().ok().filter(|pan: &f64| pan.is_finite())
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// A MIDI note's name: 60 is "C4", 0 "C-1".
pub fn note_name(note: f64) -> String {
    let note = note.round() as i64;
    let name = NOTE_NAMES[note.rem_euclid(12) as usize];
    format!("{name}{}", note.div_euclid(12) - 1)
}

/// A note's name, as [`note_name`] gives it, in lowercase, and with a flat
/// for its sharp if need be: "c4", "db4", "c#-1".
fn parse_note(text: &str) -> Option<f64> {
    let letter = text.chars().next()?;
    let base = match letter {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest = &text[1..];
    let (shift, octave) = if let Some(octave) = rest.strip_prefix('#') {
        (1, octave)
    } else if let Some(octave) = rest.strip_prefix('b').filter(|o| !o.is_empty()) {
        (-1, octave)
    } else {
        (0, rest)
    };
    let octave: i64 = octave.parse().ok()?;
    Some(((octave + 1) * 12 + base + shift) as f64)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn kinds_have_unique_keys_that_find_them() {
        let keys: BTreeSet<_> = NativeKind::ALL.iter().map(|kind| kind.key()).collect();
        assert_eq!(keys.len(), NativeKind::ALL.len());
        for kind in NativeKind::ALL {
            assert_eq!(NativeKind::from_key(kind.key()), Some(kind));
        }
        assert_eq!(NativeKind::from_key("EQ"), None);
        let instruments: Vec<_> = NativeKind::ALL
            .into_iter()
            .filter(|kind| kind.is_instrument())
            .collect();
        assert_eq!(instruments, [NativeKind::Sampler, NativeKind::Synth]);
    }

    /// The rules every table keeps, which the format, the engine and the
    /// controls rely on.
    #[test]
    fn tables_are_well_formed() {
        for kind in NativeKind::ALL {
            let params = kind.params();
            let keys: BTreeSet<_> = params.iter().map(|p| p.key).collect();
            assert_eq!(keys.len(), params.len(), "{kind:?} has a key twice");
            for p in params {
                let at = format!("{kind:?} {}", p.key);
                let lowercase = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
                assert!(p.key.chars().all(lowercase), "{at}: key");
                assert!(!p.name.is_empty(), "{at}: name");
                assert!(p.min < p.max, "{at}: range");
                assert!(p.is_valid(p.default), "{at}: default");
                match p.scale {
                    Scale::Linear => {}
                    Scale::Log => assert!(p.min > 0.0, "{at}: log from 0"),
                    Scale::Fader => assert_eq!(p.min, 0.0, "{at}: fader not from 0"),
                }
                if p.stepped {
                    assert_eq!(p.scale, Scale::Linear, "{at}: stepped scale");
                    assert!(!p.modulated, "{at}: modulated steps");
                    assert_eq!(p.min.fract() + p.max.fract(), 0.0, "{at}: steps");
                }
                match p.unit {
                    Unit::Switch => assert_eq!((p.min, p.max, p.stepped), (0.0, 1.0, true)),
                    Unit::Choice(choices) => {
                        assert!(p.stepped, "{at}: choice");
                        assert_eq!(p.max as usize + 1, choices.len(), "{at}: choices");
                    }
                    Unit::Note => assert!(p.stepped && p.min >= 0.0 && p.max <= 127.0),
                    Unit::Gain => assert_eq!(p.scale, Scale::Fader, "{at}: gain"),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn new_devices_start_at_the_defaults() {
        for kind in NativeKind::ALL {
            let device = NativeDevice::new(kind);
            assert!(device.is_valid(), "{kind:?}");
            let defaults: Vec<f64> = kind.params().iter().map(|p| p.default).collect();
            assert_eq!(device.values, defaults);
            assert_eq!(device.sample, None);
        }
    }

    #[test]
    fn setting_a_parameter_keeps_it_in_range() {
        let mut eq = NativeDevice::new(NativeKind::Eq);
        let freq = eq::FREQ;
        assert_eq!(eq.set(freq, 1000.0), Some(30.0));
        assert_eq!(eq.value(freq), Some(1000.0));
        eq.set(freq, 1e9);
        assert_eq!(eq.value(freq), Some(22_000.0));
        eq.set(freq, f64::NAN);
        assert_eq!(eq.value(freq), Some(30.0));
        // Stepped parameters round.
        eq.set(eq::SHAPE, 3.6);
        assert_eq!(eq.value(eq::SHAPE), Some(4.0));
        assert_eq!(eq.set(eq.values.len(), 1.0), None);
        assert!(eq.is_valid());
    }

    #[test]
    fn valid_devices_have_a_good_value_for_each_parameter() {
        let mut eq = NativeDevice::new(NativeKind::Eq);
        eq.values[eq::FREQ] = 5.0;
        assert!(!eq.is_valid());
        let mut eq = NativeDevice::new(NativeKind::Eq);
        eq.values[eq::SHAPE] = 1.5;
        assert!(!eq.is_valid());
        let mut eq = NativeDevice::new(NativeKind::Eq);
        eq.values.pop();
        assert!(!eq.is_valid());
        let mut eq = NativeDevice::new(NativeKind::Eq);
        eq.values[eq::GAIN] = f64::NAN;
        assert!(!eq.is_valid());
        // Only samplers have samples.
        let mut eq = NativeDevice::new(NativeKind::Eq);
        eq.sample = Some("kick.wav".to_owned());
        assert!(!eq.is_valid());
        let mut sampler = NativeDevice::new(NativeKind::Sampler);
        sampler.sample = Some("kick.wav".to_owned());
        assert!(sampler.is_valid());
        sampler.sample = Some(String::new());
        assert!(!sampler.is_valid());
    }

    #[test]
    fn scales_spread_values_along_the_control() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(Scale::Linear.to_unit(5.0, 0.0, 10.0), 0.5));
        assert!(close(Scale::Log.to_unit(100.0, 10.0, 1000.0), 0.5));
        assert!(close(Scale::Log.from_unit(0.5, 10.0, 1000.0), 100.0));
        // Half way up a fader is a sixteenth of the gain at the top.
        assert!(close(Scale::Fader.from_unit(0.5, 0.0, 16.0), 1.0));
        assert!(close(Scale::Fader.to_unit(1.0, 0.0, 16.0), 0.5));
        assert_eq!(Scale::Fader.from_unit(0.0, 0.0, 16.0), 0.0);
        for scale in [Scale::Linear, Scale::Log, Scale::Fader] {
            let min = if scale == Scale::Fader { 0.0 } else { 20.0 };
            assert_eq!(scale.to_unit(-1.0, min, 20_000.0), 0.0);
            assert_eq!(scale.to_unit(1e9, min, 20_000.0), 1.0);
            for unit in [0.0, 0.25, 0.5, 1.0] {
                let value = scale.from_unit(unit, min, 20_000.0);
                assert!(
                    close(scale.to_unit(value, min, 20_000.0), unit),
                    "{scale:?}"
                );
            }
        }
    }

    fn param(kind: NativeKind, key: &str) -> ParamInfo {
        *kind.params().iter().find(|p| p.key == key).unwrap()
    }

    #[test]
    fn values_show_in_their_units() {
        let eq = |key| param(NativeKind::Eq, key);
        assert_eq!(eq("band1_freq").format(31.5), "31.5 Hz");
        assert_eq!(eq("band1_freq").format(250.0), "250 Hz");
        assert_eq!(eq("band1_freq").format(1200.0), "1.20 kHz");
        assert_eq!(eq("band1_freq").format(12_000.0), "12.0 kHz");
        assert_eq!(eq("band1_gain").format(3.0), "+3.0 dB");
        assert_eq!(eq("band1_gain").format(-0.01), "0.0 dB");
        assert_eq!(eq("band1_gain").format(-6.0), "-6.0 dB");
        assert_eq!(eq("band1_q").format(0.5), "0.50");
        assert_eq!(eq("band1_shape").format(4.0), "Bell");
        assert_eq!(eq("band1_on").format(1.0), "On");
        let utility = |key| param(NativeKind::Utility, key);
        assert_eq!(utility("gain").format(1.0), "0.0 dB");
        assert_eq!(utility("gain").format(0.5), "-6.0 dB");
        assert_eq!(utility("gain").format(0.0), "-inf dB");
        assert_eq!(utility("pan").format(-0.5), "25L");
        assert_eq!(utility("pan").format(0.0), "C");
        assert_eq!(utility("width").format(100.0), "100%");
        let compressor = |key| param(NativeKind::Compressor, key);
        assert_eq!(compressor("ratio").format(4.0), "4.0:1");
        assert_eq!(compressor("attack").format(0.5), "0.50 ms");
        assert_eq!(compressor("release").format(1500.0), "1.50 s");
        let synth = |key| param(NativeKind::Synth, key);
        assert_eq!(synth("osc2_pitch").format(7.0), "+7 st");
        assert_eq!(synth("osc2_detune").format(-5.0), "-5 ct");
        assert_eq!(synth("osc1_octave").format(-1.0), "-1");
        assert_eq!(param(NativeKind::Sampler, "root").format(60.0), "C4");
        assert_eq!(param(NativeKind::Sampler, "root").format(61.0), "C#4");
        assert_eq!(param(NativeKind::Sampler, "root").format(0.0), "C-1");
        assert_eq!(param(NativeKind::Reverb, "decay").format(2.0), "2.00 s");
    }

    #[test]
    fn typed_values_read_in_their_units() {
        let eq = |key| param(NativeKind::Eq, key);
        assert_eq!(eq("band1_freq").parse("1.2k"), Some(1200.0));
        assert_eq!(eq("band1_freq").parse(" 1.2 kHz "), Some(1200.0));
        assert_eq!(eq("band1_freq").parse("440 Hz"), Some(440.0));
        assert_eq!(eq("band1_freq").parse("1e9"), Some(22_000.0));
        assert_eq!(eq("band1_gain").parse("-6 dB"), Some(-6.0));
        assert_eq!(eq("band1_shape").parse("high shelf"), Some(6.0));
        assert_eq!(eq("band1_shape").parse("shelf"), None);
        assert_eq!(eq("band1_on").parse("off"), Some(0.0));
        assert_eq!(eq("band1_q").parse("loud"), None);
        let utility = |key| param(NativeKind::Utility, key);
        assert_eq!(utility("gain").parse("-inf"), Some(0.0));
        assert_eq!(utility("gain").parse("0 dB"), Some(1.0));
        assert_eq!(utility("pan").parse("25L"), Some(-0.5));
        assert_eq!(utility("pan").parse("c"), Some(0.0));
        let compressor = |key| param(NativeKind::Compressor, key);
        assert_eq!(compressor("release").parse("1.5 s"), Some(1500.0));
        assert_eq!(compressor("ratio").parse("8:1"), Some(8.0));
        let root = param(NativeKind::Sampler, "root");
        assert_eq!(root.parse("C4"), Some(60.0));
        assert_eq!(root.parse("db4"), Some(61.0));
        assert_eq!(root.parse("c-1"), Some(0.0));
        assert_eq!(root.parse("b"), None);
        assert_eq!(root.parse("64"), Some(64.0));
        // Every value shown reads back as itself, near enough.
        for kind in NativeKind::ALL {
            for p in kind.params() {
                for unit in [0.0, 0.3, 1.0] {
                    let value = p.from_unit(unit);
                    let read = p.parse(&p.format(value)).unwrap_or(f64::NAN);
                    let near = (p.to_unit(read) - p.to_unit(value)).abs() < 0.01;
                    assert!(
                        near,
                        "{kind:?} {}: {value} shows as {}",
                        p.key,
                        p.format(value)
                    );
                }
            }
        }
    }
}
