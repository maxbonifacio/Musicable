//! The open project's plugins: an instance of the plugin each plugin device
//! names, kept in step with the project, and the changes made in the
//! plugins, which go back into the project.
//!
//! The project holds each plugin's settings: the state it last saved, and
//! the parameters set after it. [`Plugins::sync`] brings every instance in
//! line with them after each change to the project, undo and redo included,
//! giving a plugin only what changed. What a plugin changes itself, as when
//! someone turns a knob in its window, comes back from [`Plugins::poll`]
//! for the session to make into transactions, so undo covers it. The
//! plugin has those changes already, so they aren't sent back to it.
//!
//! Only what someone does in a plugin's window is a step in the undo
//! history. Plugins change things on their own too: as they start, as they
//! take in a state or parameters they were given, by undo and redo among
//! others, and with their windows closed, as when a MIDI controller turns a
//! knob it learned. Those come back marked as the plugin's own, for the
//! project to follow without a step, so they don't take the place of what
//! redo would make again.
//!
//! The engine sends a plugin the values of its automated parameters as it
//! plays, so those are left to it: the plugin isn't given their values
//! here until their automation stops, and the values it reports for them
//! are the automation's, which don't go into the project. Moving one in the
//! plugin's window overrides its automation, as moving any control does.
//!
//! A plugin's side inputs go into the project as the plugin lists them, so
//! wires can go into them, and those wires switch them on: see
//! [`Plugins::sync`]. So does whether it has a note output, so the point
//! after an instrument or audio effect plugin with one carries the notes
//! it plays out.
//!
//! A plugin that must render live, as the audio device asks for each block,
//! has its device render live, as does one the user set to Render Ahead
//! Never ([`Plugins::set_render_ahead`]): see [`Plugins::devices`].
//!
//! A plugin's own bypass parameter, if it has one, is its device's Bypass:
//! it follows the device's `bypassed`, rather than the plugin's settings,
//! and clicking it in the plugin's window bypasses the device. Musicable
//! still does the bypassing, with the device's switch in the engine, which
//! stops the plugin. So the plugin isn't bypassing itself as the switch
//! fades it out, the parameter only turns on once the plugin's node has
//! stopped. While a lane moves the device's Bypass, the parameter stays
//! off, and the switch does it all.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use musicable_engine::{Devices, EngineConfig, NodeMaker};
use musicable_model::param::ParamAddress;
use musicable_model::plugin::{PluginDevice, PluginInput, PluginKind, PluginSettings, PluginState};
use musicable_model::routing::Port;
use musicable_model::{DeviceId, PluginFormat, Project};
use musicable_plugins::{Instance, PluginEvent, PluginInfo, Waker};

/// How long what a plugin reports, after it starts or is given a state or
/// parameters, counts as it taking that in rather than as someone's change
/// in its window. Plugins take a block or two, and some a tick of a timer
/// after that, to say they changed.
pub const PLUGIN_SETTLING: Duration = Duration::from_secs(1);

/// A change a plugin made itself, for the project to follow.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Change {
    /// Someone started changing the plugin's parameters in its window. Its
    /// changes up to [`GestureEnd`](Self::GestureEnd) are one step in the
    /// undo history.
    GestureBegin,
    /// One of the plugin's parameters changed. `by_hand` says someone
    /// changed it in the plugin's window, which makes it a step in the undo
    /// history; otherwise the plugin changed it on its own.
    Param {
        param: u32,
        value: f64,
        by_hand: bool,
    },
    GestureEnd,
    /// The plugin's own bypass parameter turned on or off: its device is to
    /// be bypassed, or brought back. `by_hand` as for [`Param`](Self::Param).
    Bypass {
        bypassed: bool,
        by_hand: bool,
    },
    /// The plugin's settings as a whole, as it saved them after a change its
    /// parameters don't say, such as someone picking a preset. `by_hand`
    /// as for [`Param`](Self::Param).
    Settings {
        settings: PluginSettings,
        by_hand: bool,
    },
    /// The plugin restarted with another latency, so the graph needs
    /// building again for delay compensation.
    Latency,
    /// The plugin started or stopped saying it must render live, so the
    /// graph needs building again, with its device on the other side.
    Live,
    /// The plugin's side inputs, as it lists them now, for its device to
    /// take.
    Inputs(Vec<PluginInput>),
    /// Whether the plugin has a note output, as it says now, for its device
    /// to take.
    NoteOutput(bool),
    /// The plugin's window asks to be another size, or closed.
    Window(PluginEvent),
    /// Something went wrong with the plugin.
    Failed(String),
}

/// A plugin's own bypass parameter: its id, and its values off and on.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BypassParam {
    id: u32,
    off: f64,
    on: f64,
}

impl BypassParam {
    /// The first of the plugin's parameters that says it's the plugin's
    /// bypass.
    fn of(instance: &Instance) -> Option<Self> {
        let param = instance.params().iter().find(|p| p.bypass)?;
        Some(Self {
            id: param.id,
            off: param.min,
            on: param.max,
        })
    }

    fn is_on(&self, value: f64) -> bool {
        value >= (self.off + self.on) / 2.0
    }
}

/// The instances of the open project's plugins.
pub(crate) struct Plugins {
    /// The plugins installed, as the last scan found them: `None` until the
    /// app says, and no plugin loads before then.
    installed: Option<Vec<PluginInfo>>,
    /// What the user marked plugins as, by format and id, where the scan's
    /// rule gets them wrong ([`set_kind`](Self::set_kind)).
    kinds: BTreeMap<(PluginFormat, String), PluginKind>,
    /// The plugins the user set to Render Ahead Never, by format and id,
    /// which render live wherever they are
    /// ([`set_render_ahead`](Self::set_render_ahead)).
    never_ahead: BTreeSet<(PluginFormat, String)>,
    /// Whether `kinds` or `never_ahead` changed since
    /// [`take_marks_changed`](Self::take_marks_changed) last said.
    marks_changed: bool,
    /// Whether a device's plugin was set to render live or ahead since
    /// [`take_live_changed`](Self::take_live_changed) last said.
    live_changed: bool,
    hosted: BTreeMap<DeviceId, Hosted>,
    /// Plugin devices whose plugin didn't load, and why. They're tried
    /// again when the installed plugins change.
    failed: BTreeMap<DeviceId, String>,
    config: EngineConfig,
    waker: Option<Waker>,
    /// What went wrong since the last [`take_problems`](Self::take_problems).
    problems: Vec<String>,
}

/// A plugin device's instance, and what it has of the device's settings.
struct Hosted {
    instance: Instance,
    nodes: Arc<dyn NodeMaker>,
    /// What the Edit menu calls the plugin.
    name: String,
    /// The settings the plugin has: the device's, as last given to it, with
    /// the changes the plugin made since.
    applied: PluginSettings,
    /// The side inputs the device has from the plugin, as it last listed
    /// them.
    inputs: Vec<PluginInput>,
    /// Whether the device has a note output from the plugin, as it last
    /// said.
    note_output: bool,
    /// Whether the plugin must render live, as it last said.
    must_render_live: bool,
    /// Each parameter's value in the state the plugin last loaded, or as it
    /// started.
    base: BTreeMap<u32, f64>,
    /// The plugin's state as it started, which a state of `None` stands
    /// for. `None` for a plugin without state.
    fresh: Option<PluginState>,
    /// The parameters that follow automation, which the engine gives the
    /// plugin.
    automated: BTreeSet<u32>,
    /// The plugin's own bypass parameter, if it has one.
    bypass: Option<BypassParam>,
    /// Whether the plugin's own bypass is on, as it was last given or said.
    bypassed: bool,
    /// Whether it's to be on: the device is bypassed, and no lane moves its
    /// Bypass.
    wanted: bool,
    /// Whether a lane moves the device's Bypass.
    bypass_lane: bool,
    /// How many gestures are open in the plugin's window.
    gestures: u32,
    /// The plugin's state changed in a way its parameters don't say, and
    /// goes into the project once no gesture is open.
    dirty: bool,
    /// Until when what the plugin reports is its own doing, as it takes in
    /// what it was given last: see [`PLUGIN_SETTLING`].
    settling: Instant,
}

impl Plugins {
    /// Plugins activated for `config`, with none installed yet.
    pub fn new(config: EngineConfig) -> Self {
        Self {
            installed: None,
            kinds: BTreeMap::new(),
            never_ahead: BTreeSet::new(),
            marks_changed: false,
            live_changed: false,
            hosted: BTreeMap::new(),
            failed: BTreeMap::new(),
            config,
            waker: None,
            problems: Vec::new(),
        }
    }

    /// Finds plugins among `installed` from now on. Devices whose plugin
    /// didn't load are tried again on the next [`sync`](Self::sync).
    pub fn set_installed(&mut self, installed: Vec<PluginInfo>) {
        self.installed = Some(installed);
        self.failed.clear();
    }

    pub fn installed(&self) -> &[PluginInfo] {
        self.installed.as_deref().unwrap_or_default()
    }

    /// What `plugin` does with notes: what the user marked it as, or what
    /// the scan found.
    pub fn kind_of(&self, plugin: &PluginInfo) -> PluginKind {
        let key = (plugin.format, plugin.id.clone());
        self.kinds
            .get(&key)
            .copied()
            .unwrap_or_else(|| plugin.kind())
    }

    /// Marks the installed plugin `id` in `format` as `kind`, or takes the
    /// mark away if that's what the scan found. A VST3 plugin's class id
    /// can come in either case.
    pub fn set_kind(
        &mut self,
        format: PluginFormat,
        id: &str,
        kind: PluginKind,
    ) -> Result<(), String> {
        let plugin = self.find_installed(format, id)?;
        let key = (format, plugin.id.clone());
        let changed = match kind == plugin.kind() {
            true => self.kinds.remove(&key).is_some(),
            false => self.kinds.insert(key, kind) != Some(kind),
        };
        self.marks_changed |= changed;
        Ok(())
    }

    /// The installed plugin `id` in `format`, whose class id, for VST3,
    /// can come in either case.
    fn find_installed(&self, format: PluginFormat, id: &str) -> Result<&PluginInfo, String> {
        let found = self.installed().iter().find(|plugin| {
            plugin.format == format
                && match format {
                    PluginFormat::Vst3 => plugin.id.eq_ignore_ascii_case(id),
                    PluginFormat::Clap => plugin.id == id,
                }
        });
        found.ok_or_else(|| format!("there's no plugin {id:?} installed in that format"))
    }

    /// What the user marked plugins as, by format and id.
    pub fn kinds(&self) -> &BTreeMap<(PluginFormat, String), PluginKind> {
        &self.kinds
    }

    /// Has the plugins marked as in `kinds`, as the settings file had them.
    pub fn set_kinds(&mut self, kinds: BTreeMap<(PluginFormat, String), PluginKind>) {
        self.kinds = kinds;
    }

    /// Whether `plugin` is set to Render Ahead Never, so it renders live
    /// wherever it is.
    pub fn never_ahead(&self, plugin: &PluginInfo) -> bool {
        self.never_ahead
            .contains(&(plugin.format, plugin.id.clone()))
    }

    /// Sets the installed plugin `id` in `format` to Render Ahead Never, so
    /// its devices render live, as the audio device asks for each block,
    /// with what they play into, or with `never` false back to Auto, where
    /// they render ahead unless the plugin says it must render live. A VST3
    /// plugin's class id can come in either case.
    pub fn set_render_ahead(
        &mut self,
        format: PluginFormat,
        id: &str,
        never: bool,
    ) -> Result<(), String> {
        let key = (format, self.find_installed(format, id)?.id.clone());
        let changed = match never {
            true => self.never_ahead.insert(key.clone()),
            false => self.never_ahead.remove(&key),
        };
        self.marks_changed |= changed;
        let plays = |h: &Hosted| h.instance.info().format == key.0 && h.instance.info().id == key.1;
        self.live_changed |= changed && self.hosted.values().any(plays);
        Ok(())
    }

    /// The plugins the user set to Render Ahead Never, by format and id.
    pub fn never_ahead_marks(&self) -> &BTreeSet<(PluginFormat, String)> {
        &self.never_ahead
    }

    /// Has the plugins set to Render Ahead Never as in `never`, as the
    /// settings file had them.
    pub fn set_never_ahead_marks(&mut self, never: BTreeSet<(PluginFormat, String)>) {
        self.never_ahead = never;
    }

    /// Whether the user marked a plugin as another kind, or set its Render
    /// Ahead, since this last said.
    pub fn take_marks_changed(&mut self) -> bool {
        std::mem::take(&mut self.marks_changed)
    }

    /// Whether the user set the plugin of one of the devices to render
    /// live, or ahead, since this last said, so the graph needs building
    /// again.
    pub fn take_live_changed(&mut self) -> bool {
        std::mem::take(&mut self.live_changed)
    }

    /// Whether the device `id`'s plugin says it must render live, as it
    /// last said, rather than being set to.
    pub fn must_render_live(&self, id: DeviceId) -> bool {
        self.hosted.get(&id).is_some_and(|h| h.must_render_live)
    }

    /// Plugins loaded from now on call `waker` when they need
    /// [`poll`](Self::poll) called.
    pub fn set_waker(&mut self, waker: Waker) {
        self.waker = Some(waker);
    }

    /// Brings the instances in line with `project`'s plugin devices: loads
    /// the plugins of new ones, drops the instances of ones that went, and
    /// gives each plugin the settings that changed. Each plugin's side
    /// inputs that wires go into are on, and the others off, which restarts
    /// a plugin as they change. Returns whether that needs a new graph: a
    /// device gained or lost its instance, a plugin with automation loaded a
    /// state, which moves the automated parameters too, so the engine sends
    /// it their values afresh, or a plugin restarted with another latency.
    pub fn sync(&mut self, project: &Project) -> bool {
        let devices: BTreeMap<DeviceId, (&PluginDevice, bool)> = project
            .chains()
            .flat_map(|(_, devices)| devices)
            .filter_map(|d| Some((d.id, (d.kind.plugin()?, d.bypassed))))
            .collect();
        let mut wired: BTreeMap<DeviceId, Vec<u32>> = BTreeMap::new();
        for wire in project.live_wires() {
            if let Port::PluginInput(device, input) = wire.to {
                wired.entry(device).or_default().push(input);
            }
        }
        let mut automated: BTreeMap<DeviceId, BTreeSet<u32>> = BTreeMap::new();
        let mut bypass_lanes = BTreeSet::new();
        for lane in project.automation.iter().filter(|lane| !lane.overridden) {
            match lane.param {
                ParamAddress::Device(device, param) => {
                    automated.entry(device).or_default().insert(param);
                }
                ParamAddress::Bypass(device) => {
                    bypass_lanes.insert(device);
                }
                _ => {}
            }
        }
        let before = self.hosted.len();
        self.hosted.retain(|id, _| devices.contains_key(id));
        self.failed.retain(|id, _| devices.contains_key(id));
        let mut changed = self.hosted.len() != before;
        for (&id, &(plugin, bypassed)) in &devices {
            let automated = automated.remove(&id).unwrap_or_default();
            let bypass = Bypass {
                bypassed,
                lane: bypass_lanes.contains(&id),
            };
            let wired = wired.remove(&id).unwrap_or_default();
            if let Some(hosted) = self.hosted.get_mut(&id) {
                match hosted.apply(&plugin.settings, automated, bypass) {
                    Ok(loaded) => changed |= loaded && !hosted.automated.is_empty(),
                    Err(problem) => self.problems.push(problem),
                }
                let latency = hosted.instance.latency();
                match hosted.instance.set_side_inputs(&wired) {
                    Ok(restarted) => {
                        if restarted {
                            hosted.settle();
                            changed |= hosted.instance.latency() != latency;
                        }
                    }
                    Err(e) => self.problems.push(format!("{e}: it plays nothing")),
                }
            } else if self.installed.is_some() && !self.failed.contains_key(&id) {
                match self.load(plugin, automated, bypass, &wired) {
                    Ok(hosted) => {
                        self.hosted.insert(id, hosted);
                        changed = true;
                    }
                    Err(problem) => {
                        self.problems.push(problem.clone());
                        self.failed.insert(id, problem);
                    }
                }
            }
        }
        changed
    }

    /// Loads `plugin` and gives it its settings, but for the `automated`
    /// parameters, and its device's Bypass, with the side inputs `wired`
    /// on.
    fn load(
        &mut self,
        plugin: &PluginDevice,
        automated: BTreeSet<u32>,
        bypass: Bypass,
        wired: &[u32],
    ) -> Result<Hosted, String> {
        let name = display_name(plugin);
        let Some(info) = self
            .installed()
            .iter()
            .find(|p| p.format == plugin.format && p.id == plugin.id)
        else {
            return Err(format!(
                "{name} isn't installed, or Musicable hasn't found it: it plays nothing"
            ));
        };
        let instance = Instance::new(info.format, &info.path, &info.id, self.waker.clone())
            .map_err(|e| format!("{name} didn't load: {e}"))?;
        let fresh = match instance.has_state() {
            true => instance.save_state().ok().map(PluginState::new),
            false => None,
        };
        let mut hosted = Hosted {
            nodes: Arc::new(instance.node_source()),
            base: values(&instance),
            bypass: BypassParam::of(&instance),
            instance,
            name,
            applied: PluginSettings::default(),
            inputs: plugin.inputs.clone(),
            note_output: plugin.note_output,
            must_render_live: false,
            fresh,
            automated: BTreeSet::new(),
            bypassed: false,
            wanted: false,
            bypass_lane: false,
            gestures: 0,
            dirty: false,
            settling: Instant::now(),
        };
        hosted.bypassed = hosted.own_bypass();
        if let Err(problem) = hosted.apply(&plugin.settings, automated, bypass) {
            self.problems.push(problem);
        }
        // Before it's active, so it starts with them.
        if let Err(e) = hosted.instance.set_side_inputs(wired) {
            self.problems.push(e.to_string());
        }
        if let Err(e) = hosted.instance.activate(self.config) {
            self.problems.push(format!("{e}: it plays nothing"));
        }
        // Set up, as VST3 plugins may say.
        hosted.must_render_live = hosted.instance.must_render_live();
        hosted.settle();
        Ok(hosted)
    }

    /// The nodes the compiler makes for the plugin devices, and which of
    /// them render live: those whose plugin says it must, and those set to
    /// Render Ahead Never.
    pub fn devices(&self) -> Devices {
        let mut devices = Devices::new();
        for (&id, hosted) in &self.hosted {
            devices.insert(id, Arc::clone(&hosted.nodes));
            if hosted.must_render_live || self.never_ahead(hosted.instance.info()) {
                devices.set_live(id);
            }
        }
        devices
    }

    /// Activates every plugin for `config`, as when the audio device
    /// changes.
    pub fn set_config(&mut self, config: EngineConfig) {
        self.config = config;
        for hosted in self.hosted.values_mut() {
            if let Err(e) = hosted.instance.activate(config) {
                self.problems.push(format!("{e}: it plays nothing"));
            }
            hosted.settle();
        }
    }

    /// Drops every instance, as when another project opens.
    pub fn clear(&mut self) {
        self.hosted.clear();
        self.failed.clear();
    }

    /// Carries out what the plugins asked for, and hands back the changes
    /// they made themselves, oldest first, for the project to follow.
    pub fn poll(&mut self) -> Vec<(DeviceId, Change)> {
        let mut changes = Vec::new();
        for (&id, hosted) in &mut self.hosted {
            for change in hosted.poll() {
                changes.push((id, change));
            }
        }
        changes
    }

    /// When a plugin next needs [`poll`](Self::poll), for a timer it set.
    pub fn next_timer(&self) -> Option<Instant> {
        self.hosted
            .values()
            .filter_map(|h| h.instance.next_timer())
            .min()
    }

    /// `project` with each plugin's state saved afresh, for saving the
    /// project to a file: the state has whatever the plugin keeps that its
    /// parameters don't say.
    pub fn saved(&mut self, project: &Project) -> Project {
        let mut saved = project.clone();
        for (&id, hosted) in &self.hosted {
            if hosted.fresh.is_none() {
                continue;
            }
            let Some(plugin) = saved.device_mut(id).and_then(|d| d.kind.plugin_mut()) else {
                continue;
            };
            match hosted.instance.save_state() {
                // The parameters set since too, in case some haven't
                // reached the plugin yet: it gets them as it next plays.
                Ok(state) => plugin.settings.state = Some(PluginState::new(state)),
                Err(e) => self
                    .problems
                    .push(format!("{e}, so the project has its state from before")),
            }
        }
        saved
    }

    /// The instance of the device `id`'s plugin, if it loaded.
    pub fn instance(&self, id: DeviceId) -> Option<&Instance> {
        self.hosted.get(&id).map(|h| &h.instance)
    }

    pub fn instance_mut(&mut self, id: DeviceId) -> Option<&mut Instance> {
        self.hosted.get_mut(&id).map(|h| &mut h.instance)
    }

    /// Why the device `id`'s plugin didn't load, if it didn't.
    pub fn problem(&self, id: DeviceId) -> Option<&str> {
        self.failed.get(&id).map(String::as_str)
    }

    /// What the Edit menu calls the device `id`'s plugin.
    pub fn name(&self, id: DeviceId) -> Option<&str> {
        self.hosted.get(&id).map(|h| h.name.as_str())
    }

    /// What went wrong since the last call, for the app to show.
    pub fn take_problems(&mut self) -> Vec<String> {
        std::mem::take(&mut self.problems)
    }
}

/// What a plugin device's Bypass is: whether the device is bypassed, and
/// whether a lane moves it.
#[derive(Debug, Clone, Copy)]
struct Bypass {
    bypassed: bool,
    lane: bool,
}

impl Hosted {
    /// Gives the plugin `settings`, as far as they differ from what it has,
    /// but for the `automated` parameters, which the engine gives it. A
    /// parameter whose automation stopped gets its value back. Its own
    /// bypass parameter follows `bypass`, whatever the settings say. Returns
    /// whether the plugin loaded a state.
    fn apply(
        &mut self,
        settings: &PluginSettings,
        automated: BTreeSet<u32>,
        bypass: Bypass,
    ) -> Result<bool, String> {
        let released: BTreeSet<u32> = self.automated.difference(&automated).copied().collect();
        self.automated = automated;
        let mut result = Ok(false);
        if settings.state != self.applied.state {
            result = Ok(true);
            self.settle();
            if let Some(state) = settings.state.as_ref().or(self.fresh.as_ref())
                && let Err(e) = self.instance.load_state(state.bytes())
            {
                result = Err(e.to_string());
            }
            // Either way, so a state the plugin can't load isn't tried
            // again with every change.
            self.applied = PluginSettings {
                state: settings.state.clone(),
                params: Vec::new(),
            };
            self.base = values(&self.instance);
            // The state can turn its bypass on or off: the device's wins.
            self.bypassed = self.own_bypass();
        }
        let ids: BTreeSet<u32> = (settings.params.iter())
            .chain(&self.applied.params)
            .map(|p| p.id)
            .chain(released.iter().copied())
            .collect();
        let mut given = false;
        let own_bypass = self.bypass.map(|b| b.id);
        for id in ids.difference(&self.automated) {
            if Some(*id) == own_bypass {
                continue;
            }
            let base = self.base.get(id).copied();
            let wanted = settings.param(*id).or(base);
            let had = self.applied.param(*id).or(base);
            if let Some(value) = wanted
                && (wanted != had || released.contains(id))
            {
                self.instance.set_param(*id, value);
                given = true;
            }
        }
        if given {
            self.settle();
        }
        self.applied.params.clone_from(&settings.params);
        self.wanted = bypass.bypassed && !bypass.lane;
        self.bypass_lane = bypass.lane;
        self.link_bypass();
        result
    }

    /// Whether the plugin's own bypass is on, as it has it.
    fn own_bypass(&self) -> bool {
        self.bypass.is_some_and(|param| {
            (self.instance.param_value(param.id)).is_some_and(|value| param.is_on(value))
        })
    }

    /// Turns the plugin's own bypass on or off as its device's Bypass has
    /// it, once it can: off at once, and on once the plugin's node has
    /// stopped, after the switch's fade. A parameter with a lane of its own
    /// follows that instead.
    fn link_bypass(&mut self) {
        let Some(param) = self.bypass else {
            return;
        };
        if self.bypassed == self.wanted || self.automated.contains(&param.id) {
            return;
        }
        if self.wanted && self.instance.is_running() {
            return;
        }
        self.bypassed = self.wanted;
        let value = if self.wanted { param.on } else { param.off };
        self.instance.set_param(param.id, value);
        self.settle();
    }

    /// Takes in the plugin's own bypass moving to `value`, as made
    /// `by_hand` or not. By hand, it bypasses the device or brings it back.
    /// On the plugin's own, as when a MIDI control it learned moves it, the
    /// device follows, unless a lane moves the device's Bypass, or the
    /// plugin is taking in what it was given: the link sets it back then.
    fn follow_bypass(&mut self, param: BypassParam, value: f64, by_hand: bool) -> Option<Change> {
        let bypassed = param.is_on(value);
        if bypassed == self.bypassed {
            return None;
        }
        self.bypassed = bypassed;
        if !by_hand && (self.bypass_lane || Instant::now() < self.settling) {
            return None;
        }
        // The device follows, so the link has nothing to set back.
        self.wanted = bypassed;
        Some(Change::Bypass { bypassed, by_hand })
    }

    /// Starts the plugin's settling: what it reports for a while from now
    /// is it taking in what it was just given, or starting.
    fn settle(&mut self) {
        self.settling = Instant::now() + PLUGIN_SETTLING;
    }

    /// Whether what the plugin reports now, outside a gesture, is
    /// someone's change in its window: it's open, and the plugin isn't
    /// settling.
    fn by_hand(&self) -> bool {
        self.instance.is_window_open() && Instant::now() >= self.settling
    }

    /// Polls the plugin, and turns what it says into changes for the
    /// project.
    fn poll(&mut self) -> Vec<Change> {
        let mut changes = Vec::new();
        for event in self.instance.poll() {
            match event {
                PluginEvent::GestureBegin { .. } => {
                    self.gestures += 1;
                    if self.gestures == 1 {
                        changes.push(Change::GestureBegin);
                    }
                }
                // Automation moving the parameter, or the plugin echoing
                // it: only someone moving it overrides the automation.
                PluginEvent::ParamChanged { param, .. }
                    if self.gestures == 0 && self.automated.contains(&param) => {}
                PluginEvent::ParamChanged { param, value }
                    if let Some(bypass) = self.bypass
                        && bypass.id == param
                        && !self.automated.contains(&param) =>
                {
                    let by_hand = self.gestures > 0 || self.by_hand();
                    changes.extend(self.follow_bypass(bypass, value, by_hand));
                }
                PluginEvent::ParamChanged { param, value } => {
                    let had = self.applied.param(param).or(self.base.get(&param).copied());
                    // A plugin can echo a value the host set.
                    if had != Some(value) {
                        self.applied.set_param(param, Some(value));
                        let by_hand = self.gestures > 0 || self.by_hand();
                        changes.push(Change::Param {
                            param,
                            value,
                            by_hand,
                        });
                    }
                }
                PluginEvent::GestureEnd { .. } if self.gestures > 0 => {
                    self.gestures -= 1;
                    if self.gestures == 0 {
                        self.capture(true, &mut changes);
                        changes.push(Change::GestureEnd);
                    }
                }
                PluginEvent::GestureEnd { .. } => {}
                PluginEvent::StateChanged => self.dirty = true,
                PluginEvent::ParamsChanged => {
                    self.dirty = true;
                    self.bypass = BypassParam::of(&self.instance);
                }
                PluginEvent::Restarted { latency_changed } => {
                    self.settle();
                    if latency_changed {
                        changes.push(Change::Latency);
                    }
                }
                PluginEvent::WindowResize { .. } | PluginEvent::WindowClosed => {
                    changes.push(Change::Window(event));
                }
                PluginEvent::Failed(message) => changes.push(Change::Failed(message)),
            }
        }
        if self.gestures == 0 {
            let by_hand = self.by_hand();
            self.capture(by_hand, &mut changes);
        }
        // Its node may have stopped since, or the plugin moved its bypass
        // when it wasn't to.
        self.link_bypass();
        // As it loads, and as it restarts with other ports.
        if self.instance.side_inputs() != self.inputs {
            self.inputs = self.instance.side_inputs().to_vec();
            changes.push(Change::Inputs(self.inputs.clone()));
        }
        if self.instance.note_output() != self.note_output {
            self.note_output = self.instance.note_output();
            changes.push(Change::NoteOutput(self.note_output));
        }
        if self.instance.must_render_live() != self.must_render_live {
            self.must_render_live = self.instance.must_render_live();
            changes.push(Change::Live);
        }
        changes
    }

    /// Takes in the change the plugin said its state had, if it said so,
    /// as made `by_hand` or not. A plugin with a state saves it; one
    /// without has only its parameters to read.
    fn capture(&mut self, by_hand: bool, changes: &mut Vec<Change>) {
        if !std::mem::take(&mut self.dirty) {
            return;
        }
        if self.fresh.is_none() {
            for (id, value) in values(&self.instance) {
                if self.automated.contains(&id) {
                    continue;
                }
                if let Some(bypass) = self.bypass
                    && bypass.id == id
                {
                    changes.extend(self.follow_bypass(bypass, value, by_hand));
                    continue;
                }
                let had = self.applied.param(id).or(self.base.get(&id).copied());
                if had != Some(value) {
                    self.applied.set_param(id, Some(value));
                    changes.push(Change::Param {
                        param: id,
                        value,
                        by_hand,
                    });
                }
            }
            return;
        }
        match self.instance.save_state() {
            Ok(state) => {
                self.applied = PluginSettings {
                    state: Some(PluginState::new(state)),
                    params: Vec::new(),
                };
                self.base = values(&self.instance);
                changes.push(Change::Settings {
                    settings: self.applied.clone(),
                    by_hand,
                });
            }
            Err(e) => changes.push(Change::Failed(e.to_string())),
        }
    }
}

/// Each of the plugin's parameters, with its value as the plugin has it.
fn values(instance: &Instance) -> BTreeMap<u32, f64> {
    instance
        .params()
        .iter()
        .filter_map(|p| Some((p.id, instance.param_value(p.id)?)))
        .collect()
}

/// What a plugin device is called: its name, or its id if it has none.
fn display_name(plugin: &PluginDevice) -> String {
    match plugin.name.trim() {
        "" => plugin.id.clone(),
        name => name.to_owned(),
    }
}
