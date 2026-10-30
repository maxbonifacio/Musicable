//! The plugins: one type for all three, which tells them apart by
//! [`Kind`].

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::f64::consts::TAU;
use std::ffi::{CStr, c_char, c_void};
use std::fmt::Write as _;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::events::{
    CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_MIDI, CLAP_EVENT_NOTE_CHOKE, CLAP_EVENT_NOTE_END,
    CLAP_EVENT_NOTE_EXPRESSION, CLAP_EVENT_NOTE_OFF, CLAP_EVENT_NOTE_ON,
    CLAP_EVENT_PARAM_GESTURE_BEGIN, CLAP_EVENT_PARAM_GESTURE_END, CLAP_EVENT_PARAM_MOD,
    CLAP_EVENT_PARAM_VALUE, CLAP_EVENT_TRANSPORT, CLAP_NOTE_EXPRESSION_BRIGHTNESS,
    CLAP_NOTE_EXPRESSION_PRESSURE, CLAP_NOTE_EXPRESSION_TUNING, clap_event_header, clap_event_midi,
    clap_event_note, clap_event_note_expression, clap_event_param_gesture, clap_event_param_mod,
    clap_event_param_value, clap_event_transport, clap_input_events, clap_output_events,
};
use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, CLAP_PORT_STEREO, clap_audio_port_info,
    clap_plugin_audio_ports,
};
use clap_sys::ext::audio_ports_activation::{
    CLAP_EXT_AUDIO_PORTS_ACTIVATION, CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT,
    clap_plugin_audio_ports_activation,
};
use clap_sys::ext::gui::{
    CLAP_EXT_GUI, CLAP_WINDOW_API_COCOA, CLAP_WINDOW_API_WIN32, CLAP_WINDOW_API_X11,
    clap_gui_resize_hints, clap_host_gui, clap_plugin_gui, clap_window,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency, clap_plugin_latency};
use clap_sys::ext::log::{CLAP_EXT_LOG, clap_host_log};
use clap_sys::ext::note_ports::{
    CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI,
    CLAP_NOTE_DIALECT_MIDI_MPE, clap_note_port_info, clap_plugin_note_ports,
};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, CLAP_PARAM_IS_BYPASS, CLAP_PARAM_IS_MODULATABLE,
    CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID, CLAP_PARAM_IS_STEPPED, clap_host_params,
    clap_param_info, clap_plugin_params,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state, clap_plugin_state};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::ext::thread_pool::{
    CLAP_EXT_THREAD_POOL, clap_host_thread_pool, clap_plugin_thread_pool,
};
use clap_sys::host::clap_host;
use clap_sys::id::{CLAP_INVALID_ID, clap_id};
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::plugin_features::{
    CLAP_PLUGIN_FEATURE_AUDIO_EFFECT, CLAP_PLUGIN_FEATURE_INSTRUMENT,
    CLAP_PLUGIN_FEATURE_NOTE_EFFECT, CLAP_PLUGIN_FEATURE_STEREO, CLAP_PLUGIN_FEATURE_SYNTHESIZER,
    CLAP_PLUGIN_FEATURE_UTILITY,
};
use clap_sys::process::{
    CLAP_PROCESS_CONTINUE, CLAP_PROCESS_ERROR, clap_process, clap_process_status,
};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};

use crate::arp::{Arp, Stepped};
use crate::{
    ALIVE, ECHO, ECHO_UP, FIXED_PORTS, MIDI_OUT, MPE, PARTS, gui, part_transpose, violation,
};

pub const GAIN_ID: &CStr = c"org.musicable.test.gain";
pub const SYNTH_ID: &CStr = c"org.musicable.test.synth";
pub const ARPEGGIATOR_ID: &CStr = c"org.musicable.test.arpeggiator";

/// A nul-terminated list of feature strings.
struct Features<const N: usize>([*const c_char; N]);

// SAFETY: the pointers are to static strings, which nothing writes.
unsafe impl<const N: usize> Sync for Features<N> {}

static GAIN_FEATURES: Features<4> = Features([
    CLAP_PLUGIN_FEATURE_AUDIO_EFFECT.as_ptr(),
    CLAP_PLUGIN_FEATURE_UTILITY.as_ptr(),
    CLAP_PLUGIN_FEATURE_STEREO.as_ptr(),
    ptr::null(),
]);

static SYNTH_FEATURES: Features<4> = Features([
    CLAP_PLUGIN_FEATURE_INSTRUMENT.as_ptr(),
    CLAP_PLUGIN_FEATURE_SYNTHESIZER.as_ptr(),
    CLAP_PLUGIN_FEATURE_STEREO.as_ptr(),
    ptr::null(),
]);

static ARPEGGIATOR_FEATURES: Features<3> = Features([
    CLAP_PLUGIN_FEATURE_NOTE_EFFECT.as_ptr(),
    CLAP_PLUGIN_FEATURE_UTILITY.as_ptr(),
    ptr::null(),
]);

static GAIN_DESCRIPTOR: clap_plugin_descriptor = descriptor(
    GAIN_ID,
    c"Test Gain",
    c"Multiplies by its gain and delays by its latency.",
    &GAIN_FEATURES.0,
);

static SYNTH_DESCRIPTOR: clap_plugin_descriptor = descriptor(
    SYNTH_ID,
    c"Test Synth",
    c"Plays each note as a sine.",
    &SYNTH_FEATURES.0,
);

static ARPEGGIATOR_DESCRIPTOR: clap_plugin_descriptor = descriptor(
    ARPEGGIATOR_ID,
    c"Test Arpeggiator",
    c"Plays the notes held in turn.",
    &ARPEGGIATOR_FEATURES.0,
);

pub(crate) static DESCRIPTORS: [&clap_plugin_descriptor; 3] =
    [&GAIN_DESCRIPTOR, &SYNTH_DESCRIPTOR, &ARPEGGIATOR_DESCRIPTOR];

const fn descriptor(
    id: &'static CStr,
    name: &'static CStr,
    description: &'static CStr,
    features: &'static [*const c_char],
) -> clap_plugin_descriptor {
    clap_plugin_descriptor {
        clap_version: CLAP_VERSION,
        id: id.as_ptr(),
        name: name.as_ptr(),
        vendor: c"Musicable".as_ptr(),
        url: c"".as_ptr(),
        manual_url: c"".as_ptr(),
        support_url: c"".as_ptr(),
        version: c"1.0.0".as_ptr(),
        description: description.as_ptr(),
        features: features.as_ptr(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Gain,
    Synth,
    Arpeggiator,
}

/// A parameter, as the plugin describes it.
struct Param {
    id: clap_id,
    name: &'static str,
    min: f64,
    max: f64,
    default: f64,
    flags: u32,
}

/// The ids are far from the parameters' indices, so a host that mixes the
/// two up gets the wrong parameter.
pub const GAIN: clap_id = 10;
pub const LATENCY: clap_id = 11;
pub const BYPASS: clap_id = 12;
pub const LEVEL: clap_id = 20;
pub const THREADS: clap_id = 21;
pub const TRANSPOSE: clap_id = 30;

/// The gain's sidechain input port's id, far from its index, 1, so a host
/// that mixes the two up feeds the wrong port.
pub const SIDECHAIN: clap_id = 7;

const GAIN_PARAMS: [Param; 3] = [
    Param {
        id: GAIN,
        name: "Gain",
        min: 0.0,
        max: 2.0,
        default: 1.0,
        flags: CLAP_PARAM_IS_AUTOMATABLE | CLAP_PARAM_IS_MODULATABLE,
    },
    Param {
        id: LATENCY,
        name: "Latency",
        min: 0.0,
        max: 48_000.0,
        default: 0.0,
        flags: CLAP_PARAM_IS_STEPPED,
    },
    // The plugin's own bypass, as many effects have: it plays its input
    // unchanged, as late as its latency makes it.
    Param {
        id: BYPASS,
        name: "Bypass",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        flags: CLAP_PARAM_IS_BYPASS | CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_AUTOMATABLE,
    },
];

const SYNTH_PARAMS: [Param; 2] = [
    Param {
        id: LEVEL,
        name: "Level",
        min: 0.0,
        max: 1.0,
        default: 0.5,
        flags: CLAP_PARAM_IS_AUTOMATABLE
            | CLAP_PARAM_IS_MODULATABLE
            | CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID,
    },
    Param {
        id: THREADS,
        name: "Threads",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        flags: CLAP_PARAM_IS_STEPPED,
    },
];

const ARPEGGIATOR_PARAMS: [Param; 1] = [Param {
    id: TRANSPOSE,
    name: "Transpose",
    min: -24.0,
    max: 24.0,
    default: 0.0,
    flags: CLAP_PARAM_IS_STEPPED | CLAP_PARAM_IS_AUTOMATABLE,
}];

/// Notes the synth plays at once. Its thread pool tasks are one per voice.
const VOICES: usize = 16;

/// How many threads running thread pool tasks the synth tells apart.
const POOL_THREADS: usize = 16;

/// How many MIDI messages the synth's log holds.
const MIDI_LOG: usize = 4096;

/// MIDI's pitch bend at rest.
const CENTRE: u16 = 8192;

/// A number of the calling thread's own, to tell threads apart. Not the
/// address of a thread-local: a library loaded at run time may allocate a
/// thread's copy of those the first time the thread touches one, as glibc
/// does, and the threads that run tasks are real-time.
fn thread_mark() -> usize {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            // A `pthread_t` is a pointer, or an integer as wide.
            safe fn pthread_self() -> usize;
        }
        pthread_self()
    }
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            safe fn GetCurrentThreadId() -> u32;
        }
        GetCurrentThreadId() as usize
    }
}

/// The host's extensions, as the plugin found them in `init`.
#[derive(Clone, Copy)]
struct HostExtensions {
    params: *const clap_host_params,
    state: *const clap_host_state,
    latency: *const clap_host_latency,
    thread_check: *const clap_host_thread_check,
    thread_pool: *const clap_host_thread_pool,
    gui: *const clap_host_gui,
    log: *const clap_host_log,
}

impl HostExtensions {
    const NONE: Self = Self {
        params: ptr::null(),
        state: ptr::null(),
        latency: ptr::null(),
        thread_check: ptr::null(),
        thread_pool: ptr::null(),
        gui: ptr::null(),
        log: ptr::null(),
    };
}

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    active: bool,
    note_id: i32,
    port: i16,
    channel: i16,
    key: i16,
    velocity: f64,
    /// Semitones.
    tuning: f64,
    pressure: f64,
    brightness: f64,
    /// Modulation of the level for this voice alone, on top of the
    /// modulation for every voice.
    modulation: f64,
    /// In cycles.
    phase: f64,
    /// Let go while the sustain pedal was down: it sounds until the pedal
    /// comes up.
    held: bool,
}

/// What MIDI set on one of its channels, which shapes the notes on it.
#[derive(Debug, Clone, Copy)]
struct MidiChannel {
    /// Pitch bend, MIDI's 14 bits.
    bend: u16,
    /// How far it bends either way, in semitones.
    range: f64,
    pressure: f64,
    /// Controller 74's.
    brightness: f64,
    /// The registered parameter data entry sets, coarse then fine: none is
    /// 127 and 127.
    rpn: [u8; 2],
}

impl MidiChannel {
    const DEFAULT: Self = Self {
        bend: CENTRE,
        range: 2.0,
        pressure: 0.0,
        brightness: 0.0,
        rpn: [127, 127],
    };

    /// Its pitch bend, in semitones.
    fn tuning(&self) -> f64 {
        (f64::from(self.bend) - f64::from(CENTRE)) / f64::from(CENTRE) * self.range
    }
}

/// What only the thread processing the plugin touches, or the main thread
/// while the plugin is inactive.
struct Audio {
    sample_rate: f64,
    max_frames: u32,
    /// The gain's.
    gain: f64,
    bypassed: bool,
    /// Whether the sidechain was on as the plugin was activated, and how
    /// late it's heard, as late as the input.
    sidechain: bool,
    latency: usize,
    delay: [Vec<f32>; 2],
    side_delay: [Vec<f32>; 2],
    position: usize,
    /// The synth's.
    level: f64,
    threads: bool,
    voices: [Voice; VOICES],
    /// One block per voice, for rendering on the host's thread pool.
    buffers: [Vec<f32>; VOICES],
    /// Modulation of the first parameter, on top of its value.
    modulation: f64,
    /// The frames of the block the thread pool's tasks render.
    job: (usize, usize),
    /// The MIDI channels, as MIDI set them.
    channels: [MidiChannel; 16],
    /// Whether the sustain pedal is down. It holds every note, whatever its
    /// channel, as MPE's master channel does.
    sustain: bool,
    /// The arpeggiator's.
    arp: Arp,
}

/// What the plugin's pretend window holds.
#[derive(Debug, Default)]
struct Window {
    created: bool,
    floating: bool,
    parent: usize,
    visible: bool,
    width: u32,
    height: u32,
}

pub(crate) struct TestPlugin {
    clap: clap_plugin,
    kind: Kind,
    /// Whether it's the synth taking notes only as MIDI with MPE.
    mpe: bool,
    /// Whether it's the synth playing each channel as a part of its own.
    parts: bool,
    /// Whether it's the synth playing the notes it gets out again.
    echo: bool,
    host: *const clap_host,
    /// Written once, in `init`, on the main thread.
    ext: UnsafeCell<HostExtensions>,
    initialized: AtomicBool,
    active: AtomicBool,
    processing: AtomicBool,
    /// Whether the gain's sidechain is on, as the host switched it with
    /// audio-ports-activation. CLAP has every port on at first.
    sidechain: AtomicBool,
    /// The parameters' values, as the main thread sees them, as bits.
    values: [AtomicU64; 3],
    /// How many times the host started the plugin processing, stopped it
    /// and reset it, and how many blocks of frames it processed.
    lifecycle: [AtomicU32; 4],
    /// The gain's latency since it was last activated.
    active_latency: AtomicU32,
    label: Mutex<String>,
    main_thread_calls: AtomicU32,
    /// What the pretend user did in the window, for the plugin to apply as
    /// it next processes or flushes: an action, a parameter and a value.
    gui_actions: Mutex<VecDeque<(u32, clap_id, f64)>>,
    window: Mutex<Window>,
    /// Thread pool requests that the host ran.
    pool_requests: AtomicU32,
    /// The threads that ran thread pool tasks, each as the address of a
    /// thread-local, which differs from thread to thread: up to
    /// [`POOL_THREADS`] of them. Atomic, as the tasks run in parallel and
    /// on the host's real-time threads.
    pool_threads: [AtomicUsize; POOL_THREADS],
    /// The transport as the host last reported it: flags, song position in
    /// beats and tempo, the last two as bits.
    transport: [AtomicU64; 3],
    /// Whether the host has reported a transport.
    has_transport: AtomicBool,
    /// Whether the pretend window has actions waiting, so processing only
    /// looks at the lock when it has to.
    gui_waiting: AtomicBool,
    /// The MIDI messages the synth got, oldest first, each as its status
    /// and two data bytes, from the lowest byte up: up to [`MIDI_LOG`] of
    /// them. Atomic, as the thread processing writes it.
    midi_log: [AtomicU32; MIDI_LOG],
    /// How many messages it got since a test last took them, which can be
    /// more than the log holds.
    midi_logged: AtomicUsize,
    audio: UnsafeCell<Audio>,
}

// SAFETY: the host calls a plugin from several threads, and CLAP's rules say
// which call comes from where. Shared state is atomic or locked; `audio` is
// only touched by the thread processing, or by the main thread while the
// plugin is inactive, and `ext` is written once before anything reads it.
unsafe impl Send for TestPlugin {}
// SAFETY: as above.
unsafe impl Sync for TestPlugin {}

pub(crate) fn create(host: *const clap_host, id: &CStr) -> *const clap_plugin {
    // SAFETY: the host passed a host struct.
    if !clap_version_is_compatible(unsafe { (*host).clap_version }) {
        violation("the host's CLAP version is incompatible");
        return ptr::null();
    }
    let kind = if id == GAIN_ID {
        Kind::Gain
    } else if id == SYNTH_ID {
        Kind::Synth
    } else if id == ARPEGGIATOR_ID {
        Kind::Arpeggiator
    } else {
        return ptr::null();
    };
    let (descriptor, params): (_, &[Param]) = match kind {
        Kind::Gain => (&GAIN_DESCRIPTOR, &GAIN_PARAMS),
        Kind::Synth => (&SYNTH_DESCRIPTOR, &SYNTH_PARAMS),
        Kind::Arpeggiator => (&ARPEGGIATOR_DESCRIPTOR, &ARPEGGIATOR_PARAMS),
    };
    let plugin = Box::new(TestPlugin {
        clap: clap_plugin {
            desc: descriptor,
            plugin_data: ptr::null_mut(),
            init: Some(plugin_init),
            destroy: Some(plugin_destroy),
            activate: Some(plugin_activate),
            deactivate: Some(plugin_deactivate),
            start_processing: Some(plugin_start_processing),
            stop_processing: Some(plugin_stop_processing),
            reset: Some(plugin_reset),
            process: Some(plugin_process),
            get_extension: Some(plugin_get_extension),
            on_main_thread: Some(plugin_on_main_thread),
        },
        kind,
        mpe: kind == Kind::Synth && MPE.load(Ordering::SeqCst),
        parts: kind == Kind::Synth && PARTS.load(Ordering::SeqCst),
        echo: kind == Kind::Synth && ECHO.load(Ordering::SeqCst),
        host,
        ext: UnsafeCell::new(HostExtensions::NONE),
        initialized: AtomicBool::new(false),
        active: AtomicBool::new(false),
        processing: AtomicBool::new(false),
        sidechain: AtomicBool::new(kind == Kind::Gain),
        values: std::array::from_fn(|i| {
            AtomicU64::new(params.get(i).map_or(0.0, |p| p.default).to_bits())
        }),
        lifecycle: std::array::from_fn(|_| AtomicU32::new(0)),
        active_latency: AtomicU32::new(0),
        label: Mutex::new(String::new()),
        main_thread_calls: AtomicU32::new(0),
        gui_actions: Mutex::new(VecDeque::new()),
        window: Mutex::new(Window::default()),
        pool_requests: AtomicU32::new(0),
        pool_threads: std::array::from_fn(|_| AtomicUsize::new(0)),
        transport: std::array::from_fn(|_| AtomicU64::new(0)),
        has_transport: AtomicBool::new(false),
        gui_waiting: AtomicBool::new(false),
        midi_log: std::array::from_fn(|_| AtomicU32::new(0)),
        midi_logged: AtomicUsize::new(0),
        audio: UnsafeCell::new(Audio {
            sample_rate: 0.0,
            max_frames: 0,
            gain: params[0].default,
            bypassed: false,
            sidechain: false,
            latency: 0,
            delay: [Vec::new(), Vec::new()],
            side_delay: [Vec::new(), Vec::new()],
            position: 0,
            level: params[0].default,
            threads: false,
            voices: [Voice::default(); VOICES],
            buffers: std::array::from_fn(|_| Vec::new()),
            modulation: 0.0,
            job: (0, 0),
            channels: [MidiChannel::DEFAULT; 16],
            sustain: false,
            arp: Arp::new(48_000.0, 0.0),
        }),
    });
    ALIVE.fetch_add(1, Ordering::SeqCst);
    let plugin = Box::into_raw(plugin);
    // SAFETY: just allocated, and only this thread knows of it yet.
    unsafe {
        (*plugin).clap.plugin_data = plugin.cast();
        &raw const (*plugin).clap
    }
}

/// The plugin behind a `clap_plugin` of this library.
///
/// # Safety
///
/// `plugin` must be one of this library's, not yet destroyed.
pub(crate) unsafe fn from_clap<'a>(plugin: *const clap_plugin) -> &'a TestPlugin {
    // SAFETY: the caller's promise: plugin_data is the plugin.
    unsafe { &*(*plugin).plugin_data.cast::<TestPlugin>() }
}

/// Copies `text` into a C buffer of `capacity` bytes, nul-terminated and cut
/// short if need be.
///
/// # Safety
///
/// `buffer` must be valid for writing `capacity` bytes.
pub(crate) unsafe fn copy_str(text: &str, buffer: *mut c_char, capacity: usize) {
    if buffer.is_null() || capacity == 0 {
        return;
    }
    let len = text.len().min(capacity - 1);
    // SAFETY: the caller's promise, and `len` leaves room for the nul.
    unsafe {
        ptr::copy_nonoverlapping(text.as_ptr().cast::<c_char>(), buffer, len);
        buffer.add(len).write(0);
    }
}

impl TestPlugin {
    fn ext(&self) -> HostExtensions {
        // SAFETY: written once in `init`, before anything that reads it.
        unsafe { *self.ext.get() }
    }

    fn params(&self) -> &'static [Param] {
        match self.kind {
            Kind::Gain => &GAIN_PARAMS,
            Kind::Synth => &SYNTH_PARAMS,
            Kind::Arpeggiator => &ARPEGGIATOR_PARAMS,
        }
    }

    fn index_of(&self, id: clap_id) -> Option<usize> {
        self.params().iter().position(|p| p.id == id)
    }

    fn value(&self, index: usize) -> f64 {
        f64::from_bits(self.values[index].load(Ordering::SeqCst))
    }

    fn set_value(&self, index: usize, value: f64) {
        self.values[index].store(value.to_bits(), Ordering::SeqCst);
    }

    /// How many times the host started the plugin processing, stopped it
    /// and reset it, and how many blocks of frames it processed.
    pub(crate) fn lifecycle(&self) -> [u32; 4] {
        self.lifecycle
            .each_ref()
            .map(|count| count.load(Ordering::SeqCst))
    }

    /// Whether the gain's sidechain is on.
    pub(crate) fn sidechain(&self) -> bool {
        self.sidechain.load(Ordering::SeqCst)
    }

    fn count(&self, what: usize) {
        self.lifecycle[what].fetch_add(1, Ordering::SeqCst);
    }

    /// Asks the host whether this is the main thread: `None` if it can't
    /// say.
    fn is_main_thread(&self) -> Option<bool> {
        let check = self.ext().thread_check;
        // SAFETY: the host's extension stays valid while the plugin lives.
        unsafe { check.as_ref()?.is_main_thread.map(|f| f(self.host)) }
    }

    fn is_audio_thread(&self) -> Option<bool> {
        let check = self.ext().thread_check;
        // SAFETY: as above.
        unsafe { check.as_ref()?.is_audio_thread.map(|f| f(self.host)) }
    }

    /// Whether it has a note output: the arpeggiator's, and the echoing
    /// synth's.
    fn plays_notes(&self) -> bool {
        self.kind == Kind::Arpeggiator || self.echo
    }

    /// Checks that the host called `what` on its main thread.
    fn on_main(&self, what: &str) {
        match self.is_main_thread() {
            Some(true) => {}
            Some(false) => violation(&format!("{what} was called off the main thread")),
            None => violation("the host has no thread-check extension"),
        }
        if self.is_audio_thread() == Some(true) {
            violation(&format!("{what} was called on an audio thread"));
        }
    }

    /// Checks that the host called `what` on an audio thread.
    fn on_audio(&self, what: &str) {
        match self.is_audio_thread() {
            Some(true) => {}
            Some(false) => violation(&format!("{what} was called off an audio thread")),
            None => violation("the host has no thread-check extension"),
        }
    }

    fn log(&self, message: &CStr) {
        let log = self.ext().log;
        // SAFETY: the host's extension stays valid while the plugin lives.
        if let Some(log) = unsafe { log.as_ref() }.and_then(|l| l.log) {
            // SAFETY: as above, and the message is nul-terminated.
            unsafe {
                log(
                    self.host,
                    clap_sys::ext::log::CLAP_LOG_INFO,
                    message.as_ptr(),
                )
            };
        }
    }

    fn request_restart(&self) {
        // SAFETY: the host struct stays valid while the plugin lives.
        if let Some(restart) = unsafe { (*self.host).request_restart } {
            // SAFETY: as above.
            unsafe { restart(self.host) };
        }
    }

    pub(crate) fn request_callback(&self) {
        // SAFETY: as above.
        if let Some(callback) = unsafe { (*self.host).request_callback } {
            // SAFETY: as above.
            unsafe { callback(self.host) };
        }
    }

    pub(crate) fn main_thread_calls(&self) -> u32 {
        self.main_thread_calls.load(Ordering::SeqCst)
    }

    pub(crate) fn gui_action(&self, action: u32, param: clap_id, value: f64) {
        self.on_main("a window action");
        if action > gui::END || self.index_of(param).is_none() {
            violation("the test asked for a window action that doesn't exist");
            return;
        }
        self.gui_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back((action, param, value));
        self.gui_waiting.store(true, Ordering::SeqCst);
        let params = self.ext().params;
        // SAFETY: the host's extension stays valid while the plugin lives.
        if let Some(flush) = unsafe { params.as_ref() }.and_then(|p| p.request_flush) {
            // SAFETY: as above.
            unsafe { flush(self.host) };
        }
    }

    pub(crate) fn set_label(&self, label: &str) {
        self.on_main("changing the label");
        *self.label.lock().unwrap_or_else(|e| e.into_inner()) = label.to_owned();
        let state = self.ext().state;
        // SAFETY: the host's extension stays valid while the plugin lives.
        if let Some(dirty) = unsafe { state.as_ref() }.and_then(|s| s.mark_dirty) {
            // SAFETY: as above.
            unsafe { dirty(self.host) };
        }
    }

    pub(crate) fn label(&self) -> String {
        self.label.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub(crate) fn window(&self) -> (i32, usize) {
        let window = self.window.lock().unwrap_or_else(|e| e.into_inner());
        let state = match (window.created, window.visible) {
            (false, _) => -1,
            (true, false) => 0,
            (true, true) => 1,
        };
        (state, window.parent)
    }

    pub(crate) fn request_resize(&self, width: u32, height: u32) -> bool {
        let gui = self.ext().gui;
        // SAFETY: the host's extension stays valid while the plugin lives.
        let Some(resize) = (unsafe { gui.as_ref() }).and_then(|g| g.request_resize) else {
            return false;
        };
        // SAFETY: as above.
        let accepted = unsafe { resize(self.host, width, height) };
        if accepted {
            let mut window = self.window.lock().unwrap_or_else(|e| e.into_inner());
            window.width = width;
            window.height = height;
        }
        accepted
    }

    pub(crate) fn pool_use(&self) -> (u32, u32) {
        let threads = self
            .pool_threads
            .iter()
            .filter(|t| t.load(Ordering::SeqCst) != 0)
            .count();
        (self.pool_requests.load(Ordering::SeqCst), threads as u32)
    }

    pub(crate) fn transport(&self) -> Option<(u32, i64, f64)> {
        self.has_transport.load(Ordering::SeqCst).then(|| {
            let [flags, beats, tempo] = &self.transport;
            (
                flags.load(Ordering::SeqCst) as u32,
                beats.load(Ordering::SeqCst) as i64,
                f64::from_bits(tempo.load(Ordering::SeqCst)),
            )
        })
    }

    fn record_transport(&self, transport: &clap_event_transport) {
        let [flags, beats, tempo] = &self.transport;
        flags.store(u64::from(transport.flags), Ordering::SeqCst);
        beats.store(transport.song_pos_beats as u64, Ordering::SeqCst);
        tempo.store(transport.tempo.to_bits(), Ordering::SeqCst);
        self.has_transport.store(true, Ordering::SeqCst);
    }

    /// Takes the MIDI messages logged since the last time, oldest first,
    /// into `buffer`, and returns how many there were, which can be more
    /// than the log or the buffer holds. Called between blocks.
    pub(crate) fn take_midi(&self, buffer: &mut [u32]) -> usize {
        let logged = self.midi_logged.swap(0, Ordering::SeqCst);
        for (slot, message) in buffer
            .iter_mut()
            .zip(&self.midi_log[..logged.min(MIDI_LOG)])
        {
            *slot = message.load(Ordering::SeqCst);
        }
        logged
    }

    fn log_midi(&self, data: [u8; 3]) {
        let index = self.midi_logged.fetch_add(1, Ordering::SeqCst);
        if let Some(slot) = self.midi_log.get(index) {
            slot.store(
                u32::from_le_bytes([data[0], data[1], data[2], 0]),
                Ordering::SeqCst,
            );
        }
    }

    /// The plugin's state, as text.
    fn save_state(&self) -> String {
        let kind = match self.kind {
            Kind::Gain => "gain",
            Kind::Synth => "synth",
            Kind::Arpeggiator => "arpeggiator",
        };
        let mut state = format!("musicable-test-plugin 1\nkind={kind}\n");
        for (index, param) in self.params().iter().enumerate() {
            let _ = writeln!(state, "p{}={}", param.id, self.value(index));
        }
        let _ = writeln!(state, "label={}", self.label());
        state
    }

    fn load_state(&self, state: &str) -> bool {
        let mut lines = state.lines();
        if lines.next() != Some("musicable-test-plugin 1") {
            return false;
        }
        let mut values: Vec<f64> = (0..self.params().len()).map(|i| self.value(i)).collect();
        let mut label = String::new();
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                return false;
            };
            if let Some(id) = key.strip_prefix('p') {
                let (Ok(id), Ok(value)) = (id.parse::<clap_id>(), value.parse::<f64>()) else {
                    return false;
                };
                match self.index_of(id) {
                    Some(index) => values[index] = value,
                    None => return false,
                }
            } else if key == "label" {
                value.clone_into(&mut label);
            }
        }
        for (index, &value) in values.iter().enumerate() {
            self.set_value(index, value);
        }
        *self.label.lock().unwrap_or_else(|e| e.into_inner()) = label;
        if self.kind == Kind::Gain
            && self.active.load(Ordering::SeqCst)
            && latency_frames(values[1]) != self.active_latency.load(Ordering::SeqCst) as usize
        {
            self.request_restart();
        }
        // An inactive plugin takes the values when it activates, and an
        // active one as it next processes.
        true
    }

    /// Applies one of the host's events, or the pretend window's, as the
    /// thread processing does.
    fn apply_param(&self, audio: &mut Audio, id: clap_id, value: f64) {
        let Some(index) = self.index_of(id) else {
            violation("the host set a parameter that doesn't exist");
            return;
        };
        let param = &self.params()[index];
        if !(param.min..=param.max).contains(&value) {
            violation("the host set a parameter out of its range");
        }
        self.set_value(index, value);
        match (self.kind, index) {
            (Kind::Gain, 0) => audio.gain = value,
            (Kind::Gain, 1) => {
                if latency_frames(value) != audio.latency {
                    self.request_restart();
                }
            }
            (Kind::Gain, _) => audio.bypassed = value >= 0.5,
            (Kind::Synth, 0) => audio.level = value,
            (Kind::Synth, _) => audio.threads = value >= 0.5,
            (Kind::Arpeggiator, _) => audio.arp.transpose = value,
        }
    }

    /// Takes the pretend window's actions, tells the host about each, and
    /// applies the values.
    ///
    /// # Safety
    ///
    /// `out` must be the host's output events.
    unsafe fn apply_gui(&self, audio: &mut Audio, out: *const clap_output_events) {
        if !self.gui_waiting.load(Ordering::SeqCst) {
            return;
        }
        let Ok(mut actions) = self.gui_actions.try_lock() else {
            return;
        };
        self.gui_waiting.store(false, Ordering::SeqCst);
        while let Some((action, param, value)) = actions.pop_front() {
            let header = |type_, size| clap_event_header {
                size,
                time: 0,
                space_id: CLAP_CORE_EVENT_SPACE_ID,
                type_,
                flags: 0,
            };
            match action {
                gui::BEGIN | gui::END => {
                    let type_ = if action == gui::BEGIN {
                        CLAP_EVENT_PARAM_GESTURE_BEGIN
                    } else {
                        CLAP_EVENT_PARAM_GESTURE_END
                    };
                    let event = clap_event_param_gesture {
                        header: header(type_, size_of::<clap_event_param_gesture>() as u32),
                        param_id: param,
                    };
                    // SAFETY: the caller's promise.
                    unsafe { push(out, &event.header) };
                }
                _ => {
                    self.apply_param(audio, param, value);
                    let event = clap_event_param_value {
                        header: header(
                            CLAP_EVENT_PARAM_VALUE,
                            size_of::<clap_event_param_value>() as u32,
                        ),
                        param_id: param,
                        cookie: ptr::null_mut(),
                        note_id: -1,
                        port_index: -1,
                        channel: -1,
                        key: -1,
                        value,
                    };
                    // SAFETY: the caller's promise.
                    unsafe { push(out, &event.header) };
                }
            }
        }
    }

    /// Applies one input event.
    ///
    /// # Safety
    ///
    /// `header` must be the header of an event of the size it says, and
    /// `out` the host's output events.
    unsafe fn apply_event(
        &self,
        audio: &mut Audio,
        header: &clap_event_header,
        out: *const clap_output_events,
    ) {
        if header.space_id != CLAP_CORE_EVENT_SPACE_ID {
            return;
        }
        let event = ptr::from_ref(header);
        match header.type_ {
            CLAP_EVENT_PARAM_VALUE => {
                // SAFETY: the type says what the event is.
                let event = unsafe { &*event.cast::<clap_event_param_value>() };
                self.apply_param(audio, event.param_id, event.value);
            }
            CLAP_EVENT_PARAM_MOD => {
                // SAFETY: as above.
                let event = unsafe { &*event.cast::<clap_event_param_mod>() };
                if Some(event.param_id) != self.params().first().map(|p| p.id) {
                    return;
                }
                let target = (event.note_id, event.port_index, event.channel, event.key);
                if target == (-1, -1, -1, -1) {
                    audio.modulation = event.amount;
                    return;
                }
                // The synth's level can be modulated for each voice too.
                for voice in audio.voices.iter_mut().filter(|v| {
                    v.active
                        && matches_note(
                            v,
                            event.note_id,
                            event.port_index,
                            event.channel,
                            event.key,
                        )
                }) {
                    voice.modulation = event.amount;
                }
            }
            CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF | CLAP_EVENT_NOTE_CHOKE => {
                // SAFETY: as above.
                let event = unsafe { &*event.cast::<clap_event_note>() };
                if self.kind == Kind::Arpeggiator {
                    let (key, time) = (event.key, header.time);
                    // SAFETY: the caller's promise.
                    let emit = &mut |note| unsafe { play_stepped(note, out) };
                    if header.type_ == CLAP_EVENT_NOTE_ON {
                        audio.arp.hold(key, event.velocity, time, emit);
                    } else {
                        audio.arp.let_go(key, time, emit);
                    }
                    return;
                }
                if self.echo && header.type_ != CLAP_EVENT_NOTE_CHOKE {
                    // SAFETY: the caller's promise.
                    unsafe { echo(header, event, out) };
                }
                if header.type_ == CLAP_EVENT_NOTE_ON {
                    note_on(
                        audio,
                        event.note_id,
                        event.port_index,
                        event.channel,
                        event.key,
                        event.velocity,
                    );
                } else {
                    // A choked note ends at once, pedal or not.
                    let choke = header.type_ == CLAP_EVENT_NOTE_CHOKE;
                    // SAFETY: the caller's promise.
                    unsafe {
                        note_off(
                            audio,
                            event.note_id,
                            event.port_index,
                            event.channel,
                            event.key,
                            choke,
                            out,
                        );
                    }
                }
            }
            CLAP_EVENT_NOTE_EXPRESSION => {
                // SAFETY: as above.
                let event = unsafe { &*event.cast::<clap_event_note_expression>() };
                for voice in audio.voices.iter_mut().filter(|v| {
                    v.active
                        && matches_note(
                            v,
                            event.note_id,
                            event.port_index,
                            event.channel,
                            event.key,
                        )
                }) {
                    match event.expression_id {
                        CLAP_NOTE_EXPRESSION_TUNING => voice.tuning = event.value,
                        CLAP_NOTE_EXPRESSION_PRESSURE => voice.pressure = event.value,
                        CLAP_NOTE_EXPRESSION_BRIGHTNESS => voice.brightness = event.value,
                        _ => {}
                    }
                }
            }
            CLAP_EVENT_MIDI => {
                // SAFETY: as above.
                let event = unsafe { &*event.cast::<clap_event_midi>() };
                self.log_midi(event.data);
                if self.kind == Kind::Arpeggiator {
                    let [status, key, velocity] = event.data;
                    let (key, time) = (i16::from(key), header.time);
                    // SAFETY: the caller's promise.
                    let emit = &mut |note| unsafe { play_stepped(note, out) };
                    match status & 0xF0 {
                        0x90 if velocity > 0 => {
                            let velocity = f64::from(velocity) / 127.0;
                            audio.arp.hold(key, velocity, time, emit);
                        }
                        0x80 | 0x90 => audio.arp.let_go(key, time, emit),
                        _ => {}
                    }
                    return;
                }
                // SAFETY: the caller's promise.
                unsafe { apply_midi(audio, event.port_index as i16, event.data, out) };
            }
            CLAP_EVENT_TRANSPORT => {
                // SAFETY: as above.
                self.record_transport(unsafe { &*event.cast::<clap_event_transport>() });
            }
            _ => {}
        }
    }

    /// Renders frames `start..end` of the block into `outputs`.
    ///
    /// # Safety
    ///
    /// `process` must be the host's process call, with buffers at least
    /// `end` frames long.
    unsafe fn render(&self, process: &clap_process, start: usize, end: usize) {
        if self.kind == Kind::Arpeggiator {
            // SAFETY: only the thread processing touches it.
            let audio = unsafe { &mut *self.audio.get() };
            let out = process.out_events;
            // SAFETY: the caller's promise: the host's output events.
            let emit = &mut |note| unsafe { play_stepped(note, out) };
            audio.arp.run(start as u32, end as u32, emit);
            return;
        }
        // SAFETY: the caller's promise: there's one output port, stereo,
        // which `plugin_process` checked.
        let Some(out) = (unsafe { stereo(&*process.audio_outputs) }) else {
            return;
        };
        match self.kind {
            Kind::Gain => {
                // SAFETY: only the thread processing touches it.
                let audio = unsafe { &mut *self.audio.get() };
                let input = if process.audio_inputs_count > 0 {
                    // SAFETY: the caller's promise.
                    unsafe { stereo(&*process.audio_inputs) }
                } else {
                    None
                };
                let side = if audio.sidechain && process.audio_inputs_count > 1 {
                    // SAFETY: the caller's promise.
                    unsafe { stereo(&*process.audio_inputs.add(1)) }
                } else {
                    None
                };
                let gain = if audio.bypassed {
                    1.0
                } else {
                    audio.gain + audio.modulation
                };
                for frame in start..end {
                    for (channel, out) in out.iter().enumerate() {
                        // SAFETY: the buffers are `end` frames long.
                        let read = |port: &Option<[*mut f32; 2]>| unsafe {
                            port.map_or(0.0, |port| *port[channel].add(frame))
                        };
                        let (x, key) = (read(&input), read(&side));
                        let (y, key) = if audio.latency == 0 {
                            (x, key)
                        } else {
                            let at = audio.position;
                            let line = &mut audio.delay[channel][at];
                            let side_line = &mut audio.side_delay[channel][at];
                            (
                                std::mem::replace(line, x),
                                std::mem::replace(side_line, key),
                            )
                        };
                        // The sidechain ducks it, unless it's bypassed.
                        let duck = if audio.bypassed {
                            1.0
                        } else {
                            1.0 - f64::from(key.abs()).min(1.0)
                        };
                        // SAFETY: as above.
                        unsafe { *out.add(frame) = (f64::from(y) * gain * duck) as f32 };
                    }
                    if audio.latency > 0 {
                        audio.position = (audio.position + 1) % audio.latency;
                    }
                }
            }
            Kind::Arpeggiator => {}
            Kind::Synth => {
                // SAFETY: as above.
                let pool = unsafe { (*self.audio.get()).threads } && {
                    // SAFETY: as above.
                    unsafe { (*self.audio.get()).job = (start, end) };
                    self.request_exec(VOICES as u32)
                };
                if !pool {
                    for voice in 0..VOICES {
                        // SAFETY: no tasks are running.
                        unsafe { self.render_voice(voice, start, end) };
                    }
                }
                // SAFETY: the tasks are done.
                let audio = unsafe { &mut *self.audio.get() };
                for (voice, buffer) in audio.voices.iter().zip(&audio.buffers) {
                    if !voice.active {
                        continue;
                    }
                    for out in &out {
                        for (i, sample) in buffer[start..end].iter().enumerate() {
                            // SAFETY: the buffers are `end` frames long.
                            unsafe { *out.add(start + i) += sample };
                        }
                    }
                }
            }
        }
    }

    fn request_exec(&self, tasks: u32) -> bool {
        let pool = self.ext().thread_pool;
        // SAFETY: the host's extension stays valid while the plugin lives.
        let Some(exec) = (unsafe { pool.as_ref() }).and_then(|p| p.request_exec) else {
            return false;
        };
        // SAFETY: as above.
        let done = unsafe { exec(self.host, tasks) };
        if done {
            self.pool_requests.fetch_add(1, Ordering::SeqCst);
        }
        done
    }

    /// Renders one voice's frames `start..end` into its buffer.
    ///
    /// # Safety
    ///
    /// Only one thread at a time renders a given voice, while the thread
    /// processing waits.
    unsafe fn render_voice(&self, voice: usize, start: usize, end: usize) {
        let audio = self.audio.get();
        // SAFETY: the caller's promise: each task has its own voice and
        // buffer, and nothing else touches them meanwhile.
        let (v, buffer, sample_rate, level) = unsafe {
            (
                &mut (*audio).voices[voice],
                &mut (*audio).buffers[voice],
                (*audio).sample_rate,
                (*audio).level + (*audio).modulation,
            )
        };
        if !v.active {
            return;
        }
        let key = f64::from(v.key) + part_transpose(self.parts, v.channel);
        let frequency = 440.0 * ((key + v.tuning - 69.0) / 12.0).exp2();
        let amplitude = v.velocity * (level + v.modulation) * (1.0 + v.pressure);
        for sample in &mut buffer[start..end] {
            let phase = v.phase * TAU;
            *sample = (amplitude * (phase.sin() + 0.5 * v.brightness * (2.0 * phase).sin())) as f32;
            v.phase = (v.phase + frequency / sample_rate).fract();
        }
    }

    fn exec(&self, task: u32) {
        self.on_audio("thread_pool.exec");
        let mark = thread_mark();
        for slot in &self.pool_threads {
            match slot.compare_exchange(0, mark, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => break,
                Err(seen) if seen == mark => break,
                Err(_) => {}
            }
        }
        // SAFETY: only the thread processing touches it, and it's waiting.
        let (start, end) = unsafe { (*self.audio.get()).job };
        if (task as usize) < VOICES {
            // SAFETY: each task renders its own voice.
            unsafe { self.render_voice(task as usize, start, end) };
        }
    }
}

fn latency_frames(value: f64) -> usize {
    value.round().max(0.0) as usize
}

/// Whether a voice is the note an event names: -1 matches any.
fn matches_note(voice: &Voice, note_id: i32, port: i16, channel: i16, key: i16) -> bool {
    (note_id == -1 || voice.note_id == note_id)
        && (port == -1 || voice.port == port)
        && (channel == -1 || voice.channel == channel)
        && (key == -1 || voice.key == key)
}

/// Starts a voice for a note, and returns it.
fn note_on(
    audio: &mut Audio,
    note_id: i32,
    port: i16,
    channel: i16,
    key: i16,
    velocity: f64,
) -> &mut Voice {
    let voice = audio.voices.iter().position(|v| !v.active).unwrap_or(0);
    audio.voices[voice] = Voice {
        active: true,
        note_id,
        port,
        channel,
        key,
        velocity,
        ..Voice::default()
    };
    &mut audio.voices[voice]
}

/// Lets go of a note: its voices end, and the host hears they did, unless
/// the sustain pedal holds them and the note isn't choked.
///
/// # Safety
///
/// `out` must be the host's output events.
unsafe fn note_off(
    audio: &mut Audio,
    note_id: i32,
    port: i16,
    channel: i16,
    key: i16,
    choke: bool,
    out: *const clap_output_events,
) {
    let held = audio.sustain && !choke;
    for voice in &mut audio.voices {
        if voice.active && matches_note(voice, note_id, port, channel, key) {
            if held {
                voice.held = true;
            } else {
                // SAFETY: the caller's promise.
                unsafe { end_voice(voice, out) };
            }
        }
    }
}

/// Ends a voice at once, and tells the host it ended.
///
/// # Safety
///
/// `out` must be the host's output events.
unsafe fn end_voice(voice: &mut Voice, out: *const clap_output_events) {
    voice.active = false;
    voice.held = false;
    let end = clap_event_note {
        header: clap_event_header {
            size: size_of::<clap_event_note>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_NOTE_END,
            flags: 0,
        },
        note_id: voice.note_id,
        port_index: voice.port,
        channel: voice.channel,
        key: voice.key,
        velocity: 0.0,
    };
    // SAFETY: the caller's promise.
    unsafe { push(out, &end.header) };
}

/// Plays a MIDI message that came in on `port`.
///
/// # Safety
///
/// `out` must be the host's output events.
unsafe fn apply_midi(audio: &mut Audio, port: i16, data: [u8; 3], out: *const clap_output_events) {
    let [status, first, second] = data;
    let index = usize::from(status & 0x0f);
    let channel = index as i16;
    let value = f64::from(second) / 127.0;
    let on_channel = |voice: &Voice| voice.active && voice.channel == channel;
    match status & 0xf0 {
        0x90 if second > 0 => {
            // A note starts as its channel is.
            let state = audio.channels[index];
            let voice = note_on(audio, -1, port, channel, i16::from(first), value);
            voice.tuning = state.tuning();
            voice.pressure = state.pressure;
            voice.brightness = state.brightness;
        }
        0x80 | 0x90 => {
            // SAFETY: the caller's promise.
            unsafe { note_off(audio, -1, port, channel, i16::from(first), false, out) };
        }
        0xA0 => {
            for voice in audio
                .voices
                .iter_mut()
                .filter(|v| on_channel(v) && v.key == i16::from(first))
            {
                voice.pressure = value;
            }
        }
        0xB0 => {
            // SAFETY: the caller's promise.
            unsafe { apply_controller(audio, index, first, second, out) };
        }
        0xD0 => {
            let pressure = f64::from(first) / 127.0;
            audio.channels[index].pressure = pressure;
            for voice in audio.voices.iter_mut().filter(|v| on_channel(v)) {
                voice.pressure = pressure;
            }
        }
        0xE0 => {
            let state = &mut audio.channels[index];
            state.bend = u16::from(first) | u16::from(second) << 7;
            let tuning = state.tuning();
            for voice in audio.voices.iter_mut().filter(|v| on_channel(v)) {
                voice.tuning = tuning;
            }
        }
        _ => {}
    }
}

/// Moves controller `number` on channel `index` to `value`.
///
/// # Safety
///
/// `out` must be the host's output events.
unsafe fn apply_controller(
    audio: &mut Audio,
    index: usize,
    number: u8,
    value: u8,
    out: *const clap_output_events,
) {
    let state = &mut audio.channels[index];
    match number {
        // Data entry, for the registered parameter selected.
        6 => match state.rpn {
            // The pitch bend range, in semitones. Its cents, in data
            // entry's fine part, are left alone.
            [0, 0] => state.range = f64::from(value),
            // MPE's configuration, on a lower zone's master channel: its
            // member channels bend 48 semitones, and it bends 2.
            [0, 6] if index == 0 => {
                for (channel, state) in audio.channels.iter_mut().enumerate() {
                    if channel <= usize::from(value.min(15)) {
                        state.range = if channel == 0 { 2.0 } else { 48.0 };
                    }
                }
            }
            _ => {}
        },
        // The sustain pedal: up lets go of the notes it held.
        64 => {
            audio.sustain = value >= 64;
            if !audio.sustain {
                for voice in audio.voices.iter_mut().filter(|v| v.active && v.held) {
                    // SAFETY: the caller's promise.
                    unsafe { end_voice(voice, out) };
                }
            }
        }
        74 => {
            let brightness = f64::from(value) / 127.0;
            state.brightness = brightness;
            let channel = index as i16;
            for voice in audio
                .voices
                .iter_mut()
                .filter(|v| v.active && v.channel == channel)
            {
                voice.brightness = brightness;
            }
        }
        100 => state.rpn[1] = value,
        101 => state.rpn[0] = value,
        _ => {}
    }
}

/// Plays a note the arpeggiator starts or ends, as CLAP's note, or as MIDI
/// from a library named to.
///
/// # Safety
///
/// `out` must be the host's output events, or null.
unsafe fn play_stepped(note: Stepped, out: *const clap_output_events) {
    let header = |type_, size: usize| clap_event_header {
        size: size as u32,
        time: note.time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    };
    if MIDI_OUT.load(Ordering::Relaxed) {
        let key = note.key as u8;
        let data = if note.on {
            let velocity = (note.velocity * 127.0).round().clamp(1.0, 127.0);
            [0x90, key, velocity as u8]
        } else {
            [0x80, key, 0]
        };
        let event = clap_event_midi {
            header: header(CLAP_EVENT_MIDI, size_of::<clap_event_midi>()),
            port_index: 0,
            data,
        };
        // SAFETY: the caller's promise.
        unsafe { push(out, &event.header) };
    } else {
        let type_ = if note.on {
            CLAP_EVENT_NOTE_ON
        } else {
            CLAP_EVENT_NOTE_OFF
        };
        let event = clap_event_note {
            header: header(type_, size_of::<clap_event_note>()),
            note_id: note.id,
            port_index: 0,
            channel: 0,
            key: note.key,
            velocity: note.velocity,
        };
        // SAFETY: the caller's promise.
        unsafe { push(out, &event.header) };
    }
}

/// Plays a note event the echoing synth gets out again, an octave up, on
/// its frame.
///
/// # Safety
///
/// `out` must be the host's output events, or null.
unsafe fn echo(header: &clap_event_header, note: &clap_event_note, out: *const clap_output_events) {
    let echoed = clap_event_note {
        header: *header,
        key: note.key + ECHO_UP,
        port_index: 0,
        ..*note
    };
    // SAFETY: the caller's promise.
    unsafe { push(out, &echoed.header) };
}

/// Pushes an event to the host's output events.
///
/// # Safety
///
/// `out` must be the host's output events, or null.
unsafe fn push(out: *const clap_output_events, event: &clap_event_header) {
    // SAFETY: the caller's promise.
    if let Some(try_push) = unsafe { out.as_ref() }.and_then(|o| o.try_push) {
        // SAFETY: as above.
        unsafe { try_push(out, event) };
    }
}

/// A buffer's first two channels, or its one channel twice if it's mono.
///
/// # Safety
///
/// The buffer must hold `channel_count` channels of 32-bit samples.
unsafe fn stereo(buffer: &clap_audio_buffer) -> Option<[*mut f32; 2]> {
    if buffer.data32.is_null() || buffer.channel_count == 0 {
        return None;
    }
    // SAFETY: the caller's promise.
    let left = unsafe { *buffer.data32 };
    let right = if buffer.channel_count > 1 {
        // SAFETY: as above.
        unsafe { *buffer.data32.add(1) }
    } else {
        left
    };
    Some([left, right])
}

/// # Safety
///
/// `plugin` must be one of this library's, not yet destroyed: for every
/// callback below.
unsafe fn this<'a>(plugin: *const clap_plugin) -> &'a TestPlugin {
    // SAFETY: the caller's promise.
    unsafe { from_clap(plugin) }
}

unsafe extern "C" fn plugin_init(plugin: *const clap_plugin) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    let get = |id: &CStr| -> *const c_void {
        // SAFETY: the host struct stays valid while the plugin lives.
        match unsafe { (*this.host).get_extension } {
            // SAFETY: as above.
            Some(get) => unsafe { get(this.host, id.as_ptr()) },
            None => ptr::null(),
        }
    };
    let ext = HostExtensions {
        params: get(CLAP_EXT_PARAMS).cast(),
        state: get(CLAP_EXT_STATE).cast(),
        latency: get(CLAP_EXT_LATENCY).cast(),
        thread_check: get(CLAP_EXT_THREAD_CHECK).cast(),
        thread_pool: get(CLAP_EXT_THREAD_POOL).cast(),
        gui: get(CLAP_EXT_GUI).cast(),
        log: get(CLAP_EXT_LOG).cast(),
    };
    // SAFETY: nothing reads it before `init` returns.
    unsafe { *this.ext.get() = ext };
    this.on_main("init");
    if this.initialized.swap(true, Ordering::SeqCst) {
        violation("init was called twice");
    }
    this.log(c"initialized");
    true
}

unsafe extern "C" fn plugin_destroy(plugin: *const clap_plugin) {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    if this.initialized.load(Ordering::SeqCst) {
        this.on_main("destroy");
    }
    if this.active.load(Ordering::SeqCst) {
        violation("destroy was called while the plugin was active");
    }
    if this.window.lock().is_ok_and(|w| w.created) {
        violation("destroy was called with the window still created");
    }
    // SAFETY: created by `create` with `Box::into_raw`, and the host is done
    // with it.
    drop(unsafe { Box::from_raw((*plugin).plugin_data.cast::<TestPlugin>()) });
    ALIVE.fetch_sub(1, Ordering::SeqCst);
}

unsafe extern "C" fn plugin_activate(
    plugin: *const clap_plugin,
    sample_rate: f64,
    min_frames: u32,
    max_frames: u32,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("activate");
    if !this.initialized.load(Ordering::SeqCst) {
        violation("activate came before init");
    }
    if this.active.load(Ordering::SeqCst) {
        violation("activate was called while the plugin was active");
        return false;
    }
    if sample_rate.is_nan() || sample_rate <= 0.0 || min_frames < 1 || min_frames > max_frames {
        violation("activate got a nonsense sample rate or block size");
        return false;
    }
    // SAFETY: inactive, so nothing processes.
    let audio = unsafe { &mut *this.audio.get() };
    audio.sample_rate = sample_rate;
    audio.max_frames = max_frames;
    audio.gain = this.value(0);
    audio.bypassed = this.kind == Kind::Gain && this.value(2) >= 0.5;
    audio.sidechain = this.sidechain();
    audio.level = this.value(0);
    audio.threads = this.value(1) >= 0.5;
    audio.modulation = 0.0;
    audio.latency = match this.kind {
        Kind::Gain => latency_frames(this.value(1)),
        Kind::Synth | Kind::Arpeggiator => 0,
    };
    audio.delay = [vec![0.0; audio.latency], vec![0.0; audio.latency]];
    audio.side_delay = audio.delay.clone();
    audio.position = 0;
    audio.voices = [Voice::default(); VOICES];
    audio.buffers = std::array::from_fn(|_| vec![0.0; max_frames as usize]);
    audio.channels = [MidiChannel::DEFAULT; 16];
    audio.sustain = false;
    audio.arp = Arp::new(sample_rate, this.value(0));
    let previous = this
        .active_latency
        .swap(audio.latency as u32, Ordering::SeqCst);
    if previous != audio.latency as u32 {
        // CLAP's way to say so, from within activate.
        let latency = this.ext().latency;
        // SAFETY: the host's extension stays valid while the plugin lives.
        if let Some(changed) = unsafe { latency.as_ref() }.and_then(|l| l.changed) {
            // SAFETY: as above.
            unsafe { changed(this.host) };
        }
    }
    this.active.store(true, Ordering::SeqCst);
    true
}

unsafe extern "C" fn plugin_deactivate(plugin: *const clap_plugin) {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("deactivate");
    if !this.active.load(Ordering::SeqCst) {
        violation("deactivate was called while the plugin was inactive");
    }
    if this.processing.load(Ordering::SeqCst) {
        violation("deactivate was called while the plugin was processing");
    }
    this.active.store(false, Ordering::SeqCst);
}

unsafe extern "C" fn plugin_start_processing(plugin: *const clap_plugin) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_audio("start_processing");
    if !this.active.load(Ordering::SeqCst) {
        violation("start_processing was called while the plugin was inactive");
        return false;
    }
    if this.processing.swap(true, Ordering::SeqCst) {
        violation("start_processing was called while the plugin was processing");
    }
    this.count(0);
    true
}

unsafe extern "C" fn plugin_stop_processing(plugin: *const clap_plugin) {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_audio("stop_processing");
    if !this.processing.swap(false, Ordering::SeqCst) {
        violation("stop_processing was called while the plugin wasn't processing");
    }
    this.count(1);
}

unsafe extern "C" fn plugin_reset(plugin: *const clap_plugin) {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_audio("reset");
    if !this.active.load(Ordering::SeqCst) {
        violation("reset was called while the plugin was inactive");
        return;
    }
    this.count(2);
    // SAFETY: the thread processing calls it.
    let audio = unsafe { &mut *this.audio.get() };
    for line in audio.delay.iter_mut().chain(&mut audio.side_delay) {
        line.fill(0.0);
    }
    audio.voices = [Voice::default(); VOICES];
    audio.channels = [MidiChannel::DEFAULT; 16];
    audio.sustain = false;
    audio.arp.clear();
}

unsafe extern "C" fn plugin_process(
    plugin: *const clap_plugin,
    process: *const clap_process,
) -> clap_process_status {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_audio("process");
    if !this.processing.load(Ordering::SeqCst) {
        violation("process was called while the plugin wasn't processing");
        return CLAP_PROCESS_ERROR;
    }
    // SAFETY: the host passes its process struct.
    let process = unsafe { &*process };
    let frames = process.frames_count as usize;
    if frames > 0 {
        this.count(3);
    }
    {
        // SAFETY: the thread processing touches it.
        let audio = unsafe { &*this.audio.get() };
        if process.frames_count > audio.max_frames {
            violation("process got a block longer than the plugin was activated for");
            return CLAP_PROCESS_ERROR;
        }
    }
    let (inputs, outputs) = match this.kind {
        Kind::Gain => (2, 1),
        Kind::Synth => (0, 1),
        Kind::Arpeggiator => (0, 0),
    };
    if process.audio_inputs_count != inputs || process.audio_outputs_count != outputs {
        violation("process got the wrong number of audio ports");
        return CLAP_PROCESS_ERROR;
    }
    if this.kind == Kind::Gain && !this.sidechain() {
        // SAFETY: the host passes two input ports, as just checked.
        let side = unsafe { &*process.audio_inputs.add(1) };
        // SAFETY: as above, `frames` long.
        let silent = unsafe { stereo(side) }.is_some_and(|channels| {
            (channels.iter()).all(|&c| {
                unsafe { std::slice::from_raw_parts(c, frames) }
                    .iter()
                    .all(|&s| s == 0.0)
            })
        });
        if side.constant_mask & 0b11 != 0b11 || !silent {
            violation("process got a port the host turned off without silence marked constant");
        }
    }
    if outputs > 0 {
        // SAFETY: the host passes one output port.
        let output = unsafe { &*process.audio_outputs };
        if output.channel_count != 2 || output.data32.is_null() {
            violation("process got an output that isn't stereo");
            return CLAP_PROCESS_ERROR;
        }
        // SAFETY: as above.
        for out in unsafe { stereo(output) }.into_iter().flatten() {
            // SAFETY: each channel is `frames` long.
            unsafe { std::slice::from_raw_parts_mut(out, frames) }.fill(0.0);
        }
    }
    // SAFETY: the host passes its transport, or null.
    if let Some(transport) = unsafe { process.transport.as_ref() } {
        this.record_transport(transport);
    }
    // SAFETY: the host passes its event lists.
    let events = unsafe { &*process.in_events };
    let (Some(size), Some(get)) = (events.size, events.get) else {
        violation("process got an input event list without functions");
        return CLAP_PROCESS_ERROR;
    };
    // SAFETY: as above.
    let count = unsafe { size(events) };
    let time = |index: u32| -> u32 {
        // SAFETY: the index is below the list's size.
        unsafe { (*get(events, index)).time }
    };
    let mut next = 0;
    let mut start = 0;
    let mut last_time = 0;
    loop {
        while next < count && (time(next) as usize <= start || start >= frames) {
            // SAFETY: as above.
            let header = unsafe { &*get(events, next) };
            if header.time < last_time {
                violation("process got input events out of order");
            }
            if header.time as usize >= frames.max(1) {
                violation("process got an input event past the end of the block");
            }
            last_time = header.time;
            // SAFETY: the thread processing touches it, and the header is
            // the host's.
            unsafe { this.apply_event(&mut *this.audio.get(), header, process.out_events) };
            next += 1;
        }
        if start >= frames {
            break;
        }
        let end = if next < count {
            (time(next) as usize).min(frames)
        } else {
            frames
        };
        // SAFETY: the buffers are `frames` long.
        unsafe { this.render(process, start, end) };
        start = end;
    }
    // SAFETY: the thread processing touches it, and the list is the host's.
    unsafe { this.apply_gui(&mut *this.audio.get(), process.out_events) };
    CLAP_PROCESS_CONTINUE
}

unsafe extern "C" fn plugin_get_extension(
    plugin: *const clap_plugin,
    id: *const c_char,
) -> *const c_void {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    if id.is_null() {
        return ptr::null();
    }
    // SAFETY: the host passes a nul-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    let (gain, synth) = (this.kind == Kind::Gain, this.kind == Kind::Synth);
    if id == CLAP_EXT_PARAMS {
        (&raw const PARAMS).cast()
    } else if id == CLAP_EXT_STATE {
        (&raw const STATE).cast()
    } else if id == CLAP_EXT_AUDIO_PORTS {
        (&raw const AUDIO_PORTS).cast()
    } else if (id == CLAP_EXT_AUDIO_PORTS_ACTIVATION
        || id == CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT)
        && gain
        && !FIXED_PORTS.load(Ordering::SeqCst)
    {
        (&raw const PORTS_ACTIVATION).cast()
    } else if id == CLAP_EXT_GUI {
        (&raw const GUI).cast()
    } else if id == CLAP_EXT_LATENCY && gain {
        (&raw const LATENCY_EXT).cast()
    } else if id == CLAP_EXT_NOTE_PORTS && !gain {
        (&raw const NOTE_PORTS).cast()
    } else if id == CLAP_EXT_THREAD_POOL && synth {
        (&raw const THREAD_POOL).cast()
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn plugin_on_main_thread(plugin: *const clap_plugin) {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("on_main_thread");
    this.main_thread_calls.fetch_add(1, Ordering::SeqCst);
}

static PARAMS: clap_plugin_params = clap_plugin_params {
    count: Some(params_count),
    get_info: Some(params_get_info),
    get_value: Some(params_get_value),
    value_to_text: Some(params_value_to_text),
    text_to_value: Some(params_text_to_value),
    flush: Some(params_flush),
};

unsafe extern "C" fn params_count(plugin: *const clap_plugin) -> u32 {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("params.count");
    this.params().len() as u32
}

unsafe extern "C" fn params_get_info(
    plugin: *const clap_plugin,
    index: u32,
    info: *mut clap_param_info,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("params.get_info");
    let Some(param) = this.params().get(index as usize) else {
        return false;
    };
    let mut name = [0; 256];
    let mut module = [0; 1024];
    // SAFETY: the arrays hold as much as they say.
    unsafe {
        copy_str(param.name, name.as_mut_ptr(), name.len());
        copy_str("Test", module.as_mut_ptr(), module.len());
    }
    // SAFETY: the host passes somewhere to write.
    unsafe {
        info.write(clap_param_info {
            id: param.id,
            flags: param.flags,
            cookie: ptr::null_mut(),
            name,
            module,
            min_value: param.min,
            max_value: param.max,
            default_value: param.default,
        });
    }
    true
}

unsafe extern "C" fn params_get_value(
    plugin: *const clap_plugin,
    id: clap_id,
    value: *mut f64,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("params.get_value");
    let Some(index) = this.index_of(id) else {
        return false;
    };
    // SAFETY: the host passes somewhere to write.
    unsafe { value.write(this.value(index)) };
    true
}

unsafe extern "C" fn params_value_to_text(
    plugin: *const clap_plugin,
    id: clap_id,
    value: f64,
    buffer: *mut c_char,
    capacity: u32,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("params.value_to_text");
    let text = match id {
        GAIN => format!("{value:.2}"),
        LATENCY => format!("{} samples", latency_frames(value)),
        LEVEL => format!("{:.0} %", value * 100.0),
        TRANSPOSE => format!("{value:+.0} st"),
        THREADS | BYPASS => if value >= 0.5 { "On" } else { "Off" }.to_owned(),
        _ => return false,
    };
    // SAFETY: the host passes a buffer of `capacity` bytes.
    unsafe { copy_str(&text, buffer, capacity as usize) };
    true
}

unsafe extern "C" fn params_text_to_value(
    plugin: *const clap_plugin,
    id: clap_id,
    text: *const c_char,
    value: *mut f64,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("params.text_to_value");
    // SAFETY: the host passes a nul-terminated string.
    let text = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let number = || text.split_whitespace().next()?.parse::<f64>().ok();
    let parsed = match id {
        GAIN | LATENCY | TRANSPOSE => number(),
        LEVEL => number().map(|n| n / 100.0),
        THREADS | BYPASS => match text.as_ref() {
            "On" => Some(1.0),
            "Off" => Some(0.0),
            _ => None,
        },
        _ => None,
    };
    let Some(parsed) = parsed else {
        return false;
    };
    // SAFETY: the host passes somewhere to write.
    unsafe { value.write(parsed) };
    true
}

unsafe extern "C" fn params_flush(
    plugin: *const clap_plugin,
    in_: *const clap_input_events,
    out: *const clap_output_events,
) {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    if this.active.load(Ordering::SeqCst) {
        this.on_audio("params.flush on an active plugin");
    } else {
        this.on_main("params.flush on an inactive plugin");
    }
    if this.processing.load(Ordering::SeqCst) {
        violation("params.flush was called while the plugin was processing");
    }
    // SAFETY: the host passes its event lists.
    if let Some(events) = unsafe { in_.as_ref() }
        && let (Some(size), Some(get)) = (events.size, events.get)
    {
        // SAFETY: as above.
        for index in 0..unsafe { size(events) } {
            // SAFETY: as above. Nothing processes while the host flushes.
            unsafe { this.apply_event(&mut *this.audio.get(), &*get(events, index), out) };
        }
    }
    // SAFETY: as above.
    unsafe { this.apply_gui(&mut *this.audio.get(), out) };
}

static STATE: clap_plugin_state = clap_plugin_state {
    save: Some(state_save),
    load: Some(state_load),
};

unsafe extern "C" fn state_save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("state.save");
    let state = this.save_state();
    let mut bytes = state.as_bytes();
    // SAFETY: the host passes its stream.
    let Some(write) = (unsafe { &*stream }).write else {
        return false;
    };
    while !bytes.is_empty() {
        // SAFETY: as above, with a buffer of the size given.
        let written = unsafe { write(stream, bytes.as_ptr().cast(), bytes.len() as u64) };
        if written <= 0 {
            return false;
        }
        bytes = &bytes[written as usize..];
    }
    true
}

unsafe extern "C" fn state_load(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("state.load");
    // SAFETY: the host passes its stream.
    let Some(read) = (unsafe { &*stream }).read else {
        return false;
    };
    let mut state = Vec::new();
    let mut chunk = [0u8; 7];
    loop {
        // SAFETY: as above, with a buffer of the size given. A small one,
        // so the host's stream has to cope with reads in pieces.
        let read = unsafe { read(stream, chunk.as_mut_ptr().cast(), chunk.len() as u64) };
        match read {
            0 => break,
            n if n < 0 => return false,
            n => state.extend_from_slice(&chunk[..n as usize]),
        }
    }
    match String::from_utf8(state) {
        Ok(state) => this.load_state(&state),
        Err(_) => false,
    }
}

static LATENCY_EXT: clap_plugin_latency = clap_plugin_latency {
    get: Some(latency_get),
};

unsafe extern "C" fn latency_get(plugin: *const clap_plugin) -> u32 {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("latency.get");
    if !this.active.load(Ordering::SeqCst) {
        violation("latency.get was called while the plugin was inactive");
    }
    this.active_latency.load(Ordering::SeqCst)
}

static AUDIO_PORTS: clap_plugin_audio_ports = clap_plugin_audio_ports {
    count: Some(audio_ports_count),
    get: Some(audio_ports_get),
};

unsafe extern "C" fn audio_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("audio_ports.count");
    match (this.kind, is_input) {
        (Kind::Arpeggiator, _) => 0,
        (Kind::Synth, true) => 0,
        (Kind::Gain, true) => 2,
        (_, false) => 1,
    }
}

unsafe extern "C" fn audio_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_audio_port_info,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("audio_ports.get");
    let (port, id, flags) = match (this.kind, is_input, index) {
        (Kind::Gain, true, 0) => ("In", 0, CLAP_AUDIO_PORT_IS_MAIN),
        (Kind::Gain, true, 1) => ("Sidechain", SIDECHAIN, 0),
        (_, false, 0) => ("Out", 1, CLAP_AUDIO_PORT_IS_MAIN),
        _ => return false,
    };
    let mut name = [0; 256];
    // SAFETY: the array holds as much as it says.
    unsafe { copy_str(port, name.as_mut_ptr(), name.len()) };
    // SAFETY: the host passes somewhere to write.
    unsafe {
        info.write(clap_audio_port_info {
            id,
            name,
            flags,
            channel_count: 2,
            port_type: CLAP_PORT_STEREO.as_ptr(),
            in_place_pair: CLAP_INVALID_ID,
        });
    }
    true
}

static PORTS_ACTIVATION: clap_plugin_audio_ports_activation = clap_plugin_audio_ports_activation {
    can_activate_while_processing: Some(ports_activation_while_processing),
    set_active: Some(ports_set_active),
};

unsafe extern "C" fn ports_activation_while_processing(plugin: *const clap_plugin) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("audio_ports_activation.can_activate_while_processing");
    false
}

/// Switches the gain's sidechain, as it can only while it's inactive: it
/// takes no other port.
unsafe extern "C" fn ports_set_active(
    plugin: *const clap_plugin,
    is_input: bool,
    port_index: u32,
    is_active: bool,
    sample_size: u32,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    // While inactive, which is on the main thread.
    this.on_main("audio_ports_activation.set_active");
    if this.active.load(Ordering::SeqCst) {
        violation("audio_ports_activation.set_active was called while the plugin was active");
        return false;
    }
    if !matches!(sample_size, 0 | 32 | 64) {
        violation("audio_ports_activation.set_active got a nonsense sample size");
        return false;
    }
    if !is_input || port_index != 1 {
        return false;
    }
    this.sidechain.store(is_active, Ordering::SeqCst);
    true
}

static NOTE_PORTS: clap_plugin_note_ports = clap_plugin_note_ports {
    count: Some(note_ports_count),
    get: Some(note_ports_get),
};

unsafe extern "C" fn note_ports_count(plugin: *const clap_plugin, is_input: bool) -> u32 {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("note_ports.count");
    // The arpeggiator plays notes too, and so does the echoing synth.
    u32::from(is_input || this.plays_notes())
}

unsafe extern "C" fn note_ports_get(
    plugin: *const clap_plugin,
    index: u32,
    is_input: bool,
    info: *mut clap_note_port_info,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let this = unsafe { this(plugin) };
    this.on_main("note_ports.get");
    if index != 0 || !(is_input || this.plays_notes()) {
        return false;
    }
    let mut name = [0; 256];
    let port = if is_input { "Notes" } else { "Notes Out" };
    // SAFETY: the array holds as much as it says.
    unsafe { copy_str(port, name.as_mut_ptr(), name.len()) };
    let both = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
    let (supported_dialects, preferred_dialect) = if this.mpe && this.kind == Kind::Synth {
        (CLAP_NOTE_DIALECT_MIDI_MPE, CLAP_NOTE_DIALECT_MIDI_MPE)
    } else if !is_input && MIDI_OUT.load(Ordering::SeqCst) {
        (both, CLAP_NOTE_DIALECT_MIDI)
    } else {
        (both, CLAP_NOTE_DIALECT_CLAP)
    };
    // SAFETY: the host passes somewhere to write.
    unsafe {
        info.write(clap_note_port_info {
            id: 0,
            supported_dialects,
            preferred_dialect,
            name,
        });
    }
    true
}

static THREAD_POOL: clap_plugin_thread_pool = clap_plugin_thread_pool {
    exec: Some(thread_pool_exec),
};

unsafe extern "C" fn thread_pool_exec(plugin: *const clap_plugin, task: u32) {
    // SAFETY: the host calls back with its plugin.
    unsafe { this(plugin) }.exec(task);
}

static GUI: clap_plugin_gui = clap_plugin_gui {
    is_api_supported: Some(gui_is_api_supported),
    get_preferred_api: Some(gui_get_preferred_api),
    create: Some(gui_create),
    destroy: Some(gui_destroy),
    set_scale: Some(gui_set_scale),
    get_size: Some(gui_get_size),
    can_resize: Some(gui_can_resize),
    get_resize_hints: Some(gui_get_resize_hints),
    adjust_size: Some(gui_adjust_size),
    set_size: Some(gui_set_size),
    set_parent: Some(gui_set_parent),
    set_transient: Some(gui_set_transient),
    suggest_title: Some(gui_suggest_title),
    show: Some(gui_show),
    hide: Some(gui_hide),
};

/// The plugin's pretend window, for a GUI call made on the main thread.
///
/// # Safety
///
/// As for the callbacks.
unsafe fn window<'a>(plugin: *const clap_plugin, what: &str) -> std::sync::MutexGuard<'a, Window> {
    // SAFETY: the caller's promise.
    let this = unsafe { this(plugin) };
    this.on_main(what);
    this.window.lock().unwrap_or_else(|e| e.into_inner())
}

fn known_api(api: *const c_char) -> bool {
    if api.is_null() {
        return false;
    }
    // SAFETY: the host passes a nul-terminated string.
    let api = unsafe { CStr::from_ptr(api) };
    [
        CLAP_WINDOW_API_WIN32,
        CLAP_WINDOW_API_COCOA,
        CLAP_WINDOW_API_X11,
    ]
    .contains(&api)
}

unsafe extern "C" fn gui_is_api_supported(
    plugin: *const clap_plugin,
    api: *const c_char,
    _: bool,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    drop(unsafe { window(plugin, "gui.is_api_supported") });
    known_api(api)
}

unsafe extern "C" fn gui_get_preferred_api(
    plugin: *const clap_plugin,
    api: *mut *const c_char,
    floating: *mut bool,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    drop(unsafe { window(plugin, "gui.get_preferred_api") });
    let preferred = if cfg!(windows) {
        CLAP_WINDOW_API_WIN32
    } else if cfg!(target_os = "macos") {
        CLAP_WINDOW_API_COCOA
    } else {
        CLAP_WINDOW_API_X11
    };
    // SAFETY: the host passes somewhere to write.
    unsafe {
        api.write(preferred.as_ptr());
        floating.write(false);
    }
    true
}

unsafe extern "C" fn gui_create(
    plugin: *const clap_plugin,
    api: *const c_char,
    floating: bool,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let mut window = unsafe { window(plugin, "gui.create") };
    if window.created {
        violation("gui.create was called with the window already created");
        return false;
    }
    if !known_api(api) {
        return false;
    }
    *window = Window {
        created: true,
        floating,
        width: 400,
        height: 300,
        ..Window::default()
    };
    true
}

unsafe extern "C" fn gui_destroy(plugin: *const clap_plugin) {
    // SAFETY: the host calls back with its plugin.
    let mut window = unsafe { window(plugin, "gui.destroy") };
    if !window.created {
        violation("gui.destroy was called with no window");
    }
    *window = Window::default();
}

unsafe extern "C" fn gui_set_scale(plugin: *const clap_plugin, scale: f64) -> bool {
    // SAFETY: the host calls back with its plugin.
    drop(unsafe { window(plugin, "gui.set_scale") });
    scale > 0.0
}

unsafe extern "C" fn gui_get_size(
    plugin: *const clap_plugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let window = unsafe { window(plugin, "gui.get_size") };
    if !window.created {
        violation("gui.get_size was called with no window");
        return false;
    }
    // SAFETY: the host passes somewhere to write.
    unsafe {
        width.write(window.width);
        height.write(window.height);
    }
    true
}

unsafe extern "C" fn gui_can_resize(plugin: *const clap_plugin) -> bool {
    // SAFETY: the host calls back with its plugin.
    drop(unsafe { window(plugin, "gui.can_resize") });
    true
}

unsafe extern "C" fn gui_get_resize_hints(
    plugin: *const clap_plugin,
    hints: *mut clap_gui_resize_hints,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    drop(unsafe { window(plugin, "gui.get_resize_hints") });
    // SAFETY: the host passes somewhere to write.
    unsafe {
        hints.write(clap_gui_resize_hints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            preserve_aspect_ratio: false,
            aspect_ratio_width: 0,
            aspect_ratio_height: 0,
        });
    }
    true
}

unsafe extern "C" fn gui_adjust_size(
    plugin: *const clap_plugin,
    width: *mut u32,
    height: *mut u32,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    drop(unsafe { window(plugin, "gui.adjust_size") });
    // SAFETY: the host passes its size, to adjust in place.
    unsafe {
        width.write((*width).max(100));
        height.write((*height).max(100));
    }
    true
}

unsafe extern "C" fn gui_set_size(plugin: *const clap_plugin, width: u32, height: u32) -> bool {
    // SAFETY: the host calls back with its plugin.
    let mut window = unsafe { window(plugin, "gui.set_size") };
    if !window.created || window.floating {
        violation("gui.set_size was called with no embedded window");
        return false;
    }
    window.width = width;
    window.height = height;
    true
}

unsafe extern "C" fn gui_set_parent(
    plugin: *const clap_plugin,
    parent: *const clap_window,
) -> bool {
    // SAFETY: the host calls back with its plugin.
    let mut window = unsafe { window(plugin, "gui.set_parent") };
    if !window.created || window.floating || parent.is_null() {
        violation("gui.set_parent was called with no embedded window");
        return false;
    }
    // SAFETY: the host passes a window; every API's handle fits a pointer.
    window.parent = unsafe { (*parent).specific.ptr } as usize;
    true
}

unsafe extern "C" fn gui_set_transient(plugin: *const clap_plugin, _: *const clap_window) -> bool {
    // SAFETY: the host calls back with its plugin.
    let window = unsafe { window(plugin, "gui.set_transient") };
    window.floating
}

unsafe extern "C" fn gui_suggest_title(plugin: *const clap_plugin, _: *const c_char) {
    // SAFETY: the host calls back with its plugin.
    drop(unsafe { window(plugin, "gui.suggest_title") });
}

unsafe extern "C" fn gui_show(plugin: *const clap_plugin) -> bool {
    // SAFETY: the host calls back with its plugin.
    let mut window = unsafe { window(plugin, "gui.show") };
    if !window.created || (!window.floating && window.parent == 0) {
        violation("gui.show was called with no window, or one without a parent");
        return false;
    }
    window.visible = true;
    true
}

unsafe extern "C" fn gui_hide(plugin: *const clap_plugin) -> bool {
    // SAFETY: the host calls back with its plugin.
    let mut window = unsafe { window(plugin, "gui.hide") };
    if !window.created {
        violation("gui.hide was called with no window");
        return false;
    }
    window.visible = false;
    true
}
