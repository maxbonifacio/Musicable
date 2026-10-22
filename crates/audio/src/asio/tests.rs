//! Streams on the fake driver, with the test playing the driver's thread.

use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use musicable_engine::nodes::Constant;
use musicable_engine::{
    AudioMut, Engine, EngineHandle, Graph, Inputs, Node, NodeKey, ProcessContext, rt,
};

use super::context::CALLBACKS;
use super::detector::{SETTLING, STARTUP};
use super::fake::{Fake, Settings};
use super::stream::{self, OpenStream, Stream};
use super::sys::{self, Long, message, sample_type};
use crate::format::to_int;
use crate::input::{InputClock, InputInfo, StreamInput};
use crate::{
    Api, AsioCapabilities, Capabilities, Device, DriverType, Error, SampleFormat, StreamInfo,
    StreamRequest,
};

/// The tests share the process's one ASIO stream, so they take turns.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

const ID: &str = "asio:{00000000-0000-0000-0000-000000000001}";

fn device() -> Device {
    Device {
        id: ID.into(),
        name: "Fake".into(),
        is_default: false,
        driver_type: DriverType::Asio,
    }
}

fn open(fake: &Arc<Fake>, request: &StreamRequest) -> Result<(StreamInfo, OpenStream), Error> {
    stream::open(request, device(), fake.loader()).map(|(info, _, open)| (info, open))
}

/// An engine for `info` that renders `value` in every channel.
fn engine(info: &StreamInfo, value: f32) -> (Engine, EngineHandle) {
    let config = info.engine_config();
    let (engine, mut handle) = Engine::new(config);
    let mut graph = Graph::new();
    let node = graph.add(NodeKey(1), Constant::new(value));
    graph.set_output(node).unwrap();
    handle.swap_graph(graph.prepare(&config).unwrap()).unwrap();
    (engine, handle)
}

/// Opens the fake as `request` asks, and plays `value` through it.
fn play(
    fake: &Arc<Fake>,
    request: &StreamRequest,
    value: f32,
) -> (StreamInfo, Stream, EngineHandle) {
    let (info, open) = open(fake, request).unwrap();
    let (engine, handle) = engine(&info, value);
    let stream = open.start(engine).unwrap();
    (info, stream, handle)
}

/// Asks for `count` halves in turn, as a punctual driver does.
fn switch(fake: &Fake, count: usize) {
    for _ in 0..count {
        let index = fake.record().next_index;
        fake.switch(index);
        fake.record().next_index = index ^ 1;
    }
}

/// Waits a few seconds at most for `condition`.
fn eventually(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        if Instant::now() > deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
    true
}

/// 32-bit little-endian samples.
fn ints(bytes: &[u8]) -> Vec<i32> {
    let (samples, _) = bytes.as_chunks();
    samples.iter().copied().map(i32::from_le_bytes).collect()
}

/// A request for the largest size the fake takes, 2048 frames, so a busy
/// test machine doesn't make a callback overrun.
fn large() -> StreamRequest {
    StreamRequest {
        period: Some(2048),
        ..StreamRequest::default()
    }
}

#[test]
fn plays_what_the_engine_renders() {
    let _turn = turn();
    drop(Box::new(0));
    assert!(
        rt::checker_installed(),
        "callbacks are checked for allocation"
    );
    let fake = Fake::new(Settings::default());
    let (info, open) = open(&fake, &StreamRequest::default()).unwrap();
    assert_eq!(info.api, Api::Asio);
    assert_eq!(info.device, device());
    assert_eq!(
        (info.sample_rate, info.channels, info.format),
        (48_000, 2, SampleFormat::I32)
    );
    assert_eq!(
        (info.period, info.buffer, info.latency),
        (256, 256, 300),
        "the size the driver prefers, and the latency it reports"
    );
    assert!(info.realtime);
    for output in 0..2 {
        for half in 0..2 {
            assert!(
                fake.output(output, half).iter().all(|&byte| byte == 0),
                "silent before the first callback"
            );
        }
    }

    let (engine, _handle) = engine(&info, 0.5);
    let stream = open.start(engine).unwrap();
    switch(&fake, 4);
    let expected = to_int(0.5, 32);
    for output in 0..2 {
        for half in 0..2 {
            assert!(
                ints(&fake.output(output, half))
                    .iter()
                    .all(|&s| s == expected)
            );
        }
    }
    let stats = stream.stats();
    assert_eq!((stats.wakeups, stats.frames, stats.underruns), (4, 1024, 0));
    assert_eq!(stats.queued, 300, "the latency the driver reports");
    assert_eq!(
        fake.positions_asked(),
        0,
        "each callback said where its half starts"
    );
    assert_eq!(
        fake.outputs_ready(),
        1 + 4,
        "asked once, then told at each half"
    );

    let stopped = stream.stop();
    assert!(stopped.error.is_none(), "{:?}", stopped.error);
    assert_eq!(stopped.engine.stats().xruns, 0);
    assert_eq!(
        fake.threads(),
        ["musicable-asio"],
        "every call but the callbacks' on the stream's thread"
    );
    let calls = fake.calls();
    assert_eq!(calls.iter().filter(|&&call| call == "start").count(), 1);
    assert!(
        calls.ends_with(&["stop", "dispose_buffers", "release"]),
        "{calls:?}"
    );
    assert_eq!(fake.record().live, 0);
}

#[test]
fn asks_the_driver_where_a_half_starts_when_the_callback_does_not_say() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, stream, _handle) = play(&fake, &StreamRequest::default(), 0.0);
    fake.switch_without_time(0);
    fake.switch_without_time(1);
    assert_eq!(fake.positions_asked(), 2);
    assert_eq!(stream.stats().frames, 512);
}

#[test]
fn a_driver_that_calls_back_from_start_plays() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        calls_back_on_start: true,
        ..Settings::default()
    });
    let (_, stream, _handle) = play(&fake, &StreamRequest::default(), 0.5);
    assert_eq!(stream.stats().wakeups, 1, "called back from inside start");
    assert!(
        ints(&fake.output(0, 0))
            .iter()
            .all(|&s| s == to_int(0.5, 32))
    );
    fake.switch(1);
    assert_eq!(stream.stats().wakeups, 2);
}

#[test]
fn sets_the_requested_rate_and_rounds_the_buffer_size_up() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        rate: 44_100.0,
        ..Settings::default()
    });
    let request = StreamRequest {
        sample_rate: Some(48_000),
        period: Some(100),
        ..StreamRequest::default()
    };
    let (info, _open) = open(&fake, &request).unwrap();
    assert_eq!(info.sample_rate, 48_000);
    assert_eq!(fake.settings().rate, 48_000.0);
    assert_eq!(info.period, 128, "the next size the driver takes");
    assert_eq!(fake.record().size, 128);
}

#[test]
fn a_driver_that_refuses_a_size_it_offered_gets_its_preferred_one() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        only_preferred: true,
        ..Settings::default()
    });
    let (info, _open) = open(&fake, &large()).unwrap();
    assert_eq!(info.period, 256);
    let calls = fake.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|&&call| call == "create_buffers")
            .count(),
        2
    );
}

#[test]
fn a_rate_the_driver_cannot_run_at_fails_cleanly() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let request = StreamRequest {
        sample_rate: Some(192_000),
        ..StreamRequest::default()
    };
    let error = open(&fake, &request).err().expect("the driver can't");
    assert!(
        matches!(
            error,
            Error::SampleRate {
                requested: 192_000,
                device: 48_000
            }
        ),
        "{error:?}"
    );
    assert!(!fake.called("create_buffers"));
    assert_eq!(fake.record().live, 0, "the driver was released");
    // Nothing of the stream is left behind.
    let (info, _open) = open(&fake, &StreamRequest::default()).unwrap();
    assert_eq!(info.sample_rate, 48_000);
}

#[test]
fn writes_each_output_in_its_sample_type() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        sample_types: vec![sample_type::INT16_MSB, sample_type::FLOAT64_LSB],
        ..Settings::default()
    });
    let (info, _stream, _handle) = play(&fake, &StreamRequest::default(), 0.5);
    assert_eq!(info.format, SampleFormat::I16, "as the first output has it");
    fake.switch(0);
    let half = to_int(0.5, 16) as i16;
    let first = fake.output(0, 0);
    let (first, _) = first.as_chunks();
    assert!(
        first
            .iter()
            .all(|&sample| i16::from_be_bytes(sample) == half)
    );
    let second = fake.output(1, 0);
    let (second, _) = second.as_chunks();
    assert!(
        second
            .iter()
            .all(|&sample| f64::from_le_bytes(sample) == 0.5)
    );
}

#[test]
fn a_sample_type_we_cannot_write_fails_to_open() {
    let _turn = turn();
    // DSD, which the second output has.
    let dsd: Long = 32;
    let fake = Fake::new(Settings {
        sample_types: vec![sample_type::INT32_LSB, dsd],
        ..Settings::default()
    });
    let error = open(&fake, &StreamRequest::default())
        .err()
        .expect("DSD isn't supported");
    assert!(matches!(error, Error::UnsupportedFormat), "{error:?}");
    assert_eq!(fake.record().live, 0);
}

#[test]
fn counts_the_dropouts_a_driver_reports() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        reports_overloads: true,
        ..Settings::default()
    });
    let (_, stream, _handle) = play(&fake, &large(), 0.0);
    switch(&fake, 1);
    assert_eq!(fake.message(message::OVERLOAD, 0), 1);
    switch(&fake, 1);
    let stats = stream.stats();
    assert_eq!((stats.underruns, stats.lost_frames), (1, 2048));

    // The driver says it reports every dropout, so a skip it didn't report
    // doesn't count.
    switch(&fake, 12);
    fake.skip(2);
    switch(&fake, 1);
    assert_eq!(stream.stats().underruns, 1);
    // A resync request means lost audio too.
    fake.message(message::RESYNC_REQUEST, 0);
    switch(&fake, 1);
    let stats = stream.stats();
    assert_eq!((stats.underruns, stats.lost_frames), (2, 4096));
    assert_eq!(stream.stop().engine.stats().xruns, 2);
}

#[test]
fn counts_a_jump_in_the_sample_position() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, stream, _handle) = play(&fake, &large(), 0.0);
    switch(&fake, 10);
    assert_eq!(stream.stats().underruns, 0);
    // Two halves played that the driver never asked for.
    fake.skip(2);
    switch(&fake, 1);
    let stats = stream.stats();
    assert_eq!((stats.underruns, stats.lost_frames), (1, 4096));
    assert_eq!(stream.stop().engine.stats().xruns, 1);
}

#[test]
fn counts_a_half_asked_for_twice() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, stream, _handle) = play(&fake, &large(), 0.0);
    switch(&fake, 10);
    // The last half again, without the switch to the other.
    let last = fake.record().next_index ^ 1;
    fake.switch(last);
    let stats = stream.stats();
    assert_eq!((stats.underruns, stats.lost_frames), (1, 2048));
}

#[test]
fn counts_a_callback_after_its_half_started_playing() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (info, stream, _handle) = play(&fake, &large(), 0.0);
    // Evenly, with the position and the halves in step, as a driver calls
    // back for every half: more than a quarter of a buffer apart is enough.
    // The time between these doesn't count, so a busy machine can't fail
    // the test.
    for _ in 0..STARTUP + SETTLING {
        thread::sleep(info.period_duration() / 3);
        switch(&fake, 1);
    }
    assert_eq!(stream.stats().underruns, 0);
    // The first callback timed: more than a buffer and the latency after
    // the last one.
    thread::sleep(info.period_duration() * 3);
    switch(&fake, 1);
    let stats = stream.stats();
    assert_eq!((stats.underruns, stats.lost_frames), (1, 2048));
    assert_eq!(stream.stop().engine.stats().xruns, 1);
}

#[test]
fn does_not_time_a_driver_that_calls_back_in_bursts() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (info, stream, _handle) = play(&fake, &large(), 0.0);
    // All at once, as the tests call back.
    switch(&fake, 100);
    thread::sleep(info.period_duration() * 3);
    switch(&fake, 1);
    assert_eq!(stream.stats().underruns, 0);
}

#[test]
fn a_reset_request_ends_the_stream() {
    let _turn = turn();
    for selector in [message::RESET_REQUEST, message::BUFFER_SIZE_CHANGE] {
        let fake = Fake::new(Settings::default());
        let (_, stream, _handle) = play(&fake, &StreamRequest::default(), 0.0);
        switch(&fake, 2);
        assert_eq!(fake.message(selector, 0), 1, "handled");
        assert!(eventually(|| stream.has_ended()));
        assert_eq!(fake.record().live, 0, "closed before the stream ended");
        let stopped = stream.stop();
        assert!(
            matches!(stopped.error, Some(Error::DriverReset)),
            "{:?}",
            stopped.error
        );
    }
}

#[test]
fn a_change_of_rate_ends_the_stream_and_a_latency_change_does_not() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, stream, _handle) = play(&fake, &StreamRequest::default(), 0.0);
    switch(&fake, 2);
    // The rate it runs at already: nothing to do.
    fake.rate_changed(48_000.0);
    fake.settings().latency = 500;
    assert_eq!(fake.message(message::LATENCIES_CHANGED, 0), 1);
    assert!(eventually(|| stream.stats().queued == 500));
    assert!(!stream.has_ended());

    fake.rate_changed(44_100.0);
    assert!(eventually(|| stream.has_ended()));
    let stopped = stream.stop();
    assert!(
        matches!(stopped.error, Some(Error::DriverReset)),
        "{:?}",
        stopped.error
    );
}

#[test]
fn gives_up_on_a_driver_that_stops_calling_back() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let give_up = Duration::from_millis(100);
    let (info, _, open) =
        stream::open_with(&StreamRequest::default(), device(), fake.loader(), give_up).unwrap();
    let (engine, _handle) = engine(&info, 0.0);
    let stream = open.start(engine).unwrap();
    switch(&fake, 2);
    assert!(eventually(|| stream.has_ended()));
    let stopped = stream.stop();
    assert!(
        matches!(stopped.error, Some(Error::Timeout)),
        "{:?}",
        stopped.error
    );
    assert!(!fake.record().running, "stopped");
    assert_eq!(fake.record().live, 0);
}

#[test]
fn opens_one_stream_at_a_time() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, first) = open(&fake, &StreamRequest::default()).unwrap();
    let other = Fake::new(Settings::default());
    let error = open(&other, &StreamRequest::default())
        .err()
        .expect("one is open");
    assert!(
        matches!(&error, Error::Os { message, .. } if message.contains("only be one")),
        "{error:?}"
    );
    assert_eq!(other.record().loads, 0);
    drop(first);
    assert_eq!(fake.record().live, 0);
    let (_, _second) = open(&other, &StreamRequest::default()).unwrap();
}

#[test]
fn shows_the_driver_settings_from_the_stream_thread() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, stream, _handle) = play(&fake, &StreamRequest::default(), 0.0);
    stream.show_driver_settings();
    assert!(eventually(|| fake.called("control_panel")));
    assert_eq!(fake.threads(), ["musicable-asio"]);
    assert!(!stream.has_ended());
}

#[test]
fn capabilities_come_from_the_driver() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        outputs: 1,
        inputs: 70,
        sizes: [32, 1024, 128, -1],
        reports_overloads: true,
        ..Settings::default()
    });
    let capabilities = stream::capabilities(ID, fake.loader()).unwrap();
    assert_eq!(
        capabilities,
        Capabilities {
            sample_rate: 48_000,
            channels: 1,
            min_period: Some(32),
            default_period: Some(128),
            exclusive: None,
            asio: Some(AsioCapabilities {
                sample_rates: vec![44_100, 48_000, 96_000],
                buffer_sizes: vec![32, 64, 128, 256, 512, 1024],
                reports_dropouts: true,
                inputs: 64,
            }),
        }
    );
    assert_eq!(fake.threads(), ["musicable-asio-query"]);
    assert_eq!(fake.record().live, 0);
}

#[test]
fn asking_about_the_open_driver_does_not_load_it_again() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, stream, _handle) = play(&fake, &StreamRequest::default(), 0.0);
    let capabilities = stream::capabilities(ID, fake.loader()).unwrap();
    assert_eq!(capabilities.default_period, Some(256));
    assert_eq!(fake.record().loads, 1);
    drop(stream);
    stream::capabilities(ID, fake.loader()).unwrap();
    assert_eq!(
        fake.record().loads,
        2,
        "loaded to ask once the stream closed"
    );
}

#[test]
fn a_driver_that_fails_to_start_hands_the_engine_back() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        start: sys::HW_MALFUNCTION,
        error: "The device is unplugged",
        ..Settings::default()
    });
    let (info, open) = open(&fake, &StreamRequest::default()).unwrap();
    let (engine, _handle) = engine(&info, 0.0);
    let failed = open.start(engine).err().expect("the driver fails");
    assert!(
        matches!(
            &failed.error,
            Error::Os { call: "ASIOStart", message }
                if message == "the hardware is malfunctioning: The device is unplugged"
        ),
        "{:?}",
        failed.error
    );
    assert_eq!(failed.engine.config().sample_rate, 48_000);
    assert!(
        fake.calls()
            .ends_with(&["start", "dispose_buffers", "release"]),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fake.record().live, 0);
}

#[test]
fn a_driver_that_fails_to_initialize_says_why() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        init: false,
        error: "No hardware found",
        ..Settings::default()
    });
    let error = open(&fake, &StreamRequest::default())
        .err()
        .expect("init fails");
    assert!(
        matches!(&error, Error::Os { call: "initializing the ASIO driver", message }
            if message == "No hardware found"),
        "{error:?}"
    );
    fake.settings().error = "";
    let error = stream::capabilities(ID, fake.loader()).unwrap_err();
    assert!(
        matches!(&error, Error::Os { message, .. } if message == "the driver didn't say why"),
        "{error:?}"
    );
    assert_eq!(fake.record().live, 0);
}

#[test]
fn closing_an_open_stream_releases_the_driver() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, open) = open(&fake, &StreamRequest::default()).unwrap();
    drop(open);
    assert!(!fake.called("start"));
    assert!(
        fake.calls().ends_with(&["dispose_buffers", "release"]),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fake.record().live, 0);
}

#[test]
fn callbacks_after_the_stream_closed_do_nothing() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (_, stream, _handle) = play(&fake, &StreamRequest::default(), 0.0);
    switch(&fake, 1);
    let stats = stream.stats();
    drop(stream);
    // A driver that calls late, with its buffers gone.
    switch(&fake, 1);
    fake.message(message::OVERLOAD, 0);
    fake.message(message::RESET_REQUEST, 0);
    fake.rate_changed(44_100.0);
    assert_eq!(stats.wakeups, 1);
}

#[test]
fn tells_the_driver_what_it_supports() {
    let supported = |selector| {
        // SAFETY: as a driver calls it.
        unsafe {
            (CALLBACKS.message)(
                message::SELECTOR_SUPPORTED,
                selector,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        }
    };
    for selector in [
        message::ENGINE_VERSION,
        message::RESET_REQUEST,
        message::BUFFER_SIZE_CHANGE,
        message::RESYNC_REQUEST,
        message::LATENCIES_CHANGED,
        message::SUPPORTS_TIME_INFO,
        message::OVERLOAD,
    ] {
        assert_eq!(supported(selector), 1, "{selector}");
    }
    // Time code, which we don't take.
    let time_code: Long = 8;
    assert_eq!(supported(time_code), 0);
    // SAFETY: as a driver calls it.
    let ask =
        |selector| unsafe { (CALLBACKS.message)(selector, 0, ptr::null_mut(), ptr::null_mut()) };
    assert_eq!(ask(message::ENGINE_VERSION), 2);
    assert_eq!(ask(message::SUPPORTS_TIME_INFO), 1);
    assert_eq!(ask(time_code), 0);
}

/// Plays device input channel `.0`, or silence if there's no such channel.
struct Echo(usize);

impl Node for Echo {
    fn channels(&self) -> usize {
        1
    }

    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        _inputs: Inputs<'_>,
        output: &mut AudioMut<'_>,
    ) {
        match ctx.input.channels() > self.0 {
            true => output
                .channel_mut(0)
                .copy_from_slice(ctx.input.channel(self.0)),
            false => output.clear(),
        }
    }
}

/// Opens the fake recording from `inputs`, and starts it playing device
/// input channel `echo`.
fn record(
    fake: &Arc<Fake>,
    inputs: &[&str],
    echo: usize,
) -> (StreamInfo, Vec<StreamInput>, Stream, EngineHandle) {
    let request = StreamRequest {
        inputs: inputs.iter().map(|&id| id.to_owned()).collect(),
        ..StreamRequest::default()
    };
    let (info, inputs, open) = stream::open(&request, device(), fake.loader()).unwrap();
    let config = info.engine_config();
    let (engine, mut handle) = Engine::new(config);
    let mut graph = Graph::new();
    let node = graph.add(NodeKey(1), Echo(echo));
    graph.set_output(node).unwrap();
    handle.swap_graph(graph.prepare(&config).unwrap()).unwrap();
    let stream = open.start(engine).unwrap();
    (info, inputs, stream, handle)
}

/// A sample that counts: frame `n` as a 32-bit integer the engine reads as
/// `n / 32768` exactly.
fn counting(n: u64) -> i32 {
    (n as i32) << 16
}

#[test]
fn records_the_drivers_inputs_where_they_were_played() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let (info, inputs, stream, _handle) = record(&fake, &[ID], 1);
    assert_eq!(
        info.inputs,
        [InputInfo {
            id: ID.into(),
            name: "Fake".into(),
            channels: 2,
            first_channel: 0,
            sample_rate: 48_000,
            clock: InputClock::Output,
            error: None,
        }]
    );
    // Channel 0 is noise, channel 1 counts the frames the driver captured.
    let period = 256;
    let halves = 16;
    for k in 0..halves {
        let index = fake.record().next_index;
        let noise: Vec<u8> = (0..period).flat_map(|_| 12_345_i32.to_le_bytes()).collect();
        let count: Vec<u8> = (0..period)
            .flat_map(|i| counting(k * period + i).to_le_bytes())
            .collect();
        fake.capture(0, index as usize, &noise);
        fake.capture(1, index as usize, &count);
        switch(&fake, 1);
        // An input half's first frame was captured the input latency before
        // its buffer switch, and the half rendered then plays the output
        // latency after: 290 + 300 frames of round trip, so engine frame
        // `n` plays what was captured as frame `n` was heard, 590 frames
        // later.
        let played = ints(&fake.output(0, index as usize));
        for (i, &sample) in played.iter().enumerate() {
            let frame = k * period + i as u64;
            let expected = if frame >= 590 { counting(frame) } else { 0 };
            assert_eq!(sample, expected, "frame {frame}");
        }
    }
    // The recording has what was captured as each engine frame was heard.
    let buffer = inputs[0].buffer.clone().unwrap();
    let frames = buffer.frames();
    assert_eq!(frames, 0..halves * period - 590);
    let mut recorded = vec![0.0; frames.end as usize];
    buffer.read_channel(0, 1, &mut recorded).unwrap();
    for (frame, &sample) in recorded.iter().enumerate() {
        assert_eq!(sample, (frame + 590) as f32 / 32_768.0, "frame {frame}");
    }
    buffer.read_channel(0, 0, &mut recorded).unwrap();
    assert!(recorded.iter().all(|&s| s == 12_345.0 / 2_147_483_648.0));

    let stats = stream.stats();
    assert_eq!((stats.input_delay, stats.input_late), (590, 0));
    let input = inputs[0].counters.snapshot();
    assert_eq!(input.captured, halves * period);
    assert_eq!((input.lost, input.slips, input.error), (0, 0, None));
    assert!(input.aligned);
    assert_eq!(input.clock, InputClock::Output);
}

#[test]
fn a_driver_that_cannot_record_still_plays() {
    let _turn = turn();
    let fake = Fake::new(Settings {
        refuses_inputs: true,
        ..Settings::default()
    });
    let (info, inputs, stream, _handle) = record(&fake, &[ID], 0);
    assert_eq!(info.inputs[0].channels, 0);
    assert!(info.inputs[0].error.is_some(), "{:?}", info.inputs);
    assert!(inputs[0].buffer.is_none());
    assert!(inputs[0].counters.snapshot().error.is_some());
    switch(&fake, 4);
    assert_eq!(fake.record().inputs.len(), 0, "made buffers without inputs");
    assert!(ints(&fake.output(0, 0)).iter().all(|&s| s == 0));
    assert!(!stream.has_ended());
}

#[test]
fn inputs_it_cannot_open_are_silent_and_say_why() {
    let _turn = turn();
    let fake = Fake::new(Settings::default());
    let other = "asio:{00000000-0000-0000-0000-000000000002}";
    let system = "{0.0.1.00000000}.{00000000-0000-0000-0000-000000000003}";
    let (info, inputs, _stream, _handle) = record(&fake, &[other, ID, system], 2);
    let opened: Vec<(u16, usize, bool)> = info
        .inputs
        .iter()
        .map(|input| (input.channels, input.first_channel, input.error.is_none()))
        .collect();
    assert_eq!(opened, [(0, 0, false), (2, 0, true), (0, 2, false)]);
    let error = info.inputs[0].error.as_deref().unwrap();
    assert!(error.contains("one driver at a time"), "{error}");
    assert_eq!(inputs[1].counters.snapshot().error, None);
    switch(&fake, 4);
    // The engine sees the driver's two channels, and only those.
    assert!(ints(&fake.output(0, 1)).iter().all(|&s| s == 0));
}
