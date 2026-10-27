//! The test plugins as a VST3 module: Test Gain and Test Synth again.
//!
//! - **Test Gain** is a component and an edit controller of its own, as most
//!   VST3 plugins are, which talk through the host's messages. Its Gain and
//!   Latency parameters work as the CLAP plugin's do, normalized: Gain from
//!   0 to 2 and Latency from 0 to 48,000 frames in as many steps. Changing
//!   the latency at the controller asks the host to restart the plugin, and
//!   the processor takes the new latency as it's activated again. Peak is a
//!   meter: the processor sets it to each block's peak. Its second input
//!   bus, Sidechain ([`VST3_SIDECHAIN`]), ducks it as the CLAP gain's does,
//!   while the host has it on: VST3's buses start off, and the host turns
//!   them on and off only while the plugin is inactive.
//! - **Test Synth** is one object, component and controller at once. It
//!   plays each note as the CLAP synth does, shaped by VST3's tuning and
//!   brightness note expressions and its poly pressure. Its MIDI mapping
//!   has MIDI's volume controller, 7, move its Level, as VST3 plugins take
//!   controllers.
//!
//! Both count each breach of VST3's rules as a violation, in the same count
//! as the CLAP plugins: which thread calls what, and in which state. Their
//! windows are pretend ones. Tests drive them through the hooks here, which
//! take the plugin's component or controller, as the host has it. The
//! module's entry is where a copy named to misbehave misbehaves, as the
//! CLAP entry does.

use std::f64::consts::TAU;
use std::ffi::{CStr, c_char, c_void};
use std::fmt::Write as _;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use vst3::Steinberg::PClassInfo_::ClassCardinality_::kManyInstances;
use vst3::Steinberg::PFactoryInfo_::FactoryFlags_::kUnicode;
use vst3::Steinberg::Vst::BusDirections_::kInput;
use vst3::Steinberg::Vst::BusInfo_::BusFlags_::kDefaultActive;
use vst3::Steinberg::Vst::BusTypes_::{kAux, kMain};
use vst3::Steinberg::Vst::Event_::EventTypes_::{
    kNoteExpressionValueEvent, kNoteOffEvent, kNoteOnEvent, kPolyPressureEvent,
};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::NoteExpressionTypeIDs_::{kBrightnessTypeID, kTuningTypeID};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsReadOnly};
use vst3::Steinberg::Vst::RestartFlags_::kLatencyChanged;
use vst3::Steinberg::Vst::SpeakerArr::kStereo;
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::{
    BusDirection, BusInfo, BusType, CtrlNumber, Event, IAttributeListTrait, IAudioProcessor,
    IAudioProcessorTrait, IComponent, IComponentHandler, IComponentHandler2,
    IComponentHandler2Trait, IComponentHandlerTrait, IComponentTrait, IConnectionPoint,
    IConnectionPointTrait, IEditController, IEditControllerTrait, IEventList, IEventListTrait,
    IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait, IMidiMapping,
    IMidiMappingTrait, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait, IUnitInfo,
    IUnitInfoTrait, IoMode, MediaType, NoteExpressionTypeID, ParamID, ParamValue, ParameterInfo,
    ProcessContext, ProcessData, ProcessSetup, ProgramListID, ProgramListInfo, RoutingInfo,
    SpeakerArrangement, String128, TChar, UnitID, UnitInfo, kNoParentUnitId, kRootUnitId,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IBStream, IBStreamTrait, IPlugFrame, IPlugFrameTrait, IPlugView,
    IPlugViewContentScaleSupport, IPlugViewContentScaleSupportTrait, IPlugViewTrait,
    IPluginBaseTrait, IPluginFactory, IPluginFactory2Trait, IPluginFactory3, IPluginFactory3Trait,
    IPluginFactoryTrait, PClassInfo, PClassInfo2, PClassInfoW, PFactoryInfo, TBool, TUID, ViewRect,
    char16, int16, int32, int64, kInvalidArgument, kNotImplemented, kPlatformTypeHWND,
    kPlatformTypeNSView, kPlatformTypeX11EmbedWindowID, kResultFalse, kResultOk, kResultTrue,
    tresult, uint16, uint32,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface, uid};

use crate::plugin::copy_str;
use crate::{ALIVE, GAIN, LATENCY, LEVEL, gui, violation};

/// The class ids, made from `MUSICABLETEST` and a number.
const fn class_id(n: u32) -> TUID {
    uid(0x4D55_5349, 0x4341_424C, 0x4554_4553, 0x5400_0000 | n)
}

const GAIN_CID: TUID = class_id(1);
const GAIN_CONTROLLER_CID: TUID = class_id(2);
const SYNTH_CID: TUID = class_id(3);

/// Test Gain's class id, as hosts name VST3 plugins: 32 hex digits.
pub const VST3_GAIN_ID: &str = "4D5553494341424C4554455354000001";
/// Test Synth's.
pub const VST3_SYNTH_ID: &str = "4D5553494341424C4554455354000003";

/// Test Gain's meter, which its processor sets to each block's peak.
pub const PEAK: u32 = 12;

/// Test Gain's sidechain: its second audio input bus, by index, as hosts
/// know VST3's buses.
pub const VST3_SIDECHAIN: u32 = 1;

/// The transport, as a VST3 test plugin last saw it: see
/// [`musicable_test_vst3_transport`].
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vst3Transport {
    /// VST3's `ProcessContext` flags.
    pub state: u32,
    pub samples: i64,
    /// In quarter notes, as the rest.
    pub beats: f64,
    pub bar_start: f64,
    pub tempo: f64,
    pub cycle: [f64; 2],
    pub signature: [i32; 2],
    /// Frames since the plugin was activated.
    pub steady: i64,
}

impl Vst3Transport {
    fn of(context: &ProcessContext) -> Self {
        Self {
            state: context.state,
            samples: context.projectTimeSamples,
            beats: context.projectTimeMusic,
            bar_start: context.barPositionMusic,
            tempo: context.tempo,
            cycle: [context.cycleStartMusic, context.cycleEndMusic],
            signature: [context.timeSigNumerator, context.timeSigDenominator],
            steady: context.continousTimeSamples,
        }
    }
}

// The module's entry and its factory.

/// Whether the module is entered: between its entry and its exit.
static ENTERED: AtomicBool = AtomicBool::new(false);

fn enter() -> bool {
    let name = own_file_name();
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
    if ENTERED.swap(true, Ordering::SeqCst) {
        violation("the VST3 module was entered twice without exiting");
    }
    true
}

fn exit() -> bool {
    if !ENTERED.swap(false, Ordering::SeqCst) {
        violation("the VST3 module exited without being entered");
    }
    if ALIVE.load(Ordering::SeqCst) > 0 {
        violation("the VST3 module exited with objects of its alive");
    }
    true
}

/// The module's entry on Linux, with the library's handle.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn ModuleEntry(_: *mut c_void) -> bool {
    enter()
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn ModuleExit() -> bool {
    exit()
}

/// The module's entry on Windows.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn InitDll() -> bool {
    enter()
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn ExitDll() -> bool {
    exit()
}

/// The module's entry on macOS, with its bundle.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn bundleEntry(_: *mut c_void) -> bool {
    enter()
}

#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "C" fn bundleExit() -> bool {
    exit()
}

/// The module's factory, a reference to it for the host to let go of.
#[allow(non_snake_case)]
#[unsafe(no_mangle)]
pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
    if !ENTERED.load(Ordering::SeqCst) {
        violation("GetPluginFactory came before the module's entry");
    }
    ComWrapper::new(Factory::default())
        .to_com_ptr::<IPluginFactory>()
        .map_or(ptr::null_mut(), ComPtr::into_raw)
}

/// The lowercased name of the file this library was loaded from.
fn own_file_name() -> String {
    let path = own_path();
    Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

#[cfg(unix)]
fn own_path() -> String {
    #[repr(C)]
    struct DlInfo {
        fname: *const c_char,
        fbase: *mut c_void,
        sname: *const c_char,
        saddr: *mut c_void,
    }
    unsafe extern "C" {
        fn dladdr(address: *const c_void, info: *mut DlInfo) -> i32;
    }
    let mut info = DlInfo {
        fname: ptr::null(),
        fbase: ptr::null_mut(),
        sname: ptr::null(),
        saddr: ptr::null_mut(),
    };
    // SAFETY: an address in this library, and somewhere to write.
    let found = unsafe { dladdr((&raw const ENTERED).cast(), &mut info) } != 0;
    if !found || info.fname.is_null() {
        return String::new();
    }
    // SAFETY: dladdr gives a nul-terminated path.
    unsafe { CStr::from_ptr(info.fname) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn own_path() -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleExW(flags: u32, address: *const c_void, module: *mut *mut c_void) -> i32;
        fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
    }
    const UNCHANGED_REFCOUNT: u32 = 0x2;
    const FROM_ADDRESS: u32 = 0x4;
    let mut module = ptr::null_mut();
    // SAFETY: an address in this library, and somewhere to write.
    let found = unsafe {
        GetModuleHandleExW(
            FROM_ADDRESS | UNCHANGED_REFCOUNT,
            (&raw const ENTERED).cast(),
            &mut module,
        )
    } != 0;
    if !found {
        return String::new();
    }
    let mut name = vec![0u16; 32_768];
    // SAFETY: the module is this library, with room for the name.
    let len = unsafe { GetModuleFileNameW(module, name.as_mut_ptr(), name.len() as u32) };
    String::from_utf16_lossy(&name[..(len as usize).min(name.len())])
}

/// A class in the module.
struct ClassInfo {
    cid: TUID,
    category: &'static str,
    name: &'static str,
    sub_categories: &'static str,
}

const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

static CLASSES: [ClassInfo; 3] = [
    ClassInfo {
        cid: GAIN_CID,
        category: AUDIO_MODULE_CLASS,
        name: "Test Gain",
        sub_categories: "Fx",
    },
    ClassInfo {
        cid: GAIN_CONTROLLER_CID,
        category: "Component Controller Class",
        name: "Test Gain",
        sub_categories: "",
    },
    ClassInfo {
        cid: SYNTH_CID,
        category: AUDIO_MODULE_CLASS,
        name: "Test Synth",
        sub_categories: "Instrument|Synth",
    },
];

const VENDOR: &str = "Musicable";
const VERSION: &str = "1.0.0";
const SDK_VERSION: &str = "VST 3.7.12";

#[derive(Default)]
struct Factory {
    /// What the host gave as its context, if it did.
    host: Mutex<Option<ComPtr<FUnknown>>>,
}

impl Class for Factory {
    type Interfaces = (IPluginFactory3,);
}

impl Factory {
    fn class(index: int32) -> Option<&'static ClassInfo> {
        CLASSES.get(usize::try_from(index).ok()?)
    }
}

impl IPluginFactoryTrait for Factory {
    unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
        // SAFETY: the host passes somewhere to write, or null.
        let Some(info) = (unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        // SAFETY: zeroed is a valid info.
        *info = unsafe { std::mem::zeroed() };
        write_ascii(&mut info.vendor, VENDOR);
        write_ascii(&mut info.url, "https://github.com/maxbonifacio/musicable");
        info.flags = kUnicode as int32;
        kResultOk
    }

    unsafe fn countClasses(&self) -> int32 {
        CLASSES.len() as int32
    }

    unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
        // SAFETY: as above.
        let (Some(class), Some(info)) = (Self::class(index), unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        // SAFETY: zeroed is a valid info.
        *info = unsafe { std::mem::zeroed() };
        info.cid = class.cid;
        info.cardinality = kManyInstances as int32;
        write_ascii(&mut info.category, class.category);
        write_ascii(&mut info.name, class.name);
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: FIDString,
        iid: FIDString,
        obj: *mut *mut c_void,
    ) -> tresult {
        if obj.is_null() || cid.is_null() || iid.is_null() {
            violation("createInstance got a null id or nowhere to write");
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; ids are 16 bytes.
        let cid = unsafe {
            obj.write(ptr::null_mut());
            *cid.cast::<TUID>()
        };
        let made = if cid == GAIN_CID {
            Gain::create()
        } else if cid == GAIN_CONTROLLER_CID {
            GainController::create()
        } else if cid == SYNTH_CID {
            Synth::create()
        } else {
            return kInvalidArgument;
        };
        let unknown = made.as_ptr();
        // SAFETY: an object just made, asked for the interface the host
        // wants, which then holds a reference of its own.
        unsafe { ((*(*unknown).vtbl).queryInterface)(unknown, iid.cast(), obj) }
    }
}

impl IPluginFactory2Trait for Factory {
    unsafe fn getClassInfo2(&self, index: int32, info: *mut PClassInfo2) -> tresult {
        // SAFETY: as for `getClassInfo`.
        let (Some(class), Some(info)) = (Self::class(index), unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        // SAFETY: zeroed is a valid info.
        *info = unsafe { std::mem::zeroed() };
        info.cid = class.cid;
        info.cardinality = kManyInstances as int32;
        write_ascii(&mut info.category, class.category);
        write_ascii(&mut info.name, class.name);
        write_ascii(&mut info.subCategories, class.sub_categories);
        write_ascii(&mut info.vendor, VENDOR);
        write_ascii(&mut info.version, VERSION);
        write_ascii(&mut info.sdkVersion, SDK_VERSION);
        kResultOk
    }
}

impl IPluginFactory3Trait for Factory {
    unsafe fn getClassInfoUnicode(&self, index: int32, info: *mut PClassInfoW) -> tresult {
        // SAFETY: as for `getClassInfo`.
        let (Some(class), Some(info)) = (Self::class(index), unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        // SAFETY: zeroed is a valid info.
        *info = unsafe { std::mem::zeroed() };
        info.cid = class.cid;
        info.cardinality = kManyInstances as int32;
        write_ascii(&mut info.category, class.category);
        write_utf16(&mut info.name, class.name);
        write_ascii(&mut info.subCategories, class.sub_categories);
        write_utf16(&mut info.vendor, VENDOR);
        write_utf16(&mut info.version, VERSION);
        write_utf16(&mut info.sdkVersion, SDK_VERSION);
        kResultOk
    }

    unsafe fn setHostContext(&self, context: *mut FUnknown) -> tresult {
        // SAFETY: the host passes its context, or null.
        let context = unsafe { ComRef::from_raw(context) }.map(|c| c.to_com_ptr());
        *lock(&self.host) = context;
        kResultOk
    }
}

// What every object checks.

const CREATED: u8 = 0;
const INITIALIZED: u8 = 1;
const TERMINATED: u8 = 2;

thread_local! {
    /// Only its address matters: each thread's is its own.
    static MARK: u8 = const { 0 };
}

fn thread_mark() -> usize {
    MARK.with(|mark| ptr::from_ref(mark) as usize)
}

/// What every object checks the host does: that it's initialized once,
/// before anything else, on the thread that's its UI thread from then on,
/// and terminated once, after which nothing calls it. It counts as alive
/// for as long as it is.
struct Checks {
    name: &'static str,
    ui: AtomicUsize,
    state: AtomicU8,
}

impl Checks {
    fn new(name: &'static str) -> Self {
        ALIVE.fetch_add(1, Ordering::SeqCst);
        Self {
            name,
            ui: AtomicUsize::new(0),
            state: AtomicU8::new(CREATED),
        }
    }

    fn initialize(&self, context: *mut FUnknown) -> tresult {
        if context.is_null() {
            violation(&format!("{} was initialized without a context", self.name));
        }
        let state = &self.state;
        if state
            .compare_exchange(CREATED, INITIALIZED, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            violation(&format!("{} was initialized twice", self.name));
            return kResultFalse;
        }
        self.ui.store(thread_mark(), Ordering::SeqCst);
        kResultOk
    }

    fn terminate(&self) -> tresult {
        self.ui("terminate");
        if self.state.swap(TERMINATED, Ordering::SeqCst) != INITIALIZED {
            violation(&format!("{} was terminated twice", self.name));
        }
        kResultOk
    }

    /// Checks a call that comes when the object is initialized, from any
    /// thread.
    fn alive(&self, what: &str) -> bool {
        match self.state.load(Ordering::SeqCst) {
            INITIALIZED => true,
            CREATED => {
                violation(&format!("{what} came before {} was initialized", self.name));
                false
            }
            _ => {
                violation(&format!("{what} came after {} was terminated", self.name));
                false
            }
        }
    }

    /// Checks a call that comes on the UI thread, with the object
    /// initialized.
    fn ui(&self, what: &str) -> bool {
        if !self.alive(what) {
            return false;
        }
        if thread_mark() != self.ui.load(Ordering::SeqCst) {
            violation(&format!("{what} came on a thread other than the UI thread"));
        }
        true
    }
}

impl Drop for Checks {
    fn drop(&mut self) {
        if self.state.load(Ordering::SeqCst) == INITIALIZED {
            violation(&format!(
                "{} was let go of without being terminated",
                self.name
            ));
        }
        ALIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How the host set the processor up, and whether it's active and
/// processing.
#[derive(Default)]
struct Lifecycle {
    setup: Mutex<Option<ProcessSetup>>,
    active: AtomicBool,
    processing: AtomicBool,
    /// How many times the host turned processing on and off, and how many
    /// blocks of frames it processed.
    counts: [AtomicU32; 3],
}

impl Lifecycle {
    fn setup_processing(&self, checks: &Checks, setup: *mut ProcessSetup) -> tresult {
        checks.ui("setupProcessing");
        if self.active.load(Ordering::SeqCst) {
            violation("setupProcessing came while the plugin was active");
        }
        // SAFETY: the host passes its setup, or null.
        let Some(setup) = (unsafe { setup.as_ref() }) else {
            return kInvalidArgument;
        };
        if setup.sampleRate <= 0.0 || setup.maxSamplesPerBlock <= 0 {
            violation("setupProcessing got a nonsense sample rate or block size");
            return kResultFalse;
        }
        if setup.symbolicSampleSize != kSample32 as int32 {
            violation("setupProcessing asked for a sample size the plugin can't do");
            return kResultFalse;
        }
        *lock(&self.setup) = Some(*setup);
        kResultOk
    }

    fn set_active(&self, checks: &Checks, state: TBool) -> bool {
        checks.ui("setActive");
        if state != 0 {
            if lock(&self.setup).is_none() {
                violation("setActive came before setupProcessing");
            }
            if self.active.swap(true, Ordering::SeqCst) {
                violation("the plugin was activated twice");
            }
        } else {
            if !self.active.swap(false, Ordering::SeqCst) {
                violation("the plugin was deactivated while inactive");
            }
            if self.processing.load(Ordering::SeqCst) {
                violation("the plugin was deactivated while processing");
            }
        }
        state != 0
    }

    fn set_processing(&self, checks: &Checks, state: TBool) -> tresult {
        checks.alive("setProcessing");
        if !self.active.load(Ordering::SeqCst) {
            violation("setProcessing came while the plugin was inactive");
            return kResultFalse;
        }
        self.processing.store(state != 0, Ordering::SeqCst);
        let count = if state != 0 { 0 } else { 1 };
        self.counts[count].fetch_add(1, Ordering::SeqCst);
        kResultOk
    }

    /// Checks a process call, and says the frames to process and the sample
    /// rate.
    fn process(&self, checks: &Checks, data: &ProcessData) -> Option<(usize, f64)> {
        checks.alive("process");
        if !self.active.load(Ordering::SeqCst) {
            violation("process came while the plugin was inactive");
            return None;
        }
        if !self.processing.load(Ordering::SeqCst) {
            violation("process came before setProcessing");
        }
        let setup = (*lock(&self.setup))?;
        if data.numSamples < 0 || data.numSamples > setup.maxSamplesPerBlock {
            violation("process got a block longer than the plugin was set up for");
            return None;
        }
        if data.symbolicSampleSize != setup.symbolicSampleSize {
            violation("process got another sample size than the plugin was set up for");
            return None;
        }
        if data.numSamples > 0 {
            self.counts[2].fetch_add(1, Ordering::SeqCst);
        }
        Some((data.numSamples as usize, setup.sampleRate))
    }

    fn counts(&self) -> [u32; 3] {
        self.counts.each_ref().map(|c| c.load(Ordering::SeqCst))
    }

    fn check_inactive(&self, what: &str) {
        if self.active.load(Ordering::SeqCst) {
            violation(&format!("{what} came while the plugin was active"));
        }
    }
}

// Test Gain's processor.

/// Test Gain's component and processor.
struct Gain {
    checks: Checks,
    lifecycle: Lifecycle,
    /// Gain and Latency, normalized, as the processor has them.
    values: [AtomicU64; 2],
    /// The latency it took as it was last activated, in frames.
    latency: AtomicU32,
    /// Whether the host turned its sidechain bus on. VST3's buses start
    /// off.
    sidechain: AtomicBool,
    label: Mutex<String>,
    peer: Mutex<Option<ComPtr<IConnectionPoint>>>,
    transport: Mutex<Option<Vst3Transport>>,
    audio: Mutex<GainAudio>,
}

struct GainAudio {
    /// Whether the sidechain was on as the plugin was activated.
    sidechain: bool,
    delay: [Vec<f32>; 2],
    side_delay: [Vec<f32>; 2],
    position: usize,
    /// The block's parameter changes: see [`read_changes`].
    changes: Vec<(ParamID, i32, f64)>,
}

struct GainComponent(Arc<Gain>);

impl Class for GainComponent {
    type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

impl Gain {
    fn create() -> ComPtr<FUnknown> {
        let gain = Arc::new(Self {
            checks: Checks::new("Test Gain's component"),
            lifecycle: Lifecycle::default(),
            values: [AtomicU64::new(0.5f64.to_bits()), AtomicU64::new(0)],
            latency: AtomicU32::new(0),
            sidechain: AtomicBool::new(false),
            label: Mutex::new(String::new()),
            peer: Mutex::new(None),
            transport: Mutex::new(None),
            audio: Mutex::new(GainAudio {
                sidechain: false,
                delay: Default::default(),
                side_delay: Default::default(),
                position: 0,
                changes: Vec::with_capacity(BLOCK_INPUTS),
            }),
        });
        let wrapper = ComWrapper::new(GainComponent(Arc::clone(&gain)));
        register(
            &wrapper.as_com_ref::<IComponent>(),
            Object::Gain(Arc::downgrade(&gain)),
        );
        wrapper.to_com_ptr::<FUnknown>().expect("it's a COM object")
    }

    fn value(&self, index: usize) -> f64 {
        f64::from_bits(self.values[index].load(Ordering::SeqCst))
    }

    fn set_value(&self, index: usize, value: f64) {
        self.values[index].store(value.to_bits(), Ordering::SeqCst);
    }

    fn state(&self) -> String {
        state_text(
            "gain",
            &[(GAIN, self.value(0)), (LATENCY, self.value(1))],
            &lock(&self.label),
        )
    }

    /// Applies one of the host's changes, as the thread processing does.
    fn apply(&self, id: ParamID, value: f64) {
        match id {
            GAIN => self.set_value(0, value),
            // It takes it as it's activated again: its controller asks the
            // host to restart it.
            LATENCY => self.set_value(1, value),
            _ => violation("the host set a parameter the processor doesn't have"),
        }
    }
}

impl IPluginBaseTrait for GainComponent {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        self.0.checks.initialize(context)
    }

    unsafe fn terminate(&self) -> tresult {
        let gain = &self.0;
        if gain.lifecycle.active.load(Ordering::SeqCst) {
            violation("Test Gain's component was terminated while active");
        }
        if lock(&gain.peer).is_some() {
            violation("Test Gain's component was terminated while connected");
        }
        gain.checks.terminate()
    }
}

impl IComponentTrait for GainComponent {
    unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
        self.0.checks.ui("getControllerClassId");
        if class_id.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the host passes somewhere to write.
        unsafe { class_id.write(GAIN_CONTROLLER_CID) };
        kResultOk
    }

    unsafe fn setIoMode(&self, _: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, direction: BusDirection) -> int32 {
        self.0.checks.ui("getBusCount");
        match (
            media == kAudio as MediaType,
            direction == kInput as BusDirection,
        ) {
            // Its input, and its sidechain.
            (true, true) => 2,
            (true, false) => 1,
            (false, _) => 0,
        }
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: int32,
        bus: *mut BusInfo,
    ) -> tresult {
        self.0.checks.ui("getBusInfo");
        let input = direction == kInput as BusDirection;
        let (name, main) = match (media == kAudio as MediaType, input, index) {
            (true, true, 0) => ("Input", true),
            (true, true, 1) => ("Sidechain", false),
            (true, false, 0) => ("Output", true),
            _ => return kInvalidArgument,
        };
        // SAFETY: the host passes somewhere to write, or null.
        unsafe { write_bus(bus, media, direction, 2, name, main) }
    }

    unsafe fn getRoutingInfo(&self, _: *mut RoutingInfo, _: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: int32,
        state: TBool,
    ) -> tresult {
        self.0.checks.ui("activateBus");
        self.0.lifecycle.check_inactive("activateBus");
        let input = direction == kInput as BusDirection;
        match (media == kAudio as MediaType, input, index) {
            (true, true, 1) => {
                self.0.sidechain.store(state != 0, Ordering::SeqCst);
                kResultOk
            }
            (true, _, 0) => kResultOk,
            _ => kInvalidArgument,
        }
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        let gain = &self.0;
        if gain.lifecycle.set_active(&gain.checks, state) {
            let latency = latency_frames(gain.value(1));
            gain.latency.store(latency as u32, Ordering::SeqCst);
            let mut audio = lock(&gain.audio);
            audio.sidechain = gain.sidechain.load(Ordering::SeqCst);
            audio.delay = [vec![0.0; latency], vec![0.0; latency]];
            audio.side_delay = audio.delay.clone();
            audio.position = 0;
        }
        kResultOk
    }

    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let gain = &self.0;
        gain.checks.ui("setState");
        // SAFETY: the host passes its stream, or null.
        let Some(state) = (unsafe { read_stream(stream) }) else {
            return kResultFalse;
        };
        let Some((values, label)) = parse_state(&state, "gain", &[GAIN, LATENCY]) else {
            return kResultFalse;
        };
        gain.set_value(0, values[0]);
        gain.set_value(1, values[1]);
        *lock(&gain.label) = label;
        kResultOk
    }

    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        self.0.checks.ui("getState");
        // SAFETY: the host passes its stream, or null.
        unsafe { write_stream(stream, self.0.state().as_bytes()) }
    }
}

impl IAudioProcessorTrait for GainComponent {
    unsafe fn setBusArrangements(
        &self,
        inputs: *mut SpeakerArrangement,
        inputs_count: int32,
        outputs: *mut SpeakerArrangement,
        outputs_count: int32,
    ) -> tresult {
        self.0.checks.ui("setBusArrangements");
        self.0.lifecycle.check_inactive("setBusArrangements");
        // Every bus stereo: its input and its sidechain, and its output.
        let stereo = |list: *mut SpeakerArrangement, count: int32, buses: int32| {
            count == buses
                && !list.is_null()
                // SAFETY: the host passes as many as it says.
                && (0..count as usize).all(|bus| unsafe { *list.add(bus) } == kStereo)
        };
        if stereo(inputs, inputs_count, 2) && stereo(outputs, outputs_count, 1) {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(
        &self,
        direction: BusDirection,
        index: int32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        let buses = if direction == kInput as BusDirection {
            2
        } else {
            1
        };
        if !(0..buses).contains(&index) || arrangement.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the host passes somewhere to write.
        unsafe { arrangement.write(kStereo) };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: int32) -> tresult {
        if size == kSample32 as int32 {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        self.0.checks.alive("getLatencySamples");
        self.0.latency.load(Ordering::SeqCst)
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        self.0.lifecycle.setup_processing(&self.0.checks, setup)
    }

    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        self.0.lifecycle.set_processing(&self.0.checks, state)
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let gain = &self.0;
        // SAFETY: the host passes its data, or null.
        let Some(data) = (unsafe { data.as_mut() }) else {
            violation("process got no data");
            return kInvalidArgument;
        };
        let Some((frames, _)) = gain.lifecycle.process(&gain.checks, data) else {
            return kResultFalse;
        };
        let Ok(mut audio) = gain.audio.try_lock() else {
            violation("process came on two threads at once");
            return kResultFalse;
        };
        let audio = &mut *audio;
        // SAFETY: the host passes its context, or null.
        if let Some(context) = unsafe { data.processContext.as_ref() } {
            *lock(&gain.transport) = Some(Vst3Transport::of(context));
        }
        // SAFETY: the host passes its changes, or null.
        unsafe { read_changes(data.inputParameterChanges, frames, &mut audio.changes) };
        let changes = &audio.changes;
        if frames == 0 {
            // Only handing over parameter changes.
            for &(id, _, value) in changes {
                gain.apply(id, value);
            }
            return kResultOk;
        }
        // The host hands it every bus, those it turned off too.
        // SAFETY: the host passes as many buses as it says.
        let (Some(input), Some(output)) = (
            unsafe { stereo_bus(data.inputs, data.numInputs, 0) },
            unsafe { stereo_bus(data.outputs, data.numOutputs, 0) },
        ) else {
            violation("process got buses that aren't the plugin's");
            return kResultFalse;
        };
        if data.numInputs != 2 || data.numOutputs != 1 {
            violation("process got buses that aren't the plugin's");
            return kResultFalse;
        }
        let side = if audio.sidechain {
            // SAFETY: as above.
            unsafe { stereo_bus(data.inputs, data.numInputs, 1) }
        } else {
            None
        };
        let mut next = 0;
        let mut peak = 0.0f32;
        for frame in 0..frames {
            while let Some(&(id, offset, value)) = changes.get(next)
                && offset as usize <= frame
            {
                gain.apply(id, value);
                next += 1;
            }
            let factor = 2.0 * gain.value(0);
            for channel in 0..2 {
                // SAFETY: the buffers are `frames` long.
                let x = unsafe { *input[channel].add(frame) };
                // SAFETY: as above.
                let key = side.map_or(0.0, |side| unsafe { *side[channel].add(frame) });
                let (y, key) = if audio.delay[channel].is_empty() {
                    (x, key)
                } else {
                    let at = audio.position;
                    (
                        std::mem::replace(&mut audio.delay[channel][at], x),
                        std::mem::replace(&mut audio.side_delay[channel][at], key),
                    )
                };
                // The sidechain ducks it.
                let duck = 1.0 - f64::from(key.abs()).min(1.0);
                let y = (f64::from(y) * factor * duck) as f32;
                peak = peak.max(y.abs());
                // SAFETY: as above.
                unsafe { *output[channel].add(frame) = y };
            }
            if !audio.delay[0].is_empty() {
                audio.position = (audio.position + 1) % audio.delay[0].len();
            }
        }
        // SAFETY: the host passes its output buses, as checked.
        unsafe { (*data.outputs).silenceFlags = 0 };
        // SAFETY: the host passes its list for the plugin's changes, or
        // null.
        unsafe { add_change(data.outputParameterChanges, PEAK, f64::from(peak.min(1.0))) };
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        0
    }
}

impl IConnectionPointTrait for GainComponent {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        // SAFETY: the host passes the other side, or null.
        unsafe { connect(&self.0.checks, &self.0.peer, other) }
    }

    unsafe fn disconnect(&self, other: *mut IConnectionPoint) -> tresult {
        disconnect(&self.0.checks, &self.0.peer, other)
    }

    unsafe fn notify(&self, message: *mut IMessage) -> tresult {
        let gain = &self.0;
        gain.checks.ui("notify");
        // SAFETY: the controller passes the message the host made for it.
        match unsafe { read_label_message(message) } {
            Some(label) => {
                *lock(&gain.label) = label;
                kResultOk
            }
            None => kResultFalse,
        }
    }
}

// Test Gain's controller.

/// Test Gain's edit controller.
struct GainController {
    checks: Checks,
    /// Gain, Latency and Peak, normalized, as the controller has them.
    values: [AtomicU64; 3],
    /// The component's label, as it last heard of it.
    label: Mutex<String>,
    /// Its own state: how big its pretend window draws.
    zoom: AtomicU64,
    host: Mutex<Option<ComPtr<IHostApplication>>>,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
    peer: Mutex<Option<ComPtr<IConnectionPoint>>>,
    view: Mutex<Weak<ViewState>>,
}

struct GainEditController(Arc<GainController>);

impl Class for GainEditController {
    type Interfaces = (IEditController, IConnectionPoint, IUnitInfo);
}

/// Test Gain's parameters: id, title, default, steps, flags and unit.
const GAIN_PARAMS: [(ParamID, &str, f64, int32, int32, UnitID); 3] = [
    (GAIN, "Gain", 0.5, 0, kCanAutomate, 1),
    (LATENCY, "Latency", 0.0, 48_000, 0, kRootUnitId),
    (PEAK, "Peak", 0.0, 0, kIsReadOnly, kRootUnitId),
];

impl GainController {
    fn create() -> ComPtr<FUnknown> {
        let controller = Arc::new(Self {
            checks: Checks::new("Test Gain's controller"),
            values: [
                AtomicU64::new(0.5f64.to_bits()),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
            label: Mutex::new(String::new()),
            zoom: AtomicU64::new(1.0f64.to_bits()),
            host: Mutex::new(None),
            handler: Mutex::new(None),
            peer: Mutex::new(None),
            view: Mutex::new(Weak::new()),
        });
        let wrapper = ComWrapper::new(GainEditController(Arc::clone(&controller)));
        register(
            &wrapper.as_com_ref::<IEditController>(),
            Object::Controller(Arc::downgrade(&controller)),
        );
        wrapper.to_com_ptr::<FUnknown>().expect("it's a COM object")
    }

    fn index(id: ParamID) -> Option<usize> {
        GAIN_PARAMS.iter().position(|p| p.0 == id)
    }

    fn value(&self, index: usize) -> f64 {
        f64::from_bits(self.values[index].load(Ordering::SeqCst))
    }

    /// Takes a value, and asks the host to restart the plugin if it's a
    /// new latency.
    fn set_value(&self, index: usize, value: f64) {
        let old = f64::from_bits(self.values[index].swap(value.to_bits(), Ordering::SeqCst));
        if GAIN_PARAMS[index].0 == LATENCY && latency_frames(old) != latency_frames(value) {
            restart(&self.handler, kLatencyChanged);
        }
    }

    /// Edits a parameter as someone would in the window.
    fn edit(&self, action: u32, id: ParamID, value: f64) -> bool {
        self.checks.ui("an edit in the window");
        let Some(handler) = lock(&self.handler).clone() else {
            return false;
        };
        let Some(index) = Self::index(id) else {
            return false;
        };
        // SAFETY: on the UI thread, with the host's handler.
        let result = unsafe {
            match action {
                gui::BEGIN => handler.beginEdit(id),
                gui::VALUE => {
                    self.values[index].store(value.to_bits(), Ordering::SeqCst);
                    let result = handler.performEdit(id, value);
                    if id == LATENCY {
                        handler.restartComponent(kLatencyChanged);
                    }
                    result
                }
                _ => handler.endEdit(id),
            }
        };
        result == kResultOk
    }

    /// Sets the label as someone would in the window: it tells the component
    /// in a message, and the host that the state changed.
    fn set_label(&self, label: &str) -> bool {
        self.checks.ui("a label set in the window");
        let (Some(host), Some(peer)) = (lock(&self.host).clone(), lock(&self.peer).clone()) else {
            return false;
        };
        // SAFETY: on the UI thread, with the host's objects.
        let sent = unsafe { send_label(&host, &peer, label) };
        *lock(&self.label) = label.to_owned();
        // SAFETY: as above.
        unsafe { set_dirty(&self.handler) };
        sent
    }
}

impl IPluginBaseTrait for GainEditController {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        let controller = &self.0;
        // SAFETY: the host passes its context, or null.
        let host = unsafe { ComRef::from_raw(context) }.and_then(|c| c.cast::<IHostApplication>());
        if host.is_none() {
            violation("Test Gain's controller got a context that isn't the host's");
        }
        *lock(&controller.host) = host;
        controller.checks.initialize(context)
    }

    unsafe fn terminate(&self) -> tresult {
        let controller = &self.0;
        if lock(&controller.view).strong_count() > 0 {
            violation("Test Gain's controller was terminated with its window open");
        }
        if lock(&controller.peer).is_some() {
            violation("Test Gain's controller was terminated while connected");
        }
        *lock(&controller.host) = None;
        *lock(&controller.handler) = None;
        controller.checks.terminate()
    }
}

impl IEditControllerTrait for GainEditController {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        let controller = &self.0;
        controller.checks.ui("setComponentState");
        // SAFETY: the host passes the component's state, or null.
        let Some(state) = (unsafe { read_stream(stream) }) else {
            return kResultFalse;
        };
        let Some((values, label)) = parse_state(&state, "gain", &[GAIN, LATENCY]) else {
            violation("setComponentState got a state that isn't the component's");
            return kResultFalse;
        };
        // A new latency needs a restart, as when it's edited.
        for (index, value) in values.into_iter().enumerate() {
            controller.set_value(index, value);
        }
        *lock(&controller.label) = label;
        kResultOk
    }

    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let controller = &self.0;
        controller.checks.ui("setState");
        // SAFETY: the host passes the controller's state, or null.
        let state = unsafe { read_stream(stream) }.unwrap_or_default();
        let zoom = state
            .strip_prefix("zoom=")
            .and_then(|z| z.trim().parse::<f64>().ok());
        let Some(zoom) = zoom else {
            return kResultFalse;
        };
        controller.zoom.store(zoom.to_bits(), Ordering::SeqCst);
        kResultOk
    }

    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        let controller = &self.0;
        controller.checks.ui("getState");
        let zoom = f64::from_bits(controller.zoom.load(Ordering::SeqCst));
        // SAFETY: the host passes its stream, or null.
        unsafe { write_stream(stream, format!("zoom={zoom}").as_bytes()) }
    }

    unsafe fn getParameterCount(&self) -> int32 {
        self.0.checks.ui("getParameterCount");
        GAIN_PARAMS.len() as int32
    }

    unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
        self.0.checks.ui("getParameterInfo");
        let param = usize::try_from(index).ok().and_then(|i| GAIN_PARAMS.get(i));
        // SAFETY: the host passes somewhere to write, or null.
        let (Some(&(id, title, default, steps, flags, unit)), Some(info)) =
            (param, unsafe { info.as_mut() })
        else {
            return kInvalidArgument;
        };
        write_param(info, id, title, default, steps, flags, unit);
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        id: ParamID,
        value: ParamValue,
        string: *mut String128,
    ) -> tresult {
        self.0.checks.ui("getParamStringByValue");
        let text = match id {
            GAIN => format!("{:.2}", 2.0 * value),
            LATENCY => format!("{} samples", latency_frames(value)),
            PEAK => format!("{value:.2}"),
            _ => return kInvalidArgument,
        };
        // SAFETY: the host passes somewhere to write, or null.
        let Some(string) = (unsafe { string.as_mut() }) else {
            return kInvalidArgument;
        };
        write_utf16(string, &text);
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        id: ParamID,
        string: *mut TChar,
        value: *mut ParamValue,
    ) -> tresult {
        self.0.checks.ui("getParamValueByString");
        if string.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the host passes a nul-terminated string.
        let text = unsafe { read_tchars(string) };
        let number = text
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<f64>().ok());
        let parsed = match id {
            GAIN => number.map(|n| n / 2.0),
            LATENCY => number.map(|n| n / 48_000.0),
            _ => None,
        };
        let Some(parsed) = parsed.filter(|v| (0.0..=1.0).contains(v)) else {
            return kResultFalse;
        };
        // SAFETY: checked non-null; the host passes somewhere to write.
        unsafe { value.write(parsed) };
        kResultOk
    }

    unsafe fn normalizedParamToPlain(&self, id: ParamID, value: ParamValue) -> ParamValue {
        match id {
            GAIN => 2.0 * value,
            LATENCY => latency_frames(value) as f64,
            _ => value,
        }
    }

    unsafe fn plainParamToNormalized(&self, id: ParamID, value: ParamValue) -> ParamValue {
        match id {
            GAIN => value / 2.0,
            LATENCY => value / 48_000.0,
            _ => value,
        }
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        let controller = &self.0;
        controller.checks.ui("getParamNormalized");
        match GainController::index(id) {
            Some(index) => controller.value(index),
            None => {
                violation("the host asked for a parameter that doesn't exist");
                0.0
            }
        }
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        let controller = &self.0;
        controller.checks.ui("setParamNormalized");
        if !(0.0..=1.0).contains(&value) {
            violation("the host set a parameter out of its range");
            return kInvalidArgument;
        }
        let Some(index) = GainController::index(id) else {
            violation("the host set a parameter that doesn't exist");
            return kInvalidArgument;
        };
        controller.set_value(index, value);
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        let controller = &self.0;
        controller.checks.ui("setComponentHandler");
        // SAFETY: the host passes its handler, or null.
        *lock(&controller.handler) = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        let controller = &self.0;
        controller.checks.ui("createView");
        // SAFETY: the host passes a nul-terminated name, or null.
        unsafe { create_view(&controller.view, name, 1.0) }
    }
}

impl IConnectionPointTrait for GainEditController {
    unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
        // SAFETY: the host passes the other side, or null.
        unsafe { connect(&self.0.checks, &self.0.peer, other) }
    }

    unsafe fn disconnect(&self, other: *mut IConnectionPoint) -> tresult {
        disconnect(&self.0.checks, &self.0.peer, other)
    }

    /// The component has nothing to tell the controller.
    unsafe fn notify(&self, _: *mut IMessage) -> tresult {
        self.0.checks.ui("notify");
        kResultFalse
    }
}

/// Test Gain's units: Gain is in Levels, the rest at the root.
const UNITS: [(UnitID, UnitID, &str); 2] = [
    (kRootUnitId, kNoParentUnitId, "Root"),
    (1, kRootUnitId, "Levels"),
];

impl IUnitInfoTrait for GainEditController {
    unsafe fn getUnitCount(&self) -> int32 {
        UNITS.len() as int32
    }

    unsafe fn getUnitInfo(&self, index: int32, info: *mut UnitInfo) -> tresult {
        let unit = usize::try_from(index).ok().and_then(|i| UNITS.get(i));
        // SAFETY: the host passes somewhere to write, or null.
        let (Some(&(id, parent, name)), Some(info)) = (unit, unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        info.id = id;
        info.parentUnitId = parent;
        write_utf16(&mut info.name, name);
        info.programListId = -1;
        kResultOk
    }

    unsafe fn getProgramListCount(&self) -> int32 {
        0
    }

    unsafe fn getProgramListInfo(&self, _: int32, _: *mut ProgramListInfo) -> tresult {
        kInvalidArgument
    }

    unsafe fn getProgramName(&self, _: ProgramListID, _: int32, _: *mut String128) -> tresult {
        kInvalidArgument
    }

    unsafe fn getProgramInfo(
        &self,
        _: ProgramListID,
        _: int32,
        _: *const c_char,
        _: *mut String128,
    ) -> tresult {
        kInvalidArgument
    }

    unsafe fn hasProgramPitchNames(&self, _: ProgramListID, _: int32) -> tresult {
        kResultFalse
    }

    unsafe fn getProgramPitchName(
        &self,
        _: ProgramListID,
        _: int32,
        _: int16,
        _: *mut String128,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn getSelectedUnit(&self) -> UnitID {
        kRootUnitId
    }

    unsafe fn selectUnit(&self, _: UnitID) -> tresult {
        kResultOk
    }

    unsafe fn getUnitByBus(
        &self,
        _: MediaType,
        _: BusDirection,
        _: int32,
        _: int32,
        _: *mut UnitID,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn setUnitProgramData(&self, _: int32, _: int32, _: *mut IBStream) -> tresult {
        kNotImplemented
    }
}

// Test Synth.

/// Notes the synth plays at once.
const VOICES: usize = 16;

#[derive(Debug, Clone, Copy, Default)]
struct Voice {
    active: bool,
    note_id: i32,
    pitch: i16,
    velocity: f64,
    /// Semitones.
    tuning: f64,
    pressure: f64,
    brightness: f64,
    /// In cycles.
    phase: f64,
}

/// Test Synth: component, processor and controller in one.
struct Synth {
    checks: Checks,
    lifecycle: Lifecycle,
    /// Level, normalized, as the processor has it, and as the controller
    /// does.
    level: AtomicU64,
    controller_level: AtomicU64,
    label: Mutex<String>,
    handler: Mutex<Option<ComPtr<IComponentHandler>>>,
    transport: Mutex<Option<Vst3Transport>>,
    audio: Mutex<SynthAudio>,
    view: Mutex<Weak<ViewState>>,
}

struct SynthAudio {
    voices: [Voice; VOICES],
    /// The block's parameter changes and events: see [`read_changes`] and
    /// [`read_events`].
    changes: Vec<(ParamID, i32, f64)>,
    events: Vec<Event>,
}

struct SynthComponent(Arc<Synth>);

impl Class for SynthComponent {
    type Interfaces = (IComponent, IAudioProcessor, IEditController, IMidiMapping);
}

impl Synth {
    fn create() -> ComPtr<FUnknown> {
        let synth = Arc::new(Self {
            checks: Checks::new("Test Synth"),
            lifecycle: Lifecycle::default(),
            level: AtomicU64::new(0.5f64.to_bits()),
            controller_level: AtomicU64::new(0.5f64.to_bits()),
            label: Mutex::new(String::new()),
            handler: Mutex::new(None),
            transport: Mutex::new(None),
            audio: Mutex::new(SynthAudio {
                voices: [Voice::default(); VOICES],
                changes: Vec::with_capacity(BLOCK_INPUTS),
                events: Vec::with_capacity(BLOCK_INPUTS),
            }),
            view: Mutex::new(Weak::new()),
        });
        let wrapper = ComWrapper::new(SynthComponent(Arc::clone(&synth)));
        for pointer in [
            wrapper
                .as_com_ref::<IComponent>()
                .map(|p| p.as_ptr().cast()),
            wrapper
                .as_com_ref::<IEditController>()
                .map(|p| p.as_ptr().cast()),
        ] {
            register_ptr(pointer, Object::Synth(Arc::downgrade(&synth)));
        }
        wrapper.to_com_ptr::<FUnknown>().expect("it's a COM object")
    }

    fn level(&self) -> f64 {
        f64::from_bits(self.level.load(Ordering::SeqCst))
    }

    fn state(&self) -> String {
        state_text("synth", &[(LEVEL, self.level())], &lock(&self.label))
    }

    fn edit(&self, action: u32, id: ParamID, value: f64) -> bool {
        self.checks.ui("an edit in the window");
        let Some(handler) = lock(&self.handler).clone() else {
            return false;
        };
        if id != LEVEL {
            return false;
        }
        // SAFETY: on the UI thread, with the host's handler.
        let result = unsafe {
            match action {
                gui::BEGIN => handler.beginEdit(id),
                gui::VALUE => {
                    self.controller_level
                        .store(value.to_bits(), Ordering::SeqCst);
                    handler.performEdit(id, value)
                }
                _ => handler.endEdit(id),
            }
        };
        result == kResultOk
    }

    fn set_label(&self, label: &str) -> bool {
        self.checks.ui("a label set in the window");
        *lock(&self.label) = label.to_owned();
        // SAFETY: on the UI thread.
        unsafe { set_dirty(&self.handler) };
        true
    }

    /// Plays one of the host's events.
    fn play(&self, voices: &mut [Voice; VOICES], event: &Event) {
        let matches = |voice: &Voice, note_id: i32, pitch: i16| {
            voice.active && (note_id == -1 && voice.pitch == pitch || voice.note_id == note_id)
        };
        // SAFETY, for each union field read: the event's type says which it
        // is.
        match event.r#type {
            t if t == kNoteOnEvent as uint16 => {
                let on = unsafe { event.__field0.noteOn };
                let voice = voices.iter().position(|v| !v.active).unwrap_or(0);
                voices[voice] = Voice {
                    active: true,
                    note_id: on.noteId,
                    pitch: on.pitch,
                    velocity: f64::from(on.velocity),
                    tuning: f64::from(on.tuning) / 100.0,
                    ..Voice::default()
                };
            }
            t if t == kNoteOffEvent as uint16 => {
                let off = unsafe { event.__field0.noteOff };
                for voice in voices
                    .iter_mut()
                    .filter(|v| matches(v, off.noteId, off.pitch))
                {
                    voice.active = false;
                }
            }
            t if t == kPolyPressureEvent as uint16 => {
                let pressure = unsafe { event.__field0.polyPressure };
                for voice in voices
                    .iter_mut()
                    .filter(|v| matches(v, pressure.noteId, pressure.pitch))
                {
                    voice.pressure = f64::from(pressure.pressure);
                }
            }
            t if t == kNoteExpressionValueEvent as uint16 => {
                let expression = unsafe { event.__field0.noteExpressionValue };
                for voice in voices
                    .iter_mut()
                    .filter(|v| v.active && v.note_id == expression.noteId)
                {
                    match expression.typeId {
                        id if id == kTuningTypeID as NoteExpressionTypeID => {
                            voice.tuning = 240.0 * (expression.value - 0.5);
                        }
                        id if id == kBrightnessTypeID as NoteExpressionTypeID => {
                            voice.brightness = expression.value
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl IPluginBaseTrait for SynthComponent {
    unsafe fn initialize(&self, context: *mut FUnknown) -> tresult {
        self.0.checks.initialize(context)
    }

    unsafe fn terminate(&self) -> tresult {
        let synth = &self.0;
        if synth.lifecycle.active.load(Ordering::SeqCst) {
            violation("Test Synth was terminated while active");
        }
        if lock(&synth.view).strong_count() > 0 {
            violation("Test Synth was terminated with its window open");
        }
        *lock(&synth.handler) = None;
        synth.checks.terminate()
    }
}

impl IComponentTrait for SynthComponent {
    /// It's its own controller.
    unsafe fn getControllerClassId(&self, _: *mut TUID) -> tresult {
        kResultFalse
    }

    unsafe fn setIoMode(&self, _: IoMode) -> tresult {
        kResultOk
    }

    unsafe fn getBusCount(&self, media: MediaType, direction: BusDirection) -> int32 {
        self.0.checks.ui("getBusCount");
        // A note input, and an audio output.
        int32::from((media == kEvent as MediaType) == (direction == kInput as BusDirection))
    }

    unsafe fn getBusInfo(
        &self,
        media: MediaType,
        direction: BusDirection,
        index: int32,
        bus: *mut BusInfo,
    ) -> tresult {
        self.0.checks.ui("getBusInfo");
        let input = direction == kInput as BusDirection;
        if index != 0 || input != (media == kEvent as MediaType) {
            return kInvalidArgument;
        }
        let (channels, name) = if input { (16, "Notes") } else { (2, "Output") };
        // SAFETY: the host passes somewhere to write, or null.
        unsafe { write_bus(bus, media, direction, channels, name, true) }
    }

    unsafe fn getRoutingInfo(&self, _: *mut RoutingInfo, _: *mut RoutingInfo) -> tresult {
        kNotImplemented
    }

    unsafe fn activateBus(&self, _: MediaType, _: BusDirection, index: int32, _: TBool) -> tresult {
        self.0.checks.ui("activateBus");
        self.0.lifecycle.check_inactive("activateBus");
        if index == 0 {
            kResultOk
        } else {
            kInvalidArgument
        }
    }

    unsafe fn setActive(&self, state: TBool) -> tresult {
        let synth = &self.0;
        if synth.lifecycle.set_active(&synth.checks, state) {
            lock(&synth.audio).voices = [Voice::default(); VOICES];
        }
        kResultOk
    }

    unsafe fn setState(&self, stream: *mut IBStream) -> tresult {
        let synth = &self.0;
        synth.checks.ui("setState");
        // SAFETY: the host passes its stream, or null.
        let state = unsafe { read_stream(stream) };
        let Some((values, label)) = state.and_then(|s| parse_state(&s, "synth", &[LEVEL])) else {
            return kResultFalse;
        };
        synth.level.store(values[0].to_bits(), Ordering::SeqCst);
        *lock(&synth.label) = label;
        kResultOk
    }

    unsafe fn getState(&self, stream: *mut IBStream) -> tresult {
        self.0.checks.ui("getState");
        // SAFETY: the host passes its stream, or null.
        unsafe { write_stream(stream, self.0.state().as_bytes()) }
    }
}

impl IAudioProcessorTrait for SynthComponent {
    unsafe fn setBusArrangements(
        &self,
        _: *mut SpeakerArrangement,
        inputs_count: int32,
        outputs: *mut SpeakerArrangement,
        outputs_count: int32,
    ) -> tresult {
        self.0.checks.ui("setBusArrangements");
        self.0.lifecycle.check_inactive("setBusArrangements");
        // SAFETY: the host passes as many as it says.
        let stereo = outputs_count == 1 && !outputs.is_null() && unsafe { *outputs } == kStereo;
        if inputs_count == 0 && stereo {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getBusArrangement(
        &self,
        direction: BusDirection,
        index: int32,
        arrangement: *mut SpeakerArrangement,
    ) -> tresult {
        if direction == kInput as BusDirection || index != 0 || arrangement.is_null() {
            return kInvalidArgument;
        }
        // SAFETY: checked non-null; the host passes somewhere to write.
        unsafe { arrangement.write(kStereo) };
        kResultOk
    }

    unsafe fn canProcessSampleSize(&self, size: int32) -> tresult {
        if size == kSample32 as int32 {
            kResultTrue
        } else {
            kResultFalse
        }
    }

    unsafe fn getLatencySamples(&self) -> uint32 {
        0
    }

    unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
        self.0.lifecycle.setup_processing(&self.0.checks, setup)
    }

    unsafe fn setProcessing(&self, state: TBool) -> tresult {
        self.0.lifecycle.set_processing(&self.0.checks, state)
    }

    unsafe fn process(&self, data: *mut ProcessData) -> tresult {
        let synth = &self.0;
        // SAFETY: the host passes its data, or null.
        let Some(data) = (unsafe { data.as_mut() }) else {
            violation("process got no data");
            return kInvalidArgument;
        };
        let Some((frames, sample_rate)) = synth.lifecycle.process(&synth.checks, data) else {
            return kResultFalse;
        };
        let Ok(mut audio) = synth.audio.try_lock() else {
            violation("process came on two threads at once");
            return kResultFalse;
        };
        let audio = &mut *audio;
        // SAFETY: the host passes its context, or null.
        if let Some(context) = unsafe { data.processContext.as_ref() } {
            *lock(&synth.transport) = Some(Vst3Transport::of(context));
        }
        // SAFETY: the host passes its changes and events, or null.
        unsafe {
            read_changes(data.inputParameterChanges, frames, &mut audio.changes);
            read_events(data.inputEvents, frames, &mut audio.events);
        }
        let (changes, events) = (&audio.changes, &audio.events);
        if frames == 0 {
            for &(_, _, value) in changes {
                synth.level.store(value.to_bits(), Ordering::SeqCst);
            }
            return kResultOk;
        }
        if data.numInputs != 0 {
            violation("process got audio inputs the synth doesn't have");
            return kResultFalse;
        }
        // SAFETY: the host passes as many buses as it says.
        let output = unsafe { stereo_bus(data.outputs, data.numOutputs, 0) };
        let Some(output) = output.filter(|_| data.numOutputs == 1) else {
            violation("process got outputs that aren't the synth's");
            return kResultFalse;
        };
        let (mut next_change, mut next_event) = (0, 0);
        for frame in 0..frames {
            while let Some(&(id, offset, value)) = changes.get(next_change)
                && offset as usize <= frame
            {
                if id == LEVEL {
                    synth.level.store(value.to_bits(), Ordering::SeqCst);
                } else {
                    violation("the host set a parameter the synth doesn't have");
                }
                next_change += 1;
            }
            while let Some(event) = events.get(next_event)
                && event.sampleOffset as usize <= frame
            {
                synth.play(&mut audio.voices, event);
                next_event += 1;
            }
            let level = synth.level();
            let mut sample = 0.0;
            for voice in audio.voices.iter_mut().filter(|v| v.active) {
                let frequency =
                    440.0 * ((f64::from(voice.pitch) + voice.tuning - 69.0) / 12.0).exp2();
                let amplitude = voice.velocity * level * (1.0 + voice.pressure);
                let phase = voice.phase * TAU;
                sample += amplitude * (phase.sin() + 0.5 * voice.brightness * (2.0 * phase).sin());
                voice.phase = (voice.phase + frequency / sample_rate).fract();
            }
            for channel in output {
                // SAFETY: the buffers are `frames` long.
                unsafe { *channel.add(frame) = sample as f32 };
            }
        }
        // SAFETY: the host passes its output buses, as checked.
        unsafe { (*data.outputs).silenceFlags = 0 };
        kResultOk
    }

    unsafe fn getTailSamples(&self) -> uint32 {
        0
    }
}

impl IEditControllerTrait for SynthComponent {
    unsafe fn setComponentState(&self, stream: *mut IBStream) -> tresult {
        let synth = &self.0;
        synth.checks.ui("setComponentState");
        // SAFETY: the host passes the component's state, or null.
        let state = unsafe { read_stream(stream) };
        let Some((values, _)) = state.and_then(|s| parse_state(&s, "synth", &[LEVEL])) else {
            violation("setComponentState got a state that isn't the component's");
            return kResultFalse;
        };
        synth
            .controller_level
            .store(values[0].to_bits(), Ordering::SeqCst);
        kResultOk
    }

    /// The controller has no state of its own.
    unsafe fn setState(&self, _: *mut IBStream) -> tresult {
        kNotImplemented
    }

    unsafe fn getState(&self, _: *mut IBStream) -> tresult {
        kNotImplemented
    }

    unsafe fn getParameterCount(&self) -> int32 {
        self.0.checks.ui("getParameterCount");
        1
    }

    unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
        self.0.checks.ui("getParameterInfo");
        // SAFETY: the host passes somewhere to write, or null.
        let (0, Some(info)) = (index, unsafe { info.as_mut() }) else {
            return kInvalidArgument;
        };
        write_param(info, LEVEL, "Level", 0.5, 0, kCanAutomate, kRootUnitId);
        kResultOk
    }

    unsafe fn getParamStringByValue(
        &self,
        id: ParamID,
        value: ParamValue,
        string: *mut String128,
    ) -> tresult {
        // SAFETY: the host passes somewhere to write, or null.
        let (LEVEL, Some(string)) = (id, unsafe { string.as_mut() }) else {
            return kInvalidArgument;
        };
        write_utf16(string, &format!("{:.0} %", value * 100.0));
        kResultOk
    }

    unsafe fn getParamValueByString(
        &self,
        _: ParamID,
        _: *mut TChar,
        _: *mut ParamValue,
    ) -> tresult {
        kResultFalse
    }

    unsafe fn normalizedParamToPlain(&self, _: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn plainParamToNormalized(&self, _: ParamID, value: ParamValue) -> ParamValue {
        value
    }

    unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
        self.0.checks.ui("getParamNormalized");
        if id != LEVEL {
            violation("the host asked for a parameter that doesn't exist");
        }
        f64::from_bits(self.0.controller_level.load(Ordering::SeqCst))
    }

    unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
        let synth = &self.0;
        synth.checks.ui("setParamNormalized");
        if id != LEVEL || !(0.0..=1.0).contains(&value) {
            violation("the host set a parameter that doesn't exist, or out of its range");
            return kInvalidArgument;
        }
        synth
            .controller_level
            .store(value.to_bits(), Ordering::SeqCst);
        kResultOk
    }

    unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
        let synth = &self.0;
        synth.checks.ui("setComponentHandler");
        // SAFETY: the host passes its handler, or null.
        *lock(&synth.handler) = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
        kResultOk
    }

    unsafe fn createView(&self, name: FIDString) -> *mut IPlugView {
        let synth = &self.0;
        synth.checks.ui("createView");
        // SAFETY: the host passes a nul-terminated name, or null.
        unsafe { create_view(&synth.view, name, 1.0) }
    }
}

impl IMidiMappingTrait for SynthComponent {
    unsafe fn getMidiControllerAssignment(
        &self,
        bus: int32,
        _channel: int16,
        number: CtrlNumber,
        id: *mut ParamID,
    ) -> tresult {
        self.0.checks.ui("getMidiControllerAssignment");
        // SAFETY: the host passes somewhere to write, or null.
        let (0, VOLUME, Some(id)) = (bus, number, unsafe { id.as_mut() }) else {
            return kResultFalse;
        };
        *id = LEVEL;
        kResultTrue
    }
}

/// MIDI's volume controller, which moves the synth's level.
const VOLUME: CtrlNumber = 7;

// The pretend window.

/// What the pretend window holds.
#[derive(Debug, Default)]
struct WindowState {
    attached: bool,
    parent: usize,
    /// 1 for a Win32 window, 2 for a Cocoa view, 3 for an X11 window.
    platform: i32,
    width: i32,
    height: i32,
    scale: f64,
}

struct ViewState {
    window: Mutex<WindowState>,
    frame: Mutex<Option<ComPtr<IPlugFrame>>>,
    /// The view itself, for asking the frame to resize it.
    this: AtomicPtr<IPlugView>,
    /// For counting it as alive, and checking it's let go of once removed.
    _alive: Alive,
}

/// Counts an object as alive.
struct Alive;

impl Alive {
    fn new() -> Self {
        ALIVE.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        ALIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Drop for ViewState {
    fn drop(&mut self) {
        if lock(&self.window).attached {
            violation("the window was let go of while it was in the host's");
        }
    }
}

struct PlugView(Arc<ViewState>);

impl Class for PlugView {
    type Interfaces = (IPlugView, IPlugViewContentScaleSupport);
}

/// The window's size, unscaled.
const WINDOW_SIZE: (i32, i32) = (400, 300);

/// Makes the pretend window, if the host asks for the editor.
///
/// # Safety
///
/// `name` must be nul-terminated, or null.
unsafe fn create_view(
    slot: &Mutex<Weak<ViewState>>,
    name: FIDString,
    scale: f64,
) -> *mut IPlugView {
    // SAFETY: the caller's promise.
    if name.is_null() || unsafe { CStr::from_ptr(name) } != c"editor" {
        return ptr::null_mut();
    }
    if lock(slot).strong_count() > 0 {
        violation("the host made a second window before letting go of the first");
    }
    let state = Arc::new(ViewState {
        window: Mutex::new(WindowState {
            width: WINDOW_SIZE.0,
            height: WINDOW_SIZE.1,
            scale,
            ..WindowState::default()
        }),
        frame: Mutex::new(None),
        this: AtomicPtr::default(),
        _alive: Alive::new(),
    });
    let view = ComWrapper::new(PlugView(Arc::clone(&state)));
    let view = view.to_com_ptr::<IPlugView>().expect("it's a view");
    state.this.store(view.as_ptr(), Ordering::SeqCst);
    *lock(slot) = Arc::downgrade(&state);
    view.into_raw()
}

impl ViewState {
    /// Asks the host to resize the window, as when someone drags its
    /// corner, and says what it answered.
    fn request_resize(&self, width: i32, height: i32) -> bool {
        let Some(frame) = lock(&self.frame).clone() else {
            return false;
        };
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        // SAFETY: on the UI thread, with the host's frame and this view,
        // and no lock held, as the host calls the view back.
        unsafe { frame.resizeView(self.this.load(Ordering::SeqCst), &mut rect) == kResultTrue }
    }
}

impl IPlugViewTrait for PlugView {
    unsafe fn isPlatformTypeSupported(&self, platform: FIDString) -> tresult {
        // SAFETY: the host passes a nul-terminated type, or null.
        if unsafe { platform_of(platform) } == 0 {
            kResultFalse
        } else {
            kResultTrue
        }
    }

    unsafe fn attached(&self, parent: *mut c_void, platform: FIDString) -> tresult {
        // SAFETY: as above.
        let platform = unsafe { platform_of(platform) };
        if parent.is_null() || platform == 0 {
            violation("attached got no window, or one of no platform");
            return kInvalidArgument;
        }
        let mut window = lock(&self.0.window);
        if window.attached {
            violation("the window was put in the host's twice");
        }
        window.attached = true;
        window.parent = parent as usize;
        window.platform = platform;
        kResultOk
    }

    unsafe fn removed(&self) -> tresult {
        let mut window = lock(&self.0.window);
        if !window.attached {
            violation("the window was removed while it wasn't in the host's");
        }
        window.attached = false;
        kResultOk
    }

    unsafe fn onWheel(&self, _: f32) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyDown(&self, _: char16, _: int16, _: int16) -> tresult {
        kResultFalse
    }

    unsafe fn onKeyUp(&self, _: char16, _: int16, _: int16) -> tresult {
        kResultFalse
    }

    unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
        // SAFETY: the host passes somewhere to write, or null.
        let Some(size) = (unsafe { size.as_mut() }) else {
            return kInvalidArgument;
        };
        let window = lock(&self.0.window);
        *size = ViewRect {
            left: 0,
            top: 0,
            right: window.width,
            bottom: window.height,
        };
        kResultOk
    }

    unsafe fn onSize(&self, size: *mut ViewRect) -> tresult {
        // SAFETY: the host passes the new size, or null.
        let Some(size) = (unsafe { size.as_ref() }) else {
            return kInvalidArgument;
        };
        let mut window = lock(&self.0.window);
        window.width = size.right - size.left;
        window.height = size.bottom - size.top;
        kResultOk
    }

    unsafe fn onFocus(&self, _: TBool) -> tresult {
        kResultOk
    }

    unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
        // SAFETY: the host passes its frame, or null.
        *lock(&self.0.frame) = unsafe { ComRef::from_raw(frame) }.map(|f| f.to_com_ptr());
        kResultOk
    }

    unsafe fn canResize(&self) -> tresult {
        kResultTrue
    }

    /// It's at least 100 by 100.
    unsafe fn checkSizeConstraint(&self, rect: *mut ViewRect) -> tresult {
        // SAFETY: the host passes the size it wants, or null.
        let Some(rect) = (unsafe { rect.as_mut() }) else {
            return kInvalidArgument;
        };
        rect.right = rect.right.max(rect.left + 100);
        rect.bottom = rect.bottom.max(rect.top + 100);
        kResultOk
    }
}

impl IPlugViewContentScaleSupportTrait for PlugView {
    /// It draws bigger, and asks the host to make room, as VST3 says.
    unsafe fn setContentScaleFactor(&self, scale: f32) -> tresult {
        let (width, height) = {
            let mut window = lock(&self.0.window);
            window.scale = f64::from(scale);
            window.width = (f64::from(WINDOW_SIZE.0) * window.scale).round() as i32;
            window.height = (f64::from(WINDOW_SIZE.1) * window.scale).round() as i32;
            (window.width, window.height)
        };
        self.0.request_resize(width, height);
        kResultTrue
    }
}

/// Which platform a VST3 platform type is, as [`WindowState::platform`]
/// numbers them, or 0.
///
/// # Safety
///
/// `platform` must be nul-terminated, or null.
unsafe fn platform_of(platform: FIDString) -> i32 {
    if platform.is_null() {
        return 0;
    }
    // SAFETY: the caller's promise, and VST3's types are nul-terminated.
    let (platform, types) = unsafe {
        (
            CStr::from_ptr(platform),
            [
                CStr::from_ptr(kPlatformTypeHWND),
                CStr::from_ptr(kPlatformTypeNSView),
                CStr::from_ptr(kPlatformTypeX11EmbedWindowID),
            ],
        )
    };
    types
        .iter()
        .position(|t| *t == platform)
        .map_or(0, |i| i as i32 + 1)
}

// What the objects share.

/// Locks a mutex, whether or not a panic poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn latency_frames(value: f64) -> usize {
    (value * 48_000.0).round().max(0.0) as usize
}

fn write_ascii(target: &mut [c_char], text: &str) {
    let last = target.len() - 1;
    let mut bytes = text.bytes();
    for slot in &mut target[..last] {
        *slot = bytes.next().unwrap_or(0) as c_char;
    }
    target[last] = 0;
}

fn write_utf16(target: &mut [u16], text: &str) {
    let last = target.len() - 1;
    let mut chars = text.encode_utf16();
    for slot in &mut target[..last] {
        *slot = chars.next().unwrap_or(0);
    }
    target[last] = 0;
}

/// A nul-terminated UTF-16 string.
///
/// # Safety
///
/// `text` must be nul-terminated.
unsafe fn read_tchars(text: *const TChar) -> String {
    let mut chars = Vec::new();
    for i in 0.. {
        // SAFETY: the caller's promise.
        let c = unsafe { *text.add(i) };
        if c == 0 {
            break;
        }
        chars.push(c);
    }
    String::from_utf16_lossy(&chars)
}

fn write_param(
    info: &mut ParameterInfo,
    id: ParamID,
    title: &str,
    default: f64,
    steps: int32,
    flags: int32,
    unit: UnitID,
) {
    // SAFETY: zeroed is a valid info.
    *info = unsafe { std::mem::zeroed() };
    info.id = id;
    write_utf16(&mut info.title, title);
    write_utf16(&mut info.shortTitle, title);
    info.stepCount = steps;
    info.defaultNormalizedValue = default;
    info.unitId = unit;
    info.flags = flags;
}

/// Describes a bus: a main one, which the host is to turn on, or an aux
/// one, which it needn't.
///
/// # Safety
///
/// `bus` must be somewhere to write, or null.
unsafe fn write_bus(
    bus: *mut BusInfo,
    media: MediaType,
    direction: BusDirection,
    channels: int32,
    name: &str,
    main: bool,
) -> tresult {
    // SAFETY: the caller's promise.
    let Some(bus) = (unsafe { bus.as_mut() }) else {
        return kInvalidArgument;
    };
    // SAFETY: zeroed is a valid info.
    *bus = unsafe { std::mem::zeroed() };
    bus.mediaType = media;
    bus.direction = direction;
    bus.channelCount = channels;
    write_utf16(&mut bus.name, name);
    if main {
        bus.busType = kMain as BusType;
        bus.flags = kDefaultActive as uint32;
    } else {
        bus.busType = kAux as BusType;
    }
    kResultOk
}

/// A plugin's state, as text: its kind, its parameters' values and its
/// label.
fn state_text(kind: &str, values: &[(ParamID, f64)], label: &str) -> String {
    let mut state = format!("musicable-test-vst3 1\nkind={kind}\n");
    for (id, value) in values {
        let _ = writeln!(state, "p{id}={value}");
    }
    let _ = writeln!(state, "label={label}");
    state
}

/// The values of parameters `ids`, in order, and the label, in a state of
/// the plugin of `kind`.
fn parse_state(state: &str, kind: &str, ids: &[ParamID]) -> Option<(Vec<f64>, String)> {
    let mut lines = state.lines();
    if lines.next() != Some("musicable-test-vst3 1")
        || lines.next() != Some(&format!("kind={kind}"))
    {
        return None;
    }
    let mut values = vec![None; ids.len()];
    let mut label = String::new();
    for line in lines {
        let (key, value) = line.split_once('=')?;
        if let Some(id) = key.strip_prefix('p') {
            let (id, value) = (id.parse::<ParamID>().ok()?, value.parse::<f64>().ok()?);
            values[ids.iter().position(|&i| i == id)?] = Some(value);
        } else if key == "label" {
            value.clone_into(&mut label);
        }
    }
    Some((values.into_iter().collect::<Option<_>>()?, label))
}

/// Everything in a stream the host passes, from where it is, as text.
///
/// # Safety
///
/// `stream` must be a stream, or null.
unsafe fn read_stream(stream: *mut IBStream) -> Option<String> {
    // SAFETY: the caller's promise.
    let stream = unsafe { ComRef::from_raw(stream) }?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 256];
    loop {
        let mut read = 0;
        // SAFETY: a buffer of the size given, and somewhere to write.
        let result =
            unsafe { stream.read(buffer.as_mut_ptr().cast(), buffer.len() as int32, &mut read) };
        if result != kResultOk || read <= 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read as usize]);
    }
    String::from_utf8(bytes).ok()
}

/// # Safety
///
/// `stream` must be a stream, or null.
unsafe fn write_stream(stream: *mut IBStream, bytes: &[u8]) -> tresult {
    // SAFETY: the caller's promise.
    let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
        return kInvalidArgument;
    };
    let mut written = 0;
    // SAFETY: `bytes` holds as many as it says, and there's somewhere to
    // write the count.
    let result = unsafe {
        stream.write(
            bytes.as_ptr().cast_mut().cast(),
            bytes.len() as int32,
            &mut written,
        )
    };
    if result == kResultOk && written == bytes.len() as int32 {
        kResultOk
    } else {
        kResultFalse
    }
}

/// As many parameter changes as the host gives a plugin in a block, and as
/// many events. The processors read them into buffers this long, made with
/// the plugin, as a plugin mustn't allocate while it processes.
const BLOCK_INPUTS: usize = 4096;

/// Reads the parameter changes the host gives for a block `frames` long
/// into `changes`, as `(id, offset, value)`, by offset, each parameter's in
/// its order.
///
/// # Safety
///
/// `list` must be the host's list, or null.
unsafe fn read_changes(
    list: *mut IParameterChanges,
    frames: usize,
    changes: &mut Vec<(ParamID, i32, f64)>,
) {
    changes.clear();
    // SAFETY: the caller's promise.
    let Some(list) = (unsafe { ComRef::from_raw(list) }) else {
        return;
    };
    // SAFETY: the host's list, and its queues, for the block: every call.
    unsafe {
        let id_of =
            |index| ComRef::from_raw(list.getParameterData(index)).map(|q| q.getParameterId());
        for index in 0..list.getParameterCount() {
            let Some(queue) = ComRef::from_raw(list.getParameterData(index)) else {
                violation("the host's parameter changes are missing a parameter's");
                continue;
            };
            let id = queue.getParameterId();
            if (0..index).any(|other| id_of(other) == Some(id)) {
                violation("the host gave one parameter two lists of changes");
            }
            let mut last = 0;
            for point in 0..queue.getPointCount() {
                let (mut offset, mut value) = (0, 0.0);
                if queue.getPoint(point, &mut offset, &mut value) != kResultOk {
                    violation("a parameter change couldn't be read");
                    continue;
                }
                if offset < last || offset as usize >= frames.max(1) {
                    violation("a parameter change is out of order, or outside the block");
                }
                if !(0.0..=1.0).contains(&value) {
                    violation("a parameter change is out of its range");
                }
                last = offset;
                if changes.len() == changes.capacity() {
                    violation("a block came with more parameter changes than the host gives");
                    return;
                }
                // After those at the same offset, which came first.
                let at = changes.partition_point(|change| change.1 <= offset);
                changes.insert(at, (id, offset, value));
            }
        }
    }
}

/// Reads the events the host gives for a block `frames` long into
/// `events`, checking they're in order.
///
/// # Safety
///
/// `list` must be the host's list, or null.
unsafe fn read_events(list: *mut IEventList, frames: usize, events: &mut Vec<Event>) {
    events.clear();
    // SAFETY: the caller's promise.
    let Some(list) = (unsafe { ComRef::from_raw(list) }) else {
        return;
    };
    // SAFETY: the host's list, for the block: every call.
    unsafe {
        for index in 0..list.getEventCount() {
            let mut event: Event = std::mem::zeroed();
            if list.getEvent(index, &mut event) != kResultOk {
                violation("an event couldn't be read");
                continue;
            }
            let last = events.last().map_or(0, |e: &Event| e.sampleOffset);
            if event.sampleOffset < last || event.sampleOffset as usize >= frames.max(1) {
                violation("an event is out of order, or outside the block");
            }
            if events.len() == events.capacity() {
                violation("a block came with more events than the host gives");
                return;
            }
            events.push(event);
        }
    }
}

/// Bus `index`'s two channels, if there's such a bus and it's stereo.
///
/// # Safety
///
/// `buses` must hold `count` buses, or be null.
unsafe fn stereo_bus(
    buses: *mut vst3::Steinberg::Vst::AudioBusBuffers,
    count: int32,
    index: int32,
) -> Option<[*mut f32; 2]> {
    if !(0..count).contains(&index) || buses.is_null() {
        return None;
    }
    // SAFETY: the caller's promise.
    let bus = unsafe { &*buses.add(index as usize) };
    // SAFETY: a 32-bit bus, as the plugin was set up.
    let channels = unsafe { bus.__field0.channelBuffers32 };
    if bus.numChannels != 2 || channels.is_null() {
        return None;
    }
    // SAFETY: the bus has two channels.
    let pair = unsafe { [*channels, *channels.add(1)] };
    pair.iter().all(|c| !c.is_null()).then_some(pair)
}

/// Adds a change of `id` to `value` at the block's start to the host's
/// list of the plugin's changes.
///
/// # Safety
///
/// `list` must be the host's list, or null.
unsafe fn add_change(list: *mut IParameterChanges, id: ParamID, value: f64) {
    // SAFETY: the caller's promise.
    let Some(list) = (unsafe { ComRef::from_raw(list) }) else {
        return;
    };
    let mut index = 0;
    // SAFETY: the host's list, and its queue.
    unsafe {
        if let Some(queue) = ComRef::from_raw(list.addParameterData(&id, &mut index)) {
            queue.addPoint(0, value, &mut index);
        }
    }
}

/// Connects one side of a plugin to the other.
///
/// # Safety
///
/// `other` must be a connection point, or null.
unsafe fn connect(
    checks: &Checks,
    peer: &Mutex<Option<ComPtr<IConnectionPoint>>>,
    other: *mut IConnectionPoint,
) -> tresult {
    checks.ui("connect");
    // SAFETY: the caller's promise.
    let Some(other) = (unsafe { ComRef::from_raw(other) }) else {
        return kInvalidArgument;
    };
    let mut peer = lock(peer);
    if peer.is_some() {
        violation("the host connected one side of a plugin twice");
    }
    *peer = Some(other.to_com_ptr());
    kResultOk
}

fn disconnect(
    checks: &Checks,
    peer: &Mutex<Option<ComPtr<IConnectionPoint>>>,
    other: *mut IConnectionPoint,
) -> tresult {
    checks.ui("disconnect");
    let mut peer = lock(peer);
    if peer.as_ref().map(ComPtr::as_ptr) != Some(other) {
        violation("the host disconnected a plugin from something it wasn't connected to");
        return kInvalidArgument;
    }
    *peer = None;
    kResultOk
}

/// Asks the host to restart the plugin.
fn restart(handler: &Mutex<Option<ComPtr<IComponentHandler>>>, flags: int32) -> bool {
    let Some(handler) = lock(handler).clone() else {
        return false;
    };
    // SAFETY: on the UI thread, with the host's handler.
    unsafe { handler.restartComponent(flags) == kResultOk }
}

/// Tells the host the plugin's state changed.
///
/// # Safety
///
/// On the UI thread.
unsafe fn set_dirty(handler: &Mutex<Option<ComPtr<IComponentHandler>>>) {
    let handler = lock(handler).clone();
    if let Some(handler) = handler.and_then(|h| h.cast::<IComponentHandler2>()) {
        // SAFETY: the caller's promise, with the host's handler.
        unsafe { handler.setDirty(1) };
    }
}

/// What the label message holds besides its text, which the component
/// checks came through.
const LABEL_VERSION: f64 = 1.5;

/// Sends the component the label, in a message the host makes.
///
/// # Safety
///
/// On the UI thread, with the host's objects.
unsafe fn send_label(
    host: &ComPtr<IHostApplication>,
    peer: &ComPtr<IConnectionPoint>,
    label: &str,
) -> bool {
    let mut cid: TUID = [0; 16];
    let mut iid: TUID = [0; 16];
    for (to, from) in cid.iter_mut().zip(IMessage::IID) {
        *to = from as c_char;
    }
    iid.copy_from_slice(&cid);
    let mut object = ptr::null_mut();
    // SAFETY: the caller's promise, as VST3's SDK asks for messages.
    unsafe {
        if host.createInstance(&mut cid, &mut iid, &mut object) != kResultOk {
            violation("the host couldn't make a message");
            return false;
        }
        let Some(message) = ComPtr::from_raw(object.cast::<IMessage>()) else {
            return false;
        };
        message.setMessageID(c"label".as_ptr());
        let Some(attributes) = ComRef::from_raw(message.getAttributes()) else {
            violation("the host's message has no attributes");
            return false;
        };
        let text: Vec<TChar> = label.encode_utf16().chain([0]).collect();
        attributes.setString(c"text".as_ptr(), text.as_ptr());
        attributes.setBinary(
            c"bytes".as_ptr(),
            label.as_ptr().cast(),
            label.len() as uint32,
        );
        attributes.setInt(c"length".as_ptr(), label.len() as int64);
        attributes.setFloat(c"version".as_ptr(), LABEL_VERSION);
        peer.notify(message.as_ptr()) == kResultOk
    }
}

/// The label in a message from the controller, checking every part of it
/// came through.
///
/// # Safety
///
/// `message` must be a message, or null.
unsafe fn read_label_message(message: *mut IMessage) -> Option<String> {
    // SAFETY: the caller's promise; the message's parts live as long as it
    // does: every call.
    unsafe {
        let message = ComRef::from_raw(message)?;
        let id = message.getMessageID();
        if id.is_null() || CStr::from_ptr(id) != c"label" {
            return None;
        }
        let attributes = ComRef::from_raw(message.getAttributes())?;
        let mut text = [0 as TChar; 256];
        let size = (text.len() * size_of::<TChar>()) as uint32;
        if attributes.getString(c"text".as_ptr(), text.as_mut_ptr(), size) != kResultOk {
            violation("the host's message lost its text");
            return None;
        }
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        let label = String::from_utf16_lossy(&text[..end]);
        let (mut bytes, mut len) = (ptr::null(), 0);
        let mut length = 0;
        let mut version = 0.0;
        let whole = attributes.getBinary(c"bytes".as_ptr(), &mut bytes, &mut len) == kResultOk
            && !bytes.is_null()
            && std::slice::from_raw_parts(bytes.cast::<u8>(), len as usize) == label.as_bytes()
            && attributes.getInt(c"length".as_ptr(), &mut length) == kResultOk
            && length == label.len() as int64
            && attributes.getFloat(c"version".as_ptr(), &mut version) == kResultOk
            && version == LABEL_VERSION;
        if !whole {
            violation("the host's message lost some of what was in it");
        }
        Some(label)
    }
}

// The objects tests drive, by the pointers the host has.

#[derive(Clone)]
enum Object {
    Gain(Weak<Gain>),
    Controller(Weak<GainController>),
    Synth(Weak<Synth>),
}

/// Each object, by the pointers the host has to it: a component's
/// `IComponent`, and a controller's `IEditController`.
static OBJECTS: Mutex<Vec<(usize, Object)>> = Mutex::new(Vec::new());

fn register<I: Interface>(pointer: &Option<ComRef<'_, I>>, object: Object) {
    register_ptr(pointer.map(|p| p.as_ptr().cast()), object);
}

fn register_ptr(pointer: Option<*mut c_void>, object: Object) {
    let Some(pointer) = pointer else {
        return;
    };
    let mut objects = lock(&OBJECTS);
    objects.retain(|(_, o)| match o {
        Object::Gain(w) => w.strong_count() > 0,
        Object::Controller(w) => w.strong_count() > 0,
        Object::Synth(w) => w.strong_count() > 0,
    });
    objects.push((pointer as usize, object));
}

fn find(pointer: *const c_void) -> Option<Object> {
    let objects = lock(&OBJECTS);
    objects
        .iter()
        .rev()
        .find(|(p, _)| *p == pointer as usize)
        .map(|(_, o)| o.clone())
}

// The hooks.

/// Edits a parameter in the plugin's window: [`gui::BEGIN`] starts dragging
/// its knob, [`gui::VALUE`] turns it to `value`, normalized, and
/// [`gui::END`] lets go. The controller tells the host, as a window does.
/// `object` is Test Gain's controller, or Test Synth. Returns whether the
/// host took it. Call it on the UI thread.
///
/// # Safety
///
/// `object` must be a plugin's object, as the host has it, that's alive.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_gui(
    object: *mut c_void,
    action: u32,
    param: u32,
    value: f64,
) -> bool {
    match find(object) {
        Some(Object::Controller(c)) => c.upgrade().is_some_and(|c| c.edit(action, param, value)),
        Some(Object::Synth(s)) => s.upgrade().is_some_and(|s| s.edit(action, param, value)),
        _ => false,
    }
}

/// Sets the plugin's label, as someone would in its window, which tells the
/// host its state changed. For Test Gain, `object` is the controller, which
/// tells the component in a message. Call it on the UI thread.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`], and `label` must be nul-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_set_label(
    object: *mut c_void,
    label: *const c_char,
) -> bool {
    // SAFETY: the caller's promise.
    let label = unsafe { CStr::from_ptr(label) }.to_string_lossy();
    match find(object) {
        Some(Object::Controller(c)) => c.upgrade().is_some_and(|c| c.set_label(&label)),
        Some(Object::Synth(s)) => s.upgrade().is_some_and(|s| s.set_label(&label)),
        _ => false,
    }
}

/// Copies the label the component has, which its state holds, into
/// `buffer`, as [`musicable_test_first_violation`](crate::musicable_test_first_violation)
/// does. `object` is the component.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`], and `buffer` must hold `capacity`
/// bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_label(
    object: *mut c_void,
    buffer: *mut c_char,
    capacity: u32,
) -> bool {
    let label = match find(object) {
        Some(Object::Gain(g)) => g.upgrade().map(|g| lock(&g.label).clone()),
        Some(Object::Synth(s)) => s.upgrade().map(|s| lock(&s.label).clone()),
        _ => None,
    };
    let Some(label) = label else {
        return false;
    };
    // SAFETY: the caller's promise.
    unsafe { copy_str(&label, buffer, capacity as usize) };
    true
}

/// Sets the zoom of Test Gain's window, which is its controller's own
/// state.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_set_zoom(object: *mut c_void, zoom: f64) -> bool {
    let Some(Object::Controller(c)) = find(object) else {
        return false;
    };
    c.upgrade()
        .map(|c| c.zoom.store(zoom.to_bits(), Ordering::SeqCst))
        .is_some()
}

/// The zoom of Test Gain's window, or NaN.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_zoom(object: *mut c_void) -> f64 {
    let Some(Object::Controller(c)) = find(object) else {
        return f64::NAN;
    };
    c.upgrade()
        .map_or(f64::NAN, |c| f64::from_bits(c.zoom.load(Ordering::SeqCst)))
}

/// A parameter's value, normalized, as the plugin's processor has it, or
/// NaN. `object` is the component.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_processor_value(
    object: *mut c_void,
    param: u32,
) -> f64 {
    match (find(object), param) {
        (Some(Object::Gain(g)), GAIN) => g.upgrade().map_or(f64::NAN, |g| g.value(0)),
        (Some(Object::Gain(g)), LATENCY) => g.upgrade().map_or(f64::NAN, |g| g.value(1)),
        (Some(Object::Synth(s)), LEVEL) => s.upgrade().map_or(f64::NAN, |s| s.level()),
        _ => f64::NAN,
    }
}

fn view_of(object: *mut c_void) -> Option<Arc<ViewState>> {
    match find(object)? {
        Object::Controller(c) => lock(&c.upgrade()?.view).upgrade(),
        Object::Synth(s) => lock(&s.upgrade()?.view).upgrade(),
        Object::Gain(_) => None,
    }
}

/// The plugin's pretend window: -1 if there is none, 0 if it's made but
/// not in the host's, and 1 if it is. `parent` gets the window it's in,
/// `platform` which platform's (1 Win32, 2 Cocoa, 3 X11), and `scale`
/// the scale the host gave it. `object` is the controller.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`], and the pointers must be valid for
/// writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_window(
    object: *mut c_void,
    parent: *mut usize,
    platform: *mut i32,
    scale: *mut f64,
) -> i32 {
    let Some(view) = view_of(object) else {
        return -1;
    };
    let window = lock(&view.window);
    // SAFETY: the caller's promise.
    unsafe {
        parent.write(window.parent);
        platform.write(window.platform);
        scale.write(window.scale);
    }
    i32::from(window.attached)
}

/// Asks the host to resize the plugin's window, as a window does when
/// someone drags its corner, and says what the host answered. Call it on
/// the UI thread.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_request_resize(
    object: *mut c_void,
    width: u32,
    height: u32,
) -> bool {
    view_of(object).is_some_and(|view| view.request_resize(width as i32, height as i32))
}

/// The transport as the plugin last saw it, into `transport`. Returns
/// whether it's seen one. `object` is the component.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`], and `transport` must be valid for
/// writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_transport(
    object: *mut c_void,
    transport: *mut Vst3Transport,
) -> bool {
    let seen = match find(object) {
        Some(Object::Gain(g)) => g.upgrade().and_then(|g| *lock(&g.transport)),
        Some(Object::Synth(s)) => s.upgrade().and_then(|s| *lock(&s.transport)),
        _ => None,
    };
    let Some(seen) = seen else {
        return false;
    };
    // SAFETY: the caller's promise.
    unsafe { transport.write(seen) };
    true
}

/// How many times the host turned the plugin's processing on and off, and
/// how many blocks of frames it processed, into `counts`, in that order.
/// Returns whether `object` is a component, Test Gain's or Test Synth.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`], and `counts` must be valid for
/// writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_lifecycle(
    object: *mut c_void,
    counts: *mut [u32; 3],
) -> bool {
    let seen = match find(object) {
        Some(Object::Gain(g)) => g.upgrade().map(|g| g.lifecycle.counts()),
        Some(Object::Synth(s)) => s.upgrade().map(|s| s.lifecycle.counts()),
        _ => None,
    };
    let Some(seen) = seen else {
        return false;
    };
    // SAFETY: the caller's promise.
    unsafe { counts.write(seen) };
    true
}

/// Whether Test Gain's sidechain bus is on, as the host last turned it,
/// into `on`. Returns whether `object` is Test Gain's component.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`], and `on` must be valid for writing.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_sidechain(object: *mut c_void, on: *mut bool) -> bool {
    let Some(Object::Gain(gain)) = find(object) else {
        return false;
    };
    let Some(gain) = gain.upgrade() else {
        return false;
    };
    // SAFETY: the caller's promise.
    unsafe { on.write(gain.sidechain.load(Ordering::SeqCst)) };
    true
}

/// Asks the host to restart the plugin with VST3's `flags`, from the
/// controller, as a plugin does. Call it on the UI thread.
///
/// # Safety
///
/// As for [`musicable_test_vst3_gui`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn musicable_test_vst3_restart(object: *mut c_void, flags: i32) -> bool {
    match find(object) {
        Some(Object::Controller(c)) => c.upgrade().is_some_and(|c| restart(&c.handler, flags)),
        Some(Object::Synth(s)) => s.upgrade().is_some_and(|s| restart(&s.handler, flags)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_read_as_they_are_written() {
        let state = state_text("gain", &[(GAIN, 0.25), (LATENCY, 0.5)], "warm");
        assert_eq!(
            parse_state(&state, "gain", &[GAIN, LATENCY]),
            Some((vec![0.25, 0.5], "warm".to_owned()))
        );
        assert_eq!(parse_state(&state, "synth", &[LEVEL]), None);
        assert_eq!(parse_state("nonsense", "gain", &[GAIN]), None);
    }

    #[test]
    fn the_class_ids_are_written_as_hosts_write_them() {
        let id: String = (0..16)
            .map(|i| {
                let byte = GAIN_CID[if cfg!(windows) {
                    [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15][i]
                } else {
                    i
                }] as u8;
                format!("{byte:02X}")
            })
            .collect();
        assert_eq!(id, VST3_GAIN_ID);
        assert_eq!(&VST3_SYNTH_ID[..30], &VST3_GAIN_ID[..30]);
    }
}
