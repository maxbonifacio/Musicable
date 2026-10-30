//! Plugins for testing Musicable's plugin hosting. Not shipped.
//!
//! The crate builds a CLAP shared library holding three plugins:
//!
//! - **Test Gain** (`org.musicable.test.gain`), a stereo effect that
//!   multiplies its input by its Gain parameter and delays it by its Latency
//!   parameter, which it reports to the host as its latency. Changing the
//!   latency asks the host to restart the plugin, as real plugins do. Its
//!   Bypass parameter is the plugin's own bypass, as CLAP lets a plugin say:
//!   on, it plays its input unchanged, but as late. A second input, its
//!   Sidechain (port [`SIDECHAIN`]), ducks it: each sample it plays is its
//!   input's times one less the sidechain's size, up to 1, as late as each
//!   other. The host switches the sidechain on and off with CLAP's
//!   audio-ports-activation, which it can only while the plugin is
//!   inactive, and hands it silence marked constant while it's off:
//!   [`musicable_test_sidechain`] says whether it's on.
//! - **Test Synth** (`org.musicable.test.synth`), an instrument that plays
//!   each note as a sine at the note's pitch, shaped by CLAP's note
//!   expressions for tuning, pressure and brightness. It can render its
//!   voices on the host's thread pool.
//! - **Test Arpeggiator** (`org.musicable.test.arpeggiator`), a MIDI effect,
//!   with CLAP's `note-effect` feature and no audio, that plays the keys
//!   held in turn, lowest first, a step of [`ARP_STEP`] each, its Transpose
//!   parameter's semitones up. It starts stepping with the first key held,
//!   and the note it plays ends once none is held.
//!
//! All three check that the host keeps CLAP's rules: which thread calls
//! what, and in which state. Each breach counts as a violation, which tests
//! read with [`musicable_test_violations`]. Tests also play the part of a
//! plugin's window: [`musicable_test_gui`] edits a parameter as someone
//! turning a knob would, and [`musicable_test_set_label`] changes state
//! that isn't a parameter. The plugins' windows are pretend ones that only
//! record what the host asks of them.
//!
//! The synth plays notes from MIDI too, as MPE has them or as a keyboard
//! does: pitch bend, channel and key pressure, and controller 74 shape them
//! as the note expressions do, and registered parameter 0 sets how far a
//! channel bends. Whichever way its notes come, the sustain pedal
//! (controller 64) holds the notes let go. It keeps a log of the MIDI it
//! gets, which tests read with [`musicable_test_midi`].
//!
//! Tests copy the library to a `.clap` file of their own, with
//! [`testing::Library`], and the file's name can make it misbehave as broken
//! plugins do, for the scanner to catch: a name starting with `crash` makes
//! it crash as the host initializes it, `hang` makes it never return from
//! initializing, and `refuse` makes it fail to initialize. A name starting
//! with `mpe` makes the synth an MPE synth, which takes notes only as MIDI
//! with MPE, as many do. One starting with `fixed` gives the gain no
//! audio-ports-activation, so its sidechain is always on, as with many
//! plugins. And one starting with `midi` has the arpeggiator play MIDI
//! rather than CLAP's own notes.
//!
//! The library is also a VST3 module with the same three plugins, which
//! check VST3's rules, in the same count: see [`vst3`]. Tests make a VST3
//! bundle of their own with it, with [`testing::Library::copy_vst3`], and
//! its name misbehaves in the same way.

use std::ffi::{CStr, c_char, c_void};
use std::path::Path;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::version::CLAP_VERSION;

mod arp;
mod plugin;
pub mod testing;
pub mod vst3;

pub use arp::ARP_STEP;
pub use plugin::{
    ARPEGGIATOR_ID, BYPASS, GAIN, GAIN_ID, LATENCY, LEVEL, SIDECHAIN, SYNTH_ID, THREADS, TRANSPOSE,
};

/// The CLAP entry point the host looks for.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static clap_entry: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(entry_init),
    deinit: Some(entry_deinit),
    get_factory: Some(entry_get_factory),
};

/// Whether the library is initialized: between the entry's `init` and
/// `deinit`.
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Whether the synth takes notes only as MIDI with MPE: the library's file
/// is named so.
pub(crate) static MPE: AtomicBool = AtomicBool::new(false);

/// Whether the arpeggiator plays MIDI rather than CLAP's own notes: the
/// library's file is named so.
pub(crate) static MIDI_OUT: AtomicBool = AtomicBool::new(false);

/// Whether the gain has no audio-ports-activation, so its sidechain is
/// always on: the library's file is named so.
pub(crate) static FIXED_PORTS: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn entry_init(path: *const c_char) -> bool {
    let name = if path.is_null() {
        String::new()
    } else {
        // SAFETY: the host passes a nul-terminated path.
        let path = unsafe { CStr::from_ptr(path) }.to_string_lossy();
        Path::new(path.as_ref())
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    if name.starts_with("crash") {
        // SAFETY: none. It crashes, on purpose.
        unsafe { ptr::null_mut::<u8>().write_volatile(1) };
    }
    if name.starts_with("hang") {
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    if name.starts_with("refuse") {
        return false;
    }
    MPE.store(name.starts_with("mpe"), Ordering::SeqCst);
    MIDI_OUT.store(name.starts_with("midi"), Ordering::SeqCst);
    FIXED_PORTS.store(name.starts_with("fixed"), Ordering::SeqCst);
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        violation("the library was initialized twice without deinit in between");
    }
    true
}

unsafe extern "C" fn entry_deinit() {
    if !INITIALIZED.swap(false, Ordering::SeqCst) {
        violation("deinit came without init");
    }
}

unsafe extern "C" fn entry_get_factory(id: *const c_char) -> *const c_void {
    if !INITIALIZED.load(Ordering::SeqCst) {
        violation("get_factory came before init");
    }
    if id.is_null() {
        return ptr::null();
    }
    // SAFETY: the host passes a nul-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_PLUGIN_FACTORY_ID {
        (&raw const FACTORY).cast()
    } else {
        ptr::null()
    }
}

static FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(factory_count),
    get_plugin_descriptor: Some(factory_descriptor),
    create_plugin: Some(factory_create),
};

unsafe extern "C" fn factory_count(_: *const clap_plugin_factory) -> u32 {
    plugin::DESCRIPTORS.len() as u32
}

unsafe extern "C" fn factory_descriptor(
    _: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    plugin::DESCRIPTORS
        .get(index as usize)
        .map_or(ptr::null(), |d| ptr::from_ref(*d))
}

unsafe extern "C" fn factory_create(
    _: *const clap_plugin_factory,
    host: *const clap_host,
    id: *const c_char,
) -> *const clap_plugin {
    if host.is_null() || id.is_null() {
        violation("create_plugin got a null host or id");
        return ptr::null();
    }
    // SAFETY: the host passes a nul-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    plugin::create(host, id)
}

/// Every breach of CLAP's rules seen so far, with the first one's message.
static VIOLATIONS: Mutex<(u32, Option<String>)> = Mutex::new((0, None));

/// Counts a breach of CLAP's rules by the host.
pub(crate) fn violation(message: &str) {
    eprintln!("musicable-test-plugins: host broke a rule: {message}");
    let mut violations = VIOLATIONS.lock().unwrap_or_else(|e| e.into_inner());
    violations.0 += 1;
    violations.1.get_or_insert_with(|| message.to_owned());
}

/// How many times a host broke CLAP's rules with the plugins of this copy
/// of the library.
#[unsafe(no_mangle)]
pub extern "C" fn musicable_test_violations() -> u32 {
    VIOLATIONS.lock().unwrap_or_else(|e| e.into_inner()).0
}

/// Copies the message of the first violation into `buffer`, which holds
/// `capacity` bytes, nul-terminated and cut short if need be. Returns
/// whether there was one.
///
/// # Safety
///
/// `buffer` must be valid for writing `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_first_violation(
    buffer: *mut c_char,
    capacity: u32,
) -> bool {
    let violations = VIOLATIONS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(message) = &violations.1 else {
        return false;
    };
    // SAFETY: the caller's promise.
    unsafe { plugin::copy_str(message, buffer, capacity as usize) };
    true
}

/// What a pretend user does in a plugin's window, for
/// [`musicable_test_gui`].
pub mod gui {
    /// Starts dragging a parameter's knob.
    pub const BEGIN: u32 = 0;
    /// Moves it to a value.
    pub const VALUE: u32 = 1;
    /// Lets go.
    pub const END: u32 = 2;
}

/// Acts in the plugin's window as a user would: [`gui::BEGIN`] starts
/// dragging the knob of the parameter `param`, [`gui::VALUE`] turns it to
/// `value` and [`gui::END`] lets go. The plugin applies each as it next
/// processes, or flushes its parameters, and tells the host as plugins do,
/// with gesture and value events. Call it on the main thread, as a window
/// would.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_gui(
    plugin: *const clap_plugin,
    action: u32,
    param: u32,
    value: f64,
) {
    // SAFETY: the caller's promise.
    unsafe { plugin::from_clap(plugin) }.gui_action(action, param, value);
}

/// Changes the plugin's label, state that isn't a parameter, and tells the
/// host its state changed, as a plugin does when someone picks a preset or
/// loads a sample in its window. Call it on the main thread.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed, and
/// `label` a nul-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_set_label(
    plugin: *const clap_plugin,
    label: *const c_char,
) {
    // SAFETY: the caller's promise.
    let label = unsafe { CStr::from_ptr(label) }.to_string_lossy();
    // SAFETY: the caller's promise.
    unsafe { plugin::from_clap(plugin) }.set_label(&label);
}

/// Copies the plugin's label into `buffer`, as [`musicable_test_first_violation`]
/// does with its message.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed, and
/// `buffer` valid for writing `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_label(
    plugin: *const clap_plugin,
    buffer: *mut c_char,
    capacity: u32,
) {
    // SAFETY: the caller's promise.
    let label = unsafe { plugin::from_clap(plugin) }.label();
    // SAFETY: the caller's promise.
    unsafe { plugin::copy_str(&label, buffer, capacity as usize) };
}

/// Asks the host to call the plugin's `on_main_thread` soon, from whatever
/// thread calls it.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_request_callback(plugin: *const clap_plugin) {
    // SAFETY: the caller's promise.
    unsafe { plugin::from_clap(plugin) }.request_callback();
}

/// How many times the host has called the plugin's `on_main_thread`.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_main_thread_calls(plugin: *const clap_plugin) -> u32 {
    // SAFETY: the caller's promise.
    unsafe { plugin::from_clap(plugin) }.main_thread_calls()
}

/// The plugin's pretend window: -1 if there is none, 0 if it's hidden and 1
/// if it's showing. `parent` gets the window the host put it in, or 0.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed, and
/// `parent` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_window(
    plugin: *const clap_plugin,
    parent: *mut usize,
) -> i32 {
    // SAFETY: the caller's promise.
    let (state, window) = unsafe { plugin::from_clap(plugin) }.window();
    // SAFETY: the caller's promise.
    unsafe { parent.write(window) };
    state
}

/// Asks the host to resize the plugin's window, as a plugin does when
/// someone drags its corner. Returns the host's answer.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_request_resize(
    plugin: *const clap_plugin,
    width: u32,
    height: u32,
) -> bool {
    // SAFETY: the caller's promise.
    unsafe { plugin::from_clap(plugin) }.request_resize(width, height)
}

/// How many times the synth handed its voices to the host's thread pool,
/// and how many distinct threads ran them.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed, and the
/// pointers valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_pool_use(
    plugin: *const clap_plugin,
    requests: *mut u32,
    threads: *mut u32,
) {
    // SAFETY: the caller's promise.
    let (r, t) = unsafe { plugin::from_clap(plugin) }.pool_use();
    // SAFETY: the caller's promise.
    unsafe {
        requests.write(r);
        threads.write(t);
    }
}

/// How many times the host started the plugin processing, stopped it and
/// reset it, and how many blocks of frames it processed, into `counts`, in
/// that order.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed, and
/// `counts` valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_lifecycle(
    plugin: *const clap_plugin,
    counts: *mut [u32; 4],
) {
    // SAFETY: the caller's promise.
    let lifecycle = unsafe { plugin::from_clap(plugin) }.lifecycle();
    // SAFETY: the caller's promise.
    unsafe { counts.write(lifecycle) };
}

/// Whether the gain's sidechain is on, as the host last switched it.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_sidechain(plugin: *const clap_plugin) -> bool {
    // SAFETY: the caller's promise.
    unsafe { plugin::from_clap(plugin) }.sidechain()
}

/// Counts the plugins of this library alive now, for tests that check the
/// host destroys what it creates.
pub(crate) static ALIVE: AtomicU32 = AtomicU32::new(0);

/// How many plugins of this copy of the library are alive.
#[unsafe(no_mangle)]
pub extern "C" fn musicable_test_alive() -> u32 {
    ALIVE.load(Ordering::SeqCst)
}

/// The transport the host last reported to the plugin, as it processed:
/// CLAP's transport flags, the song position in CLAP's fixed-point beats,
/// and the tempo. Returns whether it has reported one.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed, and the
/// pointers valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_transport(
    plugin: *const clap_plugin,
    flags: *mut u32,
    beats: *mut i64,
    tempo: *mut f64,
) -> bool {
    // SAFETY: the caller's promise.
    let Some((f, b, t)) = (unsafe { plugin::from_clap(plugin) }).transport() else {
        return false;
    };
    // SAFETY: the caller's promise.
    unsafe {
        flags.write(f);
        beats.write(b);
        tempo.write(t);
    }
    true
}

/// Takes the MIDI messages the synth got since the last time, oldest first:
/// each is its status byte and two data bytes, from the lowest byte up.
/// Copies up to `capacity` of them into `buffer`, and returns how many
/// there were, which is more than the synth logs if it got more than it
/// keeps. Call it between blocks.
///
/// # Safety
///
/// `plugin` must be a plugin of this library that isn't destroyed, and
/// `buffer` valid for writing `capacity` messages.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_midi(
    plugin: *const clap_plugin,
    buffer: *mut u32,
    capacity: u32,
) -> u32 {
    // SAFETY: the caller's promise.
    let buffer = unsafe { std::slice::from_raw_parts_mut(buffer, capacity as usize) };
    // SAFETY: the caller's promise.
    let logged = unsafe { plugin::from_clap(plugin) }.take_midi(buffer);
    logged.try_into().unwrap_or(u32::MAX)
}
