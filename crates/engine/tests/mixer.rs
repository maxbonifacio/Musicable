//! The mixer: groups and return tracks sum what feeds them, sends take the
//! signal before or after the fader, pan, mute and solo act where the model
//! says, clip stages clip, devices process in order, meters measure, and
//! every mix sounds the same with the parallel renderer as with the
//! reference renderer.

use std::f64::consts::TAU;

use musicable_engine::compile::keys;
use musicable_engine::nodes::{Gain, Smoothed};
use musicable_engine::{
    AudioMut, Engine, EngineConfig, Event, Meters, ParallelOptions, ParallelRenderer,
    ReferenceRenderer, Renderer, TransportCommand, compile, compile_metered, rt,
};
use musicable_model::mixer::{Bus, pan_gains};
use musicable_model::{ClipType, DeviceKind, Project, TestTone, TrackId, TrackKind};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

const CONFIG: EngineConfig = EngineConfig {
    sample_rate: 48_000,
    max_block: 64,
};

const FRAMES: usize = 2_000;

/// Plays `project` from the start, offline, and returns the left and right
/// channels.
fn play(project: &Project) -> [Vec<f32>; 2] {
    play_metered(project, &Meters::new())
}

fn play_metered(project: &Project, meters: &Meters) -> [Vec<f32>; 2] {
    let (mut engine, mut handle) = Engine::new(CONFIG);
    let graph = compile_metered(project, meters).prepare(&CONFIG).unwrap();
    handle.swap_graph(graph).unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let mut out = vec![0.0; 2 * FRAMES];
    engine.process(&mut AudioMut::from_planar(&mut out, 2));
    let (left, right) = out.split_at(FRAMES);
    [left.to_vec(), right.to_vec()]
}

/// A test tone's samples from the start of the song, as the engine renders
/// them, times `gain`.
fn tone(frequency: f64, level: f32, gain: f32) -> Vec<f32> {
    let cycles = frequency / f64::from(CONFIG.sample_rate);
    (0..FRAMES)
        .map(|i| ((i as f64 * cycles).fract() * TAU).sin() as f32 * level * gain)
        .collect()
}

/// The sum of `signals`, frame by frame.
fn mix(signals: &[Vec<f32>]) -> Vec<f32> {
    (0..FRAMES)
        .map(|i| signals.iter().map(|s| s[i]).sum())
        .collect()
}

/// Whether `actual` is `expected`, give or take rounding.
#[track_caller]
fn close(actual: &[f32], expected: &[f32]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!((a - e).abs() < 1e-5, "frame {i}: {a}, expected {e}");
    }
    assert!(expected.iter().any(|&s| s != 0.0), "a silent test");
}

#[track_caller]
fn silent(signal: &[f32]) {
    assert!(signal.iter().all(|&s| s == 0.0));
}

/// Adds a track with a test tone at `frequency` and level 0.5.
fn add(project: &mut Project, name: &str, kind: TrackKind, frequency: f64) -> TrackId {
    let id = project.add_track(name, kind);
    if frequency > 0.0 {
        project.track_mut(id).unwrap().tone = Some(TestTone {
            frequency,
            level: 0.5,
        });
    }
    id
}

/// Gives `track` a send to `to`.
fn send(project: &mut Project, track: TrackId, to: TrackId, level: f32, pre_fader: bool) {
    let mut send = project.new_send(to);
    send.level = level;
    send.pre_fader = pre_fader;
    project.track_mut(track).unwrap().sends.push(send);
}

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn groups_and_returns_sum_what_feeds_them() {
    let mut p = Project::new();
    let kick = add(&mut p, "Kick", TrackKind::Audio, 100.0);
    let snare = add(&mut p, "Snare", TrackKind::Audio, 210.0);
    let keys = add(&mut p, "Keys", TrackKind::Midi, 330.0);
    let drums = add(&mut p, "Drums", TrackKind::Group, 0.0);
    let band = add(&mut p, "Band", TrackKind::Group, 0.0);
    let reverb = add(&mut p, "Reverb", TrackKind::Return, 0.0);
    // Groups inside groups, and a group sending to the return.
    p.track_mut(kick).unwrap().output = Bus::Group(drums);
    p.track_mut(snare).unwrap().output = Bus::Group(drums);
    p.track_mut(drums).unwrap().output = Bus::Group(band);
    p.track_mut(drums).unwrap().gain = 0.5;
    p.track_mut(band).unwrap().gain = 0.8;
    p.track_mut(reverb).unwrap().gain = 0.5;
    send(&mut p, keys, reverb, 1.0, false);
    send(&mut p, drums, reverb, 0.25, false);

    let [left, right] = play(&p);
    let (kick, snare) = (tone(100.0, 0.5, 1.0), tone(210.0, 0.5, 1.0));
    let drums = mix(&[kick, snare]);
    let keys = tone(330.0, 0.5, 1.0);
    let expected: Vec<f32> = (0..FRAMES)
        .map(|i| drums[i] * 0.5 * 0.8 + keys[i] + (keys[i] + drums[i] * 0.5 * 0.25) * 0.5)
        .collect();
    close(&left, &expected);
    close(&right, &expected);
}

#[test]
fn sends_take_the_signal_before_or_after_the_fader() {
    let mut p = Project::new();
    let lead = add(&mut p, "Lead", TrackKind::Audio, 440.0);
    let pre = add(&mut p, "Pre", TrackKind::Return, 0.0);
    let post = add(&mut p, "Post", TrackKind::Return, 0.0);
    send(&mut p, lead, pre, 0.5, true);
    send(&mut p, lead, post, 0.5, false);
    let track = p.track_mut(lead).unwrap();
    track.gain = 0.25;
    track.pan = 1.0;

    // Before the fader: unpanned, at the send's level. After it: at the
    // fader's gain, and panned right with the track.
    let lead = tone(440.0, 0.5, 1.0);
    let at = |gain: f32| lead.iter().map(|s| s * gain).collect::<Vec<_>>();
    let [left, right] = play(&p);
    close(&left, &at(0.5));
    close(&right, &mix(&[at(0.5), at(0.25), at(0.25 * 0.5)]));

    // With the fader down, only the pre-fader send is left.
    p.tracks[0].gain = 0.0;
    let [left, right] = play(&p);
    close(&left, &at(0.5));
    close(&right, &at(0.5));
}

#[test]
fn pan_and_the_master_strip() {
    let mut p = Project::new();
    let id = add(&mut p, "Tone", TrackKind::Audio, 300.0);
    p.track_mut(id).unwrap().pan = -0.5;
    let signal = tone(300.0, 0.5, 1.0);
    let [left, right] = play(&p);
    close(&left, &signal);
    close(&right, &tone(300.0, 0.5, 0.5));

    p.master.gain = 0.5;
    p.master.pan = 0.75;
    let [left, right] = play(&p);
    close(&left, &tone(300.0, 0.5, 0.5 * 0.25));
    close(&right, &tone(300.0, 0.5, 0.5 * 0.5));
}

#[test]
fn mute_and_solo_cut_each_path_on_its_own() {
    let mut p = Project::new();
    let vocal = add(&mut p, "Vocal", TrackKind::Audio, 220.0);
    let guitar = add(&mut p, "Guitar", TrackKind::Audio, 330.0);
    let reverb = add(&mut p, "Reverb", TrackKind::Return, 0.0);
    send(&mut p, vocal, reverb, 0.5, true);
    let dry = tone(220.0, 0.5, 1.0);
    let wet = tone(220.0, 0.5, 0.5);
    let guitar_tone = tone(330.0, 0.5, 1.0);

    // Soloing the return leaves the wet signal alone.
    p.track_mut(reverb).unwrap().solo = true;
    close(&play(&p)[0], &wet);
    // Soloing the vocal keeps its reverb, which is solo-safe.
    p.track_mut(reverb).unwrap().solo = false;
    p.track_mut(vocal).unwrap().solo = true;
    close(&play(&p)[0], &mix(&[dry.clone(), wet.clone()]));
    // Muting the vocal cuts its pre-fader send too.
    p.track_mut(vocal).unwrap().solo = false;
    p.track_mut(vocal).unwrap().muted = true;
    close(&play(&p)[0], &guitar_tone);
    // Muting the guitar as well leaves nothing.
    p.track_mut(guitar).unwrap().muted = true;
    silent(&play(&p)[1]);
}

#[test]
fn clip_stages_clip_after_the_fader() {
    let mut p = Project::new();
    let id = add(&mut p, "Loud", TrackKind::Audio, 100.0);
    let track = p.track_mut(id).unwrap();
    track.gain = 4.0;
    track.clip_type = ClipType::Hard;
    let hard: Vec<f32> = tone(100.0, 0.5, 4.0)
        .into_iter()
        .map(|s| s.clamp(-1.0, 1.0))
        .collect();
    let [left, _] = play(&p);
    close(&left, &hard);
    assert!(left.contains(&1.0));

    // The master's soft clip stage then rounds off what's above half scale.
    p.master.clip_type = ClipType::Soft;
    let soft: Vec<f32> = hard
        .iter()
        .map(|&s| match s.abs() {
            m if m <= 0.5 => s,
            m => (1.0 - 0.25 / m).copysign(s),
        })
        .collect();
    let [left, _] = play(&p);
    close(&left, &soft);
    assert!(left.iter().all(|&s| s.abs() <= 0.75));
}

#[test]
fn devices_process_in_order_on_tracks_and_the_master() {
    let mut p = Project::new();
    let id = add(&mut p, "Tone", TrackKind::Audio, 500.0);
    let half = p.new_device(DeviceKind::TestGain { gain: 0.5 });
    let invert = p.new_device(DeviceKind::TestGain { gain: -1.0 });
    let double = p.new_device(DeviceKind::TestGain { gain: 2.0 });
    p.track_mut(id).unwrap().devices = vec![half.clone(), invert.clone()];
    p.master.devices = vec![double.clone()];
    let graph = compile(&p).prepare(&CONFIG).unwrap();
    for device in [&half, &invert, &double] {
        assert!(graph.contains(keys::device(device.id)));
    }
    close(&play(&p)[0], &tone(500.0, 0.5, -1.0));
}

#[test]
fn meters_measure_each_channel() {
    let mut p = Project::new();
    let loud = add(&mut p, "Loud", TrackKind::Audio, 200.0);
    let quiet = add(&mut p, "Quiet", TrackKind::Audio, 300.0);
    let muted = add(&mut p, "Muted", TrackKind::Audio, 400.0);
    p.track_mut(quiet).unwrap().gain = 0.25;
    p.track_mut(quiet).unwrap().pan = 1.0;
    p.track_mut(muted).unwrap().muted = true;
    let mut meters = Meters::new();
    meters.follow(&p);
    play_metered(&p, &meters);

    let level = |id| meters.track(id).unwrap().level();
    let peak = |id| level(id).peak;
    // The tones peak at 0.5 within the 2,000 frames, and the meters fall
    // back only a little from there.
    let near = |actual: f32, expected: f32| (actual - expected).abs() < 0.01 * expected;
    assert!(peak(loud).iter().all(|&p| near(p, 0.5)), "{:?}", peak(loud));
    assert!(near(peak(quiet)[1], 0.125), "{:?}", peak(quiet));
    assert_eq!(peak(quiet)[0], 0.0);
    assert_eq!(level(muted), Default::default());
    let master = meters.master().level();
    assert!(master.peak[0] > 0.4 && master.rms[0] > 0.0, "{master:?}");
}

#[test]
fn mixer_parameters_ramp_from_their_frames() {
    let mut p = Project::new();
    let lead = add(&mut p, "Lead", TrackKind::Audio, 440.0);
    let reverb = add(&mut p, "Reverb", TrackKind::Return, 0.0);
    send(&mut p, lead, reverb, 0.0, false);
    let send_id = p.tracks[0].sends[0].id;

    let (mut engine, mut handle) = Engine::new(CONFIG);
    handle
        .swap_graph(compile(&p).prepare(&CONFIG).unwrap())
        .unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let param = |node, param, value| Event::Param { node, param, value };
    let changes = [
        (100, param(keys::track_gain(lead), Gain::GAIN, 0.5)),
        (200, param(keys::track_gain(lead), Gain::PAN, -1.0)),
        (300, param(keys::send(send_id), Gain::GAIN, 1.0)),
        (400, param(keys::master_gain(), Gain::GAIN, 0.25)),
        (500, param(keys::track_output(lead), Gain::MUTE, 1.0)),
    ];
    for (frame, event) in changes {
        handle.send_at(frame, event).unwrap();
    }
    let mut out = vec![0.0; 2 * FRAMES];
    engine.process(&mut AudioMut::from_planar(&mut out, 2));
    let (left, right) = out.split_at(FRAMES);

    let lead = tone(440.0, 0.5, 1.0);
    let expected = |frame: usize| -> [f32; 2] {
        let fader = ramp(frame, 100, 1.0, 0.5);
        let balance = pan_gains(ramp(frame, 200, 0.0, -1.0));
        let send = ramp(frame, 300, 0.0, 1.0);
        let master = ramp(frame, 400, 1.0, 0.25);
        let dry = ramp(frame, 500, 1.0, 0.0);
        balance.map(|balance| {
            let post = lead[frame] * fader * balance;
            (post * dry + post * send) * master
        })
    };
    for frame in 0..FRAMES {
        let [l, r] = expected(frame);
        assert!((left[frame] - l).abs() < 1e-6, "left, frame {frame}");
        assert!((right[frame] - r).abs() < 1e-6, "right, frame {frame}");
    }
}

/// The value on `frame` of a parameter set from `from` to `to` on frame
/// `at`. It ramps from that frame on, as [`Smoothed`] does, and reaches
/// `to` on the ramp's last frame.
fn ramp(frame: usize, at: usize, from: f32, to: f32) -> f32 {
    let frames = (Smoothed::RAMP_SECONDS * f64::from(CONFIG.sample_rate)).round() as usize;
    match frame.checked_sub(at) {
        None => from,
        Some(into) => from + (to - from) * ((into + 1).min(frames) as f32 / frames as f32),
    }
}

/// SplitMix64, for making test mixes. Seeded, so a failure happens again on
/// the next run.
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

    /// From 0 to 2.
    fn gain(&mut self) -> f32 {
        self.below(201) as f32 / 100.0
    }

    /// From -1 to 1, the centre more often than not.
    fn pan(&mut self) -> f32 {
        if self.one_in(2) {
            0.0
        } else {
            self.below(201) as f32 / 100.0 - 1.0
        }
    }

    fn clip_type(&mut self) -> ClipType {
        self.pick(&[ClipType::Off, ClipType::Off, ClipType::Hard, ClipType::Soft])
    }
}

/// A random mix: tracks of every kind, groups in groups, sends before and
/// after the fader, pan, mute, solo, devices and clip types.
fn random_mix(random: &mut Random) -> Project {
    let mut p = Project::new();
    p.metronome = random.one_in(2);
    let kinds = [
        TrackKind::Audio,
        TrackKind::Midi,
        TrackKind::Return,
        TrackKind::Group,
    ];
    for i in 0..1 + random.below(24) {
        let kind = random.pick(&kinds);
        let has_tone = !kind.is_bus() || random.one_in(4);
        let frequency = 50.0 + random.below(2_000) as f64;
        let id = add(
            &mut p,
            &i.to_string(),
            kind,
            if has_tone { frequency } else { 0.0 },
        );
        let track = p.track_mut(id).unwrap();
        track.gain = random.gain();
        track.pan = random.pan();
        track.muted = random.one_in(6);
        track.solo = random.one_in(8);
        track.solo_safe = random.one_in(4);
        track.clip_type = random.clip_type();
    }
    let ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    for &id in &ids {
        p.track_mut(id).unwrap().output = random.pick(&p.buses_for(id));
        for &to in &ids {
            if p.can_send(id, to) && random.one_in(2) {
                let (level, pre_fader) = (random.gain(), random.one_in(2));
                send(&mut p, id, to, level, pre_fader);
            }
        }
        for _ in 0..random.below(3) {
            let gain = random.gain();
            let device = p.new_device(DeviceKind::TestGain { gain });
            p.track_mut(id).unwrap().devices.push(device);
        }
    }
    p.master.gain = random.gain();
    p.master.pan = random.pan();
    p.master.clip_type = random.clip_type();
    p
}

/// Plays `project` through `renderer` while its faders, pans, sends and
/// mutes move on exact frames and it's rerouted and recompiled, and returns
/// everything it played.
fn mix_session(project: &Project, seed: u64, renderer: Box<dyn Renderer>) -> Vec<f32> {
    let mut random = Random(seed);
    let mut project = project.clone();
    let (mut engine, mut handle) = Engine::with_renderer(CONFIG, renderer);
    handle
        .swap_graph(compile(&project).prepare(&CONFIG).unwrap())
        .unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let mut played = Vec::new();
    let mut device = [0.0; 2 * 500];
    for (callback, frames) in [500, 64, 333, 7, 128, 500, 250, 41].into_iter().enumerate() {
        let now = handle.stats().time;
        let param = |node, param, value: f32| Event::Param {
            node,
            param,
            value: f64::from(value),
        };
        match callback {
            1 | 4 => {
                for track in &project.tracks {
                    let at = now + random.below(200);
                    let gain = param(keys::track_gain(track.id), Gain::GAIN, random.gain());
                    handle.send_at(at, gain).unwrap();
                    let pan = param(keys::track_gain(track.id), Gain::PAN, random.pan());
                    handle.send_at(at + 1, pan).unwrap();
                    let muted = if random.one_in(3) { 1.0 } else { 0.0 };
                    let mute = param(keys::track_output(track.id), Gain::MUTE, muted);
                    handle.send_at(now + random.below(300), mute).unwrap();
                    for send in &track.sends {
                        let level = param(keys::send(send.id), Gain::GAIN, random.gain());
                        handle.send_at(now + random.below(300), level).unwrap();
                    }
                }
                let master = param(keys::master_gain(), Gain::GAIN, random.gain());
                handle.send_at(now + random.below(300), master).unwrap();
            }
            3 => {
                // Reroute and recompile: new outputs, clip types and solos.
                let ids: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
                for &id in &ids {
                    let output = random.pick(&project.buses_for(id));
                    let clip_type = random.clip_type();
                    let solo = random.one_in(8);
                    let track = project.track_mut(id).unwrap();
                    (track.output, track.clip_type, track.solo) = (output, clip_type, solo);
                }
                let graph = compile(&project).prepare(&CONFIG).unwrap();
                handle.swap_graph(graph).unwrap();
            }
            _ => {}
        }
        let device = &mut device[..2 * frames];
        engine.process(&mut AudioMut::from_planar(device, 2));
        played.extend_from_slice(device);
        handle.collect_garbage();
    }
    played
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

#[test]
fn every_mix_sounds_the_same_with_the_parallel_renderer() {
    let mut random = Random(7);
    let mut heard = 0;
    for i in 0..40 {
        let project = random_mix(&mut random);
        let seed = random.next();
        let expected = mix_session(&project, seed, Box::new(ReferenceRenderer));
        heard += usize::from(expected.iter().any(|&s| s != 0.0));
        for threads in [1, 4] {
            let renderer = ParallelRenderer::new(ParallelOptions {
                threads,
                inline_below: None,
                realtime: false,
                ..ParallelOptions::default()
            });
            let played = mix_session(&project, seed, Box::new(renderer));
            assert!(same(&played, &expected), "mix {i}, {threads} workers");
        }
    }
    assert!(heard > 30, "only {heard} of the mixes made a sound");
}
