//! Event lists: what the host sends a plugin as it processes or flushes,
//! and what the plugin sends back.

use std::ptr;

use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_END, CLAP_EVENT_NOTE_EXPRESSION,
    CLAP_EVENT_PARAM_GESTURE_BEGIN, CLAP_EVENT_PARAM_GESTURE_END, CLAP_EVENT_PARAM_MOD,
    CLAP_EVENT_PARAM_VALUE, clap_event_header, clap_event_midi, clap_event_note,
    clap_event_note_expression, clap_event_param_gesture, clap_event_param_mod,
    clap_event_param_value, clap_event_type, clap_input_events, clap_note_expression,
    clap_output_events,
};
use clap_sys::id::clap_id;

use super::voices::Voices;

/// Room for one event of any kind the host sends. Every kind starts with
/// its header, so a pointer to the event is a pointer to its header.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union Event {
    header: clap_event_header,
    note: clap_event_note,
    expression: clap_event_note_expression,
    param: clap_event_param_value,
    modulation: clap_event_param_mod,
    midi: clap_event_midi,
}

fn header<T>(type_: clap_event_type, time: u32) -> clap_event_header {
    clap_event_header {
        size: size_of::<T>() as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

impl Event {
    /// A note starting, ending or choked: `type_` says which.
    pub(crate) fn note(
        type_: clap_event_type,
        time: u32,
        note_id: i32,
        port: i16,
        key: i16,
        velocity: f64,
    ) -> Self {
        Self {
            note: clap_event_note {
                header: header::<clap_event_note>(type_, time),
                note_id,
                port_index: port,
                channel: 0,
                key,
                velocity,
            },
        }
    }

    pub(crate) fn expression(
        time: u32,
        expression: clap_note_expression,
        note_id: i32,
        port: i16,
        key: i16,
        value: f64,
    ) -> Self {
        Self {
            expression: clap_event_note_expression {
                header: header::<clap_event_note_expression>(CLAP_EVENT_NOTE_EXPRESSION, time),
                expression_id: expression,
                note_id,
                port_index: port,
                channel: 0,
                key,
                value,
            },
        }
    }

    pub(crate) fn param(time: u32, param: clap_id, value: f64) -> Self {
        Self {
            param: clap_event_param_value {
                header: header::<clap_event_param_value>(CLAP_EVENT_PARAM_VALUE, time),
                param_id: param,
                cookie: ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                value,
            },
        }
    }

    /// Modulation of a parameter for every voice.
    pub(crate) fn modulation(time: u32, param: clap_id, amount: f64) -> Self {
        Self {
            modulation: clap_event_param_mod {
                header: header::<clap_event_param_mod>(CLAP_EVENT_PARAM_MOD, time),
                param_id: param,
                cookie: ptr::null_mut(),
                note_id: -1,
                port_index: -1,
                channel: -1,
                key: -1,
                amount,
            },
        }
    }

    /// Modulation of a parameter for one note's voice: the note with
    /// `note_id` on `key`, or, when `note_id` is -1, every note on `key` on
    /// `channel`.
    pub(crate) fn note_modulation(
        time: u32,
        param: clap_id,
        amount: f64,
        note_id: i32,
        port: i16,
        channel: i16,
        key: i16,
    ) -> Self {
        Self {
            modulation: clap_event_param_mod {
                header: header::<clap_event_param_mod>(CLAP_EVENT_PARAM_MOD, time),
                param_id: param,
                cookie: ptr::null_mut(),
                note_id,
                port_index: port,
                channel,
                key,
                amount,
            },
        }
    }

    pub(crate) fn midi(time: u32, port: u16, data: [u8; 3]) -> Self {
        Self {
            midi: clap_event_midi {
                header: header::<clap_event_midi>(CLAP_EVENT_MIDI, time),
                port_index: port,
                data,
            },
        }
    }

    fn header(&self) -> *const clap_event_header {
        ptr::from_ref(self).cast()
    }
}

/// The events a plugin gets for one block, in order of time. Allocated
/// once: when it's full, more events are dropped and counted.
pub(crate) struct InputEvents {
    events: Vec<Event>,
    pub(crate) dropped: u64,
}

impl InputEvents {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
            dropped: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.events.clear();
    }

    pub(crate) fn is_full(&self) -> bool {
        self.events.len() == self.events.capacity()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Adds an event no earlier than the last, unless the list is full.
    pub(crate) fn push(&mut self, event: Event) {
        if self.is_full() {
            self.dropped += 1;
        } else {
            self.events.push(event);
        }
    }

    /// The list as CLAP takes it. It points here, so it's only good while
    /// the events neither move nor change.
    pub(crate) fn list(&self) -> clap_input_events {
        clap_input_events {
            ctx: ptr::from_ref(self).cast_mut().cast(),
            size: Some(input_size),
            get: Some(input_get),
        }
    }
}

/// # Safety
///
/// `list` must come from [`InputEvents::list`], with its events still in
/// place: for both callbacks.
unsafe fn input_events<'a>(list: *const clap_input_events) -> &'a InputEvents {
    // SAFETY: the caller's promise.
    unsafe { &*(*list).ctx.cast::<InputEvents>() }
}

unsafe extern "C" fn input_size(list: *const clap_input_events) -> u32 {
    // SAFETY: plugins call back with the list they got.
    unsafe { input_events(list) }.events.len() as u32
}

unsafe extern "C" fn input_get(
    list: *const clap_input_events,
    index: u32,
) -> *const clap_event_header {
    // SAFETY: plugins call back with the list they got.
    let events = unsafe { input_events(list) };
    events
        .events
        .get(index as usize)
        .map_or(ptr::null(), Event::header)
}

/// What a plugin tells the host as it processes or flushes: someone
/// changing its parameters in its window, or the plugin changing them
/// itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OutEvent {
    GestureBegin(clap_id),
    Value(clap_id, f64),
    GestureEnd(clap_id),
}

/// Where what a plugin sends back goes.
pub(crate) struct Output<'a> {
    /// Its parameter changes, for the main thread.
    pub(crate) queue: &'a mut rtrb::Producer<OutEvent>,
    /// The voices it plays, while it processes, which learn when it ends
    /// them.
    pub(crate) voices: Option<&'a mut Voices>,
}

/// The list a plugin sends events back on, which puts them in `output`. It
/// points at `output`, so it's only good while `output` stays where it is,
/// and nothing else uses it.
pub(crate) fn output_list(output: &mut Output<'_>) -> clap_output_events {
    clap_output_events {
        ctx: ptr::from_mut(output).cast(),
        try_push: Some(output_try_push),
    }
}

unsafe extern "C" fn output_try_push(
    list: *const clap_output_events,
    event: *const clap_event_header,
) -> bool {
    // SAFETY: plugins call back with the list they got, and pass an event
    // whose header says how big it is.
    let (Some(list), Some(header)) = (unsafe { list.as_ref() }, unsafe { event.as_ref() }) else {
        return false;
    };
    if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
        return true;
    }
    // SAFETY: the list's promise: `ctx` is the output, and only the plugin
    // pushing uses it meanwhile.
    let output = unsafe { &mut *list.ctx.cast::<Output<'_>>() };
    let fits = |size: usize| header.size as usize >= size;
    let out = match header.type_ {
        CLAP_EVENT_PARAM_VALUE if fits(size_of::<clap_event_param_value>()) => {
            // SAFETY: the type and size say what the event is.
            let event = unsafe { &*event.cast::<clap_event_param_value>() };
            OutEvent::Value(event.param_id, event.value)
        }
        CLAP_EVENT_PARAM_GESTURE_BEGIN | CLAP_EVENT_PARAM_GESTURE_END
            if fits(size_of::<clap_event_param_gesture>()) =>
        {
            // SAFETY: as above.
            let event = unsafe { &*event.cast::<clap_event_param_gesture>() };
            if header.type_ == CLAP_EVENT_PARAM_GESTURE_BEGIN {
                OutEvent::GestureBegin(event.param_id)
            } else {
                OutEvent::GestureEnd(event.param_id)
            }
        }
        CLAP_EVENT_NOTE_END if fits(size_of::<clap_event_note>()) => {
            // SAFETY: as above.
            let event = unsafe { &*event.cast::<clap_event_note>() };
            if let Some(voices) = output.voices.as_deref_mut() {
                voices.end(event.note_id, event.channel, event.key);
            }
            return true;
        }
        // Notes a plugin plays go nowhere yet.
        _ => return true,
    };
    output.queue.push(out).is_ok()
}

#[cfg(test)]
mod tests {
    use clap_sys::events::CLAP_EVENT_NOTE_ON;
    use musicable_engine::NoteId;

    use super::*;
    use crate::process::SoundingNote;

    #[test]
    fn lists_hand_out_events_by_index() {
        let mut events = InputEvents::with_capacity(2);
        events.push(Event::param(0, 7, 0.5));
        events.push(Event::midi(3, 0, [0x90, 60, 100]));
        events.push(Event::param(4, 8, 1.0));
        assert_eq!(events.dropped, 1);
        let list = events.list();
        // SAFETY: the list was just made, and `events` stays put.
        unsafe {
            assert_eq!(input_size(&list), 2);
            let first = &*input_get(&list, 0);
            assert_eq!((first.type_, first.time), (CLAP_EVENT_PARAM_VALUE, 0));
            let param = &*input_get(&list, 0).cast::<clap_event_param_value>();
            assert_eq!((param.param_id, param.value), (7, 0.5));
            let midi = &*input_get(&list, 1).cast::<clap_event_midi>();
            assert_eq!((midi.header.time, midi.data), (3, [0x90, 60, 100]));
            assert!(input_get(&list, 2).is_null());
        }
    }

    #[test]
    fn parameter_changes_and_ended_voices_come_back() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(2);
        let mut voices = Voices::new();
        voices.start(SoundingNote {
            note: NoteId(5),
            id: 1,
            key: 60,
            channel: 0,
        });
        let mut output = Output {
            queue: &mut producer,
            voices: Some(&mut voices),
        };
        let list = output_list(&mut output);
        let begin = clap_event_param_gesture {
            header: header::<clap_event_param_gesture>(CLAP_EVENT_PARAM_GESTURE_BEGIN, 0),
            param_id: 3,
        };
        let end = Event::note(CLAP_EVENT_NOTE_END, 0, 1, 0, 60, 0.0);
        let played = Event::note(CLAP_EVENT_NOTE_ON, 0, 9, 0, 64, 1.0);
        let value = Event::param(0, 3, 0.25);
        let push = list.try_push.unwrap();
        // SAFETY: the list was just made, and the output stays put.
        unsafe {
            assert!(push(&list, &begin.header));
            assert!(push(&list, end.header()));
            assert!(push(&list, played.header()));
            assert!(push(&list, value.header()));
            assert!(!push(&list, value.header()), "the queue is full");
        }
        assert_eq!(voices.ended, [NoteId(5)]);
        assert_eq!(consumer.pop(), Ok(OutEvent::GestureBegin(3)));
        assert_eq!(consumer.pop(), Ok(OutEvent::Value(3, 0.25)));
        assert!(consumer.pop().is_err());

        // A flush has no voices, so a voice's end goes nowhere.
        let mut output = Output {
            queue: &mut producer,
            voices: None,
        };
        let list = output_list(&mut output);
        // SAFETY: as above.
        unsafe { assert!(list.try_push.unwrap()(&list, end.header())) };
        assert!(consumer.pop().is_err());
    }
}
