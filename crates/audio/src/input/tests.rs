use std::f64::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use musicable_engine::rt;

use super::*;

/// A tiny deterministic generator, for jitter.
struct Lcg(u64);

impl Lcg {
    /// Uniform in -1..1.
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

#[test]
fn the_buffer_reads_back_what_was_written() {
    let (buffer, mut writer) = InputBuffer::new(2, 8);
    assert_eq!(buffer.capacity(), 8);
    assert_eq!(buffer.frames(), 0..0);
    assert_eq!(buffer.read(0, &mut [0.0; 2]), Err(ReadError::NotYet));
    writer.begin(100);
    writer.append(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    assert_eq!(buffer.frames(), 100..103);
    let mut out = [0.0; 4];
    buffer.read(101, &mut out).unwrap();
    assert_eq!(out, [2.0, -2.0, 3.0, -3.0]);
    let mut channel = [0.0; 3];
    buffer.read_channel(100, 1, &mut channel).unwrap();
    assert_eq!(channel, [-1.0, -2.0, -3.0]);
    assert_eq!(buffer.read(102, &mut [0.0; 4]), Err(ReadError::NotYet));
    assert_eq!(buffer.read(99, &mut [0.0; 2]), Err(ReadError::Gone));
    // Ten more frames: the buffer holds the newest eight.
    let more: Vec<f32> = (4..14).flat_map(|i| [i as f32, -i as f32]).collect();
    writer.append(&more);
    assert_eq!(buffer.frames(), 105..113);
    assert_eq!(buffer.read(104, &mut [0.0; 2]), Err(ReadError::Gone));
    buffer.read_channel(105, 0, &mut channel).unwrap();
    assert_eq!(channel, [6.0, 7.0, 8.0]);
    // A long silence skips ahead.
    writer.append_silence(20);
    assert_eq!(buffer.frames(), 125..133);
    buffer.read_channel(125, 1, &mut channel).unwrap();
    assert_eq!(channel, [0.0; 3]);
}

#[test]
fn readers_never_see_frames_torn_by_the_writer() {
    // A small buffer, so the writer laps readers that fall behind.
    let (buffer, mut writer) = InputBuffer::new(2, 64);
    writer.begin(0);
    let done = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..3)
        .map(|r| {
            let buffer = buffer.clone();
            let done = done.clone();
            thread::spawn(move || {
                let mut out = [0.0; 2 * 48];
                let (mut ok, mut gone) = (0u64, 0u64);
                let mut lag = r as u64 * 7;
                while !done.load(Ordering::Relaxed) {
                    let end = buffer.frames().end;
                    let from = end.saturating_sub(48 + lag % 40);
                    lag += 1;
                    match buffer.read(from, &mut out) {
                        Ok(()) => {
                            ok += 1;
                            for (i, frame) in out.chunks(2).enumerate() {
                                let expected = (from + i as u64) as f32;
                                assert_eq!(frame, [expected, -expected]);
                            }
                        }
                        Err(ReadError::Gone) => gone += 1,
                        Err(ReadError::NotYet) => {}
                    }
                }
                (ok, gone)
            })
        })
        .collect();
    let mut frame = 0u64;
    let mut chunk = Vec::new();
    while frame < 2_000_000 {
        chunk.clear();
        for i in 0..17 {
            let value = (frame + i) as f32;
            chunk.extend([value, -value]);
        }
        writer.append(&chunk);
        frame += 17;
    }
    done.store(true, Ordering::Relaxed);
    for reader in readers {
        let (ok, _gone) = reader.join().unwrap();
        assert!(ok > 0);
    }
}

#[test]
fn the_playback_clock_gives_the_frame_heard_at_any_time() {
    let clock = Playback::default();
    assert_eq!(clock.reading(), None);
    clock.publish(48_000, 2_000_000_000, 48_000);
    let reading = clock.reading().unwrap();
    assert_eq!(reading.frame_at(2_000_000_000), 48_000.0);
    assert_eq!(reading.frame_at(2_500_000_000), 72_000.0);
    assert_eq!(reading.frame_at(1_999_000_000), 47_952.0);
}

#[test]
fn readings_are_never_torn() {
    let clock = Arc::new(Playback::default());
    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let (clock, done) = (clock.clone(), done.clone());
        thread::spawn(move || {
            let mut seen = 0;
            while !done.load(Ordering::Relaxed) {
                if let Some(reading) = clock.reading() {
                    assert_eq!(reading.time, reading.frame * 1_000);
                    seen += 1;
                }
            }
            seen
        })
    };
    for frame in 1..1_000_000 {
        clock.publish(frame, frame * 1_000, 48_000);
    }
    done.store(true, Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
}

/// An output and an input, simulated: the output wakes every period and
/// publishes which frame it's playing; the input delivers packets.
struct Simulation {
    /// The output's rate, exact.
    out_rate: u32,
    out_period: u64,
    /// The input's nominal rate, and how much faster its clock really runs.
    in_rate: u32,
    in_drift: f64,
    in_period: u64,
    /// When the input captured its frame 0, in nanoseconds: the output's
    /// frame 0 is heard at 0.
    in_start: f64,
    /// How far the times in readings and packets can be off, in
    /// nanoseconds.
    jitter: f64,
    /// What the input hears at a time in seconds.
    signal: fn(f64) -> f32,
}

impl Simulation {
    fn in_rate_actual(&self) -> f64 {
        f64::from(self.in_rate) * (1.0 + self.in_drift)
    }

    /// When the input captured frame `c`, in nanoseconds.
    fn captured_at(&self, c: u64) -> f64 {
        self.in_start + c as f64 / self.in_rate_actual() * 1e9
    }

    /// Runs `seconds`, pushing packets into a conformer for an input on
    /// the output's clock or not, and returns the input's buffer and
    /// counters.
    fn run(&self, seconds: f64, same_clock: bool) -> (InputBuffer, Arc<InputCounters>) {
        let clock = Arc::new(Playback::default());
        let counters = Arc::new(InputCounters::default());
        let frames = (seconds * f64::from(self.out_rate)) as u64 + 1;
        let (buffer, writer) = InputBuffer::new(1, frames.next_power_of_two());
        let mut conformer = Conformer::new(
            writer,
            self.in_rate,
            self.out_rate,
            same_clock,
            clock.clone(),
            counters.clone(),
        );
        let mut jitter = Lcg(7);
        let end = seconds * 1e9;
        let mut wakeup = 0u64;
        let mut packet = 0u64;
        // Packets start when the input has captured frame 0.
        let first_packet =
            (-self.in_start / 1e9 * self.in_rate_actual()).max(0.0) as u64 / self.in_period;
        packet = packet.max(first_packet);
        let mut samples = vec![0.0; self.in_period as usize];
        loop {
            let wakeup_at = (wakeup * self.out_period) as f64 / f64::from(self.out_rate) * 1e9;
            let first = packet * self.in_period;
            // A packet arrives once its last frame is captured.
            let packet_at = self.captured_at(first + self.in_period);
            if wakeup_at.min(packet_at) > end {
                break;
            }
            if wakeup_at <= packet_at {
                // The output reads its clock: the frame playing now.
                let frame = wakeup * self.out_period;
                let time = wakeup_at + self.jitter * jitter.next();
                clock.publish(frame, time.max(0.0) as u64, self.out_rate);
                wakeup += 1;
            } else {
                for (i, sample) in samples.iter_mut().enumerate() {
                    let t = self.captured_at(first + i as u64) / 1e9;
                    *sample = (self.signal)(t);
                }
                let time = self.captured_at(first) + self.jitter * jitter.next();
                rt::scope(|| {
                    conformer.push(Packet {
                        position: first,
                        time: Some(time.max(0.0) as u64),
                        frames: self.in_period as usize,
                        samples: Some(&samples),
                    })
                });
                packet += 1;
            }
        }
        (buffer, counters)
    }
}

/// A count of time, in output frames at 48 kHz: what an input on the
/// output's clock should hold at each engine frame.
fn count(t: f64) -> f32 {
    (t * 48_000.0).round() as f32
}

fn tone(t: f64) -> f32 {
    (TAU * 1_000.0 * t).sin() as f32 * 0.5
}

#[test]
fn inputs_on_the_output_clock_line_up_exactly() {
    let sim = Simulation {
        out_rate: 48_000,
        out_period: 480,
        in_rate: 48_000,
        in_drift: 0.0,
        in_period: 480,
        // The input started 12.3 ms before frame 0 was heard: 590.4 frames.
        in_start: -12.3e6,
        jitter: 20_000.0,
        signal: count,
    };
    let (buffer, counters) = sim.run(10.0, true);
    let stats = counters.snapshot();
    assert!(stats.aligned);
    assert_eq!((stats.slips, stats.lost), (0, 0));
    assert_eq!(stats.clock, InputClock::Output);
    // From two seconds in, every frame is the one heard with it, to the
    // nearest frame: 0.4 of a frame off at most.
    let range = buffer.frames();
    assert!(range.start < 96_000 && range.end > 470_000, "{range:?}");
    let mut out = vec![0.0; 300_000];
    buffer.read_channel(96_000, 0, &mut out).unwrap();
    for (i, &sample) in out.iter().enumerate() {
        let w = 96_000 + i as u64;
        // The time, counted at the moment engine frame w was heard.
        let expected = count(w as f64 / 48_000.0);
        assert!(
            (sample - expected).abs() <= 1.0,
            "frame {w}: {sample}, {expected}"
        );
    }
    // And exactly: 590.4 frames rounds to 590, everywhere.
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &s)| s == (96_000 + i) as f32)
    );
}

#[test]
fn a_device_that_loses_frames_leaves_silence_in_their_place() {
    let clock = Arc::new(Playback::default());
    let counters = Arc::new(InputCounters::default());
    let (buffer, writer) = InputBuffer::new(1, 1 << 16);
    let mut conformer = Conformer::new(
        writer,
        48_000,
        48_000,
        true,
        clock.clone(),
        counters.clone(),
    );
    clock.publish(0, 0, 48_000);
    // The input's position matches the engine frame heard, exactly.
    let ones = [1.0; 100];
    let mut position = 1_000;
    for _ in 0..40 {
        if position == 3_000 {
            // The device skips 50 frames.
            position += 50;
        }
        let time = position * 1_000_000_000 / 48_000;
        conformer.push(Packet {
            position,
            time: Some(time),
            frames: 100,
            samples: Some(&ones),
        });
        position += 100;
    }
    let stats = counters.snapshot();
    assert_eq!((stats.lost, stats.slips), (50, 0));
    let mut out = [0.0; 200];
    buffer.read_channel(2_950, 0, &mut out).unwrap();
    assert!(out[..50].iter().all(|&s| s == 1.0));
    assert!(out[50..100].iter().all(|&s| s == 0.0));
    assert!(out[100..].iter().all(|&s| s == 1.0));
}

#[test]
fn an_offset_that_moves_for_good_slips() {
    let clock = Arc::new(Playback::default());
    let counters = Arc::new(InputCounters::default());
    let (buffer, writer) = InputBuffer::new(1, 1 << 16);
    let mut conformer = Conformer::new(
        writer,
        48_000,
        48_000,
        true,
        clock.clone(),
        counters.clone(),
    );
    clock.publish(0, 0, 48_000);
    let mut samples = [0.0; 100];
    for packet in 0..300u64 {
        let position = packet * 100;
        // From packet 200 on, the timestamps say the input is 10 frames
        // later than before.
        let shift = if packet >= 200 { 10 } else { 0 };
        let time = (position + shift) * 1_000_000_000 / 48_000;
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample = (position + i as u64) as f32;
        }
        conformer.push(Packet {
            position,
            time: Some(time),
            frames: 100,
            samples: Some(&samples),
        });
    }
    let stats = counters.snapshot();
    assert_eq!(stats.slips, 1);
    // Before, frame w held position w; after, position w - 10.
    let mut out = [0.0; 1];
    buffer.read_channel(10_000, 0, &mut out).unwrap();
    assert_eq!(out[0], 10_000.0);
    buffer.read_channel(29_000, 0, &mut out).unwrap();
    assert_eq!(out[0], 28_990.0);
}

/// The worst difference between what the buffer holds from `from` on and
/// what the input heard at each frame's time.
fn worst_error(sim: &Simulation, buffer: &InputBuffer, from: u64, frames: usize) -> f32 {
    let mut out = vec![0.0; frames];
    buffer.read_channel(from, 0, &mut out).unwrap();
    out.iter()
        .enumerate()
        .map(|(i, &sample)| {
            let t = (from + i as u64) as f64 / f64::from(sim.out_rate);
            (sample - (sim.signal)(t)).abs()
        })
        .fold(0.0, f32::max)
}

#[test]
fn inputs_on_another_clock_follow_its_drift() {
    for (in_rate, drift) in [(48_000, 50e-6), (48_000, -80e-6), (44_100, 30e-6)] {
        let sim = Simulation {
            out_rate: 48_000,
            out_period: 441,
            in_rate,
            in_drift: drift,
            in_period: 480,
            in_start: -5.0e6,
            jitter: 20_000.0,
            signal: tone,
        };
        let (buffer, counters) = sim.run(30.0, false);
        let stats = counters.snapshot();
        assert!(stats.aligned);
        assert_eq!(stats.clock, InputClock::Resampled);
        assert_eq!(stats.slips, 0, "{in_rate} Hz, {drift}");
        assert!(
            (stats.drift_ppm - drift * 1e6).abs() < 2.0,
            "{in_rate} Hz: drift {} ppm, not {}",
            stats.drift_ppm,
            drift * 1e6
        );
        // Settled, the tone is where it was heard: a 1 kHz tone a
        // microsecond off is 0.003 off.
        let error = worst_error(&sim, &buffer, 20 * 48_000, 8 * 48_000);
        assert!(error < 0.02, "{in_rate} Hz, {drift}: off by {error}");
    }
}

#[test]
fn an_input_that_drifts_after_all_goes_over_to_resampling() {
    // Said to be on the output's clock, but 20 ppm fast: it slips every
    // few seconds until it's resampled.
    let sim = Simulation {
        out_rate: 48_000,
        out_period: 480,
        in_rate: 48_000,
        in_drift: 20e-6,
        in_period: 480,
        in_start: -3.0e6,
        jitter: 20_000.0,
        signal: tone,
    };
    let (buffer, counters) = sim.run(40.0, true);
    let stats = counters.snapshot();
    assert_eq!(stats.clock, InputClock::Resampled);
    assert_eq!(stats.slips, 2);
    assert!(
        (stats.drift_ppm - 20.0).abs() < 2.0,
        "drift {} ppm",
        stats.drift_ppm
    );
    let error = worst_error(&sim, &buffer, 30 * 48_000, 8 * 48_000);
    assert!(error < 0.02, "off by {error}");
    // No gap where it went over: every frame since it lined up is there.
    let range = buffer.frames();
    let mut out = vec![1.0; (range.end - range.start) as usize];
    buffer.read_channel(range.start, 0, &mut out).unwrap();
    let silent = out
        .windows(64)
        .filter(|w| w.iter().all(|&s| s == 0.0))
        .count();
    assert_eq!(silent, 0);
}

#[test]
fn the_bus_delays_inputs_by_how_far_behind_they_are() {
    let (a, mut a_writer) = InputBuffer::new(1, 4_096);
    let (b, mut b_writer) = InputBuffer::new(2, 4_096);
    let mut bus = Bus::new(
        vec![
            BusInput { buffer: a, room: 0 },
            BusInput {
                buffer: b,
                room: 64,
            },
        ],
        256,
        2_000,
    );
    // Before any input starts, the bus is silent.
    let view = bus.fill(1_000, 128);
    assert_eq!(view.channels(), 3);
    assert!(view.iter().all(|c| c.iter().all(|&s| s == 0.0)));
    assert_eq!(bus.delay(), 0);
    // Input A has frames up to 1,100, a count; B up to 900.
    a_writer.begin(0);
    a_writer.append(&(0..1_100).map(|i| i as f32).collect::<Vec<_>>());
    b_writer.begin(500);
    b_writer.append(
        &(500..900)
            .flat_map(|i| [i as f32, -i as f32])
            .collect::<Vec<_>>(),
    );
    // The output wants frames up to 1,128 + 128 = 1,256: A is 156 behind,
    // B 356, plus its room.
    let view = bus.fill(1_128, 128);
    let delay = 356 + 64;
    let from = 1_128 - delay;
    for i in 0..128 {
        let frame = (from + i) as f32;
        assert_eq!(view.channel(0)[i as usize], frame);
        assert_eq!(view.channel(1)[i as usize], frame);
        assert_eq!(view.channel(2)[i as usize], -frame);
    }
    assert_eq!(bus.delay(), delay);
    // B falls further behind than the delay allows: the delay grows to
    // cover it, which repeats a stretch of what's monitored, and counts.
    a_writer.append(&(1_100..1_500).map(|i| i as f32).collect::<Vec<_>>());
    let time = 1_256 + 420;
    let view = bus.fill(time, 128);
    // B now lags 1,804 - 900 = 904 frames, plus its room.
    let delay = 904 + 64;
    for i in 0..128 {
        let frame = (time - delay + i) as f32;
        assert_eq!(view.channel(0)[i as usize], frame);
        assert_eq!(view.channel(1)[i as usize], frame);
    }
    assert_eq!((bus.delay(), bus.late()), (delay, 128));
}

#[test]
fn an_input_that_stalls_holds_nothing_back() {
    let (a, mut a_writer) = InputBuffer::new(1, 1 << 14);
    let (b, mut b_writer) = InputBuffer::new(1, 1 << 14);
    let inputs = vec![
        BusInput { buffer: a, room: 0 },
        BusInput { buffer: b, room: 0 },
    ];
    let mut bus = Bus::new(inputs, 128, 1_000);
    a_writer.begin(0);
    b_writer.begin(0);
    a_writer.append(&[1.0; 1_000]);
    b_writer.append(&[2.0; 1_000]);
    bus.fill(1_000, 100);
    assert_eq!(bus.delay(), 100);
    // A keeps up; B stops.
    let mut time = 1_100;
    while time < 5_000 {
        a_writer.append(&[1.0; 100]);
        let view = bus.fill(time, 100);
        let a = view.channel(0).to_vec();
        let b = view.channel(1).to_vec();
        assert!(a.iter().all(|&s| s == 1.0), "A at {time}");
        if time > 2_100 {
            // More than 1,000 frames behind: stalled, and silent.
            assert!(b.iter().all(|&s| s == 0.0), "B at {time}");
        }
        time += 100;
    }
    assert_eq!(bus.delay(), 1_000, "B held the delay back until it stalled");

    // An input that stopped holds nothing back from the start.
    let (a, mut a_writer) = InputBuffer::new(1, 1 << 14);
    let (b, mut b_writer) = InputBuffer::new(1, 1 << 14);
    let inputs = vec![
        BusInput { buffer: a, room: 0 },
        BusInput {
            buffer: b.clone(),
            room: 0,
        },
    ];
    let mut bus = Bus::new(inputs, 128, 1_000);
    a_writer.begin(0);
    b_writer.begin(0);
    a_writer.append(&[1.0; 1_000]);
    b_writer.append(&[2.0; 500]);
    drop(b_writer);
    assert!(b.has_ended());
    bus.fill(1_000, 100);
    assert_eq!(bus.delay(), 100);
}
