//! Round trips: random projects save and load back exactly, saving to disk
//! replaces files safely, and broken files fail without panicking.

use std::fs;
use std::path::{Path, PathBuf};

use musicable_io::project::{self, LoadError, MAX_TICKS, SaveError};
use musicable_model::automation;
use musicable_model::clip::{
    AudioClip, Clip, ClipContent, Controller, Dimension, MidiClip, Offset, Point,
};
use musicable_model::native::{NativeDevice, NativeKind};
use musicable_model::param::ClipParam;
use musicable_model::plugin::{PluginDevice, PluginParam, PluginSettings, PluginState};
use musicable_model::time::{BeatRange, Beats, TempoMap, TimeSignature};
use musicable_model::warp::{
    GranularSettings, Onset, Onsets, Origin, Preserve, SliceSettings, SpectralSettings,
    TransientLoop, Warp, WarpMarker, WarpMode,
};
use musicable_model::{
    AudioInput, ClipType, DeviceKind, Input, MidiInput, Monitor, PluginFormat, Project, TestTone,
    TrackId, TrackKind,
};
use serde_json::{Value, json};

/// SplitMix64, for making test projects. Seeded, so a failure happens again
/// on the next run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn one_in(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }

    /// Any finite f32, every bit pattern as likely as any other.
    fn f32(&mut self) -> f32 {
        loop {
            let value = f32::from_bits(self.next() as u32);
            if value.is_finite() {
                return value;
            }
        }
    }

    /// Any finite f64, every bit pattern as likely as any other.
    fn f64(&mut self) -> f64 {
        loop {
            let value = f64::from_bits(self.next());
            if value.is_finite() {
                return value;
            }
        }
    }

    /// A balance setting from -1 to 1: often the centre or an end, and
    /// otherwise any f32 in between, every bit pattern as likely.
    fn pan(&mut self) -> f32 {
        match self.below(3) {
            0 => self.pick(&[0.0, -0.0, 1.0, -1.0, 0.5]),
            _ => loop {
                let value = self.f32();
                if (-1.0..=1.0).contains(&value) {
                    return value;
                }
            },
        }
    }

    /// A gain: any finite f32, or a tidy one from 0 to 1.
    fn gain(&mut self) -> f32 {
        if self.one_in(2) {
            self.f32()
        } else {
            self.below(101) as f32 / 100.0
        }
    }

    fn clip_type(&mut self) -> ClipType {
        self.pick(&[ClipType::Off, ClipType::Hard, ClipType::Soft])
    }

    /// A plugin, as new or with a state of any bytes, and with parameters
    /// set to any finite values.
    fn plugin(&mut self) -> DeviceKind {
        let state = (!self.one_in(3)).then(|| {
            let bytes: Vec<u8> = (0..self.below(70)).map(|_| self.next() as u8).collect();
            PluginState::new(bytes)
        });
        let mut id = 0;
        let params = (0..self.below(5))
            .map(|_| {
                id += 1 + self.below(1_000) as u32;
                let value = match self.one_in(2) {
                    true => self.f64(),
                    false => self.below(101) as f64 / 100.0,
                };
                PluginParam { id, value }
            })
            .collect();
        DeviceKind::Plugin(PluginDevice {
            format: self.pick(&[PluginFormat::Clap, PluginFormat::Vst3]),
            id: format!("org.example.{}", self.below(100)),
            name: self.name(),
            instrument: self.one_in(2),
            settings: PluginSettings { state, params },
        })
    }

    /// A built-in device of any kind, some of its parameters set anywhere in
    /// their range, and a sample on some samplers.
    fn native(&mut self) -> DeviceKind {
        let kind = self.pick(&NativeKind::ALL);
        let mut device = NativeDevice::new(kind);
        for (i, param) in kind.params().iter().enumerate() {
            let value = match self.below(4) {
                0 => continue,
                1 => self.pick(&[param.min, param.max, param.default]),
                // Any value in range, every bit pattern as likely.
                2 if !param.stepped => loop {
                    let value = self.f64();
                    if param.is_valid(value) {
                        break value;
                    }
                },
                _ => param.from_unit((self.next() >> 11) as f64 / (1_u64 << 53) as f64),
            };
            device.values[i] = value;
        }
        if kind == NativeKind::Sampler && self.one_in(2) {
            device.sample = Some(format!("Samples/{}.wav", self.name()));
        }
        DeviceKind::Native(device)
    }

    fn bpm(&mut self) -> f64 {
        match self.below(3) {
            0 => self.pick(&[60.0, 90.0, 120.0, 128.0, 140.0, 174.0]),
            1 => (2_000 + self.below(97_901)) as f64 / 100.0,
            _ => {
                let unit = (self.next() >> 11) as f64 / (1_u64 << 53) as f64;
                TempoMap::MIN_BPM + unit * (TempoMap::MAX_BPM - TempoMap::MIN_BPM)
            }
        }
    }

    /// A position from the start of the song up to `MAX_TICKS`, usually near
    /// the start.
    fn ticks(&mut self) -> i64 {
        let limit = match self.below(3) {
            0 => Beats::new(64).ticks(),
            1 => Beats::new(100_000).ticks(),
            _ => MAX_TICKS,
        };
        self.below(limit as u64 + 1) as i64
    }

    fn signed_ticks(&mut self) -> i64 {
        let ticks = self.ticks();
        if self.one_in(2) { -ticks } else { ticks }
    }

    /// A length for something starting at `start`, that ends within
    /// `MAX_TICKS`: usually a few beats.
    fn length(&mut self, start: i64) -> i64 {
        let room = MAX_TICKS - start;
        match self.below(3) {
            0 => 1 + self.below(room as u64) as i64,
            _ => 1 + self.below(Beats::new(16).ticks().min(room) as u64) as i64,
        }
    }

    /// An f32 from `low` to `high`: often an end, and otherwise any f32 in
    /// between, every bit pattern as likely.
    fn within(&mut self, low: f32, high: f32) -> f32 {
        match self.below(4) {
            0 => self.pick(&[low, high]),
            _ => loop {
                let value = self.f32();
                if (low..=high).contains(&value) {
                    return value;
                }
            },
        }
    }

    /// A warp of one marker or a few, rising in frames and in ticks, at any
    /// tempo it allows, with a few transients or none yet, in any mode, with
    /// any transpose, detune, slice, granular and spectral settings.
    fn warp(&mut self) -> Warp {
        let tempo = match self.below(3) {
            0 => self.pick(&[Warp::MIN_TEMPO, Warp::MAX_TEMPO, 120.0]),
            _ => Warp::MIN_TEMPO + self.below(10_000) as f64 / 10_000.0 * 994.0,
        };
        let mut frame = self.below(1 << 20);
        let mut beat = self.signed_ticks().min(MAX_TICKS - 100);
        let mut markers = Vec::new();
        for _ in 0..=self.below(4) {
            if frame > MAX_TICKS as u64 || beat > MAX_TICKS {
                break;
            }
            markers.push(WarpMarker {
                frame,
                beat: Beats::from_ticks(beat),
            });
            frame += 1 + self.below(100_000);
            beat += 1 + self.below(Beats::new(4).ticks() as u64) as i64;
        }
        let onsets = (!self.one_in(3)).then(|| {
            let mut frame = self.below(1 << 20);
            let mut onsets = Vec::new();
            for _ in 0..self.below(6) {
                onsets.push(Onset {
                    frame: frame.min(MAX_TICKS as u64),
                    strength: self.within(0.0, 1.0),
                    percussiveness: self.within(0.0, 1.0),
                    origin: self.pick(&[Origin::Found, Origin::Added, Origin::Hidden]),
                });
                frame += 1 + self.below(100_000);
            }
            Onsets::new(onsets)
        });
        let (transpose, detune) = (Warp::TRANSPOSE_MAX, Warp::DETUNE_MAX);
        let transpose = self.below(2 * transpose as u64 + 1) as i8 - transpose;
        let detune = self.below(2 * detune as u64 + 1) as i8 - detune;
        let envelope = f64::from(self.within(0.0, SliceSettings::ENVELOPE_MAX as f32));
        let formants = f64::from(self.within(0.0, SpectralSettings::FORMANTS_MAX as f32));
        let (least, most) = (
            SpectralSettings::ENVELOPE_MIN,
            SpectralSettings::ENVELOPE_MAX,
        );
        let spectral_envelope = f64::from(self.within(least as f32, most as f32));
        let (least, most) = (
            GranularSettings::GRAIN_SIZE_MIN,
            GranularSettings::GRAIN_SIZE_MAX,
        );
        let grain_size = f64::from(self.within(least as f32, most as f32));
        let fluctuation = f64::from(self.within(0.0, GranularSettings::FLUCTUATION_MAX as f32));
        Warp {
            mode: self.pick(&WarpMode::ALL),
            markers,
            onsets,
            transpose,
            detune,
            slice: SliceSettings {
                preserve: self.pick(&Preserve::ALL),
                transient_loop: self.pick(&TransientLoop::ALL),
                envelope,
            },
            granular: GranularSettings {
                follow_pitch: self.one_in(2),
                grain_size,
                fluctuation,
            },
            spectral: SpectralSettings {
                high_quality: self.one_in(2),
                formants,
                envelope: spectral_envelope,
            },
            ..Warp::new(0, Beats::ZERO, tempo)
        }
    }

    /// A curve's points, in order, with at most two at one place, before
    /// zero or after it, with values from `min` to `max`, whole numbers if
    /// `stepped`.
    fn points(&mut self, min: f64, max: f64, stepped: bool) -> Vec<automation::Point> {
        let mut at = self.signed_ticks().min(MAX_TICKS - 100);
        let mut points: Vec<automation::Point> = Vec::new();
        for _ in 0..=self.below(4) {
            let value = match self.below(3) {
                0 => self.pick(&[min, max, min.max(0.0).min(max)]),
                _ if stepped => min + self.below((max - min) as u64 + 1) as f64,
                _ => min + self.below(1_001) as f64 / 1_000.0 * (max - min),
            };
            points.push(automation::Point::new(Beats::from_ticks(at), value));
            let step =
                points.len() >= 2 && points[points.len() - 2].at == points[points.len() - 1].at;
            if step || !self.one_in(3) {
                at = (at + 1 + self.below(Beats::new(2).ticks() as u64) as i64).min(MAX_TICKS);
            }
        }
        points.dedup_by(|b, a| a.at == b.at && a.value == b.value);
        points
    }

    /// A clip for a track of `kind`, as long as the file allows.
    fn clip(&mut self, project: &mut Project, kind: TrackKind) -> Clip {
        let start = self.ticks().min(MAX_TICKS - 1);
        let length = self.length(start);
        let content = match kind {
            TrackKind::Audio => {
                let mut file = self.name();
                if file.is_empty() {
                    file.push_str("take 1.wav");
                }
                let fade_in = self.below(length as u64 + 1) as i64;
                let fade_out = self.below((length - fade_in) as u64 + 1) as i64;
                let warp = self.one_in(2).then(|| self.warp());
                let offset = match warp {
                    Some(_) => Offset::Beats(Beats::from_ticks(self.signed_ticks())),
                    None => Offset::Frames(self.below(MAX_TICKS as u64 + 1)),
                };
                let mut audio = AudioClip {
                    file,
                    offset,
                    gain: self.gain(),
                    fade_in: Beats::from_ticks(fade_in),
                    fade_out: Beats::from_ticks(fade_out),
                    warp,
                    automation: Vec::new(),
                };
                for param in ClipParam::ALL {
                    if self.one_in(2) {
                        let spec = param.spec();
                        let points = self.points(spec.min, spec.max, param.stepped());
                        audio.set_lane(param, points);
                    }
                }
                ClipContent::Audio(audio)
            }
            _ => {
                let mut midi = MidiClip {
                    offset: Beats::from_ticks(self.signed_ticks()),
                    ..MidiClip::default()
                };
                for _ in 0..self.below(5) {
                    let start = self.signed_ticks().min(MAX_TICKS - 1);
                    let length = Beats::from_ticks(self.length(start));
                    let key = self.below(128) as u8;
                    let velocity = self.within(0.0, 1.0);
                    let release = self.within(0.0, 1.0);
                    let note = midi.add_note(Beats::from_ticks(start), length, key, velocity);
                    note.release_velocity = release;
                    for dimension in Dimension::ALL {
                        let (low, high) = dimension.range();
                        let mut at = 0;
                        for _ in 0..self.below(4) {
                            at = (at + self.below(Beats::new(2).ticks() as u64) as i64)
                                .min(MAX_TICKS);
                            let point = Point::new(Beats::from_ticks(at), self.within(low, high));
                            note.expression.curve_mut(dimension).push(point);
                        }
                    }
                }
                // Some deleted, leaving gaps in the ids.
                midi.notes.retain(|_| !self.one_in(4));
                midi.next_note_id += self.below(3) as u32;
                for _ in 0..self.below(4) {
                    let controller = Controller::new(self.below(120) as u8).unwrap();
                    let mut at = self.signed_ticks().min(MAX_TICKS - 1);
                    let mut points = Vec::new();
                    for _ in 0..=self.below(4) {
                        points.push(Point::new(Beats::from_ticks(at), self.within(0.0, 1.0)));
                        at = (at + self.below(Beats::new(2).ticks() as u64) as i64).min(MAX_TICKS);
                    }
                    midi.set_controller(controller, points);
                }
                ClipContent::Midi(midi)
            }
        };
        let range = BeatRange::new(Beats::from_ticks(start), Beats::from_ticks(start + length));
        project.new_clip(self.name(), range, content)
    }

    fn name(&mut self) -> String {
        let length = self.below(12);
        (0..length)
            .map(|_| match self.below(4) {
                0 => self.pick(&[
                    '"', '\\', '/', '\n', '\t', '\0', '\u{1f}', '\u{7f}', '\u{2028}', '\u{feff}',
                ]),
                1 => char::from_u32(self.below(0x11_0000) as u32).unwrap_or('\u{fffd}'),
                _ => self.pick(&['a', 'Z', ' ', '9', 'é', '音', '🎛']),
            })
            .collect()
    }
}

/// A project with random everything that the model allows and a file holds.
fn random_project(random: &mut Random) -> Project {
    let mut project = Project::new();
    project.tempo.set_tempo(random.bpm());
    for _ in 0..random.below(6) {
        let at = Beats::from_ticks(random.ticks());
        project.tempo.insert_tempo(at, random.bpm());
    }
    let mut bar = 0;
    for _ in 0..random.below(5) {
        let numerator = 1 + random.below(255) as u8;
        let denominator = 1 << random.below(7);
        let signature = TimeSignature::new(numerator, denominator).unwrap();
        project.tempo.insert_signature(bar, signature);
        bar += 1 + random.below(1_000) as i64;
    }
    project.loop_region = BeatRange::new(
        Beats::from_ticks(random.signed_ticks()),
        Beats::from_ticks(random.signed_ticks()),
    );
    project.looping = random.one_in(2);
    project.metronome = random.one_in(2);

    let kinds = [
        TrackKind::Audio,
        TrackKind::Midi,
        TrackKind::Return,
        TrackKind::Group,
    ];
    for _ in 0..random.below(8) {
        let id = project.add_track(random.name(), random.pick(&kinds));
        let gain = random.gain();
        let tone = random.one_in(2).then(|| TestTone {
            frequency: random.f64(),
            level: random.f32(),
        });
        let muted = random.one_in(3);
        let track = project.track_mut(id).unwrap();
        track.gain = gain;
        track.tone = tone;
        track.muted = muted;
    }
    // Delete and reorder some, and leave gaps after the last.
    for _ in 0..random.below(3) {
        if !project.tracks.is_empty() {
            let i = random.below(project.tracks.len() as u64) as usize;
            project.tracks.remove(i);
        }
    }
    if project.tracks.len() > 1 {
        let len = project.tracks.len() as u64;
        let (a, b) = (random.below(len) as usize, random.below(len) as usize);
        project.tracks.swap(a, b);
    }
    project.next_track_id += random.below(3) as u32;

    // The mixer, once the tracks it routes between are settled. Each output
    // is one the tracks so far allow, so they never loop.
    let ids: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
    for &id in &ids {
        let output = random.pick(&project.buses_for(id));
        let mut sends = Vec::new();
        for &to in &ids {
            if project.can_send(id, to) && random.one_in(2) {
                let mut send = project.new_send(to);
                send.level = random.gain();
                send.pre_fader = random.one_in(2);
                sends.push(send);
            }
        }
        let kind = project.track(id).unwrap().kind;
        let devices = (0..random.below(3))
            .map(|_| {
                let kind = match random.below(4) {
                    0 => DeviceKind::TestInstrument {
                        level: random.gain(),
                    },
                    1 => random.plugin(),
                    2 => random.native(),
                    _ => DeviceKind::TestGain {
                        gain: random.gain(),
                    },
                };
                project.new_device(kind)
            })
            .collect();
        let clips = match kind.is_bus() {
            true => Vec::new(),
            false => (0..random.below(4))
                .map(|_| random.clip(&mut project, kind))
                .collect(),
        };
        let mut takes = Vec::new();
        if !kind.is_bus() {
            for _ in 0..random.below(3) {
                let mut take = project.new_take(random.name());
                take.clips = (0..random.below(3))
                    .map(|_| random.clip(&mut project, kind))
                    .collect();
                takes.push(take);
            }
        }
        let input = match kind {
            _ if random.one_in(4) => None,
            TrackKind::Audio => Some(Input::Audio(AudioInput {
                device: random.name(),
                name: random.name(),
                channel: random.below(65535) as u16,
                stereo: random.one_in(2),
            })),
            TrackKind::Midi => Some(Input::Midi(MidiInput {
                device: random.name(),
                name: random.name(),
                channel: (!random.one_in(2)).then(|| random.below(16) as u8),
            })),
            TrackKind::Return | TrackKind::Group => None,
        };
        let track = project.track_mut(id).unwrap();
        track.output = output;
        track.sends = sends;
        track.clips = clips;
        track.takes = takes;
        track.input = input;
        track.monitor = random.pick(&[Monitor::In, Monitor::Auto, Monitor::Off]);
        track.devices = devices;
        track.pan = random.pan();
        track.solo = random.one_in(4);
        track.solo_safe = random.one_in(3);
        track.armed = !track.kind.is_bus() && random.one_in(3);
        track.clip_type = random.clip_type();
    }
    project.master.gain = random.gain();
    project.master.pan = random.pan();
    for _ in 0..random.below(3) {
        let kind = match random.below(3) {
            0 => random.plugin(),
            1 => random.native(),
            _ => DeviceKind::TestGain {
                gain: random.gain(),
            },
        };
        let device = project.new_device(kind);
        project.master.devices.push(device);
    }
    project.master.clip_type = random.clip_type();
    project.next_send_id += random.below(3) as u32;
    project.next_device_id += random.below(3) as u32;
    project.next_clip_id += random.below(3) as u32;
    for _ in 0..random.below(4) {
        let locator = project.new_locator(random.name(), Beats::from_ticks(random.signed_ticks()));
        project.locators.push(locator);
    }
    project.next_locator_id += random.below(3) as u32;
    project.next_take_id += random.below(3) as u32;
    project.punch_in = random.one_in(2);
    project.punch_out = random.one_in(2);
    project
}

#[test]
fn random_projects_round_trip_exactly() {
    let mut random = Random(1);
    for i in 0..2_000 {
        let project = random_project(&mut random);
        let text =
            project::to_string(&project).unwrap_or_else(|error| panic!("project {i}: {error}"));
        let loaded =
            project::from_str(&text).unwrap_or_else(|error| panic!("project {i}: {error}\n{text}"));
        assert_eq!(loaded, project, "project {i}");
        // Equality can't tell -0.0 from 0.0, but the saved text can.
        assert_eq!(project::to_string(&loaded).unwrap(), text, "project {i}");
    }
}

#[test]
fn floats_load_bit_for_bit() {
    let mut random = Random(2);
    let mut project = Project::new();
    let reverb = project.add_track("Reverb", TrackKind::Return);
    for _ in 0..10_000 {
        let id = project.add_track("", TrackKind::Audio);
        let mut send = project.new_send(reverb);
        send.level = random.f32();
        let gain = random.f32();
        let device = project.new_device(DeviceKind::TestGain { gain });
        let track = project.track_mut(id).unwrap();
        track.gain = random.f32();
        track.pan = random.pan();
        track.tone = Some(TestTone {
            frequency: random.f64(),
            level: random.f32(),
        });
        track.sends.push(send);
        track.devices.push(device);
    }
    let loaded = project::from_str(&project::to_string(&project).unwrap()).unwrap();
    for (saved, loaded) in project.tracks.iter().zip(&loaded.tracks).skip(1) {
        let (tone, loaded_tone) = (saved.tone.unwrap(), loaded.tone.unwrap());
        assert_eq!(loaded.gain.to_bits(), saved.gain.to_bits());
        assert_eq!(loaded.pan.to_bits(), saved.pan.to_bits());
        assert_eq!(loaded_tone.frequency.to_bits(), tone.frequency.to_bits());
        assert_eq!(loaded_tone.level.to_bits(), tone.level.to_bits());
        let level = |track: &musicable_model::Track| track.sends[0].level.to_bits();
        assert_eq!(level(loaded), level(saved));
        let gain = |track: &musicable_model::Track| match &track.devices[0].kind {
            DeviceKind::TestGain { gain } => gain.to_bits(),
            _ => unreachable!("only test gains"),
        };
        assert_eq!(gain(loaded), gain(saved));
    }
}

#[test]
fn notes_and_clips_load_bit_for_bit() {
    let mut random = Random(6);
    let mut project = Project::new();
    let midi = project.add_track("MIDI", TrackKind::Midi);
    let audio = project.add_track("Audio", TrackKind::Audio);
    let mut notes = MidiClip::default();
    for i in 0..10_000 {
        let (velocity, release) = (random.within(0.0, 1.0), random.within(0.0, 1.0));
        let note = notes.add_note(Beats::new(i), Beats::new(1), 60, velocity);
        note.release_velocity = release;
        for dimension in Dimension::ALL {
            let (low, high) = dimension.range();
            let point = Point::new(Beats::ZERO, random.within(low, high));
            note.expression.curve_mut(dimension).push(point);
        }
    }
    let range = BeatRange::new(Beats::ZERO, Beats::new(10_000));
    let clip = project.new_clip("Notes", range, ClipContent::Midi(notes));
    project.track_mut(midi).unwrap().clips.push(clip);
    for _ in 0..10_000 {
        let mut audio_clip = AudioClip::new("take.wav");
        audio_clip.gain = random.f32();
        let clip = project.new_clip("", range, ClipContent::Audio(audio_clip));
        project.track_mut(audio).unwrap().clips.push(clip);
    }
    let loaded = project::from_str(&project::to_string(&project).unwrap()).unwrap();
    let bits = |project: &Project| {
        let mut bits = Vec::new();
        for clip in project.tracks.iter().flat_map(|t| &t.clips) {
            match &clip.content {
                ClipContent::Audio(audio) => bits.push(audio.gain.to_bits()),
                ClipContent::Midi(midi) => {
                    for note in &midi.notes {
                        bits.extend([note.velocity.to_bits(), note.release_velocity.to_bits()]);
                        for dimension in Dimension::ALL {
                            let curve = note.expression.curve(dimension);
                            bits.extend(curve.iter().map(|p| p.value.to_bits()));
                        }
                    }
                }
            }
        }
        bits
    };
    assert_eq!(bits(&loaded), bits(&project));
}

/// An empty folder for one test, in Cargo's scratch folder for tests.
fn scratch(test: &str) -> PathBuf {
    let folder = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn saving_replaces_the_file_and_leaves_nothing_else() {
    let folder = scratch("saving_replaces_the_file");
    let path = folder.join("song.musicable");
    let mut random = Random(3);
    for _ in 0..3 {
        let project = random_project(&mut random);
        project::save(&path, &project).unwrap();
        assert_eq!(project::load(&path).unwrap(), project);
    }
    assert_eq!(names_in(&folder), ["song.musicable"]);
}

#[test]
fn a_failed_save_leaves_the_old_file_alone() {
    let folder = scratch("a_failed_save");
    let path = folder.join("song.musicable");
    let project = random_project(&mut Random(4));
    project::save(&path, &project).unwrap();
    let saved = fs::read(&path).unwrap();

    let mut broken = project.clone();
    let id = broken.add_track("Broken", TrackKind::Audio);
    broken.track_mut(id).unwrap().gain = f32::NAN;
    assert!(matches!(
        project::save(&path, &broken),
        Err(SaveError::Invalid(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), saved);

    // A folder in the way: the rename fails, and the temporary file goes.
    let blocked = folder.join("blocked.musicable");
    fs::create_dir(&blocked).unwrap();
    assert!(matches!(
        project::save(&blocked, &project),
        Err(SaveError::Io(_))
    ));
    assert_eq!(names_in(&folder), ["blocked.musicable", "song.musicable"]);

    let missing = folder.join("missing");
    assert!(matches!(
        project::save(missing.join("song.musicable"), &project),
        Err(SaveError::Io(_))
    ));
    assert!(matches!(
        project::load(missing.join("song.musicable")),
        Err(LoadError::Io(_))
    ));
}

/// Loading must not panic, and whatever loads must save and load back the
/// same.
fn check(bytes: &[u8]) {
    if let Ok(project) = project::from_slice(bytes) {
        let text = project::to_string(&project).expect("whatever loads saves");
        assert_eq!(project::from_str(&text).unwrap(), project);
    }
}

#[test]
fn damaged_files_fail_cleanly() {
    let mut random = Random(5);
    let mut project = random_project(&mut random);
    while project.tracks.len() < 3 {
        project = random_project(&mut random);
    }
    let text = project::to_string(&project).unwrap();

    // Damaged text, which is mostly no longer JSON.
    for _ in 0..2_000 {
        let mut bytes = text.as_bytes().to_vec();
        for _ in 0..=random.below(3) {
            let at = random.below(bytes.len() as u64) as usize;
            let end = bytes.len().min(at + random.below(16) as usize);
            match random.below(3) {
                0 => bytes[at] = random.pick(b"0123456789-+.eE\"\\{}[],: tfn"),
                1 => drop(bytes.drain(at..end)),
                _ => {
                    let copy = bytes[at..end].to_vec();
                    bytes.splice(at..at, copy);
                }
            }
        }
        check(&bytes);
    }

    // Valid JSON with a value swapped for another or a field taken out, which
    // gets as far as the format's rules.
    let file: Value = serde_json::from_str(&text).unwrap();
    for _ in 0..5_000 {
        let mut file = file.clone();
        let node = random_node(&mut file, &mut random);
        match node {
            Value::Object(fields) if random.one_in(3) && !fields.is_empty() => {
                let key = fields
                    .keys()
                    .nth(random.below(fields.len() as u64) as usize);
                let key = key.unwrap().clone();
                fields.remove(&key);
            }
            _ => *node = random_value(&mut random),
        }
        check(file.to_string().as_bytes());
    }
}

/// Somewhere in `value`, the whole of it included.
fn random_node<'a>(value: &'a mut Value, random: &mut Random) -> &'a mut Value {
    let children = match value {
        Value::Array(items) => items.len(),
        Value::Object(fields) => fields.len(),
        _ => 0,
    };
    if children == 0 || random.one_in(4) {
        return value;
    }
    let i = random.below(children as u64) as usize;
    match value {
        Value::Array(items) => random_node(&mut items[i], random),
        Value::Object(fields) => random_node(fields.values_mut().nth(i).unwrap(), random),
        _ => unreachable!(),
    }
}

fn random_value(random: &mut Random) -> Value {
    match random.below(9) {
        0 => Value::Null,
        1 => json!(random.one_in(2)),
        2 => json!(random.pick(&[
            0,
            1,
            -1,
            3,
            64,
            255,
            256,
            MAX_TICKS,
            MAX_TICKS + 1,
            -MAX_TICKS - 1,
            i64::MAX,
            i64::MIN,
        ])),
        3 => json!(u64::MAX),
        4 => json!(random.pick(&[0.5, -0.0, 20.0, 999.0, 1e39, 1e308, -1e308, 5e-324])),
        5 => json!(random.f64()),
        6 => json!(random.pick(&["", "audio", "off", "musicable-project", "4/4"])),
        7 => [json!([]), json!({}), json!([{}])][random.below(3) as usize].clone(),
        _ => json!({"bar": 0, "numerator": 4, "denominator": 4}),
    }
}
