use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use musicable_engine::{MidiMessage, rt};

use super::bus::MAX_MESSAGES;
use super::*;

const ON: [u8; 3] = [0x90, 60, 100];
const OFF: [u8; 3] = [0x80, 60, 0];

fn event(frame: u64, bytes: [u8; 3]) -> MidiEvent {
    MidiEvent { frame, bytes }
}

fn message(time: u64, input: usize, bytes: [u8; 3]) -> MidiMessage {
    MidiMessage { time, input, bytes }
}

#[test]
fn a_reader_takes_each_message_once_its_frame_is_due() {
    let (buffer, mut writer) = MidiBuffer::new(16);
    let mut reader = buffer.reader();
    assert_eq!(reader.next_before(u64::MAX), None);
    writer.push(10, ON);
    writer.push(20, OFF);
    assert_eq!(reader.next_before(15), Some(event(10, ON)));
    assert_eq!(reader.next_before(15), None, "not due yet");
    assert_eq!(reader.next_before(21), Some(event(20, OFF)));
    assert_eq!(reader.next_before(u64::MAX), None);
    // Every reader reads them all, from the oldest the buffer holds.
    let mut another = buffer.clone().reader();
    assert_eq!(another.next_before(u64::MAX), Some(event(10, ON)));
    // A reader from now only gets what comes next.
    let mut from_now = buffer.reader_from_now();
    writer.push(30, ON);
    assert_eq!(from_now.next_before(u64::MAX), Some(event(30, ON)));
    assert_eq!(from_now.next_before(u64::MAX), None);
    assert_eq!((reader.lost(), from_now.lost()), (0, 0));
}

#[test]
fn frames_never_go_back() {
    let (buffer, mut writer) = MidiBuffer::new(16);
    writer.push(20, ON);
    writer.push(10, OFF);
    let mut reader = buffer.reader();
    assert_eq!(reader.next_before(u64::MAX), Some(event(20, ON)));
    assert_eq!(reader.next_before(u64::MAX), Some(event(20, OFF)));
}

#[test]
fn a_reader_that_falls_behind_loses_the_oldest() {
    let (buffer, mut writer) = MidiBuffer::new(3);
    assert_eq!(buffer.capacity(), 4);
    let mut reader = buffer.reader();
    for frame in 0..10 {
        writer.push(frame, ON);
    }
    let frames: Vec<u64> = std::iter::from_fn(|| reader.next_before(u64::MAX))
        .map(|event| event.frame)
        .collect();
    assert_eq!(frames, [6, 7, 8, 9]);
    assert_eq!(reader.lost(), 6);
    // A new reader starts from the oldest left.
    assert_eq!(buffer.reader().next_before(u64::MAX), Some(event(6, ON)));
}

#[test]
fn readers_never_see_messages_torn_by_the_writer() {
    // A small buffer, so the writer laps readers that fall behind.
    let (buffer, mut writer) = MidiBuffer::new(64);
    let done = Arc::new(AtomicBool::new(false));
    let readers: Vec<_> = (0..3)
        .map(|_| {
            let mut reader = buffer.reader();
            let done = done.clone();
            thread::spawn(move || {
                let mut read = 0u64;
                let mut last = None;
                loop {
                    // Once the writer is done, read what's left.
                    let finished = done.load(Ordering::Acquire);
                    while let Some(event) = reader.next_before(u64::MAX) {
                        let n = event.frame;
                        assert_eq!(event.bytes, [0x90, (n % 128) as u8, (n / 128 % 128) as u8]);
                        assert!(last < Some(n), "{last:?} then {n}");
                        last = Some(n);
                        read += 1;
                    }
                    if finished {
                        return (read, reader.lost());
                    }
                }
            })
        })
        .collect();
    let total = 1_000_000;
    for n in 0..total {
        writer.push(n, [0x90, (n % 128) as u8, (n / 128 % 128) as u8]);
    }
    done.store(true, Ordering::Release);
    for reader in readers {
        let (read, lost) = reader.join().unwrap();
        assert_eq!(read + lost, total);
        assert!(read > 0);
    }
}

#[test]
fn a_buffer_settles_as_it_says_and_ends_with_its_writer() {
    let (buffer, writer) = MidiBuffer::new(16);
    assert_eq!(buffer.settled(), 0);
    buffer.settle(100);
    buffer.settle(50);
    assert_eq!(buffer.settled(), 100, "it never goes back");
    assert!(!buffer.has_ended());
    drop(writer);
    assert!(buffer.has_ended());
    assert_eq!(buffer.settled(), u64::MAX);
}

#[test]
fn the_bus_plays_messages_a_fixed_delay_after_they_were_heard() {
    let (buffer, mut writer) = MidiBuffer::new(64);
    let mut bus = MidiBus::new(vec![(0, buffer.clone())]);
    writer.push(100, ON);
    writer.push(130, OFF);
    // Before the output plays its first frame, they wait.
    assert_eq!(bus.fill(0, 256, None, 48_000), []);
    assert_eq!(bus.fill(256, 256, Some(-44.0), 48_000), []);
    assert_eq!(bus.delay(), 0);
    // Frames 512..768 render while frame 212 is heard: 556 ahead. The
    // messages heard before frame 768 - 556 = 212 play now.
    let messages: [MidiMessage; 2] =
        rt::scope(|| bus.fill(512, 256, Some(212.5), 48_000).try_into().unwrap());
    assert_eq!(messages, [message(656, 0, ON), message(686, 0, OFF)]);
    assert_eq!((bus.delay(), bus.late()), (556, 0));
    // A callback that renders further ahead makes the delay grow. One that
    // renders less far ahead leaves it.
    writer.push(400, ON);
    assert_eq!(bus.fill(768, 256, Some(400.0), 48_000), []);
    assert_eq!(bus.delay(), 624);
    let messages = bus.fill(1_024, 256, Some(800.0), 48_000);
    assert_eq!(messages, [message(1_024, 0, ON)]);
    assert_eq!((bus.delay(), bus.late()), (624, 0));
}

#[test]
fn the_bus_takes_every_input_in_order_of_time() {
    let (a, mut a_writer) = MidiBuffer::new(64);
    let (b, mut b_writer) = MidiBuffer::new(64);
    // The stream's second MIDI input didn't open.
    let mut bus = MidiBus::new(vec![(0, a), (2, b)]);
    bus.fill(0, 100, Some(0.0), 48_000);
    a_writer.push(10, ON);
    a_writer.push(40, OFF);
    b_writer.push(20, ON);
    b_writer.push(40, OFF);
    let messages = bus.fill(100, 100, Some(100.0), 48_000);
    assert_eq!(
        messages,
        [
            message(110, 0, ON),
            message(120, 2, ON),
            message(140, 0, OFF),
            message(140, 2, OFF),
        ]
    );
}

#[test]
fn messages_that_come_too_late_play_at_once_and_count() {
    let (buffer, mut writer) = MidiBuffer::new(4_096);
    let mut bus = MidiBus::new(vec![(0, buffer)]);
    bus.fill(0, 100, Some(0.0), 48_000);
    bus.fill(100, 100, Some(100.0), 48_000);
    // Written after the callback that should have taken it: it plays at
    // the start of the next.
    writer.push(0, ON);
    let messages = bus.fill(200, 100, Some(200.0), 48_000);
    assert_eq!(messages, [message(100, 0, ON)]);
    assert_eq!((bus.delay(), bus.late()), (100, 1));
    // A callback takes so many at most, and leaves the rest for the next,
    // where they're late.
    for _ in 0..MAX_MESSAGES + 10 {
        writer.push(250, OFF);
    }
    assert_eq!(bus.fill(300, 100, Some(300.0), 48_000).len(), MAX_MESSAGES);
    assert_eq!(bus.late(), 1);
    let rest = bus.fill(400, 100, Some(400.0), 48_000);
    assert_eq!(rest.len(), 10);
    assert!(rest.iter().all(|m| m.time == 350));
    assert_eq!(bus.late(), 1 + 10);
}

#[test]
fn the_bus_settles_its_inputs_a_moment_behind_what_is_heard() {
    let (buffer, _writer) = MidiBuffer::new(64);
    let mut bus = MidiBus::new(vec![(0, buffer.clone())]);
    bus.fill(1_000, 100, Some(1_000.0), 48_000);
    assert_eq!(buffer.settled(), 1_000 - 240, "5 ms at 48 kHz");
}

#[test]
fn a_bus_without_inputs_is_quiet() {
    let mut bus = MidiBus::default();
    assert_eq!(bus.fill(0, 256, Some(1_000.0), 48_000), []);
    assert_eq!((bus.delay(), bus.late()), (0, 0));
}

#[test]
fn devices_with_the_same_name_get_numbers() {
    let names = ["Keys", "Pads", "Keys", "Keys"].map(str::to_owned);
    assert_eq!(ids(&names), ["Keys", "Pads", "Keys #2", "Keys #3"]);
}

#[test]
fn inputs_that_do_not_open_say_why() {
    let id = "no such MIDI input".to_owned();
    let inputs = MidiInputs::open(std::slice::from_ref(&id), &Arc::default());
    assert_eq!(inputs.buffers.len(), 1);
    assert!(inputs.buffers[0].is_none());
    let info = &inputs.infos[0];
    assert_eq!((&info.id, &info.name), (&id, &id));
    assert!(info.error.is_some());
    assert!(inputs.bus().fill(0, 256, Some(0.0), 48_000).is_empty());
}
