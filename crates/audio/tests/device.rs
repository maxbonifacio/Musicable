//! Streams on real devices.
//!
//! The tests that play need an output device, so they're ignored by default.
//! Run them one at a time, since an exclusive-mode stream locks other
//! streams out of the device:
//!
//! ```text
//! cargo test -p musicable-audio --test device -- --ignored --test-threads 1
//! ```

use std::thread;
use std::time::Duration;

use musicable_audio::{self as audio, Error, Mode, StreamRequest};
use musicable_engine::nodes::Sine;
use musicable_engine::{Engine, EngineConfig, Event, Graph, NodeKey, TransportCommand, rt};

#[global_allocator]
static ALLOC: rt::CheckedAlloc = rt::CheckedAlloc;

#[test]
fn the_checker_is_installed() {
    drop(Box::new(0));
    assert!(rt::checker_installed());
}

#[test]
fn an_unknown_device_is_not_found() {
    let request = StreamRequest {
        device: Some("no such device".into()),
        ..StreamRequest::default()
    };
    let error = audio::open(&request).err().expect("no device has that id");
    assert!(matches!(error, Error::DeviceNotFound), "{error:?}");
    let error = audio::capabilities(Some("no such device")).unwrap_err();
    assert!(matches!(error, Error::DeviceNotFound), "{error:?}");
}

#[test]
fn listing_devices_works_without_any() {
    // CI machines have no audio devices, and some have no audio service. All
    // that matters there is that listing fails cleanly, if at all.
    match audio::output_devices() {
        Ok(devices) => assert!(devices.iter().all(|device| !device.id.is_empty())),
        Err(error) => println!("listing failed: {error}"),
    }
}

#[test]
fn listing_inputs_works_without_any() {
    match audio::input_devices() {
        Ok(inputs) => {
            assert!(inputs.iter().all(|input| !input.id.is_empty()));
            assert!(inputs.iter().filter(|input| input.is_default).count() <= 1);
        }
        Err(error) => println!("listing failed: {error}"),
    }
}

#[test]
fn an_unknown_input_does_not_stop_the_stream_opening() {
    // On a machine without an output device, opening fails before inputs
    // matter: that's for the other tests.
    let request = StreamRequest {
        inputs: vec!["no such input".into()],
        ..StreamRequest::default()
    };
    let Ok(open) = audio::open(&request) else {
        return;
    };
    let inputs = &open.info().inputs;
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].channels, 0);
    assert!(inputs[0].error.is_some(), "{inputs:?}");
    assert!(open.input_buffers()[0].is_none());
}

#[cfg(windows)]
#[test]
#[ignore = "needs an input device"]
fn records_from_the_default_input() {
    let inputs = audio::input_devices().unwrap();
    let input = inputs.iter().find(|input| input.is_default).unwrap();
    record(&input.id);
}

#[cfg(windows)]
#[test]
#[ignore = "needs an output device"]
fn records_the_default_outputs_loopback() {
    let outputs = audio::output_devices().unwrap();
    let output = outputs.iter().find(|output| output.is_default).unwrap();
    record(&audio::loopback_id(&output.id));
}

/// Records from `input` for two seconds, with the default output.
#[cfg(windows)]
fn record(input: &str) {
    let request = StreamRequest {
        inputs: vec![input.to_owned()],
        ..StreamRequest::default()
    };
    let open = audio::open(&request).unwrap();
    let info = open.info().clone();
    println!("{:#?}", info.inputs);
    assert_eq!(info.inputs[0].error, None);
    let buffer = open.input_buffers()[0].clone().unwrap();
    let (engine, _handle) = Engine::new(info.engine_config());
    let stream = open.start(engine).unwrap();
    thread::sleep(Duration::from_secs(2));
    let stats = stream.input_stats().remove(0);
    let delay = stream.stats().input_delay;
    let frames = buffer.frames();
    drop(stream.stop());

    println!("{stats:#?}, monitoring delay {delay} frames, frames {frames:?}");
    assert!(stats.aligned && stats.error.is_none(), "{stats:?}");
    assert_eq!((stats.lost, stats.slips), (0, 0), "{stats:?}");
    // Lined up within half a second, and it kept up.
    let rate = u64::from(info.sample_rate);
    assert!(frames.end - frames.start > rate, "{frames:?}");
    assert!(delay > 0 && delay < rate / 4, "delay {delay}");
}

#[test]
#[ignore = "needs an output device"]
fn the_default_output_is_listed() {
    let devices = audio::output_devices().unwrap();
    let defaults: Vec<_> = devices.iter().filter(|device| device.is_default).collect();
    assert_eq!(defaults.len(), 1, "{devices:#?}");
    let capabilities = audio::capabilities(None).unwrap();
    assert!(capabilities.sample_rate > 0 && capabilities.channels > 0);
    assert_eq!(
        audio::capabilities(Some(&defaults[0].id)).unwrap(),
        capabilities
    );
}

#[test]
#[ignore = "needs an output device"]
fn plays_through_the_default_output() {
    play(&StreamRequest::default());
}

#[test]
#[ignore = "needs an output device"]
fn plays_at_a_requested_period() {
    play(&StreamRequest {
        period: Some(256),
        ..StreamRequest::default()
    });
}

#[cfg(windows)]
#[test]
#[ignore = "needs an output device that allows exclusive mode"]
fn plays_in_exclusive_mode() {
    play(&StreamRequest {
        mode: Mode::Exclusive,
        ..StreamRequest::default()
    });
}

#[test]
#[ignore = "needs an output device"]
fn an_engine_at_another_rate_is_handed_back() {
    let open = audio::open(&StreamRequest::default()).unwrap();
    let rate = open.info().sample_rate + 1;
    let (engine, _handle) = Engine::new(EngineConfig {
        sample_rate: rate,
        max_block: 128,
    });
    let failed = open.start(engine).err().expect("the rates differ");
    assert!(
        matches!(failed.error, Error::EngineRate { engine, .. } if engine == rate),
        "{failed:?}"
    );
    assert_eq!(failed.engine.config().sample_rate, rate);
}

/// Plays a tone through `request`'s stream for two seconds.
fn play(request: &StreamRequest) {
    let open = audio::open(request).unwrap();
    let info = open.info().clone();
    println!("{info:#?}");
    assert!(info.period > 0 && info.buffer >= info.period, "{info:?}");
    assert!(info.latency >= info.period, "{info:?}");
    if request.mode == Mode::Exclusive {
        assert_eq!(info.latency, info.buffer, "{info:?}");
    }

    let config = info.engine_config();
    let (engine, mut handle) = Engine::new(config);
    let mut graph = Graph::new();
    let tone = graph.add(NodeKey(1), Sine::new(440.0, 0.05));
    graph.set_output(tone).unwrap();
    handle.swap_graph(graph.prepare(&config).unwrap()).unwrap();
    handle
        .send(Event::Transport(TransportCommand::Play))
        .unwrap();
    let stream = open.start(engine).unwrap();
    thread::sleep(Duration::from_secs(2));
    assert!(!stream.has_ended());
    let stats = stream.stats();
    let stopped = stream.stop();

    println!("{stats:#?}");
    assert!(stopped.error.is_none(), "{:?}", stopped.error);
    let played = stats.frames as f64 / f64::from(info.sample_rate);
    assert!((1.5..2.5).contains(&played), "played {played} s");
    assert!(stats.wakeups > 0);
    assert_eq!(stats.underruns, 0, "{stats:#?}");
    let engine = stopped.engine.stats();
    assert!(engine.playing && engine.position > 0);
    assert_eq!(engine.xruns, 0);
    handle.collect_garbage();
}
