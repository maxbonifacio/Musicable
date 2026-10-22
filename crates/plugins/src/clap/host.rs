//! The host side of CLAP: the `clap_host` each plugin gets, the host
//! extensions it can call, and what an instance shares with its nodes.

use std::cell::{Cell, UnsafeCell};
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::time::{Duration, Instant};

use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_host_audio_ports};
use clap_sys::ext::gui::{CLAP_EXT_GUI, clap_host_gui};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_host_latency};
use clap_sys::ext::log::{
    CLAP_EXT_LOG, CLAP_LOG_ERROR, CLAP_LOG_FATAL, CLAP_LOG_HOST_MISBEHAVING,
    CLAP_LOG_PLUGIN_MISBEHAVING, CLAP_LOG_WARNING, clap_host_log, clap_log_severity,
};
use clap_sys::ext::note_ports::{
    CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI, clap_host_note_ports,
    clap_note_dialect,
};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, clap_host_params, clap_param_clear_flags, clap_param_rescan_flags,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_host_state};
use clap_sys::ext::thread_check::{CLAP_EXT_THREAD_CHECK, clap_host_thread_check};
use clap_sys::ext::thread_pool::{
    CLAP_EXT_THREAD_POOL, clap_host_thread_pool, clap_plugin_thread_pool,
};
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_host_timer_support};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::clap_plugin;
use clap_sys::version::CLAP_VERSION;

use super::node::Audio;
use crate::{Error, Waker};

/// Created and initialized, but not active.
pub(crate) const INACTIVE: u8 = 0;
/// Active, and not yet processing: the next node to process it starts it.
pub(crate) const ACTIVE: u8 = 1;
pub(crate) const PROCESSING: u8 = 2;
/// Active, but it failed to start processing: its nodes play silence.
pub(crate) const FAILED: u8 = 3;
/// Destroyed: nothing calls it any more.
pub(crate) const DESTROYED: u8 = 4;

/// What a plugin instance shares with its nodes and with the plugin's own
/// callbacks, at a fixed address for as long as any of them can use it.
pub(crate) struct Shared {
    /// The `clap_host` the plugin got. Its `host_data` points here.
    pub(crate) host: clap_host,
    /// Set once the plugin is created.
    pub(crate) plugin: AtomicPtr<clap_plugin>,
    /// The plugin's thread pool extension, if it has one.
    pub(crate) thread_pool: AtomicPtr<clap_plugin_thread_pool>,
    /// The plugin's main thread, which made the instance, as
    /// [`thread_mark`] gives it.
    main_thread: usize,
    /// [`INACTIVE`], [`ACTIVE`] and so on.
    lifecycle: AtomicU8,
    /// The plugin's latency, in frames, as it said when it last activated.
    latency: AtomicUsize,
    /// Set while a node processes the plugin, or while the main thread
    /// changes how it's set up. Whoever sets it has `audio`.
    busy: AtomicBool,
    pub(crate) requests: Requests,
    /// Timers the plugin registered. The main thread's only.
    pub(crate) timers: Mutex<Timers>,
    pub(crate) audio: UnsafeCell<Audio>,
    /// What the plugin calls itself, for its log messages.
    pub(crate) name: String,
    pub(crate) waker: Option<Waker>,
}

// SAFETY: `audio` is only touched by whoever holds `busy`, and the plugin
// pointer by the threads CLAP's rules allow. The rest is atomic or locked.
unsafe impl Send for Shared {}
// SAFETY: as above.
unsafe impl Sync for Shared {}

/// What a plugin asked of the host, from whatever thread, for the main
/// thread to act on.
#[derive(Debug, Default)]
pub(crate) struct Requests {
    pub(crate) restart: AtomicBool,
    pub(crate) callback: AtomicBool,
    pub(crate) flush: AtomicBool,
    pub(crate) dirty: AtomicBool,
    /// The plugin said its latency changed outside activating, where CLAP
    /// wants a restart instead.
    pub(crate) latency_changed: AtomicBool,
    pub(crate) params_rescan: AtomicU32,
    /// The plugin's audio or note ports changed: they're read again when
    /// it restarts.
    pub(crate) ports_rescan: AtomicBool,
    pub(crate) window_closed: AtomicBool,
    /// The size the window asks for, as `width << 32 | height`, or
    /// [`NO_RESIZE`].
    pub(crate) window_resize: AtomicU64,
}

pub(crate) const NO_RESIZE: u64 = u64::MAX;

impl Shared {
    /// What's shared for a plugin called `name`, made on this thread, its
    /// main thread. `host_data` is where the `Arc` will put it.
    pub(crate) fn new(
        host_data: *const Shared,
        name: &str,
        audio: Audio,
        waker: Option<Waker>,
    ) -> Self {
        Shared {
            host: clap_host {
                clap_version: CLAP_VERSION,
                host_data: host_data.cast_mut().cast(),
                name: c"Musicable".as_ptr(),
                vendor: c"Musicable".as_ptr(),
                url: c"https://github.com/maxbonifacio/Musicable".as_ptr(),
                version: HOST_VERSION.as_ptr(),
                get_extension: Some(get_extension),
                request_restart: Some(request_restart),
                request_process: Some(request_process),
                request_callback: Some(request_callback),
            },
            plugin: AtomicPtr::new(ptr::null_mut()),
            thread_pool: AtomicPtr::new(ptr::null_mut()),
            main_thread: thread_mark(),
            lifecycle: AtomicU8::new(INACTIVE),
            latency: AtomicUsize::new(0),
            busy: AtomicBool::new(false),
            requests: Requests {
                window_resize: AtomicU64::new(NO_RESIZE),
                ..Requests::default()
            },
            timers: Mutex::new(Timers::default()),
            audio: UnsafeCell::new(audio),
            name: name.to_owned(),
            waker,
        }
    }

    pub(crate) fn plugin(&self) -> *const clap_plugin {
        self.plugin.load(Ordering::Acquire)
    }

    pub(crate) fn lifecycle(&self) -> u8 {
        self.lifecycle.load(Ordering::Acquire)
    }

    pub(crate) fn set_lifecycle(&self, lifecycle: u8) {
        self.lifecycle.store(lifecycle, Ordering::Release);
    }

    pub(crate) fn latency(&self) -> usize {
        self.latency.load(Ordering::Acquire)
    }

    pub(crate) fn set_latency(&self, latency: usize) {
        self.latency.store(latency, Ordering::Release);
    }

    /// Takes `busy` if no one has it, as a node does to process.
    pub(crate) fn try_enter(&self) -> Option<Busy<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| Busy(self))
    }

    /// Takes `busy` on the main thread, waiting for a node that's
    /// processing to finish its block. Nodes that try to process meanwhile
    /// play silence.
    pub(crate) fn enter(&self) -> Result<Busy<'_>, Error> {
        let start = Instant::now();
        loop {
            if let Some(busy) = self.try_enter() {
                return Ok(busy);
            }
            if start.elapsed() > Duration::from_secs(5) {
                return Err(Error::new(format!(
                    "{} has been processing one block for over five seconds",
                    self.name
                )));
            }
            std::thread::yield_now();
        }
    }

    /// Whether this is the thread that made the instance.
    pub(crate) fn on_main_thread(&self) -> bool {
        thread_mark() == self.main_thread
    }

    /// Tells the app the plugin needs the main thread.
    pub(crate) fn wake(&self) {
        // Waking runs the app's code, which isn't real-time safe. The app
        // polls often anyway.
        if !is_audio_thread()
            && let Some(waker) = &self.waker
        {
            waker();
        }
    }
}

/// [`Shared::busy`], held: released when dropped.
pub(crate) struct Busy<'a>(&'a Shared);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

/// Timers a plugin registered, which the main thread fires as it polls.
#[derive(Debug, Default)]
pub(crate) struct Timers {
    last_id: clap_id,
    pub(crate) timers: Vec<Timer>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Timer {
    pub(crate) id: clap_id,
    pub(crate) period: Duration,
    pub(crate) next: Instant,
}

impl Timers {
    /// The ids of the timers due by `now`, which then wait a period more.
    pub(crate) fn due(&mut self, now: Instant) -> Vec<clap_id> {
        let mut due = Vec::new();
        for timer in &mut self.timers {
            if timer.next <= now {
                due.push(timer.id);
                timer.next = (timer.next + timer.period).max(now);
            }
        }
        due
    }

    /// When the next timer is due.
    pub(crate) fn next(&self) -> Option<Instant> {
        self.timers.iter().map(|t| t.next).min()
    }
}

static HOST_VERSION: &CStr =
    match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(version) => version,
        Err(_) => c"0",
    };

thread_local! {
    /// Only its address matters: each thread's is its own.
    static MARK: u8 = const { 0 };
    /// How many plugin calls the thread is inside that CLAP counts as on an
    /// audio thread: processing, and thread pool tasks.
    static AUDIO: Cell<u32> = const { Cell::new(0) };
}

/// Tells this thread from every other thread alive. Unlike
/// `thread::current().id()`, it never allocates, so it's safe on any thread.
pub(crate) fn thread_mark() -> usize {
    MARK.with(|mark| ptr::from_ref(mark) as usize)
}

/// Whether this thread is in a plugin call CLAP counts as on an audio
/// thread.
pub(crate) fn is_audio_thread() -> bool {
    AUDIO.with(Cell::get) > 0
}

/// Marks this thread as an audio thread, for CLAP, until dropped.
pub(crate) struct AudioThread(());

impl AudioThread {
    pub(crate) fn enter() -> Self {
        AUDIO.with(|depth| depth.set(depth.get() + 1));
        Self(())
    }
}

impl Drop for AudioThread {
    fn drop(&mut self) {
        AUDIO.with(|depth| depth.set(depth.get() - 1));
    }
}

/// The instance's shared state, from the host pointer a plugin calls back
/// with.
///
/// # Safety
///
/// `host` must be null or one of ours, whose instance is alive.
unsafe fn shared<'a>(host: *const clap_host) -> Option<&'a Shared> {
    // SAFETY: the caller's promise: `host_data` is the shared state.
    unsafe {
        host.as_ref()?
            .host_data
            .cast::<Shared>()
            .cast_const()
            .as_ref()
    }
}

unsafe extern "C" fn get_extension(_: *const clap_host, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    // SAFETY: the plugin passes a nul-terminated id.
    let id = unsafe { CStr::from_ptr(id) };
    let extensions: [(&CStr, *const c_void); 10] = [
        (CLAP_EXT_LOG, (&raw const LOG).cast()),
        (CLAP_EXT_THREAD_CHECK, (&raw const THREAD_CHECK).cast()),
        (CLAP_EXT_LATENCY, (&raw const LATENCY).cast()),
        (CLAP_EXT_PARAMS, (&raw const PARAMS).cast()),
        (CLAP_EXT_STATE, (&raw const STATE).cast()),
        (CLAP_EXT_AUDIO_PORTS, (&raw const AUDIO_PORTS).cast()),
        (CLAP_EXT_NOTE_PORTS, (&raw const NOTE_PORTS).cast()),
        (CLAP_EXT_GUI, (&raw const GUI).cast()),
        (CLAP_EXT_TIMER_SUPPORT, (&raw const TIMER_SUPPORT).cast()),
        (CLAP_EXT_THREAD_POOL, (&raw const THREAD_POOL).cast()),
    ];
    extensions
        .into_iter()
        .find(|(name, _)| *name == id)
        .map_or(ptr::null(), |(_, extension)| extension)
}

unsafe extern "C" fn request_restart(host: *const clap_host) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared.requests.restart.store(true, Ordering::Release);
        shared.wake();
    }
}

/// The engine processes every plugin every block, so there's nothing to do.
unsafe extern "C" fn request_process(_: *const clap_host) {}

unsafe extern "C" fn request_callback(host: *const clap_host) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared.requests.callback.store(true, Ordering::Release);
        shared.wake();
    }
}

static LOG: clap_host_log = clap_host_log { log: Some(log) };

unsafe extern "C" fn log(
    host: *const clap_host,
    severity: clap_log_severity,
    message: *const c_char,
) {
    // No I/O on an audio thread. Plugins shouldn't log there, but some do.
    if is_audio_thread() || message.is_null() {
        return;
    }
    let severity = match severity {
        CLAP_LOG_WARNING => "warning",
        CLAP_LOG_ERROR | CLAP_LOG_FATAL => "error",
        CLAP_LOG_HOST_MISBEHAVING => "says the host broke a rule",
        CLAP_LOG_PLUGIN_MISBEHAVING => "broke a rule",
        _ if cfg!(debug_assertions) => "info",
        _ => return,
    };
    // SAFETY: plugins call back with their host.
    let name = unsafe { shared(host) }.map_or("A plugin", |shared| shared.name.as_str());
    // SAFETY: the plugin passes a nul-terminated message.
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    eprintln!("{name} ({severity}): {message}");
}

static THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(is_main_thread),
    is_audio_thread: Some(is_audio_thread_callback),
};

unsafe extern "C" fn is_main_thread(host: *const clap_host) -> bool {
    // SAFETY: plugins call back with their host.
    unsafe { shared(host) }.is_some_and(Shared::on_main_thread)
}

unsafe extern "C" fn is_audio_thread_callback(_: *const clap_host) -> bool {
    is_audio_thread()
}

static LATENCY: clap_host_latency = clap_host_latency {
    changed: Some(latency_changed),
};

/// CLAP allows it only while the plugin activates, after which the host
/// reads the latency anyway. Outside that, it's taken as asking for a
/// restart.
unsafe extern "C" fn latency_changed(host: *const clap_host) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared
            .requests
            .latency_changed
            .store(true, Ordering::Release);
    }
}

static PARAMS: clap_host_params = clap_host_params {
    rescan: Some(params_rescan),
    clear: Some(params_clear),
    request_flush: Some(params_request_flush),
};

unsafe extern "C" fn params_rescan(host: *const clap_host, flags: clap_param_rescan_flags) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared
            .requests
            .params_rescan
            .fetch_or(flags, Ordering::AcqRel);
        shared.wake();
    }
}

/// Nothing automates or modulates plugin parameters yet, so there's nothing
/// to clear.
unsafe extern "C" fn params_clear(_: *const clap_host, _: clap_id, _: clap_param_clear_flags) {}

unsafe extern "C" fn params_request_flush(host: *const clap_host) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared.requests.flush.store(true, Ordering::Release);
        shared.wake();
    }
}

static STATE: clap_host_state = clap_host_state {
    mark_dirty: Some(mark_dirty),
};

unsafe extern "C" fn mark_dirty(host: *const clap_host) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared.requests.dirty.store(true, Ordering::Release);
        shared.wake();
    }
}

static AUDIO_PORTS: clap_host_audio_ports = clap_host_audio_ports {
    is_rescan_flag_supported: Some(audio_ports_rescan_supported),
    rescan: Some(ports_rescan),
};

/// Every change to a plugin's ports restarts it, which covers them all.
unsafe extern "C" fn audio_ports_rescan_supported(_: *const clap_host, _: u32) -> bool {
    true
}

unsafe extern "C" fn ports_rescan(host: *const clap_host, _: u32) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared.requests.ports_rescan.store(true, Ordering::Release);
        shared.wake();
    }
}

static NOTE_PORTS: clap_host_note_ports = clap_host_note_ports {
    supported_dialects: Some(note_dialects),
    rescan: Some(ports_rescan),
};

unsafe extern "C" fn note_dialects(_: *const clap_host) -> clap_note_dialect {
    CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI
}

static GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(gui_resize_hints_changed),
    request_resize: Some(gui_request_resize),
    request_show: Some(gui_request_show),
    request_hide: Some(gui_request_hide),
    closed: Some(gui_closed),
};

/// The window asks the plugin for its hints whenever it resizes, so new
/// ones need nothing done now.
unsafe extern "C" fn gui_resize_hints_changed(_: *const clap_host) {}

unsafe extern "C" fn gui_request_resize(host: *const clap_host, width: u32, height: u32) -> bool {
    // SAFETY: plugins call back with their host.
    let Some(shared) = (unsafe { shared(host) }) else {
        return false;
    };
    let size = u64::from(width) << 32 | u64::from(height);
    shared.requests.window_resize.store(size, Ordering::Release);
    shared.wake();
    true
}

/// The window shows when someone opens it, and only then.
unsafe extern "C" fn gui_request_show(_: *const clap_host) -> bool {
    false
}

unsafe extern "C" fn gui_request_hide(host: *const clap_host) -> bool {
    // SAFETY: plugins call back with their host.
    let Some(shared) = (unsafe { shared(host) }) else {
        return false;
    };
    shared.requests.window_closed.store(true, Ordering::Release);
    shared.wake();
    true
}

unsafe extern "C" fn gui_closed(host: *const clap_host, _was_destroyed: bool) {
    // SAFETY: plugins call back with their host.
    if let Some(shared) = unsafe { shared(host) } {
        shared.requests.window_closed.store(true, Ordering::Release);
        shared.wake();
    }
}

static TIMER_SUPPORT: clap_host_timer_support = clap_host_timer_support {
    register_timer: Some(register_timer),
    unregister_timer: Some(unregister_timer),
};

/// The shortest period a timer gets: plugins ask for anything from 1 ms.
const MIN_TIMER_PERIOD: Duration = Duration::from_millis(10);

unsafe extern "C" fn register_timer(
    host: *const clap_host,
    period_ms: u32,
    id: *mut clap_id,
) -> bool {
    // SAFETY: plugins call back with their host.
    let Some(shared) = (unsafe { shared(host) }) else {
        return false;
    };
    if id.is_null() || !shared.on_main_thread() {
        return false;
    }
    let mut timers = shared.timers.lock().unwrap_or_else(|e| e.into_inner());
    timers.last_id = timers.last_id.wrapping_add(1);
    let period = Duration::from_millis(period_ms.into()).max(MIN_TIMER_PERIOD);
    let timer = Timer {
        id: timers.last_id,
        period,
        next: Instant::now() + period,
    };
    timers.timers.push(timer);
    // SAFETY: checked non-null; the plugin passes somewhere to write.
    unsafe { id.write(timer.id) };
    drop(timers);
    shared.wake();
    true
}

unsafe extern "C" fn unregister_timer(host: *const clap_host, id: clap_id) -> bool {
    // SAFETY: plugins call back with their host.
    let Some(shared) = (unsafe { shared(host) }) else {
        return false;
    };
    let mut timers = shared.timers.lock().unwrap_or_else(|e| e.into_inner());
    let before = timers.timers.len();
    timers.timers.retain(|t| t.id != id);
    timers.timers.len() < before
}

static THREAD_POOL: clap_host_thread_pool = clap_host_thread_pool {
    request_exec: Some(request_exec),
};

/// Runs the plugin's tasks on the renderer's threads, as many at once as
/// are free, while the thread processing it waits for them
/// ([`tasks::run`](musicable_engine::tasks::run)). Each counts as an audio
/// thread while it runs one.
unsafe extern "C" fn request_exec(host: *const clap_host, tasks: u32) -> bool {
    // SAFETY: plugins call back with their host.
    let Some(shared) = (unsafe { shared(host) }) else {
        return false;
    };
    let pool = shared.thread_pool.load(Ordering::Acquire);
    // SAFETY: the extension lives as long as the plugin, and processing
    // holds the plugin.
    let Some(exec) = (unsafe { pool.as_ref() }).and_then(|p| p.exec) else {
        return false;
    };
    if !is_audio_thread() {
        return false;
    }
    let plugin = Pooled(shared.plugin());
    musicable_engine::tasks::run(tasks, &|task| {
        let _audio_thread = AudioThread::enter();
        // SAFETY: the plugin asked, from processing, which waits for its
        // tasks.
        unsafe { exec(plugin.get(), task) };
    });
    true
}

/// A plugin whose tasks run on several threads at once.
struct Pooled(*const clap_plugin);

// SAFETY: CLAP's thread pool calls a plugin's `exec` from any thread, and
// from several at once.
unsafe impl Sync for Pooled {}

impl Pooled {
    fn get(&self) -> *const clap_plugin {
        self.0
    }
}
