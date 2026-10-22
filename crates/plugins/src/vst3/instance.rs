//! A VST3 plugin instance, on the main thread.

use std::cell::Cell;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::{self, ManuallyDrop};
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use musicable_engine::{EngineConfig, Graph};
use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
use vst3::Steinberg::Vst::BusTypes_::kMain;
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{
    kCanAutomate, kIsBypass, kIsHidden, kIsReadOnly,
};
use vst3::Steinberg::Vst::ProcessModes_::kRealtime;
use vst3::Steinberg::Vst::RestartFlags_::{
    kIoChanged, kLatencyChanged, kParamIDMappingChanged, kParamTitlesChanged, kParamValuesChanged,
    kReloadComponent,
};
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::ViewType::kEditor;
use vst3::Steinberg::Vst::{
    BusDirection, BusInfo as Bus, BusType, IAudioProcessor, IAudioProcessorTrait, IComponent,
    IComponentHandler, IComponentTrait, IConnectionPoint, IConnectionPointTrait, IEditController,
    IEditControllerTrait, IUnitInfo, IUnitInfoTrait, MediaType, ParameterInfo, ProcessSetup,
    String128, TChar, UnitInfo, kNoParentUnitId, kRootUnitId,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IPlugFrame, IPlugView, IPlugViewContentScaleSupport,
    IPlugViewContentScaleSupportTrait, IPlugViewTrait, IPluginBaseTrait, TUID, ViewRect, int32,
    kPlatformTypeHWND, kPlatformTypeNSView, kPlatformTypeX11EmbedWindowID, kResultOk, kResultTrue,
};
use vst3::{ComPtr, ComWrapper};

use super::com::{ComponentHandler, HostApplication, PlugFrame, Stream, rect_size};
use super::host::{Edit, NO_RESIZE, Shared};
use super::module::{Module, parse_class_id, utf16};
use super::node::{self, Audio, BusInfo, NodeSource};
use super::preset::{self, Preset};
use crate::process::ParamChange;
use crate::threads::{DESTROYED, INACTIVE, PROCESSING};
use crate::{Error, ParamInfo, ParentWindow, PluginEvent, PluginInfo, Waker};

/// How many parameter changes can wait for the plugin's next block.
const PARAM_QUEUE: usize = 1024;
/// How many values for the controller can wait for the next poll.
const OUT_QUEUE: usize = 4096;
/// The restarts the plugin is deactivated and activated again for. A plugin
/// that asks to be reloaded whole is restarted the same way.
const RESTART: i32 = kLatencyChanged | kIoChanged | kReloadComponent;
/// The restarts after which the plugin's parameters are read again.
const PARAMS_CHANGED: i32 = kParamTitlesChanged | kParamIDMappingChanged;

impl ParentWindow {
    /// The platform type VST3 knows the window by, and the window.
    fn vst3(self) -> (FIDString, *mut c_void) {
        match self {
            Self::Win32(hwnd) => (kPlatformTypeHWND, hwnd),
            Self::Cocoa(view) => (kPlatformTypeNSView, view),
            Self::X11(window) => (
                kPlatformTypeX11EmbedWindowID,
                window as usize as *mut c_void,
            ),
        }
    }
}

/// The plugin's objects. They're let go of in order as the instance drops,
/// unless a node is stuck in the plugin, when they're left be.
struct Parts {
    component: ComPtr<IComponent>,
    processor: ComPtr<IAudioProcessor>,
    controller: Option<ComPtr<IEditController>>,
    /// Whether the controller is an object of its own, rather than the
    /// component.
    separate: bool,
    /// The component's and the controller's connection points, connected to
    /// each other.
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
}

/// The plugin's window, while it's open.
struct Window {
    view: ComPtr<IPlugView>,
    /// What the view is in, let go of after the view.
    _frame: ComWrapper<PlugFrame>,
}

/// A VST3 plugin, loaded and initialized, with its edit controller.
///
/// It lives on the thread that made it, which is the plugin's UI thread, as
/// VST3 calls it: everything but processing happens there. It makes the
/// engine's nodes for the plugin ([`node_source`](Self::node_source)), and
/// the plugin processes once the instance is [activated](Self::activate)
/// for the engine's sample rate and block size.
///
/// Call [`poll`](Self::poll) often, and when the [`Waker`] given to
/// [`new`](Self::new) is called: the plugin's requests are carried out
/// there, and what happened to the plugin comes back from it.
pub struct Instance {
    shared: Arc<Shared>,
    module: Arc<Module>,
    info: PluginInfo,
    parts: ManuallyDrop<Parts>,
    /// What the plugin knows the host by.
    _host: ComWrapper<HostApplication>,
    /// What the controller tells the host through.
    handler: ComWrapper<ComponentHandler>,
    params: Vec<ParamInfo>,
    /// The engine config it's active for.
    config: Option<EngineConfig>,
    to_audio: rtrb::Producer<ParamChange>,
    /// Values for the controller to show.
    from_audio: rtrb::Consumer<ParamChange>,
    /// Parameter changes waiting to go to the processor, oldest first, one
    /// per parameter.
    pending: Vec<ParamChange>,
    /// Whether changes went to the nodes since the instance last made sure
    /// they reached the processor.
    sent: Cell<bool>,
    window: Option<Window>,
    /// Neither `Send` nor `Sync`: VST3 ties the instance to its thread.
    _main_thread: PhantomData<*const ()>,
}

impl Instance {
    /// Loads plugin `id`, its class id as 32 hex digits, from the VST3 module
    /// at `path`, and initializes it and its controller. `waker` is called,
    /// from any thread but an audio thread, when the plugin needs
    /// [`poll`](Self::poll) called.
    pub fn new(path: &Path, id: &str, waker: Option<Waker>) -> Result<Self, Error> {
        let module = Module::load(path)?;
        let info = module
            .plugins()
            .into_iter()
            .find(|p| p.id.eq_ignore_ascii_case(id))
            .ok_or_else(|| Error::new(format!("{} has no plugin {id}", path.display())))?;
        let cid =
            parse_class_id(&info.id).ok_or_else(|| Error::new(format!("no plugin {id:?}")))?;
        let host = ComWrapper::new(HostApplication);
        let context = host
            .as_com_ref::<FUnknown>()
            .map_or(ptr::null_mut(), |c| c.as_ptr());
        let parts = parts(&module, &cid, context, &info.name)?;
        let (to_audio, from_main) = rtrb::RingBuffer::new(PARAM_QUEUE);
        let (to_main, from_audio) = rtrb::RingBuffer::new(OUT_QUEUE);
        let audio = Audio::new(from_main, to_main);
        let shared = Arc::new(Shared::new(&info.name, audio, waker));
        shared.set_processor(parts.processor.as_ptr());
        let handler = ComWrapper::new(ComponentHandler {
            shared: Arc::downgrade(&shared),
        });
        let mut instance = Self {
            shared,
            module,
            info,
            parts: ManuallyDrop::new(parts),
            _host: host,
            handler,
            params: Vec::new(),
            config: None,
            to_audio,
            from_audio,
            pending: Vec::new(),
            sent: Cell::new(false),
            window: None,
            _main_thread: PhantomData,
        };
        instance.connect();
        instance.params = instance.read_params();
        Ok(instance)
    }

    /// Introduces the controller to the host it tells, to the component it
    /// talks to, and to the component's state.
    fn connect(&mut self) {
        let Some(controller) = self.parts.controller.clone() else {
            return;
        };
        let handler = self
            .handler
            .as_com_ref::<IComponentHandler>()
            .map_or(ptr::null_mut(), |h| h.as_ptr());
        // SAFETY: on the main thread, initialized, with a handler that the
        // instance takes back as it drops.
        unsafe { controller.setComponentHandler(handler) };
        let points = (
            self.parts.component.cast::<IConnectionPoint>(),
            controller.cast::<IConnectionPoint>(),
        );
        if self.parts.separate
            && let (Some(component), Some(other)) = points
        {
            // SAFETY: on the main thread, both initialized.
            unsafe {
                component.connect(other.as_ptr());
                other.connect(component.as_ptr());
            }
            self.parts.connection = Some((component, other));
        }
        let stream = Stream::new(&[]);
        // SAFETY: on the main thread, with a stream that lives for the calls.
        unsafe {
            if self.parts.component.getState(Stream::ptr(&stream)) == kResultOk {
                stream.rewind();
                controller.setComponentState(Stream::ptr(&stream));
            }
        }
    }

    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    /// The plugin's component, as VST3's `IComponent`, for code that drives
    /// the plugin directly, such as tests driving the test plugins.
    pub fn raw_component(&self) -> *mut c_void {
        self.parts.component.as_ptr().cast()
    }

    /// The plugin's edit controller, as VST3's `IEditController`, or null if
    /// it has none.
    pub fn raw_controller(&self) -> *mut c_void {
        self.parts
            .controller
            .as_ref()
            .map_or(ptr::null_mut(), |c| c.as_ptr().cast())
    }

    /// Makes the nodes that process the plugin in the engine's graph. Every
    /// node it makes processes this instance.
    pub fn node_source(&self) -> NodeSource {
        NodeSource {
            shared: Arc::clone(&self.shared),
            module: Arc::clone(&self.module),
        }
    }

    // Parameters.

    /// The plugin's parameters, as its controller last described them.
    pub fn params(&self) -> &[ParamInfo] {
        &self.params
    }

    fn read_params(&self) -> Vec<ParamInfo> {
        let Some(controller) = &self.parts.controller else {
            return Vec::new();
        };
        let units = units(controller);
        // SAFETY: on the main thread: here and below.
        let count = unsafe { controller.getParameterCount() };
        (0..count)
            .filter_map(|index| {
                // SAFETY: zeroed is a valid info.
                let mut info: ParameterInfo = unsafe { mem::zeroed() };
                // SAFETY: as above, with somewhere to write.
                if unsafe { controller.getParameterInfo(index, &mut info) } != kResultOk {
                    return None;
                }
                let flag = |flag| info.flags & flag != 0;
                let default = info.defaultNormalizedValue;
                Some(ParamInfo {
                    id: info.id,
                    name: utf16(&info.title),
                    module: unit_path(&units, info.unitId),
                    min: 0.0,
                    max: 1.0,
                    default: if default.is_finite() {
                        default.clamp(0.0, 1.0)
                    } else {
                        0.0
                    },
                    steps: u32::try_from(info.stepCount).unwrap_or(0),
                    hidden: flag(kIsHidden),
                    read_only: flag(kIsReadOnly),
                    bypass: flag(kIsBypass),
                    automatable: flag(kCanAutomate),
                    // VST3 has no modulation.
                    modulatable: false,
                })
            })
            .collect()
    }

    /// A parameter's value, from 0 to 1, as the controller has it.
    pub fn param_value(&self, id: u32) -> Option<f64> {
        let controller = self.parts.controller.as_ref()?;
        if !self.params.iter().any(|p| p.id == id) {
            return None;
        }
        // SAFETY: on the main thread.
        let value = unsafe { controller.getParamNormalized(id) };
        value.is_finite().then_some(value)
    }

    /// A value of a parameter, from 0 to 1, as the plugin would show it:
    /// `440 Hz`.
    pub fn value_to_text(&self, id: u32, value: f64) -> Option<String> {
        let controller = self.parts.controller.as_ref()?;
        let mut text: String128 = [0; 128];
        // SAFETY: on the main thread, with somewhere to write.
        let ok = unsafe { controller.getParamStringByValue(id, value, &mut text) } == kResultOk;
        ok.then(|| utf16(&text))
    }

    /// The value, from 0 to 1, of a parameter someone typed, as the plugin
    /// reads it.
    pub fn text_to_value(&self, id: u32, text: &str) -> Option<f64> {
        let controller = self.parts.controller.as_ref()?;
        let mut text: Vec<TChar> = text.encode_utf16().chain([0]).collect();
        let mut value = 0.0;
        // SAFETY: on the main thread, with a nul-terminated string and
        // somewhere to write.
        let ok = unsafe { controller.getParamValueByString(id, text.as_mut_ptr(), &mut value) }
            == kResultOk;
        (ok && value.is_finite()).then(|| value.clamp(0.0, 1.0))
    }

    /// Sets a parameter, from 0 to 1. The controller has it at once, and the
    /// processor as a node next processes it, or as it's next activated if
    /// it's inactive: VST3 has no other way to hand it over.
    pub fn set_param(&mut self, id: u32, value: f64) {
        if !value.is_finite() {
            return;
        }
        let value = value.clamp(0.0, 1.0);
        if let Some(controller) = &self.parts.controller {
            // SAFETY: on the main thread.
            unsafe { controller.setParamNormalized(id, value) };
        }
        self.queue(ParamChange { id, value });
    }

    /// Queues a change for the processor, in place of any that waits for the
    /// same parameter.
    fn queue(&mut self, change: ParamChange) {
        match self.pending.iter_mut().find(|c| c.id == change.id) {
            Some(pending) => *pending = change,
            None => self.pending.push(change),
        }
        self.send_pending();
    }

    /// Hands the nodes the changes waiting for the processor, or as many as
    /// they have room for. An inactive plugin's wait for it to activate.
    fn send_pending(&mut self) {
        if self.pending.is_empty() || self.shared.lifecycle() != PROCESSING {
            return;
        }
        let room = self.to_audio.slots().min(self.pending.len());
        for change in self.pending.drain(..room) {
            // There's room: just checked.
            let _ = self.to_audio.push(change);
        }
        if room > 0 {
            self.sent.set(true);
        }
    }

    /// Makes sure the changes sent to the nodes reached the processor, so
    /// its state has them. A node hands them over as it next processes, so
    /// it waits a few blocks for that. If no node does, as when there's no
    /// audio device, it hands them over itself.
    fn settle(&self) {
        if !self.sent.replace(false) || self.shared.lifecycle() != PROCESSING {
            return;
        }
        let capacity = self.to_audio.buffer().capacity();
        let block = self.config.map_or(Duration::ZERO, |c| {
            Duration::from_secs_f64(c.max_block as f64 / f64::from(c.sample_rate.max(1)))
        });
        let wait = block * 3 + Duration::from_millis(5);
        let start = Instant::now();
        // A node takes the changes and processes them holding `busy`.
        while self.to_audio.slots() < capacity || self.shared.is_busy() {
            if start.elapsed() > wait {
                self.hand_over();
                return;
            }
            std::thread::yield_now();
        }
    }

    /// Hands the processor the changes waiting for the nodes, on the main
    /// thread.
    fn hand_over(&self) {
        let Ok(_busy) = self.shared.enter() else {
            return;
        };
        if self.shared.lifecycle() != PROCESSING {
            return;
        }
        // SAFETY: `busy` is held.
        let audio = unsafe { &mut *self.shared.audio.get() };
        // SAFETY: processing, holding `busy`.
        unsafe { node::flush(self.parts.processor.as_com_ref(), audio, &mut Vec::new()) };
    }

    // Activation.

    /// Whether the plugin is active, so its nodes can process it.
    pub fn is_active(&self) -> bool {
        self.config.is_some()
    }

    /// Activates the plugin for the engine's sample rate and block size, so
    /// its nodes can process it, and hands it the changes made while it was
    /// inactive. An active plugin is deactivated first.
    pub fn activate(&mut self, config: EngineConfig) -> Result<(), Error> {
        self.deactivate()?;
        let name = &self.info.name;
        let rate = config.sample_rate;
        let parts = &*self.parts;
        // SAFETY: on the main thread, initialized and inactive: every call
        // until it's active.
        if unsafe { parts.processor.canProcessSampleSize(kSample32 as int32) } != kResultOk {
            return Err(Error::new(format!("{name} can't process 32-bit audio")));
        }
        let inputs = self.buses(kInput as BusDirection);
        let outputs = self.buses(kOutput as BusDirection);
        let takes_notes = self.activate_buses(&inputs, &outputs);
        let mut setup = ProcessSetup {
            processMode: kRealtime as int32,
            symbolicSampleSize: kSample32 as int32,
            maxSamplesPerBlock: i32::try_from(config.max_block).unwrap_or(i32::MAX),
            sampleRate: f64::from(rate),
        };
        // SAFETY: as above.
        if unsafe { parts.processor.setupProcessing(&mut setup) } != kResultOk {
            return Err(Error::new(format!(
                "{name} couldn't be set up for {rate} Hz"
            )));
        }
        let busy = self.shared.enter()?;
        // SAFETY: `busy` is held.
        let audio = unsafe { &mut *self.shared.audio.get() };
        audio.prepare(config, &inputs, &outputs, takes_notes);
        // SAFETY: as above.
        if unsafe { parts.component.setActive(1) } != kResultOk {
            audio.config = None;
            return Err(Error::new(format!("{name} couldn't start at {rate} Hz")));
        }
        // It starts processing at once, on the main thread, as most hosts
        // do. Many plugins say they don't implement it, which means they're
        // ready anyway.
        // SAFETY: on the main thread, and active.
        unsafe { parts.processor.setProcessing(1) };
        self.shared.set_lifecycle(PROCESSING);
        // Before it says its latency, which they may change.
        // SAFETY: processing, holding `busy`.
        let handed = unsafe { node::flush(parts.processor.as_com_ref(), audio, &mut self.pending) };
        // A plugin that asked to restart for them, as for a new latency,
        // may only take them as it's activated, so it's activated again.
        let restart = &self.shared.requests.restart;
        if handed && restart.load(Ordering::Acquire) & RESTART != 0 {
            // SAFETY: on the main thread, active and processing.
            let again = unsafe {
                parts.processor.setProcessing(0);
                parts.component.setActive(0);
                parts.component.setActive(1) == kResultOk
            };
            if !again {
                self.shared.set_lifecycle(INACTIVE);
                audio.config = None;
                return Err(Error::new(format!("{name} couldn't start at {rate} Hz")));
            }
            // SAFETY: on the main thread, and active.
            unsafe { parts.processor.setProcessing(1) };
        }
        // SAFETY: on the main thread, and active.
        let latency = unsafe { parts.processor.getLatencySamples() };
        self.shared
            .set_latency((latency as usize).min(Graph::MAX_LATENCY));
        drop(busy);
        // This is the restart the plugin asked for, if it asked for one, as
        // it was set up or before.
        restart.fetch_and(!RESTART, Ordering::AcqRel);
        self.config = Some(config);
        Ok(())
    }

    /// Deactivates the plugin, waiting for a node that's processing it to
    /// finish its block, and hands it the changes waiting for it. Its nodes
    /// play silence until it's activated again.
    ///
    /// It fails only if a node has been stuck processing the plugin for
    /// seconds, in which case the plugin is left as it is.
    pub fn deactivate(&mut self) -> Result<(), Error> {
        if self.config.is_none() {
            return Ok(());
        }
        let busy = self.shared.enter()?;
        // SAFETY: `busy` is held.
        let audio = unsafe { &mut *self.shared.audio.get() };
        let parts = &*self.parts;
        // SAFETY: an active plugin is processing; `busy` is held.
        unsafe { node::flush(parts.processor.as_com_ref(), audio, &mut self.pending) };
        self.sent.set(false);
        // SAFETY: on the main thread, as it started processing, and active.
        unsafe {
            parts.processor.setProcessing(0);
            parts.component.setActive(0);
        }
        self.shared.set_lifecycle(INACTIVE);
        audio.config = None;
        drop(busy);
        self.config = None;
        Ok(())
    }

    /// The plugin's latency, in frames, as it last said when it activated.
    /// Delay compensation can't handle more than [`Graph::MAX_LATENCY`].
    pub fn latency(&self) -> usize {
        self.shared.latency()
    }

    /// The plugin's audio buses on one side.
    fn buses(&self, direction: BusDirection) -> BusInfo {
        let component = &self.parts.component;
        let media = kAudio as MediaType;
        // SAFETY: on the main thread: here and below.
        let count = unsafe { component.getBusCount(media, direction) };
        let mut buses = BusInfo::default();
        // More than any real plugin has: it guards against nonsense.
        for index in 0..count.clamp(0, 64) {
            // SAFETY: zeroed is a valid info.
            let mut bus: Bus = unsafe { mem::zeroed() };
            // SAFETY: as above, with somewhere to write.
            let ok =
                unsafe { component.getBusInfo(media, direction, index, &mut bus) } == kResultOk;
            let channels = if ok { bus.channelCount.clamp(0, 64) } else { 0 };
            buses.channels.push(channels as u32);
            if ok && bus.busType == kMain as BusType && buses.main.is_none() {
                buses.main = Some(index as usize);
            }
        }
        // The first bus is the main one, unless the plugin says otherwise.
        if buses.main.is_none() && !buses.channels.is_empty() {
            buses.main = Some(0);
        }
        buses
    }

    /// Turns on the buses the nodes use: the main audio ones, and the first
    /// event input, for notes, if there is one. Says whether there is.
    fn activate_buses(&self, inputs: &BusInfo, outputs: &BusInfo) -> bool {
        let component = &self.parts.component;
        let (audio, events) = (kAudio as MediaType, kEvent as MediaType);
        let input = kInput as BusDirection;
        // SAFETY: on the main thread, and inactive: every call.
        unsafe {
            for (direction, buses) in [(input, inputs), (kOutput as BusDirection, outputs)] {
                if let Some(main) = buses.main {
                    component.activateBus(audio, direction, main as i32, 1);
                }
            }
            let takes_notes = component.getBusCount(events, input) > 0;
            if takes_notes {
                component.activateBus(events, input, 0, 1);
            }
            takes_notes
        }
    }

    // State.

    /// Whether the plugin has a state to save, besides its parameters:
    /// every VST3 plugin has one.
    pub fn has_state(&self) -> bool {
        true
    }

    /// The plugin's state, as a `.vstpreset` file holds it: its component's
    /// state, and its controller's if the controller is an object of its
    /// own.
    pub fn save_state(&self) -> Result<Vec<u8>, Error> {
        self.settle();
        let parts = &*self.parts;
        let component = Stream::new(&[]);
        // SAFETY: on the main thread, with a stream that lives for the call.
        if unsafe { parts.component.getState(Stream::ptr(&component)) } != kResultOk {
            return Err(Error::new(format!(
                "{} couldn't save its state",
                self.info.name
            )));
        }
        let component = component.take();
        let controller = parts.controller.as_ref().filter(|_| parts.separate);
        let controller = controller.and_then(|controller| {
            let stream = Stream::new(&[]);
            // SAFETY: as above. One without a state of its own says so.
            let saved = unsafe { controller.getState(Stream::ptr(&stream)) } == kResultOk;
            saved.then(|| stream.take())
        });
        Ok(preset::write(&Preset {
            class_id: &self.info.id,
            component: &component,
            controller: controller.as_deref(),
        }))
    }

    /// Restores state the plugin saved, as [`save_state`](Self::save_state)
    /// saves it.
    pub fn load_state(&mut self, state: &[u8]) -> Result<(), Error> {
        let name = &self.info.name;
        let preset = preset::read(state)
            .ok_or_else(|| Error::new(format!("{name}'s state isn't a VST3 preset")))?;
        if !preset.class_id.eq_ignore_ascii_case(&self.info.id) {
            return Err(Error::new(format!("{name}'s state is another plugin's")));
        }
        // Changes on their way to the processor are older than the state.
        self.settle();
        let parts = &*self.parts;
        let component = Stream::new(preset.component);
        // SAFETY: on the main thread, with streams that live for the calls.
        unsafe {
            if parts.component.setState(Stream::ptr(&component)) != kResultOk {
                return Err(Error::new(format!("{name} couldn't load its state")));
            }
            // So are those waiting for it to activate.
            self.pending.clear();
            if let Some(controller) = &parts.controller {
                component.rewind();
                controller.setComponentState(Stream::ptr(&component));
                if parts.separate
                    && let Some(state) = preset.controller
                {
                    controller.setState(Stream::ptr(&Stream::new(state)));
                }
            }
        }
        Ok(())
    }

    // Requests.

    /// Carries out what the plugin asked for since the last poll, and says
    /// what happened to it.
    pub fn poll(&mut self) -> Vec<PluginEvent> {
        let mut events = Vec::new();
        let edits = mem::take(
            &mut *self
                .shared
                .requests
                .edits
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        for edit in edits {
            match edit {
                Edit::Begin(param) => events.push(PluginEvent::GestureBegin { param }),
                // The controller has it: it goes to the processor too.
                Edit::Value(param, value) if value.is_finite() => {
                    let value = value.clamp(0.0, 1.0);
                    self.queue(ParamChange { id: param, value });
                    events.push(PluginEvent::ParamChanged { param, value });
                }
                Edit::Value(..) => {}
                Edit::End(param) => events.push(PluginEvent::GestureEnd { param }),
            }
        }
        // What the controller is to show: the values the processor sets,
        // such as meters', and the graph's automation.
        while let Ok(change) = self.from_audio.pop() {
            if let Some(controller) = &self.parts.controller
                && change.value.is_finite()
            {
                // SAFETY: on the main thread.
                unsafe { controller.setParamNormalized(change.id, change.value) };
            }
        }
        let requests = &self.shared.requests;
        let restart = requests.restart.swap(0, Ordering::AcqRel);
        if restart & RESTART != 0 {
            match self.config {
                Some(config) => {
                    let latency = self.latency();
                    match self.activate(config) {
                        Ok(()) => events.push(PluginEvent::Restarted {
                            latency_changed: self.latency() != latency,
                        }),
                        Err(e) => events.push(PluginEvent::Failed(e.to_string())),
                    }
                }
                // An inactive plugin restarts as it's activated, which needs
                // to know it asked.
                None => {
                    requests
                        .restart
                        .fetch_or(restart & RESTART, Ordering::AcqRel);
                }
            }
        }
        if restart & PARAMS_CHANGED != 0 {
            self.params = self.read_params();
            events.push(PluginEvent::ParamsChanged);
        }
        self.send_pending();
        let dirty = self.shared.requests.dirty.swap(false, Ordering::AcqRel);
        if dirty || restart & kParamValuesChanged != 0 {
            events.push(PluginEvent::StateChanged);
        }
        let size = self
            .shared
            .requests
            .window_resize
            .swap(NO_RESIZE, Ordering::AcqRel);
        if size != NO_RESIZE && self.window.is_some() {
            events.push(PluginEvent::WindowResize {
                width: (size >> 32) as u32,
                height: size as u32,
            });
        }
        events
    }

    /// When the plugin next needs [`poll`](Self::poll) for a timer: never,
    /// as VST3 plugins keep their own.
    pub fn next_timer(&self) -> Option<Instant> {
        None
    }

    // The plugin's window.

    /// Whether the plugin may have a window to show: whether it has a
    /// controller, which makes it. One that hasn't a window says so as it's
    /// opened.
    pub fn has_window(&self) -> bool {
        self.parts.controller.is_some()
    }

    /// Whether the plugin's window is open.
    pub fn is_window_open(&self) -> bool {
        self.window.is_some()
    }

    /// Opens the plugin's window inside `parent`, at `scale` times its
    /// natural size, except on macOS, which scales windows itself, and says
    /// how big it is. An open window closes first.
    pub fn open_window(&mut self, parent: ParentWindow, scale: f64) -> Result<(u32, u32), Error> {
        self.close_window();
        let name = self.info.name.clone();
        let fail = move |what: &str| Error::new(format!("{name}'s window {what}"));
        let controller = self
            .parts
            .controller
            .as_ref()
            .ok_or_else(|| fail("isn't there"))?;
        // SAFETY: on the main thread: every call below. The controller
        // hands over a reference to the view it makes.
        let view = unsafe { ComPtr::from_raw(controller.createView(kEditor)) }
            .ok_or_else(|| fail("isn't there"))?;
        let (platform, window) = parent.vst3();
        // SAFETY: as above.
        if unsafe { view.isPlatformTypeSupported(platform) } != kResultTrue {
            return Err(fail("can't go inside another"));
        }
        let frame = PlugFrame::new(Arc::downgrade(&self.shared));
        let frame_ptr = frame
            .as_com_ref::<IPlugFrame>()
            .map_or(ptr::null_mut(), |f| f.as_ptr());
        // SAFETY: as above, with a frame the view is let go of before.
        unsafe { view.setFrame(frame_ptr) };
        if !matches!(parent, ParentWindow::Cocoa(_))
            && let Some(scaling) = view.cast::<IPlugViewContentScaleSupport>()
        {
            // SAFETY: as above.
            unsafe { scaling.setContentScaleFactor(scale as f32) };
        }
        // SAFETY: as above, with the window the caller passes.
        if unsafe { view.attached(window, platform) } != kResultOk {
            // SAFETY: as above.
            unsafe { view.setFrame(ptr::null_mut()) };
            return Err(fail("couldn't go inside Musicable's"));
        }
        self.window = Some(Window {
            view,
            _frame: frame,
        });
        // It's the size it ended up, whatever it asked for as it opened.
        let requests = &self.shared.requests;
        requests.window_resize.store(NO_RESIZE, Ordering::Release);
        self.window_size().ok_or_else(|| fail("has no size"))
    }

    /// VST3's windows only go inside the host's.
    pub fn open_floating_window(&mut self, _title: &str) -> Result<(), Error> {
        Err(Error::new(format!(
            "{}'s window can only go inside Musicable's",
            self.info.name
        )))
    }

    /// Closes the plugin's window, if it's open.
    pub fn close_window(&mut self) {
        let Some(window) = self.window.take() else {
            return;
        };
        // SAFETY: on the main thread, with the view attached.
        unsafe {
            window.view.setFrame(ptr::null_mut());
            window.view.removed();
        }
    }

    /// The size of the plugin's window, while it's open.
    pub fn window_size(&self) -> Option<(u32, u32)> {
        let window = self.window.as_ref()?;
        // SAFETY: zeroed is a valid rect.
        let mut rect: ViewRect = unsafe { mem::zeroed() };
        // SAFETY: on the main thread, with the view attached.
        (unsafe { window.view.getSize(&mut rect) } == kResultOk).then(|| rect_size(&rect))
    }

    /// Asks the plugin's window, in Musicable's, to take a new size, and
    /// says the size it took: the nearest it can, or the one it had if it
    /// can't resize.
    pub fn resize_window(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let view = &self.window.as_ref()?.view;
        // SAFETY: on the main thread, with the view attached: every call.
        if self.window_size() != Some((width, height)) && unsafe { view.canResize() } == kResultTrue
        {
            let side = |n: u32| i32::try_from(n).unwrap_or(i32::MAX);
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: side(width),
                bottom: side(height),
            };
            // SAFETY: as above.
            unsafe {
                view.checkSizeConstraint(&mut rect);
                view.onSize(&mut rect);
            }
        }
        self.window_size()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.close_window();
        if let Err(e) = self.deactivate() {
            // A node is stuck in the plugin, so it can't be let go of. Its
            // code stays loaded for as long as the process runs.
            eprintln!("{e}: leaving it be");
            mem::forget(Arc::clone(&self.module));
            return;
        }
        let Ok(busy) = self.shared.enter() else {
            mem::forget(Arc::clone(&self.module));
            return;
        };
        self.shared.set_lifecycle(DESTROYED);
        self.shared.set_processor(ptr::null_mut());
        drop(busy);
        // SAFETY: taken once, here, and not used after.
        let parts = unsafe { ManuallyDrop::take(&mut self.parts) };
        // SAFETY: on the main thread, inactive, with no window: every call.
        // The objects are let go of after, before the module can unload.
        unsafe {
            if let Some((component, controller)) = &parts.connection {
                component.disconnect(controller.as_ptr());
                controller.disconnect(component.as_ptr());
            }
            if let Some(controller) = &parts.controller {
                controller.setComponentHandler(ptr::null_mut());
                if parts.separate {
                    controller.terminate();
                }
            }
            parts.component.terminate();
        }
    }
}

/// Makes the plugin of class `cid` and initializes it, with its controller
/// if it has one. It's terminated again if it isn't a plugin the host can
/// use.
fn parts(module: &Module, cid: &TUID, context: *mut FUnknown, name: &str) -> Result<Parts, Error> {
    let fail = |what: &str| Error::new(format!("{name} {what}"));
    let component = module
        .create::<IComponent>(cid)
        .ok_or_else(|| fail("couldn't be created"))?;
    // SAFETY: on the main thread, just made, with a context that lives as
    // long as the instance. One that fails is let go of, not terminated.
    if unsafe { component.initialize(context) } != kResultOk {
        return Err(fail("failed to initialize"));
    }
    let terminate = |what: &str| {
        // SAFETY: on the main thread, initialized, and terminated once.
        unsafe { component.terminate() };
        Err(fail(what))
    };
    let Some(processor) = component.cast::<IAudioProcessor>() else {
        return terminate("isn't an audio processor");
    };
    if let Some(controller) = component.cast::<IEditController>() {
        return Ok(Parts {
            component,
            processor,
            controller: Some(controller),
            separate: false,
            connection: None,
        });
    }
    let mut controller_cid: TUID = [0; 16];
    // SAFETY: initialized, with somewhere to write.
    let named = unsafe { component.getControllerClassId(&mut controller_cid) } == kResultOk;
    // A plugin without one has no parameters to show, nor a window.
    let controller = named
        .then(|| module.create::<IEditController>(&controller_cid))
        .flatten();
    if let Some(controller) = &controller
        // SAFETY: on the main thread, just made, as for the component.
        && unsafe { controller.initialize(context) } != kResultOk
    {
        return terminate("has a controller that failed to initialize");
    }
    Ok(Parts {
        component,
        processor,
        separate: controller.is_some(),
        controller,
        connection: None,
    })
}

/// The units the plugin groups its parameters in: each one's id, its
/// parent's, and its name.
fn units(controller: &ComPtr<IEditController>) -> Vec<(i32, i32, String)> {
    let Some(units) = controller.cast::<IUnitInfo>() else {
        return Vec::new();
    };
    // SAFETY: on the main thread: here and below.
    let count = unsafe { units.getUnitCount() };
    (0..count)
        .filter_map(|index| {
            // SAFETY: zeroed is a valid info.
            let mut info: UnitInfo = unsafe { mem::zeroed() };
            // SAFETY: as above, with somewhere to write.
            let ok = unsafe { units.getUnitInfo(index, &mut info) } == kResultOk;
            ok.then(|| (info.id, info.parentUnitId, utf16(&info.name)))
        })
        .collect()
}

/// Where unit `id` is among the plugin's units, as `Oscillators/Osc 1`:
/// its parents' names, then its own, without the root's.
fn unit_path(units: &[(i32, i32, String)], id: i32) -> String {
    let mut names = Vec::new();
    let mut at = id;
    // Units nest no deeper than there are units, unless they go round in
    // circles.
    for _ in 0..units.len() {
        if at == kRootUnitId || at == kNoParentUnitId {
            break;
        }
        let Some((_, parent, name)) = units.iter().find(|(unit, ..)| *unit == at) else {
            break;
        };
        names.push(name.as_str());
        at = *parent;
    }
    names.reverse();
    names.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_paths_name_their_parents() {
        let units = [
            (kRootUnitId, kNoParentUnitId, "Root".to_owned()),
            (1, kRootUnitId, "Oscillators".to_owned()),
            (2, 1, "Osc 1".to_owned()),
            // A plugin's mistake.
            (3, 4, "Loop".to_owned()),
            (4, 3, "Back".to_owned()),
        ];
        assert_eq!(unit_path(&units, 2), "Oscillators/Osc 1");
        assert_eq!(unit_path(&units, 1), "Oscillators");
        assert_eq!(unit_path(&units, kRootUnitId), "");
        assert_eq!(unit_path(&units, 9), "");
        assert!(unit_path(&units, 3).ends_with("Loop"));
    }
}
