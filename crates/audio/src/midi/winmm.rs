//! MIDI inputs through WinMM, the system's MIDI API, which every MIDI
//! driver serves, Windows MIDI Services' included.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use windows::Win32::Media::Audio::{
    CALLBACK_FUNCTION, HMIDIIN, MIDIINCAPSW, midiInClose, midiInGetDevCapsW, midiInGetErrorTextW,
    midiInGetNumDevs, midiInOpen, midiInReset, midiInStart, midiInStop,
};
use windows::Win32::Media::{MM_MIM_DATA, MMSYSERR_ALLOCATED, MMSYSERR_NOERROR};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use super::ring::MidiWriter;
use crate::input::Playback;

/// The MIDI inputs the system has: each one's number, as WinMM numbers
/// them now, and its name.
pub(super) fn devices() -> Vec<(u32, String)> {
    // SAFETY: no arguments.
    let count = unsafe { midiInGetNumDevs() };
    (0..count)
        .filter_map(|number| {
            let mut caps = MIDIINCAPSW::default();
            let size = size_of::<MIDIINCAPSW>() as u32;
            // SAFETY: writes one MIDIINCAPSW, of the size given.
            let result = unsafe { midiInGetDevCapsW(number as usize, &mut caps, size) };
            (result == MMSYSERR_NOERROR).then(|| {
                // Copied out: the struct is packed.
                let name = { caps.szPname };
                let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
                (number, String::from_utf16_lossy(&name[..end]))
            })
        })
        .collect()
}

/// An open MIDI input. It writes what comes in to its buffer until it's
/// dropped, which closes it.
pub(super) struct Input {
    handle: HMIDIIN,
    /// The callback's. Freed once the input is closed, after the last
    /// callback.
    context: *mut Context,
}

// SAFETY: WinMM takes a handle's calls from any thread, and only the
// callback touches the context until the input is dropped.
unsafe impl Send for Input {}

/// What the callback needs.
struct Context {
    /// The callback is the one writer. WinMM makes one callback at a time
    /// for an input, so the lock is never contended.
    writer: Mutex<MidiWriter>,
    playback: Arc<Playback>,
    /// The performance counter's frequency.
    frequency: u64,
}

impl Input {
    /// Opens WinMM's MIDI input `number`. It writes each channel message
    /// that comes in to `writer`, with the engine frame `playback` says is
    /// heard then.
    pub fn open(number: u32, writer: MidiWriter, playback: Arc<Playback>) -> Result<Input, String> {
        let mut frequency = 0;
        // SAFETY: writes one i64. It can't fail on Windows XP or later.
        let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
        let context = Box::into_raw(Box::new(Context {
            writer: Mutex::new(writer),
            playback,
            frequency: (frequency as u64).max(1),
        }));
        let mut handle = HMIDIIN::default();
        // SAFETY: `callback` has the signature WinMM calls, and `context`
        // lives until the input is closed.
        let result = unsafe {
            midiInOpen(
                &mut handle,
                number,
                Some(callback as *const c_void as usize),
                Some(context as usize),
                CALLBACK_FUNCTION,
            )
        };
        if result != MMSYSERR_NOERROR {
            // SAFETY: made above, and WinMM didn't take it.
            drop(unsafe { Box::from_raw(context) });
            return Err(error_text(result));
        }
        let input = Input { handle, context };
        // SAFETY: an open input.
        let result = unsafe { midiInStart(input.handle) };
        if result != MMSYSERR_NOERROR {
            return Err(error_text(result));
        }
        Ok(input)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: an open input, closed once. No callback runs once
        // `midiInClose` returns, so the context can go then.
        unsafe {
            let _ = midiInStop(self.handle);
            let _ = midiInReset(self.handle);
            let _ = midiInClose(self.handle);
            drop(Box::from_raw(self.context));
        }
    }
}

/// What a WinMM error means.
fn error_text(error: u32) -> String {
    if error == MMSYSERR_ALLOCATED {
        return "another app is using it".to_owned();
    }
    let mut text = [0u16; 256];
    // SAFETY: writes up to the buffer's length.
    let result = unsafe { midiInGetErrorTextW(error, &mut text) };
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    match result == MMSYSERR_NOERROR && end > 0 {
        true => String::from_utf16_lossy(&text[..end]),
        false => format!("WinMM error {error}"),
    }
}

/// WinMM's callback, on a thread of WinMM's: takes a channel message that
/// came in. It mustn't call into WinMM, nor block.
unsafe extern "system" fn callback(
    _input: HMIDIIN,
    message: u32,
    instance: usize,
    param1: usize,
    _param2: usize,
) {
    if message != MM_MIM_DATA {
        return;
    }
    // SAFETY: the context `open` gave WinMM, which lives until the input
    // is closed, after the last callback.
    let context = unsafe { &*(instance as *const Context) };
    let [status, a, b, _] = (param1 as u32).to_le_bytes();
    // Channel messages: not the clock, active sensing or other system
    // messages.
    if !(0x80..0xF0).contains(&status) {
        return;
    }
    // Before the output plays, there's no frame to place it on.
    let Some(reading) = context.playback.reading() else {
        return;
    };
    let mut count = 0;
    // SAFETY: writes one i64. It can't fail on Windows XP or later.
    let _ = unsafe { QueryPerformanceCounter(&mut count) };
    let now = u128::from(count as u64) * 1_000_000_000 / u128::from(context.frequency);
    let frame = reading.frame_at(now as u64).max(0.0) as u64;
    if let Ok(mut writer) = context.writer.try_lock() {
        writer.push(frame, [status, a & 0x7F, b & 0x7F]);
    }
}
