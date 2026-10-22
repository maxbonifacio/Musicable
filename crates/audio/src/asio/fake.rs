//! A fake ASIO driver for the tests: a driver object built in Rust, set up to
//! behave as real drivers do, that records what the host asks of it. The test
//! plays the driver's own thread, calling the callbacks the host handed over.

use std::ffi::{c_char, c_void};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use super::convert::Encoding;
use super::driver::Driver;
use super::stream::Loader;
use super::sys::{
    self, Bool, BufferInfo, Callbacks, ChannelInfo, Code, Long, Object, Time, TimeCode, TimeInfo,
    Vtable, Wide, sample_type,
};

/// How the fake behaves. The defaults make a well-behaved driver with eight
/// outputs of 32-bit integers, at 48 kHz.
#[derive(Debug, Clone)]
pub(super) struct Settings {
    /// Whether `init` succeeds.
    pub init: bool,
    /// What the driver says went wrong.
    pub error: &'static str,
    pub outputs: Long,
    /// The outputs' sample types: outputs past the list have its last.
    pub sample_types: Vec<Long>,
    pub rate: f64,
    /// The rates the driver can switch to.
    pub rates: Vec<u32>,
    /// The smallest, largest and preferred buffer size, and the step.
    pub sizes: [Long; 4],
    /// Whether creating buffers fails at any size but the preferred one.
    pub only_preferred: bool,
    pub latency: Long,
    pub reports_overloads: bool,
    pub takes_output_ready: bool,
    /// What `start` returns.
    pub start: Code,
    /// Whether `start` asks for the first half itself, before it returns,
    /// as some drivers do.
    pub calls_back_on_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            init: true,
            error: "",
            outputs: 8,
            sample_types: vec![sample_type::INT32_LSB],
            rate: 48_000.0,
            rates: vec![44_100, 48_000, 96_000],
            sizes: [64, 2048, 256, -1],
            only_preferred: false,
            latency: 300,
            reports_overloads: false,
            takes_output_ready: true,
            start: sys::OK,
            calls_back_on_start: false,
        }
    }
}

/// What the host did, as the fake saw it.
#[derive(Default)]
pub(super) struct Record {
    /// The methods called off the driver's thread, in order, each with the
    /// name of the thread that called it.
    pub calls: Vec<(&'static str, String)>,
    /// Driver objects loaded, and not yet released.
    pub loads: u32,
    pub live: u32,
    pub size: Long,
    /// The buffers, while they exist: each output's two halves.
    pub buffers: Vec<[Box<[u8]>; 2]>,
    /// What the host handed over to be called.
    pub callbacks: Option<&'static Callbacks>,
    pub running: bool,
    /// The half a punctual driver asks for next, for the tests to keep.
    pub next_index: Long,
}

pub(super) struct Fake {
    settings: Mutex<Settings>,
    record: Mutex<Record>,
    /// Where the next half starts.
    position: AtomicU64,
    /// Where the half asked for now starts.
    current: AtomicU64,
    /// Calls from the callbacks, which mustn't allocate.
    positions_asked: AtomicU64,
    outputs_ready: AtomicU64,
}

impl Fake {
    pub fn new(settings: Settings) -> Arc<Fake> {
        Arc::new(Fake {
            settings: Mutex::new(settings),
            record: Mutex::new(Record::default()),
            position: AtomicU64::new(0),
            current: AtomicU64::new(0),
            positions_asked: AtomicU64::new(0),
            outputs_ready: AtomicU64::new(0),
        })
    }

    /// Loads a driver object, as COM would, and initializes it.
    pub fn loader(self: &Arc<Self>) -> Loader {
        let fake = self.clone();
        Box::new(move || {
            {
                let mut record = fake.record();
                record.loads += 1;
                record.live += 1;
                record.calls.push(("load", thread_name()));
            }
            let instance = Box::new(Instance {
                object: Object { vtable: &VTABLE },
                references: AtomicU32::new(1),
                fake,
            });
            let object = NonNull::from(Box::leak(instance)).cast::<Object>();
            // SAFETY: a driver object, and its one reference.
            unsafe { Driver::init(object, ptr::null_mut()) }
        })
    }

    pub fn settings(&self) -> MutexGuard<'_, Settings> {
        self.settings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn record(&self) -> MutexGuard<'_, Record> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The methods called so far.
    pub fn calls(&self) -> Vec<&'static str> {
        self.record().calls.iter().map(|(call, _)| *call).collect()
    }

    /// Whether `call` was made.
    pub fn called(&self, call: &str) -> bool {
        self.calls().contains(&call)
    }

    /// The threads that called the driver, in order, without repeats.
    pub fn threads(&self) -> Vec<String> {
        let mut threads: Vec<String> = Vec::new();
        for (_, thread) in &self.record().calls {
            if threads.last() != Some(thread) {
                threads.push(thread.clone());
            }
        }
        threads
    }

    fn call(&self, call: &'static str) {
        self.record().calls.push((call, thread_name()));
    }

    /// As the driver's thread: asks the host to fill half `index`, saying
    /// where it starts.
    pub fn switch(&self, index: Long) {
        let callbacks = self.callbacks();
        let position = self.advance();
        let mut time = Time {
            reserved: [0; 4],
            info: TimeInfo {
                speed: 1.0,
                system_time: Wide::default(),
                sample_position: Wide::new(position),
                sample_rate: self.settings().rate,
                flags: sys::SAMPLE_POSITION_VALID,
                reserved: [0; 12],
            },
            code: TimeCode {
                speed: 0.0,
                samples: Wide::default(),
                flags: 0,
                future: [0; 64],
            },
        };
        // SAFETY: as a driver calls it, with the time of the half. The last
        // argument says the host may render there and then, as it does.
        unsafe { (callbacks.buffer_switch_time_info)(&mut time, index, 1) };
    }

    /// As the driver's thread: asks the host to fill half `index` without
    /// saying where it starts, as drivers do with hosts that don't take the
    /// time.
    pub fn switch_without_time(&self, index: Long) {
        let callbacks = self.callbacks();
        self.advance();
        // SAFETY: as a driver calls it.
        unsafe { (callbacks.buffer_switch)(index, 1) };
    }

    /// Lets `halves` buffer halves play without asking the host for them: a
    /// driver that skipped switches while the host ran late.
    pub fn skip(&self, halves: u64) {
        let size = u64::try_from(self.record().size).unwrap_or(0);
        self.position.fetch_add(halves * size, Ordering::Relaxed);
    }

    fn advance(&self) -> u64 {
        let size = u64::try_from(self.record().size).unwrap_or(0);
        let position = self.position.fetch_add(size, Ordering::Relaxed);
        self.current.store(position, Ordering::Relaxed);
        position
    }

    fn callbacks(&self) -> &'static Callbacks {
        self.record()
            .callbacks
            .expect("the host handed over its callbacks")
    }

    /// As the driver: sends the host a message.
    pub fn message(&self, selector: Long, value: Long) -> Long {
        // SAFETY: as a driver calls it.
        unsafe { (self.callbacks().message)(selector, value, ptr::null_mut(), ptr::null_mut()) }
    }

    /// As the driver: tells the host its rate changed.
    pub fn rate_changed(&self, rate: f64) {
        // SAFETY: as a driver calls it.
        unsafe { (self.callbacks().sample_rate_changed)(rate) };
    }

    /// A copy of half `half` of output `output`.
    pub fn output(&self, output: usize, half: usize) -> Vec<u8> {
        self.record().buffers[output][half].to_vec()
    }

    /// How often the callbacks asked for the sample position.
    pub fn positions_asked(&self) -> u64 {
        self.positions_asked.load(Ordering::Relaxed)
    }

    /// How often the callbacks said a half was written.
    pub fn outputs_ready(&self) -> u64 {
        self.outputs_ready.load(Ordering::Relaxed)
    }
}

fn thread_name() -> String {
    thread::current().name().unwrap_or("unnamed").to_owned()
}

/// A driver object: the vtable first, as COM lays objects out.
#[repr(C)]
struct Instance {
    object: Object,
    references: AtomicU32,
    fake: Arc<Fake>,
}

/// The fake behind a driver object.
///
/// # Safety
///
/// `this` is a live [`Instance`].
unsafe fn fake<'a>(this: *mut Object) -> &'a Fake {
    // SAFETY: every object with the fake's vtable is an instance.
    unsafe { &(*this.cast::<Instance>()).fake }
}

static VTABLE: Vtable = Vtable {
    query_interface,
    add_ref,
    release,
    init,
    driver_name,
    driver_version,
    error_message,
    start,
    stop,
    channels,
    latencies,
    buffer_size,
    can_sample_rate,
    sample_rate,
    set_sample_rate,
    clock_sources,
    set_clock_source,
    sample_position,
    channel_info,
    create_buffers,
    dispose_buffers,
    control_panel,
    future,
    output_ready,
};

/// `E_NOINTERFACE`.
const NO_INTERFACE: i32 = 0x8000_4002_u32 as i32;

unsafe extern "system" fn query_interface(
    _this: *mut Object,
    _iid: *const c_void,
    out: *mut *mut c_void,
) -> i32 {
    // SAFETY: the caller's pointer.
    unsafe { *out = ptr::null_mut() };
    NO_INTERFACE
}

unsafe extern "system" fn add_ref(this: *mut Object) -> u32 {
    // SAFETY: an instance.
    let instance = unsafe { &*this.cast::<Instance>() };
    instance.references.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(this: *mut Object) -> u32 {
    let left = {
        // SAFETY: an instance.
        let instance = unsafe { &*this.cast::<Instance>() };
        instance.fake.call("release");
        instance.references.fetch_sub(1, Ordering::AcqRel) - 1
    };
    if left == 0 {
        // SAFETY: leaked by the loader, and freed with the last reference.
        let instance = unsafe { Box::from_raw(this.cast::<Instance>()) };
        instance.fake.record().live -= 1;
    }
    left
}

unsafe extern "C" fn init(this: *mut Object, _system: *mut c_void) -> Bool {
    // SAFETY: the caller's instance, as for every method below.
    let fake = unsafe { fake(this) };
    fake.call("init");
    Bool::from(fake.settings().init)
}

unsafe extern "C" fn driver_name(_this: *mut Object, name: *mut c_char) {
    // SAFETY: the caller's buffer, 32 characters long.
    unsafe { ptr::copy_nonoverlapping(c"Fake".as_ptr(), name, 5) };
}

unsafe extern "C" fn driver_version(_this: *mut Object) -> Long {
    1
}

unsafe extern "C" fn error_message(this: *mut Object, text: *mut c_char) {
    let fake = unsafe { fake(this) };
    let error = fake.settings().error;
    let length = error.len().min(123);
    // SAFETY: the caller's buffer, 124 characters long.
    unsafe {
        ptr::copy_nonoverlapping(error.as_ptr().cast::<c_char>(), text, length);
        *text.add(length) = 0;
    }
}

unsafe extern "C" fn start(this: *mut Object) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("start");
    let (code, calls_back) = {
        let settings = fake.settings();
        (settings.start, settings.calls_back_on_start)
    };
    if code != sys::OK {
        return code;
    }
    fake.record().running = true;
    if calls_back {
        fake.switch(0);
    }
    sys::OK
}

unsafe extern "C" fn stop(this: *mut Object) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("stop");
    fake.record().running = false;
    sys::OK
}

unsafe extern "C" fn channels(this: *mut Object, inputs: *mut Long, outputs: *mut Long) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("channels");
    // SAFETY: the caller's longs.
    unsafe {
        *inputs = 2;
        *outputs = fake.settings().outputs;
    }
    sys::OK
}

unsafe extern "C" fn latencies(this: *mut Object, input: *mut Long, output: *mut Long) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("latencies");
    let latency = fake.settings().latency;
    // SAFETY: the caller's longs.
    unsafe {
        *input = latency;
        *output = latency;
    }
    sys::OK
}

unsafe extern "C" fn buffer_size(
    this: *mut Object,
    min: *mut Long,
    max: *mut Long,
    preferred: *mut Long,
    granularity: *mut Long,
) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("buffer_size");
    let sizes = fake.settings().sizes;
    // SAFETY: the caller's longs.
    unsafe {
        *min = sizes[0];
        *max = sizes[1];
        *preferred = sizes[2];
        *granularity = sizes[3];
    }
    sys::OK
}

impl Settings {
    fn takes_rate(&self, rate: f64) -> bool {
        self.rates.iter().any(|&known| f64::from(known) == rate)
    }
}

unsafe extern "C" fn can_sample_rate(this: *mut Object, rate: f64) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("can_sample_rate");
    if fake.settings().takes_rate(rate) {
        sys::OK
    } else {
        sys::NO_CLOCK
    }
}

unsafe extern "C" fn sample_rate(this: *mut Object, rate: *mut f64) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("sample_rate");
    // SAFETY: the caller's double.
    unsafe { *rate = fake.settings().rate };
    sys::OK
}

unsafe extern "C" fn set_sample_rate(this: *mut Object, rate: f64) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("set_sample_rate");
    let mut settings = fake.settings();
    if !settings.takes_rate(rate) {
        return sys::NO_CLOCK;
    }
    settings.rate = rate;
    sys::OK
}

unsafe extern "C" fn clock_sources(
    this: *mut Object,
    _clocks: *mut c_void,
    count: *mut Long,
) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("clock_sources");
    // SAFETY: the caller's long.
    unsafe { *count = 0 };
    sys::OK
}

unsafe extern "C" fn set_clock_source(this: *mut Object, _index: Long) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("set_clock_source");
    sys::OK
}

/// Called from the callbacks: no locks, no allocation.
unsafe extern "C" fn sample_position(
    this: *mut Object,
    position: *mut Wide,
    time: *mut Wide,
) -> Code {
    let fake = unsafe { fake(this) };
    fake.positions_asked.fetch_add(1, Ordering::Relaxed);
    // SAFETY: the caller's pairs.
    unsafe {
        *position = Wide::new(fake.current.load(Ordering::Relaxed));
        *time = Wide::default();
    }
    sys::OK
}

unsafe extern "C" fn channel_info(this: *mut Object, info: *mut ChannelInfo) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("channel_info");
    let settings = fake.settings();
    // SAFETY: the caller's structure.
    let info = unsafe { &mut *info };
    if !(0..settings.outputs).contains(&info.channel) {
        return sys::INVALID_PARAMETER;
    }
    info.sample_type = settings.sample_type(info.channel);
    info.is_active = 0;
    info.group = 0;
    info.name = [0; 32];
    sys::OK
}

impl Settings {
    fn sample_type(&self, channel: Long) -> Long {
        let types = &self.sample_types;
        types[usize::try_from(channel).unwrap_or(0).min(types.len() - 1)]
    }
}

unsafe extern "C" fn create_buffers(
    this: *mut Object,
    infos: *mut BufferInfo,
    count: Long,
    size: Long,
    callbacks: *const Callbacks,
) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("create_buffers");
    let settings = fake.settings().clone();
    let [min, max, preferred, _] = settings.sizes;
    if size < min || size > max || (settings.only_preferred && size != preferred) {
        return sys::INVALID_MODE;
    }
    // SAFETY: the caller's structures, `count` of them.
    let infos = unsafe { std::slice::from_raw_parts_mut(infos, count as usize) };
    let mut buffers = Vec::new();
    for info in infos {
        if info.is_input != 0 || !(0..settings.outputs).contains(&info.channel) {
            return sys::INVALID_PARAMETER;
        }
        let bytes =
            Encoding::from_type(settings.sample_type(info.channel)).map_or(1, Encoding::bytes);
        // Whatever the memory held before: the host clears it.
        let mut halves = [0, 1].map(|_| vec![0xAA_u8; size as usize * bytes].into_boxed_slice());
        info.buffers = [0, 1].map(|half| halves[half].as_mut_ptr().cast::<c_void>());
        buffers.push(halves);
    }
    let mut record = fake.record();
    record.size = size;
    record.buffers = buffers;
    // SAFETY: the host's callbacks, which it keeps until the buffers go.
    record.callbacks = Some(unsafe { &*callbacks });
    sys::OK
}

unsafe extern "C" fn dispose_buffers(this: *mut Object) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("dispose_buffers");
    fake.record().buffers.clear();
    sys::OK
}

unsafe extern "C" fn control_panel(this: *mut Object) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("control_panel");
    sys::OK
}

unsafe extern "C" fn future(this: *mut Object, selector: Long, _params: *mut c_void) -> Code {
    let fake = unsafe { fake(this) };
    fake.call("future");
    if selector == sys::future::CAN_REPORT_OVERLOAD && fake.settings().reports_overloads {
        sys::SUCCESS
    } else {
        sys::NOT_PRESENT
    }
}

/// Called from the callbacks too: no allocation.
unsafe extern "C" fn output_ready(this: *mut Object) -> Code {
    let fake = unsafe { fake(this) };
    fake.outputs_ready.fetch_add(1, Ordering::Relaxed);
    if fake.settings().takes_output_ready {
        sys::OK
    } else {
        sys::NOT_PRESENT
    }
}
