//! A plugin instance, on the main thread.

use std::ffi::{CStr, CString, c_char, c_ulong, c_void};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::path::Path;
use std::ptr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Instant;

use clap_sys::ext::audio_ports::{
    CLAP_AUDIO_PORT_IS_MAIN, CLAP_EXT_AUDIO_PORTS, clap_audio_port_info, clap_plugin_audio_ports,
};
use clap_sys::ext::audio_ports_activation::{
    CLAP_EXT_AUDIO_PORTS_ACTIVATION, CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT,
    clap_plugin_audio_ports_activation,
};
use clap_sys::ext::gui::{
    CLAP_EXT_GUI, CLAP_WINDOW_API_COCOA, CLAP_WINDOW_API_WIN32, CLAP_WINDOW_API_X11,
    clap_plugin_gui, clap_window, clap_window_handle,
};
use clap_sys::ext::latency::{CLAP_EXT_LATENCY, clap_plugin_latency};
use clap_sys::ext::note_ports::{
    CLAP_EXT_NOTE_PORTS, CLAP_NOTE_DIALECT_CLAP, CLAP_NOTE_DIALECT_MIDI,
    CLAP_NOTE_DIALECT_MIDI_MPE, clap_note_port_info, clap_plugin_note_ports,
};
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, CLAP_PARAM_IS_BYPASS, CLAP_PARAM_IS_HIDDEN,
    CLAP_PARAM_IS_MODULATABLE, CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID, CLAP_PARAM_IS_READONLY,
    CLAP_PARAM_IS_STEPPED, clap_param_info, clap_plugin_params,
};
use clap_sys::ext::state::{CLAP_EXT_STATE, clap_plugin_state};
use clap_sys::ext::thread_pool::CLAP_EXT_THREAD_POOL;
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_plugin_timer_support};
use clap_sys::plugin::clap_plugin;
use clap_sys::stream::{clap_istream, clap_ostream};
use musicable_engine::{EngineConfig, Graph};
use musicable_model::plugin::PluginInput;

use super::events::{self, Event, OutEvent};
use super::host::{NO_RESIZE, Shared};
use super::library::Library;
use super::node::{Audio, EVENT_CAPACITY, NodeSource, NotePort, PortInfo};
use crate::process::ParamChange;
use crate::threads::{ACTIVE, AudioThread, DESTROYED, FAILED, INACTIVE, PROCESSING};
use crate::{Error, ParamInfo, ParentWindow, PluginEvent, PluginInfo, Waker};

/// How many parameter changes can wait for the plugin's next block.
const PARAM_QUEUE: usize = 1024;
/// How many events the plugin can send back between two polls.
const OUT_QUEUE: usize = 4096;

impl ParentWindow {
    fn clap(self) -> clap_window {
        match self {
            Self::Win32(hwnd) => clap_window {
                api: CLAP_WINDOW_API_WIN32.as_ptr(),
                specific: clap_window_handle { win32: hwnd },
            },
            Self::Cocoa(view) => clap_window {
                api: CLAP_WINDOW_API_COCOA.as_ptr(),
                specific: clap_window_handle { cocoa: view },
            },
            Self::X11(window) => clap_window {
                api: CLAP_WINDOW_API_X11.as_ptr(),
                specific: clap_window_handle {
                    x11: window as c_ulong,
                },
            },
        }
    }
}

/// The platform's window API, for floating windows.
fn native_api() -> &'static CStr {
    if cfg!(windows) {
        CLAP_WINDOW_API_WIN32
    } else if cfg!(target_os = "macos") {
        CLAP_WINDOW_API_COCOA
    } else {
        CLAP_WINDOW_API_X11
    }
}

/// The plugin's extensions the host uses, or null where it has none.
#[derive(Clone, Copy)]
struct Extensions {
    params: *const clap_plugin_params,
    state: *const clap_plugin_state,
    latency: *const clap_plugin_latency,
    audio_ports: *const clap_plugin_audio_ports,
    ports_activation: *const clap_plugin_audio_ports_activation,
    note_ports: *const clap_plugin_note_ports,
    gui: *const clap_plugin_gui,
    timer_support: *const clap_plugin_timer_support,
}

/// A CLAP plugin, loaded and initialized.
///
/// It lives on the thread that made it, which CLAP calls the plugin's main
/// thread: everything but processing happens there. It makes the engine's
/// nodes for the plugin ([`node_source`](Self::node_source)), and the
/// plugin processes once the instance is [activated](Self::activate) for
/// the engine's sample rate and block size.
///
/// Call [`poll`](Self::poll) often, and when the [`Waker`] given to
/// [`new`](Self::new) is called: the plugin's requests are carried out
/// there, and what happened to the plugin comes back from it.
pub struct Instance {
    shared: Arc<Shared>,
    library: Arc<Library>,
    info: PluginInfo,
    ext: Extensions,
    params: Vec<ParamInfo>,
    side_inputs: Vec<PluginInput>,
    /// Whether it has a note output, as it last described its note ports.
    note_output: bool,
    /// The side inputs wires go into, by id, which are on.
    wired: Vec<u32>,
    /// The engine config it's active for.
    config: Option<EngineConfig>,
    to_audio: rtrb::Producer<ParamChange>,
    from_audio: rtrb::Consumer<OutEvent>,
    /// Parameter changes waiting to go to the plugin, oldest first, one per
    /// parameter.
    pending: Vec<ParamChange>,
    /// Whether the plugin's window is open, and floating.
    window: Option<bool>,
    /// Neither `Send` nor `Sync`: CLAP ties the instance to its thread.
    _main_thread: PhantomData<*const ()>,
}

impl Instance {
    /// Loads plugin `id` from the CLAP library at `path` and initializes
    /// it. `waker` is called, from any thread but an audio thread, when the
    /// plugin needs [`poll`](Self::poll) called.
    pub fn new(path: &Path, id: &str, waker: Option<Waker>) -> Result<Self, Error> {
        let library = Library::load(path)?;
        let info = library
            .plugins()
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| Error::new(format!("{} has no plugin {id}", path.display())))?;
        let c_id = CString::new(id).map_err(|_| Error::new(format!("no plugin {id:?}")))?;
        let (to_audio, from_main) = rtrb::RingBuffer::new(PARAM_QUEUE);
        let (to_main, from_audio) = rtrb::RingBuffer::new(OUT_QUEUE);
        let audio = Audio::new(from_main, to_main);
        let shared = Arc::new_cyclic(|shared: &Weak<Shared>| {
            Shared::new(shared.as_ptr(), &info.name, audio, waker)
        });
        // SAFETY: `shared` keeps the host struct where it is for as long as
        // the plugin lives: the instance destroys the plugin before letting
        // go of it.
        let plugin = unsafe { library.create(&shared.host, &c_id) };
        if plugin.is_null() {
            return Err(Error::new(format!("{} couldn't be created", info.name)));
        }
        shared.plugin.store(plugin.cast_mut(), Ordering::Release);
        // SAFETY: just created, on this thread, the plugin's main thread.
        let initialized = unsafe { (*plugin).init.is_some_and(|init| init(plugin)) };
        if !initialized {
            shared.set_lifecycle(DESTROYED);
            // SAFETY: as above. CLAP says to destroy a plugin that fails to
            // initialize.
            if let Some(destroy) = unsafe { (*plugin).destroy } {
                // SAFETY: as above.
                unsafe { destroy(plugin) };
            }
            return Err(Error::new(format!("{} failed to initialize", info.name)));
        }
        // SAFETY: initialized; extensions are thread-safe to ask for.
        let get = |id: &CStr| -> *const c_void {
            // SAFETY: as above.
            unsafe {
                (*plugin)
                    .get_extension
                    .map_or(ptr::null(), |get| get(plugin, id.as_ptr()))
            }
        };
        let ext = Extensions {
            params: get(CLAP_EXT_PARAMS).cast(),
            state: get(CLAP_EXT_STATE).cast(),
            latency: get(CLAP_EXT_LATENCY).cast(),
            audio_ports: get(CLAP_EXT_AUDIO_PORTS).cast(),
            ports_activation: match get(CLAP_EXT_AUDIO_PORTS_ACTIVATION) {
                ext if ext.is_null() => get(CLAP_EXT_AUDIO_PORTS_ACTIVATION_COMPAT).cast(),
                ext => ext.cast(),
            },
            note_ports: get(CLAP_EXT_NOTE_PORTS).cast(),
            gui: get(CLAP_EXT_GUI).cast(),
            timer_support: get(CLAP_EXT_TIMER_SUPPORT).cast(),
        };
        shared.thread_pool.store(
            get(CLAP_EXT_THREAD_POOL).cast_mut().cast(),
            Ordering::Release,
        );
        shared
            .params
            .store(ext.params.cast_mut(), Ordering::Release);
        let mut instance = Self {
            shared,
            library,
            info,
            ext,
            params: Vec::new(),
            side_inputs: Vec::new(),
            note_output: false,
            wired: Vec::new(),
            config: None,
            to_audio,
            from_audio,
            pending: Vec::new(),
            window: None,
            _main_thread: PhantomData,
        };
        instance.params = instance.read_params();
        instance.side_inputs = side_inputs(&instance.audio_ports(true));
        instance.note_output = instance.read_note_output();
        Ok(instance)
    }

    pub fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn plugin(&self) -> *const clap_plugin {
        self.shared.plugin()
    }

    /// The plugin itself, as CLAP's `clap_plugin`, for code that drives it
    /// directly, such as tests driving the test plugins.
    pub fn raw(&self) -> *const clap_plugin {
        self.plugin()
    }

    /// Makes the nodes that process the plugin in the engine's graph. Every
    /// node it makes processes this instance.
    pub fn node_source(&self) -> NodeSource {
        NodeSource {
            shared: Arc::clone(&self.shared),
            library: Arc::clone(&self.library),
        }
    }

    // Parameters.

    /// The plugin's parameters, as it last described them.
    pub fn params(&self) -> &[ParamInfo] {
        &self.params
    }

    fn read_params(&self) -> Vec<ParamInfo> {
        let plugin = self.plugin();
        // SAFETY: the extension lives as long as the plugin.
        let Some(params) = (unsafe { self.ext.params.as_ref() }) else {
            return Vec::new();
        };
        let (Some(count), Some(get_info)) = (params.count, params.get_info) else {
            return Vec::new();
        };
        // SAFETY: on the main thread.
        let count = unsafe { count(plugin) };
        (0..count)
            .filter_map(|index| {
                let mut info = MaybeUninit::<clap_param_info>::zeroed();
                // SAFETY: on the main thread, with somewhere to write. A
                // zeroed info is a valid one, so it's read only if written.
                let info = unsafe {
                    get_info(plugin, index, info.as_mut_ptr()).then(|| info.assume_init())
                }?;
                let flag = |flag| info.flags & flag != 0;
                let range = info.max_value - info.min_value;
                Some(ParamInfo {
                    id: info.id,
                    name: fixed_string(&info.name),
                    module: fixed_string(&info.module),
                    min: info.min_value,
                    max: info.max_value,
                    default: info.default_value,
                    steps: if flag(CLAP_PARAM_IS_STEPPED) && range >= 1.0 {
                        range.round().min(f64::from(u32::MAX)) as u32
                    } else {
                        0
                    },
                    hidden: flag(CLAP_PARAM_IS_HIDDEN),
                    read_only: flag(CLAP_PARAM_IS_READONLY),
                    bypass: flag(CLAP_PARAM_IS_BYPASS),
                    automatable: flag(CLAP_PARAM_IS_AUTOMATABLE),
                    modulatable: flag(CLAP_PARAM_IS_MODULATABLE),
                    modulatable_per_note: flag(CLAP_PARAM_IS_MODULATABLE_PER_NOTE_ID),
                })
            })
            .collect()
    }

    /// A parameter's value, as the plugin has it.
    pub fn param_value(&self, id: u32) -> Option<f64> {
        // SAFETY: the extension lives as long as the plugin.
        let get = unsafe { self.ext.params.as_ref() }?.get_value?;
        let mut value = 0.0;
        // SAFETY: on the main thread, with somewhere to write.
        unsafe { get(self.plugin(), id, &mut value) }.then_some(value)
    }

    /// A value of a parameter, as the plugin would show it: `440 Hz`.
    pub fn value_to_text(&self, id: u32, value: f64) -> Option<String> {
        // SAFETY: the extension lives as long as the plugin.
        let to_text = unsafe { self.ext.params.as_ref() }?.value_to_text?;
        let mut text: [c_char; 256] = [0; 256];
        // SAFETY: on the main thread, with a buffer of the size given.
        let ok = unsafe {
            to_text(
                self.plugin(),
                id,
                value,
                text.as_mut_ptr(),
                text.len() as u32,
            )
        };
        ok.then(|| fixed_string(&text))
    }

    /// The value of a parameter someone typed, as the plugin reads it.
    pub fn text_to_value(&self, id: u32, text: &str) -> Option<f64> {
        // SAFETY: the extension lives as long as the plugin.
        let to_value = unsafe { self.ext.params.as_ref() }?.text_to_value?;
        let text = CString::new(text).ok()?;
        let mut value = 0.0;
        // SAFETY: on the main thread, with somewhere to write.
        unsafe { to_value(self.plugin(), id, text.as_ptr(), &mut value) }.then_some(value)
    }

    /// Sets a parameter. A plugin a node processes gets the change as it
    /// next processes. One that no node processes, inactive or not started
    /// yet, as when there's no audio device, gets it at once.
    pub fn set_param(&mut self, id: u32, value: f64) {
        if !value.is_finite() {
            return;
        }
        let change = ParamChange { id, value };
        match self.pending.iter_mut().find(|c| c.id == id) {
            Some(pending) => *pending = change,
            None => self.pending.push(change),
        }
        self.send_pending();
    }

    /// Hands the plugin the changes waiting for it, or as many as it has
    /// room for.
    fn send_pending(&mut self) {
        if self.pending.is_empty() || self.flush() {
            return;
        }
        if matches!(self.shared.lifecycle(), ACTIVE | PROCESSING) {
            let room = self.to_audio.slots().min(self.pending.len());
            for change in self.pending.drain(..room) {
                // There's room: just checked.
                let _ = self.to_audio.push(change);
            }
        }
    }

    /// Hands the plugin the changes waiting for it, and takes the ones it
    /// has, through its `flush`, unless a node processes it: an inactive
    /// plugin on the main thread, and an active one no node has started on
    /// the main thread as its audio thread. Says whether it did, or false
    /// when a node is to.
    fn flush(&mut self) -> bool {
        let unprocessed = |lifecycle| lifecycle == INACTIVE || lifecycle == ACTIVE;
        if !unprocessed(self.shared.lifecycle()) {
            return false;
        }
        // SAFETY: the extension lives as long as the plugin.
        let Some(flush) = (unsafe { self.ext.params.as_ref() }).and_then(|p| p.flush) else {
            // An active plugin without one takes the changes as it
            // processes, and an inactive one can't take them.
            if self.shared.lifecycle() == INACTIVE {
                self.pending.clear();
                return true;
            }
            return false;
        };
        let Ok(_busy) = self.shared.enter() else {
            return false;
        };
        // A node may have started it meanwhile.
        let lifecycle = self.shared.lifecycle();
        if !unprocessed(lifecycle) {
            return false;
        }
        // CLAP lets any thread be the audio thread, one at a time, and
        // `busy` makes this one the only one.
        let _audio_thread = (lifecycle == ACTIVE).then(AudioThread::enter);
        // SAFETY: `busy` is held.
        let audio = unsafe { &mut *self.shared.audio.get() };
        audio.events.clear();
        let room = self.pending.len().min(EVENT_CAPACITY);
        for change in self.pending.drain(..room) {
            audio.events.push(Event::param(0, change.id, change.value));
        }
        let in_events = audio.events.list();
        let mut output = events::Output {
            queue: &mut audio.to_main,
            voices: None,
            notes: None,
        };
        let out_events = events::output_list(&mut output);
        // SAFETY: on the main thread while inactive, or on the audio thread
        // while active and not processing, with lists that stay put for the
        // call.
        unsafe { flush(self.plugin(), &in_events, &out_events) };
        audio.events.clear();
        true
    }

    // Activation.

    /// Whether the plugin is active, so its nodes can process it.
    pub fn is_active(&self) -> bool {
        self.config.is_some()
    }

    /// Whether a node renders the plugin: from the first block it plays
    /// until it stops, as its device is bypassed. A stopped plugin isn't
    /// processing, so the main thread flushes its parameter changes.
    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Acquire)
    }

    /// Activates the plugin for the engine's sample rate and block size, so
    /// its nodes can process it. An active plugin is deactivated first.
    pub fn activate(&mut self, config: EngineConfig) -> Result<(), Error> {
        self.deactivate()?;
        // This is the restart the plugin asked for, if it asked for one,
        // such as when it was given a latency before it was first active,
        // and its ports are read afresh.
        let requests = &self.shared.requests;
        requests.restart.store(false, Ordering::Release);
        requests.ports_rescan.store(false, Ordering::Release);
        let mut inputs = self.audio_ports(true);
        let outputs = self.audio_ports(false);
        self.side_inputs = side_inputs(&inputs);
        self.switch_side_inputs(&mut inputs);
        self.note_output = self.read_note_output();
        let note_port = self.note_port();
        let plugin = self.plugin();
        let busy = self.shared.enter()?;
        // SAFETY: `busy` is held.
        let audio = unsafe { &mut *self.shared.audio.get() };
        audio.prepare(config, &inputs, &outputs, note_port);
        // The plugin says so while it activates, which is when CLAP wants
        // it to. The latency is read afterwards anyway.
        let latency_changed = &self.shared.requests.latency_changed;
        latency_changed.store(false, Ordering::Release);
        // SAFETY: on the main thread, initialized and inactive.
        let activated = unsafe {
            (*plugin).activate.is_some_and(|activate| {
                activate(
                    plugin,
                    f64::from(config.sample_rate),
                    1,
                    config.max_block as u32,
                )
            })
        };
        latency_changed.store(false, Ordering::Release);
        if !activated {
            audio.config = None;
            return Err(Error::new(format!(
                "{} couldn't start at {} Hz",
                self.info.name, config.sample_rate
            )));
        }
        self.shared.set_lifecycle(ACTIVE);
        drop(busy);
        self.config = Some(config);
        let latency = self.read_latency();
        self.shared.set_latency(latency);
        self.send_pending();
        Ok(())
    }

    /// Deactivates the plugin, waiting for a node that's processing it to
    /// finish its block. Its nodes play silence until it's activated again.
    ///
    /// It fails only if a node has been stuck processing the plugin for
    /// seconds, in which case the plugin is left as it is.
    pub fn deactivate(&mut self) -> Result<(), Error> {
        if self.config.is_none() {
            return Ok(());
        }
        let plugin = self.plugin();
        let busy = self.shared.enter()?;
        let lifecycle = self.shared.lifecycle();
        // SAFETY: `busy` is held.
        let audio = unsafe { &mut *self.shared.audio.get() };
        if lifecycle == PROCESSING {
            // CLAP lets any thread be the audio thread, one at a time, and
            // `busy` makes this one the only one.
            let _audio_thread = AudioThread::enter();
            // SAFETY: processing, and on the audio thread.
            if let Some(stop) = unsafe { (*plugin).stop_processing } {
                // SAFETY: as above.
                unsafe { stop(plugin) };
            }
        }
        if matches!(lifecycle, ACTIVE | PROCESSING | FAILED) {
            // SAFETY: on the main thread, active and not processing.
            if let Some(deactivate) = unsafe { (*plugin).deactivate } {
                // SAFETY: as above.
                unsafe { deactivate(plugin) };
            }
        }
        self.shared.set_lifecycle(INACTIVE);
        audio.config = None;
        // Changes that never got to the plugin go before the ones waiting.
        let mut pending = Vec::with_capacity(audio.from_main.slots() + self.pending.len());
        while let Ok(change) = audio.from_main.pop() {
            pending.retain(|c: &ParamChange| c.id != change.id);
            pending.push(change);
        }
        for change in self.pending.drain(..) {
            pending.retain(|c: &ParamChange| c.id != change.id);
            pending.push(change);
        }
        self.pending = pending;
        drop(busy);
        self.config = None;
        self.send_pending();
        Ok(())
    }

    /// The plugin's latency, in frames, as it last said when it activated.
    /// Delay compensation can't handle more than [`Graph::MAX_LATENCY`].
    pub fn latency(&self) -> usize {
        self.shared.latency()
    }

    fn read_latency(&self) -> usize {
        // SAFETY: the extension lives as long as the plugin.
        let Some(get) = (unsafe { self.ext.latency.as_ref() }).and_then(|l| l.get) else {
            return 0;
        };
        // SAFETY: on the main thread, and active.
        let latency = unsafe { get(self.plugin()) };
        (latency as usize).min(Graph::MAX_LATENCY)
    }

    // Side inputs.

    /// The plugin's side inputs, as it last described them: its audio input
    /// ports besides the main one, by their ids.
    pub fn side_inputs(&self) -> &[PluginInput] {
        &self.side_inputs
    }

    /// Switches on the side inputs with ids in `wired`, the ones wires go
    /// into, and off the others. An active plugin restarts if that changes
    /// which are on: CLAP switches ports only while a plugin is inactive.
    /// Says whether it restarted. Ids the plugin hasn't are left be, and
    /// switch on if it comes to have them.
    pub fn set_side_inputs(&mut self, wired: &[u32]) -> Result<bool, Error> {
        let on = |wired: &[u32]| -> Vec<u32> {
            (self.side_inputs.iter())
                .map(|input| input.id)
                .filter(|id| wired.contains(id))
                .collect()
        };
        let changed = on(wired) != on(&self.wired);
        self.wired = wired.to_vec();
        match self.config {
            Some(config) if changed => self.activate(config).map(|()| true),
            _ => Ok(false),
        }
    }

    /// The plugin's audio ports on one side, with the main input and every
    /// output on.
    fn audio_ports(&self, inputs: bool) -> Vec<PortInfo> {
        let plugin = self.plugin();
        // SAFETY: the extension lives as long as the plugin.
        let Some(ports) = (unsafe { self.ext.audio_ports.as_ref() }) else {
            // A plugin without the extension has no ports.
            return Vec::new();
        };
        let (Some(count), Some(get)) = (ports.count, ports.get) else {
            return Vec::new();
        };
        // SAFETY: on the main thread.
        let count = unsafe { count(plugin, inputs) };
        let mut ports: Vec<PortInfo> = (0..count)
            .filter_map(|index| {
                let mut info = MaybeUninit::<clap_audio_port_info>::zeroed();
                // SAFETY: on the main thread, with somewhere to write. A
                // zeroed info is a valid one.
                let info = unsafe {
                    get(plugin, index, inputs, info.as_mut_ptr()).then(|| info.assume_init())
                }?;
                Some(PortInfo {
                    id: info.id,
                    name: fixed_string(&info.name),
                    // More than any real port has: it guards against
                    // nonsense.
                    channels: info.channel_count.min(64),
                    main: info.flags & CLAP_AUDIO_PORT_IS_MAIN != 0,
                    on: !inputs,
                })
            })
            .collect();
        // The port the plugin calls main, or the first.
        let main = ports.iter().position(|p| p.main).unwrap_or(0);
        for (index, port) in ports.iter_mut().enumerate() {
            port.main = index == main;
            port.on |= port.main;
        }
        ports
    }

    /// Switches each side input in `ports`, the plugin's inputs, on if a
    /// wire goes into it and off if not, as audio-ports-activation has the
    /// host do while the plugin is inactive. A plugin without it has every
    /// port on, and gets silence in those nothing is wired to.
    fn switch_side_inputs(&self, ports: &mut [PortInfo]) {
        // SAFETY: the extension lives as long as the plugin.
        let activation = unsafe { self.ext.ports_activation.as_ref() };
        let set_active = activation.and_then(|a| a.set_active);
        for (index, port) in ports.iter_mut().enumerate().filter(|(_, p)| !p.main) {
            let wired = self.wired.contains(&port.id);
            // SAFETY: on the main thread, and inactive, for buffers of 32-bit
            // samples.
            let switched = set_active.is_none_or(|set_active| unsafe {
                set_active(self.plugin(), true, index as u32, wired, 32)
            });
            port.on = wired && switched;
        }
    }

    /// Whether the plugin has a note output, as it last described its note
    /// ports: the notes it plays out as it renders, for the point after its
    /// device to carry, or the notes a MIDI effect plays.
    pub fn note_output(&self) -> bool {
        self.note_output
    }

    /// Whether the plugin says it has a note output.
    fn read_note_output(&self) -> bool {
        // SAFETY: the extension lives as long as the plugin.
        let ports = unsafe { self.ext.note_ports.as_ref() };
        let Some(count) = ports.and_then(|p| p.count) else {
            return false;
        };
        // SAFETY: on the main thread.
        unsafe { count(self.plugin(), false) > 0 }
    }

    /// Where notes go: the first note input, in CLAP's dialect if it takes
    /// it, else as MPE, else as MIDI.
    fn note_port(&self) -> Option<NotePort> {
        let plugin = self.plugin();
        // SAFETY: the extension lives as long as the plugin.
        let ports = unsafe { self.ext.note_ports.as_ref() }?;
        let (count, get) = (ports.count?, ports.get?);
        // SAFETY: on the main thread.
        if unsafe { count(plugin, true) } == 0 {
            return None;
        }
        let mut info = MaybeUninit::<clap_note_port_info>::zeroed();
        // SAFETY: on the main thread, with somewhere to write. A zeroed
        // info is a valid one.
        let info = unsafe { get(plugin, 0, true, info.as_mut_ptr()).then(|| info.assume_init()) }?;
        let supported = info.supported_dialects;
        let dialect = [
            CLAP_NOTE_DIALECT_CLAP,
            CLAP_NOTE_DIALECT_MIDI_MPE,
            CLAP_NOTE_DIALECT_MIDI,
        ]
        .into_iter()
        .find(|&dialect| supported & dialect != 0)?;
        let midi = supported & (CLAP_NOTE_DIALECT_MIDI | CLAP_NOTE_DIALECT_MIDI_MPE) != 0;
        Some(NotePort {
            index: 0,
            dialect,
            midi,
        })
    }

    // State.

    /// Whether the plugin has a state to save, besides its parameters. One
    /// that hasn't is set by its parameters alone.
    pub fn has_state(&self) -> bool {
        // SAFETY: the extension lives as long as the plugin.
        let state = unsafe { self.ext.state.as_ref() };
        state.is_some_and(|s| s.save.is_some() && s.load.is_some())
    }

    /// The plugin's state, as it saves it: everything it needs to come back
    /// as it is, in its own format.
    pub fn save_state(&self) -> Result<Vec<u8>, Error> {
        // SAFETY: the extension lives as long as the plugin.
        let Some(save) = (unsafe { self.ext.state.as_ref() }).and_then(|s| s.save) else {
            return Ok(Vec::new());
        };
        let mut state = Vec::new();
        let stream = clap_ostream {
            ctx: ptr::from_mut(&mut state).cast(),
            write: Some(write_state),
        };
        // SAFETY: on the main thread, with a stream that lives for the call.
        if unsafe { save(self.plugin(), &stream) } {
            Ok(state)
        } else {
            Err(Error::new(format!(
                "{} couldn't save its state",
                self.info.name
            )))
        }
    }

    /// Restores state the plugin saved.
    pub fn load_state(&mut self, state: &[u8]) -> Result<(), Error> {
        // SAFETY: the extension lives as long as the plugin.
        let Some(load) = (unsafe { self.ext.state.as_ref() }).and_then(|s| s.load) else {
            return Err(Error::new(format!(
                "{} has no state to load",
                self.info.name
            )));
        };
        let mut reader = state;
        let stream = clap_istream {
            ctx: ptr::from_mut(&mut reader).cast(),
            read: Some(read_state),
        };
        // SAFETY: on the main thread, with a stream that lives for the call.
        if unsafe { load(self.plugin(), &stream) } {
            Ok(())
        } else {
            Err(Error::new(format!(
                "{} couldn't load its state",
                self.info.name
            )))
        }
    }

    // Requests.

    /// Carries out what the plugin asked for since the last poll, and says
    /// what happened to it.
    pub fn poll(&mut self) -> Vec<PluginEvent> {
        let mut events = Vec::new();
        let plugin = self.plugin();
        let requests = &self.shared.requests;
        if requests.callback.swap(false, Ordering::AcqRel) {
            // SAFETY: on the main thread, as the plugin asked.
            if let Some(callback) = unsafe { (*plugin).on_main_thread } {
                // SAFETY: as above.
                unsafe { callback(plugin) };
            }
        }
        if self.shared.lifecycle() == FAILED {
            // It failed as a node started it. Starting it again would only
            // fail again, every block.
            if let Err(e) = self.deactivate() {
                events.push(PluginEvent::Failed(e.to_string()));
            }
            events.push(PluginEvent::Failed(format!(
                "{} couldn't start processing",
                self.info.name
            )));
        }
        let requests = &self.shared.requests;
        let rescan = requests.ports_rescan.swap(false, Ordering::AcqRel);
        let restart = requests.restart.swap(false, Ordering::AcqRel)
            | requests.latency_changed.swap(false, Ordering::AcqRel)
            | rescan;
        if restart && let Some(config) = self.config {
            let latency = self.latency();
            match self.activate(config) {
                Ok(()) => events.push(PluginEvent::Restarted {
                    latency_changed: self.latency() != latency,
                }),
                Err(e) => events.push(PluginEvent::Failed(e.to_string())),
            }
        } else if rescan {
            // An inactive plugin's ports are read as it's activated, and
            // its side inputs and note output now.
            self.side_inputs = side_inputs(&self.audio_ports(true));
            self.note_output = self.read_note_output();
        }
        let requests = &self.shared.requests;
        if requests.params_rescan.swap(0, Ordering::AcqRel) != 0 {
            self.params = self.read_params();
            events.push(PluginEvent::ParamsChanged);
        }
        let requests = &self.shared.requests;
        if requests.flush.swap(false, Ordering::AcqRel) {
            // A node processing the plugin flushes it as it does.
            self.flush();
        }
        self.send_pending();
        while let Ok(event) = self.from_audio.pop() {
            events.push(match event {
                OutEvent::GestureBegin(param) => PluginEvent::GestureBegin { param },
                OutEvent::Value(param, value) if value.is_finite() => {
                    PluginEvent::ParamChanged { param, value }
                }
                OutEvent::Value(..) => continue,
                OutEvent::GestureEnd(param) => PluginEvent::GestureEnd { param },
            });
        }
        let requests = &self.shared.requests;
        if requests.dirty.swap(false, Ordering::AcqRel) {
            events.push(PluginEvent::StateChanged);
        }
        self.fire_timers();
        let requests = &self.shared.requests;
        if requests.window_closed.swap(false, Ordering::AcqRel) && self.window.is_some() {
            self.close_window();
            events.push(PluginEvent::WindowClosed);
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

    /// When the plugin next needs [`poll`](Self::poll), for a timer it set,
    /// if it set one.
    pub fn next_timer(&self) -> Option<Instant> {
        self.shared
            .timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next()
    }

    fn fire_timers(&mut self) {
        // SAFETY: the extension lives as long as the plugin.
        let Some(on_timer) = (unsafe { self.ext.timer_support.as_ref() }).and_then(|t| t.on_timer)
        else {
            return;
        };
        let due = self
            .shared
            .timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .due(Instant::now());
        for id in due {
            // SAFETY: on the main thread, for a timer the plugin set.
            unsafe { on_timer(self.plugin(), id) };
        }
    }

    // The plugin's window.

    fn gui(&self) -> Option<&clap_plugin_gui> {
        // SAFETY: the extension lives as long as the plugin.
        unsafe { self.ext.gui.as_ref() }
    }

    /// Whether the plugin has a window to show.
    pub fn has_window(&self) -> bool {
        let Some(supported) = self.gui().and_then(|g| g.is_api_supported) else {
            return false;
        };
        let api = native_api();
        // SAFETY: on the main thread.
        unsafe {
            supported(self.plugin(), api.as_ptr(), false)
                || supported(self.plugin(), api.as_ptr(), true)
        }
    }

    /// Whether the plugin's window is open.
    pub fn is_window_open(&self) -> bool {
        self.window.is_some()
    }

    /// Opens the plugin's window inside `parent`, at `scale` times its
    /// natural size, and says how big it is. An open window closes first.
    pub fn open_window(&mut self, parent: ParentWindow, scale: f64) -> Result<(u32, u32), Error> {
        self.close_window();
        let name = self.info.name.clone();
        let fail = move |what: &str| Error::new(format!("{name}'s window {what}"));
        let gui = *self.gui().ok_or_else(|| fail("isn't there"))?;
        let plugin = self.plugin();
        let window = parent.clap();
        // SAFETY: on the main thread: every call below.
        unsafe {
            if !gui
                .is_api_supported
                .is_some_and(|f| f(plugin, window.api, false))
            {
                return Err(fail("can't go inside another"));
            }
            if !gui.create.is_some_and(|f| f(plugin, window.api, false)) {
                return Err(fail("couldn't be made"));
            }
            self.window = Some(false);
            if let Some(set_scale) = gui.set_scale {
                // Some APIs size windows by themselves, and say no.
                set_scale(plugin, scale);
            }
            if !gui.set_parent.is_some_and(|f| f(plugin, &window)) {
                self.close_window();
                return Err(fail("couldn't go inside Musicable's"));
            }
            let size = self.window_size();
            if let Some(show) = gui.show {
                show(plugin);
            }
            size.ok_or_else(|| fail("has no size"))
        }
    }

    /// Opens the plugin's window as a window of its own, titled `title`.
    pub fn open_floating_window(&mut self, title: &str) -> Result<(), Error> {
        self.close_window();
        let name = self.info.name.clone();
        let fail = move |what: &str| Error::new(format!("{name}'s window {what}"));
        let gui = *self.gui().ok_or_else(|| fail("isn't there"))?;
        let plugin = self.plugin();
        let api = native_api().as_ptr();
        // SAFETY: on the main thread: every call below.
        unsafe {
            if !gui.is_api_supported.is_some_and(|f| f(plugin, api, true)) {
                return Err(fail("can't stand alone"));
            }
            if !gui.create.is_some_and(|f| f(plugin, api, true)) {
                return Err(fail("couldn't be made"));
            }
            self.window = Some(true);
            if let (Some(suggest), Ok(title)) = (gui.suggest_title, CString::new(title)) {
                suggest(plugin, title.as_ptr());
            }
            if !gui.show.is_some_and(|f| f(plugin)) {
                self.close_window();
                return Err(fail("couldn't be shown"));
            }
        }
        Ok(())
    }

    /// Closes the plugin's window, if it's open.
    pub fn close_window(&mut self) {
        if self.window.take().is_none() {
            return;
        }
        let Some(gui) = self.gui().copied() else {
            return;
        };
        let plugin = self.plugin();
        // SAFETY: on the main thread, with the window made.
        unsafe {
            if let Some(hide) = gui.hide {
                hide(plugin);
            }
            if let Some(destroy) = gui.destroy {
                destroy(plugin);
            }
        }
    }

    /// The size of the plugin's window, while it's open.
    pub fn window_size(&self) -> Option<(u32, u32)> {
        self.window?;
        let get_size = self.gui()?.get_size?;
        let (mut width, mut height) = (0, 0);
        // SAFETY: on the main thread, with the window made.
        unsafe { get_size(self.plugin(), &mut width, &mut height) }.then_some((width, height))
    }

    /// Asks the plugin's window, embedded in Musicable's, to take a new
    /// size, and says the size it took: the nearest it can, or the one it
    /// had if it can't resize.
    pub fn resize_window(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if self.window != Some(false) {
            return None;
        }
        let gui = *self.gui()?;
        let plugin = self.plugin();
        // SAFETY: on the main thread, with the window made: every call.
        unsafe {
            if gui.can_resize.is_some_and(|f| f(plugin)) {
                let (mut w, mut h) = (width, height);
                if let Some(adjust) = gui.adjust_size {
                    adjust(plugin, &mut w, &mut h);
                }
                if let Some(set_size) = gui.set_size {
                    set_size(plugin, w, h);
                }
            }
        }
        self.window_size()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.close_window();
        if let Err(e) = self.deactivate() {
            // A node is stuck in the plugin, so it can't be destroyed. Its
            // code stays loaded for as long as the process runs.
            eprintln!("{e}: leaving it be");
            std::mem::forget(Arc::clone(&self.library));
            return;
        }
        let plugin = self.plugin();
        let Ok(_busy) = self.shared.enter() else {
            std::mem::forget(Arc::clone(&self.library));
            return;
        };
        self.shared.set_lifecycle(DESTROYED);
        self.shared
            .timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .timers
            .clear();
        // SAFETY: on the main thread, inactive, with no window.
        if let Some(destroy) = unsafe { (*plugin).destroy } {
            // SAFETY: as above.
            unsafe { destroy(plugin) };
        }
        self.shared.plugin.store(ptr::null_mut(), Ordering::Release);
    }
}

/// The side inputs among a plugin's input ports: all but the main one, by
/// their ids, named "Input 2" and so on where the plugin names them
/// nothing.
fn side_inputs(ports: &[PortInfo]) -> Vec<PluginInput> {
    (ports.iter().enumerate())
        .filter(|(_, port)| !port.main)
        .map(|(index, port)| PluginInput {
            id: port.id,
            name: match port.name.trim() {
                "" => format!("Input {}", index + 1),
                name => name.to_owned(),
            },
        })
        .collect()
}

/// A C string in a fixed-size array, as CLAP's info structs hold them.
fn fixed_string(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

unsafe extern "C" fn write_state(
    stream: *const clap_ostream,
    buffer: *const c_void,
    size: u64,
) -> i64 {
    if buffer.is_null() {
        return if size == 0 { 0 } else { -1 };
    }
    let size = size.min(i64::MAX as u64) as usize;
    // SAFETY: the stream is the one `save_state` made, whose `ctx` is the
    // state being written, and the plugin passes `size` bytes.
    unsafe {
        let state = &mut *(*stream).ctx.cast::<Vec<u8>>();
        state.extend_from_slice(std::slice::from_raw_parts(buffer.cast::<u8>(), size));
    }
    size as i64
}

unsafe extern "C" fn read_state(
    stream: *const clap_istream,
    buffer: *mut c_void,
    size: u64,
) -> i64 {
    if buffer.is_null() {
        return -1;
    }
    // SAFETY: the stream is the one `load_state` made, whose `ctx` is the
    // state left to read.
    let state = unsafe { &mut *(*stream).ctx.cast::<&[u8]>() };
    let count = state.len().min(size.min(i64::MAX as u64) as usize);
    // SAFETY: the plugin passes room for `size` bytes.
    unsafe { ptr::copy_nonoverlapping(state.as_ptr(), buffer.cast::<u8>(), count) };
    *state = &state[count..];
    count as i64
}
