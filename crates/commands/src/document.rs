//! The open project, and the transactions that change it.
//!
//! Commands change the project only through [`Document::transact`]. A
//! transaction records each [`Edit`] it makes along with the value it
//! replaced, so everything that has to follow the project hooks in here: the
//! engine and the undo history now, ARA change notifications in Tier 2.

use musicable_model::automation::{self, Lane};
use musicable_model::clip::{
    AudioClip, Clip, ClipContent, ClipId, Controller, Dimension, Fades, MidiClip, Note, NoteId,
    Point, Timing, Warping,
};
use musicable_model::mixer::Bus;
use musicable_model::modulation::{
    Control, ControlKind, Modulator, ModulatorId, ModulatorKind, Output, Routing, RoutingId,
};
use musicable_model::native::NativeKind;
use musicable_model::param::{ClipParam, ParamAddress, ParamSpec};
use musicable_model::plugin::{NoteTiming, PluginInput, PluginSettings};
use musicable_model::routing::{InputError, Port, Route, Tap, Wire, WireError, WireId};
use musicable_model::time::{BeatRange, Beats, TempoMap};
use musicable_model::{
    Chain, ClipType, Device, DeviceId, DeviceKind, Input, Locator, LocatorId, Monitor, Project,
    Send, SendId, Take, TakeId, Track, TrackId, TrackKind,
};

use crate::editor::Editor;
use crate::history::{History, Steps};

/// The open project. It hands out `&Project` only, so every change goes
/// through a transaction, and it keeps the history that undo and redo step
/// through, and the arrangement's [`Editor`] state.
#[derive(Debug, Clone, Default)]
pub struct Document {
    project: Project,
    editor: Editor,
    /// Transactions committed since the last [`take_committed`](Self::take_committed).
    committed: Vec<Committed>,
    history: History,
    /// Where the batch in progress started, if one is.
    batch: Option<BatchStart>,
}

/// Where a batch started: how many transactions had been committed, and
/// the editor's state, which [`Document::cancel_batch`] goes back to.
#[derive(Debug, Clone)]
struct BatchStart {
    committed: usize,
    editor: Editor,
}

impl Document {
    /// A document for `project`, showing the view it was saved with, if
    /// any, which it then leaves out: the editor has it from then on.
    pub fn new(mut project: Project) -> Self {
        let mut editor = Editor::default();
        if let Some(view) = project.view.take() {
            editor.restore_view(&view);
        }
        Self {
            project,
            editor,
            committed: Vec::new(),
            history: History::default(),
            batch: None,
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    /// The selection, the start marker, the grid and the zoom. Changing them
    /// isn't a change to the project: it leaves the undo history alone.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The project, and the editor's state to change, at once.
    pub fn parts(&mut self) -> (&Project, &mut Editor) {
        (&self.project, &mut self.editor)
    }

    /// Runs `edit` as one transaction. `name` is what the Edit menu calls
    /// it, as in "Undo Tempo Up". A transaction that changes nothing leaves
    /// no record. Any other becomes a step in the undo history, or joins the
    /// last one while a [gesture](Self::begin_gesture) is open.
    pub fn transact<R>(&mut self, name: &str, edit: impl FnOnce(&mut Transaction<'_>) -> R) -> R {
        let mut transaction = Transaction::new(&mut self.project, &self.editor);
        let result = edit(&mut transaction);
        let edits = transaction.edits;
        if !edits.is_empty() {
            self.history.record(name, &edits);
            self.committed.push(Committed {
                name: name.to_owned(),
                edits,
            });
            self.editor.prune(&self.project);
        }
        result
    }

    /// Runs `edit` as a change the project follows rather than one someone
    /// made, such as a plugin changing its own state as it starts. It's
    /// committed as a transaction is, so the engine follows it, but it's no
    /// step in the undo history: undo doesn't take it back, and what was
    /// undone can still be redone.
    pub fn follow<R>(&mut self, name: &str, edit: impl FnOnce(&mut Transaction<'_>) -> R) -> R {
        let mut transaction = Transaction::new(&mut self.project, &self.editor);
        let result = edit(&mut transaction);
        let edits = transaction.edits;
        self.commit(name.to_owned(), edits);
        result
    }

    /// The id of the last step in the undo history, which names it while
    /// it's the last, for [`amend`](Self::amend).
    pub fn last_step(&self) -> Option<u64> {
        self.history.last_step()
    }

    /// Runs `edit` as part of the last step in the undo history, if that's
    /// the step with id `step`, as if the step had made its edits too: undo
    /// takes them back with it. Returns `None`, running nothing, if another
    /// step has come since, or that one was undone.
    pub fn amend<R>(
        &mut self,
        step: u64,
        edit: impl FnOnce(&mut Transaction<'_>) -> R,
    ) -> Option<R> {
        if self.history.last_step() != Some(step) {
            return None;
        }
        let mut transaction = Transaction::new(&mut self.project, &self.editor);
        let result = edit(&mut transaction);
        let edits = transaction.edits;
        if !edits.is_empty()
            && let Some(name) = self.history.amend(step, &edits)
        {
            self.committed.push(Committed { name, edits });
            self.editor.prune(&self.project);
        }
        Some(result)
    }

    /// Takes back the last step in the undo history, by setting what it
    /// changed back the way it was. Returns `false` if there's nothing to
    /// undo.
    ///
    /// The change is committed like any other, under the name "Undo" and
    /// the step's, so the engine follows it. Only redo can make the step
    /// again, until a new transaction forgets it.
    pub fn undo(&mut self) -> bool {
        let Some(step) = self.history.undo() else {
            return false;
        };
        let mut transaction = Transaction::new(&mut self.project, &self.editor);
        for edit in step.edits.iter().rev() {
            transaction.apply(&edit.inverse());
        }
        let edits = transaction.edits;
        let name = format!("Undo {}", step.name);
        self.commit(name, edits);
        true
    }

    /// Makes the last step that was undone again. Returns `false` if there's
    /// nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(step) = self.history.redo() else {
            return false;
        };
        let mut transaction = Transaction::new(&mut self.project, &self.editor);
        for edit in &step.edits {
            transaction.apply(edit);
        }
        let edits = transaction.edits;
        let name = format!("Redo {}", step.name);
        self.commit(name, edits);
        true
    }

    /// Undoes or redoes steps, one at a time, until the first `done` steps
    /// in the undo history are done and those after them undone, as
    /// clicking a step in the History panel does: 0 undoes every step. Each
    /// is committed as [`undo`](Self::undo) and [`redo`](Self::redo) commit
    /// theirs. Returns `false` if the history has fewer steps.
    pub fn go_to(&mut self, done: usize) -> bool {
        if done > self.steps().len() {
            return false;
        }
        while self.steps().done() > done {
            self.undo();
        }
        while self.steps().done() < done {
            self.redo();
        }
        true
    }

    /// The steps in the undo history, by name, for the History panel.
    pub fn steps(&self) -> Steps<'_> {
        self.history.steps()
    }

    /// The name of the step [`undo`](Self::undo) would take back, for the
    /// Edit menu, or `None` if there's nothing to undo.
    pub fn undo_name(&self) -> Option<&str> {
        self.steps().undo_name()
    }

    /// The name of the step [`redo`](Self::redo) would make again, or `None`.
    pub fn redo_name(&self) -> Option<&str> {
        self.steps().redo_name()
    }

    /// Starts a gesture, such as dragging a fader or the tempo field: until
    /// [`end_gesture`](Self::end_gesture), transactions with the same name
    /// join the gesture's step in the undo history, so one undo takes the
    /// whole drag back. A gesture that ends where it started leaves no step.
    /// Undo and redo end the gesture.
    pub fn begin_gesture(&mut self) {
        self.history.begin_gesture();
    }

    pub fn end_gesture(&mut self) {
        self.history.end_gesture();
    }

    /// Starts a batch: until [`end_batch`](Self::end_batch), every
    /// transaction joins one step in the undo history called `name`,
    /// whatever its own name, so one undo takes them all back. A batch that
    /// changes nothing leaves no step, and what was undone before it can
    /// still be redone. [`Registry::run_batch`](crate::Registry::run_batch)
    /// runs commands in one.
    pub fn begin_batch(&mut self, name: &str) {
        self.batch = Some(BatchStart {
            committed: self.committed.len(),
            editor: self.editor.clone(),
        });
        self.history.begin_batch(name);
    }

    pub fn end_batch(&mut self) {
        self.batch = None;
        self.history.end_batch();
    }

    /// Ends the batch by taking back everything it changed, as if it had
    /// never started: the project, the editor's state and the undo history
    /// go back to how they were. What it committed is taken back too, so
    /// whatever keeps itself in step with the project never sees it, as long
    /// as it hasn't [taken](Self::take_committed) it yet.
    pub fn cancel_batch(&mut self) {
        let Some(start) = self.batch.take() else {
            return;
        };
        let made = self
            .committed
            .split_off(start.committed.min(self.committed.len()));
        let mut transaction = Transaction::new(&mut self.project, &self.editor);
        for committed in made.iter().rev() {
            for edit in committed.edits.iter().rev() {
                transaction.apply(&edit.inverse());
            }
        }
        self.editor = start.editor;
        self.history.cancel_batch();
    }

    /// The transactions committed since the last call, oldest first, for
    /// whatever keeps itself in step with the project. The app sends the
    /// engine what they changed. Undo and redo are in here too.
    pub fn take_committed(&mut self) -> Vec<Committed> {
        std::mem::take(&mut self.committed)
    }

    /// Whether the project changed since it was opened or last saved. Undo
    /// back to where it was saved, and it hasn't.
    pub fn is_modified(&self) -> bool {
        self.history.is_modified()
    }

    /// Call once the project is saved.
    pub fn mark_saved(&mut self) {
        self.history.mark_saved();
    }

    fn commit(&mut self, name: String, edits: Vec<Edit>) {
        if !edits.is_empty() {
            self.committed.push(Committed { name, edits });
            self.editor.prune(&self.project);
        }
    }
}

/// Changes to the project, recorded as they're made. [`Document::transact`]
/// hands one out.
///
/// There's a setter for each part of the project that commands change. A
/// setter that doesn't change anything records nothing.
#[derive(Debug)]
pub struct Transaction<'a> {
    project: &'a mut Project,
    editor: &'a Editor,
    edits: Vec<Edit>,
    /// Whether a move, an output or a send that closes a loop through a
    /// wire is refused as it's made: not within [`at_once`](Self::at_once),
    /// which checks once all its edits are made, or while undo and redo
    /// [apply](Self::apply) edits.
    checks_loops: bool,
}

impl<'a> Transaction<'a> {
    fn new(project: &'a mut Project, editor: &'a Editor) -> Self {
        Self {
            project,
            editor,
            edits: Vec::new(),
            checks_loops: true,
        }
    }
}

impl Transaction<'_> {
    /// The project with the transaction's changes so far.
    pub fn project(&self) -> &Project {
        self.project
    }

    /// The editor's state, as it was when the transaction started.
    pub fn editor(&self) -> &Editor {
        self.editor
    }

    pub fn set_tempo(&mut self, tempo: TempoMap) {
        if tempo != self.project.tempo {
            let from = std::mem::replace(&mut self.project.tempo, tempo.clone());
            self.edits.push(Edit::Tempo { from, to: tempo });
        }
    }

    pub fn set_loop_region(&mut self, region: BeatRange) {
        let from = std::mem::replace(&mut self.project.loop_region, region);
        if from != region {
            self.edits.push(Edit::LoopRegion { from, to: region });
        }
    }

    pub fn set_looping(&mut self, looping: bool) {
        let from = std::mem::replace(&mut self.project.looping, looping);
        if from != looping {
            self.edits.push(Edit::Looping { from, to: looping });
        }
    }

    pub fn set_metronome(&mut self, on: bool) {
        let from = std::mem::replace(&mut self.project.metronome, on);
        if from != on {
            self.edits.push(Edit::Metronome { from, to: on });
        }
    }

    pub fn set_punch_in(&mut self, on: bool) {
        let from = std::mem::replace(&mut self.project.punch_in, on);
        if from != on {
            self.edits.push(Edit::PunchIn { from, to: on });
        }
    }

    pub fn set_punch_out(&mut self, on: bool) {
        let from = std::mem::replace(&mut self.project.punch_out, on);
        if from != on {
            self.edits.push(Edit::PunchOut { from, to: on });
        }
    }

    /// A new track with the next track id, for [`add_track`](Self::add_track):
    /// see [`Project::new_track`]. Taking an id isn't an edit, since ids
    /// are never used again, even if the track never makes it in.
    pub fn new_track(&mut self, name: impl Into<String>, kind: TrackKind) -> Track {
        self.project.new_track(name, kind)
    }

    /// A new send to the return track `to`, for [`add_send`](Self::add_send):
    /// see [`Project::new_send`].
    pub fn new_send(&mut self, to: TrackId) -> Send {
        self.project.new_send(to)
    }

    /// A new device, with the next device id, for
    /// [`add_device`](Self::add_device): see [`Project::new_device`].
    pub fn new_device(&mut self, kind: DeviceKind) -> Device {
        self.project.new_device(kind)
    }

    /// Puts `device` at `index` in `chain`, or at the end if `index` is past
    /// it. It's left out if the chain's track isn't there, if the project
    /// has a device with its id, or if a file couldn't hold its settings.
    pub fn add_device(&mut self, chain: Chain, index: usize, device: Device) {
        let fits = device.kind.plugin().is_none_or(|p| p.settings.is_valid());
        if !fits || self.project.find_device(device.id).is_some() {
            return;
        }
        let Some(devices) = self.project.devices_mut(chain) else {
            return;
        };
        let index = index.min(devices.len());
        devices.insert(index, device.clone());
        self.edits.push(Edit::AddDevice {
            chain,
            index,
            device,
        });
    }

    /// Removes the device with id `device` from its chain, and returns it.
    /// The wires to and from it, the inputs of the tracks that take what it
    /// puts out, which go to No Input, the routings to its parameters and
    /// the lanes that automate them and its Bypass go first, each as an
    /// edit of its own.
    pub fn remove_device(&mut self, device: DeviceId) -> Option<Device> {
        self.project.find_device(device)?;
        for wire in self.project.wires_of_device(device) {
            self.remove_wire(wire);
        }
        for track in self.project.inputs_from_device(device) {
            let _ = self.set_track_input(track, None);
        }
        self.remove_routings(|_, _, routing| routing.to.device() == Some(device));
        self.remove_lanes(|param| param.device() == Some(device));
        let (chain, index, _) = self.project.find_device(device)?;
        let device = self.project.devices_mut(chain)?.remove(index);
        self.edits.push(Edit::RemoveDevice {
            chain,
            index,
            device: device.clone(),
        });
        Some(device)
    }

    /// Moves the device with id `device` to `index` in `chain`, counting
    /// without it, or to the end if `index` is past it. It keeps its id, so
    /// its lanes and its wires, which name it by its id, go with it, and so
    /// do its plugin and its node's sound. A device the project hasn't got,
    /// or a chain whose track isn't there, is left alone, and a move that
    /// would close a loop through a wire is refused. Onto the master, whose
    /// one tap for an input is its Post Fader, the inputs of the tracks
    /// that take what it puts out go to No Input first, each as an edit of
    /// its own, as when it's removed.
    pub fn move_device(
        &mut self,
        device: DeviceId,
        chain: Chain,
        index: usize,
    ) -> Result<(), Loop> {
        let Some((from, at, _)) = self.project.find_device(device) else {
            return Ok(());
        };
        if self.project.devices(chain).is_none() {
            return Ok(());
        }
        let Some(moved) = (self.project.devices_mut(from)).map(|devices| devices.remove(at)) else {
            return Ok(());
        };
        let Some(devices) = self.project.devices_mut(chain) else {
            return Ok(());
        };
        let index = index.min(devices.len());
        devices.insert(index, moved);
        if (from, at) == (chain, index) {
            return Ok(());
        }
        if let Some(route) = self.closed_loop() {
            let devices = self.project.devices_mut(chain).expect("it just went in");
            let moved = devices.remove(index);
            let devices = self.project.devices_mut(from).expect("it just came out");
            devices.insert(at, moved);
            return Err(Loop(route));
        }
        if chain == Chain::Master {
            for track in self.project.inputs_from_device(device) {
                let _ = self.set_track_input(track, None);
            }
        }
        self.edits.push(Edit::MoveDevice {
            device,
            from: (from, at),
            to: (chain, index),
        });
        Ok(())
    }

    /// Bypasses the device with id `device`, or brings it back with
    /// `false`. A bypassed device stops, and what comes into it plays on,
    /// as late as its latency would make it. A device the project hasn't
    /// got is left alone.
    pub fn set_device_bypass(&mut self, device: DeviceId, bypassed: bool) {
        let Some(d) = self.project.device_mut(device) else {
            return;
        };
        if d.bypassed != bypassed {
            d.bypassed = bypassed;
            self.edits.push(Edit::DeviceBypass {
                device,
                from: !bypassed,
                to: bypassed,
            });
        }
    }

    /// Deactivates the device with id `device`, or activates it with
    /// `true`. A deactivated device is out of the signal, latency and all,
    /// though it keeps its place and its settings. A device the project
    /// hasn't got is left alone.
    pub fn set_device_active(&mut self, device: DeviceId, active: bool) {
        let Some(d) = self.project.device_mut(device) else {
            return;
        };
        if d.active != active {
            d.active = active;
            self.edits.push(Edit::DeviceActive {
                device,
                from: !active,
                to: active,
            });
        }
    }

    /// Sets the parameter `param` of the plugin device with id `device` to
    /// `value`, in the plugin's own units, or leaves it to the plugin's
    /// state with `None`. A value that isn't a finite number does nothing,
    /// and so does a device that isn't a plugin.
    pub fn set_plugin_param(&mut self, device: DeviceId, param: u32, value: Option<f64>) {
        if value.is_some_and(|v| !v.is_finite()) {
            return;
        }
        let Some(plugin) = self
            .project
            .device_mut(device)
            .and_then(|d| d.kind.plugin_mut())
        else {
            return;
        };
        if plugin.settings.param(param) != value {
            let from = plugin.settings.set_param(param, value);
            self.edits.push(Edit::PluginParam {
                device,
                param,
                from,
                to: value,
            });
        }
    }

    /// Sets the settings of the plugin device with id `device` as a whole,
    /// as when the plugin saves a new state. Settings a file couldn't hold
    /// do nothing, and so does a device that isn't a plugin.
    pub fn set_plugin_settings(&mut self, device: DeviceId, settings: PluginSettings) {
        if !settings.is_valid() {
            return;
        }
        let Some(plugin) = self
            .project
            .device_mut(device)
            .and_then(|d| d.kind.plugin_mut())
        else {
            return;
        };
        if plugin.settings != settings {
            let from = std::mem::replace(&mut plugin.settings, settings.clone());
            self.edits.push(Edit::PluginSettings {
                device,
                from,
                to: settings,
            });
        }
    }

    /// Sets the side inputs of the plugin device with id `device` to the
    /// ones its plugin says it has, `inputs`, in its order. An input it no
    /// longer has stays, after them, while a wire goes into it, so no wire
    /// loses its port. Two inputs with one id do nothing, and so does a
    /// device that isn't a plugin.
    pub fn set_plugin_inputs(&mut self, device: DeviceId, mut inputs: Vec<PluginInput>) {
        let wired: Vec<u32> = (self.project.wires.iter())
            .filter_map(|wire| match wire.to {
                Port::PluginInput(to, input) if to == device => Some(input),
                _ => None,
            })
            .collect();
        let found = self.project.find_device(device);
        let Some(plugin) = found.and_then(|(_, _, d)| d.kind.plugin()) else {
            return;
        };
        for input in &plugin.inputs {
            let gone = !inputs.iter().any(|i| i.id == input.id);
            if gone && wired.contains(&input.id) {
                inputs.push(input.clone());
            }
        }
        let mut ids: Vec<u32> = inputs.iter().map(|input| input.id).collect();
        ids.sort_unstable();
        if ids.windows(2).all(|pair| pair[0] != pair[1]) {
            self.put_plugin_inputs(device, inputs);
        }
    }

    /// Sets whether the plugin device with id `device` has a note output to
    /// what its plugin says, `on`. Without one now, it keeps it while a
    /// MIDI track takes its notes, or a note wire, so neither loses them. A
    /// device that isn't a plugin does nothing.
    pub fn set_plugin_note_output(&mut self, device: DeviceId, on: bool) {
        let tap = Tap::Device(device);
        let project = &self.project;
        let taken = (project.tracks.iter())
            .filter(|track| track.kind == TrackKind::Midi)
            .any(|track| track.input == Some(Input::Tap(tap)))
            || (project.wires.iter()).any(|wire| wire.from == tap && wire.takes_notes());
        self.put_plugin_note_output(device, on || taken);
    }

    /// Sets when the notes the plugin device with id `device` plays come,
    /// against its latency, to `timing`, as someone chose, or with `None`,
    /// leaves it to what the app finds out of the plugin, as it is until the
    /// app next says ([`follow_plugin_note_timing`](Self::follow_plugin_note_timing)).
    /// A device that isn't a plugin does nothing.
    pub fn set_plugin_note_timing(&mut self, device: DeviceId, timing: Option<NoteTiming>) {
        let Some(plugin) = (self.project.find_device(device)).and_then(|(_, _, d)| d.kind.plugin())
        else {
            return;
        };
        let to = (timing.unwrap_or(plugin.note_timing), timing.is_some());
        self.put_plugin_note_timing(device, to);
    }

    /// Sets when the notes the plugin device with id `device` plays come to
    /// `timing`, what the app found out of its plugin, unless someone chose
    /// it for the device.
    pub fn follow_plugin_note_timing(&mut self, device: DeviceId, timing: NoteTiming) {
        let Some(plugin) = (self.project.find_device(device)).and_then(|(_, _, d)| d.kind.plugin())
        else {
            return;
        };
        if !plugin.note_timing_chosen {
            self.put_plugin_note_timing(device, (timing, false));
        }
    }

    /// Sets the plugin device with id `device`'s note timing and whether
    /// someone chose it, as undo and redo do.
    fn put_plugin_note_timing(&mut self, device: DeviceId, to: (NoteTiming, bool)) {
        let Some(plugin) = (self.project.device_mut(device)).and_then(|d| d.kind.plugin_mut())
        else {
            return;
        };
        let from = (plugin.note_timing, plugin.note_timing_chosen);
        if from != to {
            (plugin.note_timing, plugin.note_timing_chosen) = to;
            self.edits.push(Edit::PluginNoteTiming { device, from, to });
        }
    }

    /// Sets whether the plugin device with id `device` has a note output to
    /// `on`, as undo and redo do.
    fn put_plugin_note_output(&mut self, device: DeviceId, on: bool) {
        let Some(plugin) = (self.project.device_mut(device)).and_then(|d| d.kind.plugin_mut())
        else {
            return;
        };
        if plugin.note_output != on {
            plugin.note_output = on;
            self.edits.push(Edit::PluginNoteOutput {
                device,
                from: !on,
                to: on,
            });
        }
    }

    /// Sets the side inputs of the plugin device with id `device` to
    /// `inputs` as they are, as undo and redo do.
    fn put_plugin_inputs(&mut self, device: DeviceId, inputs: Vec<PluginInput>) {
        let Some(plugin) = self
            .project
            .device_mut(device)
            .and_then(|d| d.kind.plugin_mut())
        else {
            return;
        };
        if plugin.inputs != inputs {
            let from = std::mem::replace(&mut plugin.inputs, inputs.clone());
            self.edits.push(Edit::PluginInputs {
                device,
                from,
                to: inputs,
            });
        }
    }

    /// Sets parameter `param` of the built-in device with id `device` to
    /// `value`, kept in the parameter's range ([`ParamInfo::clamp`]). Not a
    /// number does nothing, and so do a device that isn't built in and a
    /// parameter it hasn't got.
    ///
    /// [`ParamInfo::clamp`]: musicable_model::native::ParamInfo::clamp
    pub fn set_native_param(&mut self, device: DeviceId, param: u32, value: f64) {
        if value.is_nan() {
            return;
        }
        let Some(native) = self
            .project
            .device_mut(device)
            .and_then(|d| d.kind.native_mut())
        else {
            return;
        };
        let Some(from) = native.set(param as usize, value) else {
            return;
        };
        let to = native.values[param as usize];
        if from != to {
            self.edits.push(Edit::NativeParam {
                device,
                param,
                from,
                to,
            });
        }
    }

    /// Gives the sampler with id `device` the sample `file`, or takes its
    /// sample away with `None`. An empty path does nothing, and so does a
    /// device that isn't a sampler.
    pub fn set_native_sample(&mut self, device: DeviceId, file: Option<String>) {
        if file.as_ref().is_some_and(String::is_empty) {
            return;
        }
        let Some(native) = self
            .project
            .device_mut(device)
            .and_then(|d| d.kind.native_mut())
            .filter(|native| native.kind == NativeKind::Sampler)
        else {
            return;
        };
        if native.sample != file {
            let from = std::mem::replace(&mut native.sample, file.clone());
            self.edits.push(Edit::NativeSample {
                device,
                from,
                to: file,
            });
        }
    }

    /// Puts `track` at `index` in the track list, or at the end if `index`
    /// is past it. A track with the id of one the project has is left out.
    pub fn add_track(&mut self, index: usize, track: Track) {
        if self.project.track(track.id).is_some() {
            return;
        }
        let index = index.min(self.project.tracks.len());
        self.project.tracks.insert(index, track.clone());
        self.edits.push(Edit::AddTrack { index, track });
    }

    /// Removes the track with id `id` and returns it. What refers to it goes
    /// first, each as an edit of its own: the wires to and from it and its
    /// devices go, tracks taking their input from it or its devices go to No
    /// Input, tracks feeding it feed the master instead, sends to it go, and
    /// so do the routings on other channels to its parameters, and the lanes
    /// that automate its parameters, its sends', its devices' and its
    /// modulators'.
    pub fn remove_track(&mut self, id: TrackId) -> Option<Track> {
        self.project.track(id)?;
        for wire in self.project.wires_of_track(id) {
            self.remove_wire(wire);
        }
        for track in self.project.inputs_from_track(id) {
            let _ = self.set_track_input(track, None);
        }
        let feeding: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.output == Bus::Group(id))
            .map(|t| t.id)
            .collect();
        for track in feeding {
            // A track feeding the group reaches the master through it, so
            // feeding the master closes no loop.
            let _ = self.set_track_output(track, Bus::Master);
        }
        let sends: Vec<(TrackId, SendId)> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.sends.iter().filter(|s| s.to == id).map(|s| (t.id, s.id)))
            .collect();
        for (track, send) in sends {
            self.remove_send(track, send);
        }
        let on_track = Some(Chain::Track(id));
        // Its own routings go with it.
        self.remove_routings(|project, chain, routing| {
            Some(chain) != on_track && project.param_chain(routing.to) == on_track
        });
        let project = &*self.project;
        let lanes: Vec<ParamAddress> = (project.automation.iter())
            .map(|l| l.param)
            .filter(|&param| project.param_chain(param) == on_track)
            .collect();
        for param in lanes {
            self.remove_lane(param);
        }
        let index = self.project.track_index(id)?;
        let track = self.project.tracks.remove(index);
        self.edits.push(Edit::RemoveTrack {
            index,
            track: track.clone(),
        });
        Some(track)
    }

    pub fn set_track_name(&mut self, track: TrackId, name: &str) {
        if let Some(t) = self.project.track_mut(track)
            && t.name != name
        {
            let from = std::mem::replace(&mut t.name, name.to_owned());
            let to = name.to_owned();
            self.edits.push(Edit::TrackName { track, from, to });
        }
    }

    /// Sets the track's fader. Linear, 1.0 is unity. A gain that isn't a
    /// finite number does nothing, since a project file can't hold it.
    pub fn set_track_gain(&mut self, track: TrackId, gain: f32) {
        if !gain.is_finite() {
            return;
        }
        if let Some(from) = self.replace(track, |t| &mut t.gain, gain) {
            self.edits.push(Edit::TrackGain {
                track,
                from,
                to: gain,
            });
        }
    }

    /// Sets the track's balance, from -1.0 (left) to 1.0 (right). Values
    /// beyond move to the nearest end, and NaN does nothing.
    pub fn set_track_pan(&mut self, track: TrackId, pan: f32) {
        if pan.is_nan() {
            return;
        }
        let pan = pan.clamp(-1.0, 1.0);
        if let Some(from) = self.replace(track, |t| &mut t.pan, pan) {
            self.edits.push(Edit::TrackPan {
                track,
                from,
                to: pan,
            });
        }
    }

    pub fn set_track_muted(&mut self, track: TrackId, muted: bool) {
        if let Some(from) = self.replace(track, |t| &mut t.muted, muted) {
            self.edits.push(Edit::TrackMuted {
                track,
                from,
                to: muted,
            });
        }
    }

    pub fn set_track_solo(&mut self, track: TrackId, solo: bool) {
        if let Some(from) = self.replace(track, |t| &mut t.solo, solo) {
            self.edits.push(Edit::TrackSolo {
                track,
                from,
                to: solo,
            });
        }
    }

    pub fn set_track_solo_safe(&mut self, track: TrackId, solo_safe: bool) {
        if let Some(from) = self.replace(track, |t| &mut t.solo_safe, solo_safe) {
            self.edits.push(Edit::TrackSoloSafe {
                track,
                from,
                to: solo_safe,
            });
        }
    }

    /// Arms the track for recording, or disarms it. Only audio and MIDI
    /// tracks record, so arming any other track does nothing.
    pub fn set_track_armed(&mut self, track: TrackId, armed: bool) {
        let recordable = self.project.track(track).is_some_and(|t| !t.kind.is_bus());
        if !recordable && armed {
            return;
        }
        if let Some(from) = self.replace(track, |t| &mut t.armed, armed) {
            self.edits.push(Edit::TrackArmed {
                track,
                from,
                to: armed,
            });
        }
    }

    /// Sets what the track records from. An input of the other kind from
    /// the track, any input on a bus, one a file couldn't hold (a stereo
    /// audio input from the last channel there can be, a MIDI channel past
    /// 15) and a tap the track can't take (see [`Project::check_input`]) do
    /// nothing. A tap that would close a loop is refused.
    pub fn set_track_input(&mut self, track: TrackId, input: Option<Input>) -> Result<(), Loop> {
        let Some(kind) = self.project.track(track).map(|t| t.kind) else {
            return Ok(());
        };
        let fits = match &input {
            None => true,
            Some(Input::Audio(audio)) => {
                kind == TrackKind::Audio && !(audio.stereo && audio.channel == u16::MAX)
            }
            Some(Input::Midi(midi)) => {
                kind == TrackKind::Midi && midi.channel.is_none_or(|channel| channel <= 15)
            }
            Some(Input::Tap(tap)) => match self.project.check_input(track, *tap) {
                Ok(()) => true,
                Err(InputError::Loop) if self.checks_loops => {
                    return Err(Loop(Route::Input(track)));
                }
                Err(InputError::Loop) => true,
                Err(_) => false,
            },
        };
        if !fits {
            return Ok(());
        }
        if let Some(from) = self.replace(track, |t| &mut t.input, input.clone()) {
            self.edits.push(Edit::TrackInput {
                track,
                from,
                to: input,
            });
        }
        Ok(())
    }

    pub fn set_track_monitor(&mut self, track: TrackId, monitor: Monitor) {
        if let Some(from) = self.replace(track, |t| &mut t.monitor, monitor) {
            self.edits.push(Edit::TrackMonitor {
                track,
                from,
                to: monitor,
            });
        }
    }

    /// Has the track render live while the rest of the project renders
    /// ahead, or stop.
    pub fn set_track_always_live(&mut self, track: TrackId, always_live: bool) {
        if let Some(from) = self.replace(track, |t| &mut t.always_live, always_live) {
            self.edits.push(Edit::TrackAlwaysLive {
                track,
                from,
                to: always_live,
            });
        }
    }

    /// Feeds the track's output to `output`, if the track can feed it: see
    /// [`Project::can_feed`]. Otherwise it does nothing. An output that
    /// would close a loop through an input or a wire is refused.
    pub fn set_track_output(&mut self, track: TrackId, output: Bus) -> Result<(), Loop> {
        if !self.project.can_feed(track, output) {
            return Ok(());
        }
        let Some(from) = self.replace(track, |t| &mut t.output, output) else {
            return Ok(());
        };
        if let Some(route) = self.closed_loop() {
            self.replace(track, |t| &mut t.output, from);
            return Err(Loop(route));
        }
        self.edits.push(Edit::TrackOutput {
            track,
            from,
            to: output,
        });
        Ok(())
    }

    pub fn set_track_clip_type(&mut self, track: TrackId, clip_type: ClipType) {
        if let Some(from) = self.replace(track, |t| &mut t.clip_type, clip_type) {
            self.edits.push(Edit::TrackClipType {
                track,
                from,
                to: clip_type,
            });
        }
    }

    /// Puts `send` at `index` in the track's sends, or at the end if `index`
    /// is past them, if the track can send to the send's return track: see
    /// [`Project::can_send`]. Otherwise it does nothing. A send that would
    /// close a loop through an input or a wire is refused.
    pub fn add_send(&mut self, track: TrackId, index: usize, send: Send) -> Result<(), Loop> {
        if !self.project.can_send(track, send.to) {
            return Ok(());
        }
        let Some(t) = self.project.track_mut(track) else {
            return Ok(());
        };
        let index = index.min(t.sends.len());
        t.sends.insert(index, send);
        if let Some(route) = self.closed_loop() {
            let t = self
                .project
                .track_mut(track)
                .expect("it just took the send");
            t.sends.remove(index);
            return Err(Loop(route));
        }
        self.edits.push(Edit::AddSend { track, index, send });
        Ok(())
    }

    /// Removes the track's send with id `send`. The routings to its level
    /// and the lane that automates it go first, each as an edit of its own.
    pub fn remove_send(&mut self, track: TrackId, send: SendId) {
        let has = |t: &Track| t.sends.iter().any(|s| s.id == send);
        if !self.project.track(track).is_some_and(has) {
            return;
        }
        let level = ParamAddress::Send(send);
        self.remove_routings(|_, _, routing| routing.to == level);
        self.remove_lane(level);
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        if let Some(index) = t.sends.iter().position(|s| s.id == send) {
            let send = t.sends.remove(index);
            self.edits.push(Edit::RemoveSend { track, index, send });
        }
    }

    /// Sets a send's level. Linear, 1.0 is unity. A level that isn't a
    /// finite number does nothing.
    pub fn set_send_level(&mut self, track: TrackId, send: SendId, level: f32) {
        if !level.is_finite() {
            return;
        }
        if let Some(from) = self.replace_send(track, send, |s| &mut s.level, level) {
            self.edits.push(Edit::SendLevel {
                track,
                send,
                from,
                to: level,
            });
        }
    }

    pub fn set_send_pre_fader(&mut self, track: TrackId, send: SendId, pre_fader: bool) {
        if let Some(from) = self.replace_send(track, send, |s| &mut s.pre_fader, pre_fader) {
            self.edits.push(Edit::SendPreFader {
                track,
                send,
                from,
                to: pre_fader,
            });
        }
    }

    /// Sets the master's fader. Linear, 1.0 is unity. A gain that isn't a
    /// finite number does nothing.
    pub fn set_master_gain(&mut self, gain: f32) {
        if !gain.is_finite() {
            return;
        }
        let from = std::mem::replace(&mut self.project.master.gain, gain);
        if from != gain {
            self.edits.push(Edit::MasterGain { from, to: gain });
        }
    }

    /// Sets the master's balance, as for a track.
    pub fn set_master_pan(&mut self, pan: f32) {
        if pan.is_nan() {
            return;
        }
        let pan = pan.clamp(-1.0, 1.0);
        let from = std::mem::replace(&mut self.project.master.pan, pan);
        if from != pan {
            self.edits.push(Edit::MasterPan { from, to: pan });
        }
    }

    pub fn set_master_clip_type(&mut self, clip_type: ClipType) {
        let from = std::mem::replace(&mut self.project.master.clip_type, clip_type);
        if from != clip_type {
            self.edits.push(Edit::MasterClipType {
                from,
                to: clip_type,
            });
        }
    }

    /// A new clip with the next clip id, for [`add_clip`](Self::add_clip):
    /// see [`Project::new_clip`].
    pub fn new_clip(
        &mut self,
        name: impl Into<String>,
        range: BeatRange,
        content: ClipContent,
    ) -> Clip {
        self.project.new_clip(name, range, content)
    }

    /// The next clip id, for a copy of a clip.
    pub fn new_clip_id(&mut self) -> ClipId {
        self.project.new_clip_id()
    }

    /// Puts `clip` at `index` in the clips of the track with id `track`, or
    /// at the end if `index` is past them. It's left out if the track can't
    /// hold it, if the project has a clip with its id, or if a file couldn't
    /// hold it: it must start at the song's start or after, last more than
    /// zero, and have room for its fades.
    pub fn add_clip(&mut self, track: TrackId, index: usize, clip: Clip) {
        let fits = clip.start >= Beats::ZERO
            && clip.length > Beats::ZERO
            && fades_of(&clip).is_none_or(|fades| fades.total() <= clip.length);
        if !fits || self.project.uses_clip_id(clip.id) {
            return;
        }
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        if !t.holds(&clip.content) {
            return;
        }
        let index = index.min(t.clips.len());
        t.clips.insert(index, clip.clone());
        self.edits.push(Edit::AddClip { track, index, clip });
    }

    /// Removes the clip with id `clip` from its track, and returns it.
    pub fn remove_clip(&mut self, clip: ClipId) -> Option<Clip> {
        let track = self.project.find_clip(clip)?.0.id;
        let t = self.project.track_mut(track)?;
        let index = t.clip_index(clip)?;
        let clip = t.clips.remove(index);
        self.edits.push(Edit::RemoveClip {
            track,
            index,
            clip: clip.clone(),
        });
        Some(clip)
    }

    /// Moves or trims the clip with id `clip` to `timing`. An audio clip's
    /// fades shorten first, each as an edit of its own, if they wouldn't fit
    /// the new length. A timing a file couldn't hold, starting before the
    /// song or without length, or with the other kind of offset from the
    /// clip's content, does nothing.
    pub fn set_clip_timing(&mut self, clip: ClipId, timing: Timing) {
        let Some((track, current)) = self.project.find_clip(clip) else {
            return;
        };
        let track = track.id;
        if timing.start < Beats::ZERO
            || timing.length <= Beats::ZERO
            || std::mem::discriminant(&timing.offset)
                != std::mem::discriminant(&current.content.offset())
        {
            return;
        }
        if let Some(fades) = fades_of(current)
            && fades.total() > timing.length
        {
            let fade_in = fades.fade_in.min(timing.length);
            let fade_out = fades.fade_out.min(timing.length - fade_in);
            self.set_clip_fades(clip, Fades { fade_in, fade_out });
        }
        let Some(c) = self.clip_mut(track, clip) else {
            return;
        };
        let from = c.timing();
        if from != timing && c.set_timing(timing) {
            self.edits.push(Edit::ClipTiming {
                track,
                clip,
                from,
                to: timing,
            });
        }
    }

    /// Sets an audio clip's fades. Each is at least zero, the fade in at
    /// most the clip's length, and the fade out at most what that leaves.
    /// MIDI clips have no fades, so it does nothing to them.
    pub fn set_clip_fades(&mut self, clip: ClipId, fades: Fades) {
        let Some((track, current)) = self.project.find_clip(clip) else {
            return;
        };
        let track = track.id;
        let length = current.length;
        let fade_in = fades.fade_in.clamp(Beats::ZERO, length);
        let fade_out = fades.fade_out.clamp(Beats::ZERO, length - fade_in);
        let to = Fades { fade_in, fade_out };
        let Some(Clip {
            content: ClipContent::Audio(audio),
            ..
        }) = self.clip_mut(track, clip)
        else {
            return;
        };
        let from = audio.fades();
        if from != to {
            audio.fade_in = to.fade_in;
            audio.fade_out = to.fade_out;
            self.edits.push(Edit::ClipFades {
                track,
                clip,
                from,
                to,
            });
        }
    }

    pub fn set_clip_name(&mut self, clip: ClipId, name: &str) {
        let Some(track) = self.project.find_clip(clip).map(|(t, _)| t.id) else {
            return;
        };
        if let Some(c) = self.clip_mut(track, clip)
            && c.name != name
        {
            let from = std::mem::replace(&mut c.name, name.to_owned());
            let to = name.to_owned();
            self.edits.push(Edit::ClipName {
                track,
                clip,
                from,
                to,
            });
        }
    }

    /// Sets whether an audio clip follows the tempo, and how: its offset
    /// with its warp. Warping that doesn't keep its rules does nothing (see
    /// [`AudioClip::set_warping`]), and so does a MIDI clip. Its timing and
    /// the curves in its own time stay as they are: change them in the same
    /// transaction, so the clip plays what it should.
    pub fn set_clip_warping(&mut self, clip: ClipId, warping: Warping) {
        let Some((track, audio)) = audio_mut(self.project, clip) else {
            return;
        };
        let from = audio.warping();
        if from != warping && audio.set_warping(warping.clone()) {
            self.edits.push(Edit::ClipWarping {
                track,
                clip,
                from,
                to: warping,
            });
        }
    }

    /// Sets an audio clip's gain, as linear gain, from silence to +24 dB:
    /// what it plays at where its curve doesn't say otherwise. Values that
    /// aren't finite do nothing, and so does a MIDI clip.
    pub fn set_clip_gain(&mut self, clip: ClipId, gain: f32) {
        if !gain.is_finite() {
            return;
        }
        let gain = gain.clamp(0.0, ParamSpec::CLIP_GAIN_MAX as f32);
        let Some((track, audio)) = audio_mut(self.project, clip) else {
            return;
        };
        if audio.gain != gain {
            let from = std::mem::replace(&mut audio.gain, gain);
            self.edits.push(Edit::ClipGain {
                track,
                clip,
                from,
                to: gain,
            });
        }
    }

    /// An id for a new note in the MIDI clip with id `clip`, for
    /// [`add_note`](Self::add_note), or `None` if there's no MIDI clip on a
    /// track with that id. Taking an id isn't an edit, since ids are never
    /// used again in the clip, as with tracks.
    pub fn new_note_id(&mut self, clip: ClipId) -> Option<NoteId> {
        let (_, midi) = midi_mut(self.project, clip)?;
        Some(midi.new_note_id())
    }

    /// Puts `note` at `index` in the notes of the MIDI clip with id `clip`,
    /// or at the end if `index` is past them. It's left out if there's no
    /// MIDI clip on a track with that id, if the clip has a note with the
    /// note's id, or if a file couldn't hold the note: see [`note_fits`].
    ///
    /// The note's id is one the clip handed out (see
    /// [`new_note_id`](Self::new_note_id)), or else the clip takes it then,
    /// and hands out the ids after it from then on. Redo needs that: it
    /// makes a clip again as it was made, with none of the ids handed out
    /// since, then the notes added to it.
    pub fn add_note(&mut self, clip: ClipId, index: usize, note: Note) {
        if !note_fits(&note) {
            return;
        }
        let Some((track, midi)) = midi_mut(self.project, clip) else {
            return;
        };
        let Some(after) = note.id.0.checked_add(1) else {
            return;
        };
        if midi.note(note.id).is_some() {
            return;
        }
        midi.next_note_id = midi.next_note_id.max(after);
        let index = index.min(midi.notes.len());
        midi.notes.insert(index, note.clone());
        self.edits.push(Edit::AddNote {
            track,
            clip,
            index,
            note,
        });
    }

    /// Removes the note with id `note` from the MIDI clip with id `clip`,
    /// and returns it.
    pub fn remove_note(&mut self, clip: ClipId, note: NoteId) -> Option<Note> {
        let (track, midi) = midi_mut(self.project, clip)?;
        let index = midi.notes.iter().position(|n| n.id == note)?;
        let note = midi.notes.remove(index);
        self.edits.push(Edit::RemoveNote {
            track,
            clip,
            index,
            note: note.clone(),
        });
        Some(note)
    }

    /// Moves a note, or changes its length: it sounds over `range`, in its
    /// clip's own time, from now on. An empty range does nothing.
    pub fn set_note_timing(&mut self, clip: ClipId, note: NoteId, range: BeatRange) {
        if range.is_empty() {
            return;
        }
        let Some((track, n)) = note_mut(self.project, clip, note) else {
            return;
        };
        let from = n.range();
        if from != range {
            n.start = range.start;
            n.length = range.len();
            self.edits.push(Edit::NoteTiming {
                track,
                clip,
                note,
                from,
                to: range,
            });
        }
    }

    /// Sets a note's key, its MIDI note number. A key past
    /// [`Note::MAX_KEY`] does nothing.
    pub fn set_note_key(&mut self, clip: ClipId, note: NoteId, key: u8) {
        if key > Note::MAX_KEY {
            return;
        }
        let Some((track, n)) = note_mut(self.project, clip, note) else {
            return;
        };
        if n.key != key {
            let from = std::mem::replace(&mut n.key, key);
            self.edits.push(Edit::NoteKey {
                track,
                clip,
                note,
                from,
                to: key,
            });
        }
    }

    /// Sets how hard a note starts, from 0.0 to 1.0. Velocities beyond move
    /// to the nearest end, and NaN does nothing.
    pub fn set_note_velocity(&mut self, clip: ClipId, note: NoteId, velocity: f32) {
        if velocity.is_nan() {
            return;
        }
        let velocity = velocity.clamp(0.0, 1.0);
        let Some((track, n)) = note_mut(self.project, clip, note) else {
            return;
        };
        if n.velocity != velocity {
            let from = std::mem::replace(&mut n.velocity, velocity);
            self.edits.push(Edit::NoteVelocity {
                track,
                clip,
                note,
                from,
                to: velocity,
            });
        }
    }

    /// Sets how the MIDI clip with id `clip` moves `controller`: by
    /// `points`, in the clip's own time, in order, with values from 0.0 to
    /// 1.0. No points stops the clip moving it. Points out of order or with
    /// values beyond do nothing.
    pub fn set_controller(&mut self, clip: ClipId, controller: Controller, points: Vec<Point>) {
        if !curve_fits(&points) {
            return;
        }
        let Some((track, midi)) = midi_mut(self.project, clip) else {
            return;
        };
        let from = midi.controller(controller).to_vec();
        if from != points {
            midi.set_controller(controller, points.clone());
            self.edits.push(Edit::Controller {
                track,
                clip,
                controller,
                from,
                to: points,
            });
        }
    }

    /// A new, empty take with the next take id, for
    /// [`add_take`](Self::add_take): see [`Project::new_take`].
    pub fn new_take(&mut self, name: impl Into<String>) -> Take {
        self.project.new_take(name)
    }

    /// Puts `take` at `index` in the takes of the track with id `track`, or
    /// at the end if `index` is past them. It's left out if the track is a
    /// bus, if the project has a take with its id, or if one of its clips
    /// couldn't go on the track (see [`add_clip`](Self::add_clip)).
    pub fn add_take(&mut self, track: TrackId, index: usize, take: Take) {
        if self.project.find_take(take.id).is_some() {
            return;
        }
        let Some(t) = self.project.track(track) else {
            return;
        };
        let mut ids = std::collections::BTreeSet::new();
        let fits = !t.kind.is_bus()
            && take.clips.iter().all(|clip| {
                t.holds(&clip.content)
                    && clip.start >= Beats::ZERO
                    && clip.length > Beats::ZERO
                    && fades_of(clip).is_none_or(|fades| fades.total() <= clip.length)
                    && !self.project.uses_clip_id(clip.id)
                    && ids.insert(clip.id)
            });
        if !fits {
            return;
        }
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        let index = index.min(t.takes.len());
        t.takes.insert(index, take.clone());
        self.edits.push(Edit::AddTake { track, index, take });
    }

    /// Removes the take with id `take` from its track, and returns it.
    pub fn remove_take(&mut self, take: TakeId) -> Option<Take> {
        let track = self.project.find_take(take)?.0.id;
        let t = self.project.track_mut(track)?;
        let index = t.take_index(take)?;
        let take = t.takes.remove(index);
        self.edits.push(Edit::RemoveTake {
            track,
            index,
            take: take.clone(),
        });
        Some(take)
    }

    pub fn set_take_name(&mut self, take: TakeId, name: &str) {
        let Some(track) = self.project.find_take(take).map(|(t, _)| t.id) else {
            return;
        };
        if let Some(t) = self.project.track_mut(track).and_then(|t| t.take_mut(take))
            && t.name != name
        {
            let from = std::mem::replace(&mut t.name, name.to_owned());
            let to = name.to_owned();
            self.edits.push(Edit::TakeName {
                track,
                take,
                from,
                to,
            });
        }
    }

    /// A new locator with the next locator id, for
    /// [`add_locator`](Self::add_locator): see [`Project::new_locator`].
    pub fn new_locator(&mut self, name: impl Into<String>, at: Beats) -> Locator {
        self.project.new_locator(name, at)
    }

    /// Puts `locator` at `index` in the project's locators, or at the end if
    /// `index` is past them. A locator with the id of one the project has is
    /// left out.
    pub fn add_locator(&mut self, index: usize, locator: Locator) {
        if self.project.locator(locator.id).is_some() {
            return;
        }
        let index = index.min(self.project.locators.len());
        self.project.locators.insert(index, locator.clone());
        self.edits.push(Edit::AddLocator { index, locator });
    }

    pub fn remove_locator(&mut self, locator: LocatorId) -> Option<Locator> {
        let index = self.project.locator_index(locator)?;
        let locator = self.project.locators.remove(index);
        self.edits.push(Edit::RemoveLocator {
            index,
            locator: locator.clone(),
        });
        Some(locator)
    }

    pub fn set_locator_at(&mut self, locator: LocatorId, at: Beats) {
        if let Some(l) = self.project.locator_mut(locator)
            && l.at != at
        {
            let from = std::mem::replace(&mut l.at, at);
            self.edits.push(Edit::LocatorAt {
                locator,
                from,
                to: at,
            });
        }
    }

    pub fn set_locator_name(&mut self, locator: LocatorId, name: &str) {
        if let Some(l) = self.project.locator_mut(locator)
            && l.name != name
        {
            let from = std::mem::replace(&mut l.name, name.to_owned());
            let to = name.to_owned();
            self.edits.push(Edit::LocatorName { locator, from, to });
        }
    }

    /// A new wire from `from` to `to` at unity gain, with the next wire id,
    /// for [`add_wire`](Self::add_wire): see [`Project::new_wire`].
    pub fn new_wire(&mut self, from: Tap, to: Port) -> Wire {
        self.project.new_wire(from, to)
    }

    /// Puts `wire` at `index` in the project's wires, or at the end if
    /// `index` is past them, with its gain kept to its range, if it can join
    /// the project: see [`Project::check_wire`]. A note wire's gain is 1.0,
    /// and its channel one there is, or its notes' own, and an audio wire
    /// has none. A wire with the id of one the project has is refused as
    /// wired already.
    pub fn add_wire(&mut self, index: usize, mut wire: Wire) -> Result<(), WireError> {
        if self.project.wire(wire.id).is_some() {
            return Err(WireError::Wired(wire.id));
        }
        self.project.check_wire(wire.from, wire.to)?;
        (wire.gain, wire.channel) = match wire.takes_notes() {
            true => (1.0, wire.channel.filter(|&c| c < Wire::CHANNELS)),
            false => (wire_gain(wire.gain).unwrap_or(1.0), None),
        };
        let index = index.min(self.project.wires.len());
        self.project.wires.insert(index, wire);
        self.edits.push(Edit::AddWire { index, wire });
        Ok(())
    }

    /// Removes the wire with id `wire`, and returns it. The routings to its
    /// gain and the lane that automates it go first, each as an edit of its
    /// own.
    pub fn remove_wire(&mut self, wire: WireId) -> Option<Wire> {
        self.project.wire(wire)?;
        let gain = ParamAddress::Wire(wire);
        self.remove_routings(|_, _, routing| routing.to == gain);
        self.remove_lane(gain);
        let index = self.project.wire_index(wire)?;
        let wire = self.project.wires.remove(index);
        self.edits.push(Edit::RemoveWire { index, wire });
        Some(wire)
    }

    /// Has the wire with id `wire` take its signal from `from` instead,
    /// keeping its gain, the lane on it and its place, if it can: see
    /// [`Project::check_wire`].
    pub fn set_wire_from(&mut self, wire: WireId, from: Tap) -> Result<(), WireError> {
        let Some(&Wire { from: was, to, .. }) = self.project.wire(wire) else {
            return Ok(());
        };
        if was == from {
            return Ok(());
        }
        self.project.check_wire(from, to)?;
        if let Some(w) = self.project.wire_mut(wire) {
            w.from = from;
        }
        self.edits.push(Edit::WireFrom {
            wire,
            from: was,
            to: from,
        });
        Ok(())
    }

    /// Sets a wire's gain. Linear, 1.0 is unity, kept from silence to
    /// [`Wire::GAIN_MAX`]. A gain that isn't a finite number does nothing,
    /// and so does a note wire's, which has none.
    pub fn set_wire_gain(&mut self, wire: WireId, gain: f32) {
        let Some(gain) = wire_gain(gain) else {
            return;
        };
        if let Some(w) = self.project.wire_mut(wire)
            && !w.takes_notes()
            && w.gain != gain
        {
            let from = std::mem::replace(&mut w.gain, gain);
            self.edits.push(Edit::WireGain {
                wire,
                from,
                to: gain,
            });
        }
    }

    /// Sets the MIDI channel a note wire plays its notes on, from 0 to 15,
    /// or with `None`, has it pass each note on as it comes. A channel past
    /// 15 does nothing, and so does an audio wire's, which has none.
    pub fn set_wire_channel(&mut self, wire: WireId, channel: Option<u8>) {
        if channel.is_some_and(|c| c >= Wire::CHANNELS) {
            return;
        }
        if let Some(w) = self.project.wire_mut(wire)
            && w.takes_notes()
            && w.channel != channel
        {
            let from = std::mem::replace(&mut w.channel, channel);
            self.edits.push(Edit::WireChannel {
                wire,
                from,
                to: channel,
            });
        }
    }

    /// Puts `lane` at `index` in the project's lanes, or at the end if
    /// `index` is past them, with its points tidied (see
    /// [`automation::tidy`]). A lane left without points is left out, and
    /// so is one for a parameter that has a lane already, or whose channel,
    /// send or device the project hasn't.
    pub fn add_lane(&mut self, index: usize, mut lane: Lane) {
        automation::tidy(&mut lane.points);
        let project = &*self.project;
        if lane.points.is_empty()
            || project.lane(lane.param).is_some()
            || !project.has_param(lane.param)
        {
            return;
        }
        let index = index.min(self.project.automation.len());
        self.project.automation.insert(index, lane.clone());
        self.edits.push(Edit::AddLane { index, lane });
    }

    /// Removes the lane that automates `param`, and returns it. The
    /// parameter keeps its own value from then on. A clip's parameter has
    /// its curve in the clip, which goes, and nothing is returned.
    pub fn remove_lane(&mut self, param: ParamAddress) -> Option<Lane> {
        if let ParamAddress::Clip(..) = param {
            self.set_lane_points(param, Vec::new());
            return None;
        }
        let index = self.project.lane_index(param)?;
        let lane = self.project.automation.remove(index);
        self.edits.push(Edit::RemoveLane {
            index,
            lane: lane.clone(),
        });
        Some(lane)
    }

    /// Sets the points of the lane that automates `param`, tidied (see
    /// [`automation::tidy`]). A parameter without a lane gets one, if the
    /// project has the parameter, and a lane left without points goes.
    ///
    /// A clip's parameter has its curve in the clip, in the clip's own
    /// time, which can start before zero: its points are tidied with
    /// [`automation::tidy_anywhere`] instead, kept in the parameter's
    /// range, and rounded to whole steps for a stepped one.
    pub fn set_lane_points(&mut self, param: ParamAddress, mut points: Vec<automation::Point>) {
        if let ParamAddress::Clip(clip, clip_param) = param {
            self.set_clip_lane(clip, clip_param, points);
            return;
        }
        automation::tidy(&mut points);
        let Some(lane) = self.project.lane_mut(param) else {
            self.add_lane(usize::MAX, Lane::new(param, points));
            return;
        };
        if points.is_empty() {
            self.remove_lane(param);
        } else if lane.points != points {
            let from = std::mem::replace(&mut lane.points, points.clone());
            self.edits.push(Edit::LanePoints {
                param,
                from,
                to: points,
            });
        }
    }

    /// Sets the curve of the audio clip `clip`'s parameter `param`: see
    /// [`set_lane_points`](Self::set_lane_points).
    fn set_clip_lane(
        &mut self,
        clip: ClipId,
        param: ClipParam,
        mut points: Vec<automation::Point>,
    ) {
        automation::tidy_anywhere(&mut points);
        let spec = param.spec();
        for point in &mut points {
            point.value = spec.clamp(point.value);
            if param.stepped() {
                point.value = point.value.round();
            }
        }
        let Some((_, audio)) = audio_mut(self.project, clip) else {
            return;
        };
        let from = audio
            .lane(param)
            .map(|l| l.points.clone())
            .unwrap_or_default();
        if from != points {
            audio.set_lane(param, points.clone());
            self.edits.push(Edit::LanePoints {
                param: ParamAddress::Clip(clip, param),
                from,
                to: points,
            });
        }
    }

    /// Overrides the automation of `param`, so the parameter keeps its own
    /// value, or re-enables it with `false`. What moving an automated
    /// parameter by hand does. A parameter without a lane is left alone.
    pub fn set_lane_overridden(&mut self, param: ParamAddress, overridden: bool) {
        if let Some(lane) = self.project.lane_mut(param)
            && lane.overridden != overridden
        {
            lane.overridden = overridden;
            self.edits.push(Edit::LaneOverridden {
                param,
                from: !overridden,
                to: overridden,
            });
        }
    }

    /// A new modulator of `kind` called `name`, with the next modulator id,
    /// for [`add_modulator`](Self::add_modulator): see
    /// [`Project::new_modulator`]. As with tracks, taking an id isn't an
    /// edit.
    pub fn new_modulator(&mut self, kind: ModulatorKind, name: impl Into<String>) -> Modulator {
        self.project.new_modulator(kind, name)
    }

    /// Puts `modulator` at `index` in the modulator strip of `chain`, or at
    /// the end if `index` is past it. It's left out if the chain's track
    /// isn't there, if the project has a modulator with its id, or if a
    /// file couldn't hold it ([`Modulator::is_valid`]).
    pub fn add_modulator(&mut self, chain: Chain, index: usize, modulator: Modulator) {
        if !modulator.is_valid() || self.project.find_modulator(modulator.id).is_some() {
            return;
        }
        let Some(modulation) = self.project.modulation_mut(chain) else {
            return;
        };
        let index = index.min(modulation.modulators.len());
        modulation.modulators.insert(index, modulator.clone());
        self.edits.push(Edit::AddModulator {
            chain,
            index,
            modulator,
        });
    }

    /// Removes the modulator with id `id` from its strip, and returns it.
    /// What refers to it goes first, each as an edit of its own: the
    /// routings from its outputs and to its parameters, wherever they are,
    /// and the lanes that automate its parameters. Routings it scales lose
    /// their scale.
    pub fn remove_modulator(&mut self, id: ModulatorId) -> Option<Modulator> {
        self.project.find_modulator(id)?;
        self.remove_routings(|_, _, routing| {
            routing.from.modulator == id || routing.to.modulator() == Some(id)
        });
        self.unscale(|output| output.modulator == id);
        self.remove_lanes(|param| param.modulator() == Some(id));
        let (chain, index, _) = self.project.find_modulator(id)?;
        let modulator = self.project.modulation_mut(chain)?.modulators.remove(index);
        self.edits.push(Edit::RemoveModulator {
            chain,
            index,
            modulator: modulator.clone(),
        });
        Some(modulator)
    }

    /// Moves the modulator with id `id` to `index` in its strip, counting
    /// without it, or to the end if `index` is past it. A modulator the
    /// project hasn't got is left alone.
    pub fn move_modulator(&mut self, id: ModulatorId, index: usize) {
        let Some((chain, from, _)) = self.project.find_modulator(id) else {
            return;
        };
        let Some(modulation) = self.project.modulation_mut(chain) else {
            return;
        };
        let modulator = modulation.modulators.remove(from);
        let to = index.min(modulation.modulators.len());
        modulation.modulators.insert(to, modulator);
        if from != to {
            self.edits.push(Edit::MoveModulator {
                modulator: id,
                from,
                to,
            });
        }
    }

    /// Renames the modulator with id `id`. An empty name does nothing,
    /// since a file can't hold it.
    pub fn set_modulator_name(&mut self, id: ModulatorId, name: &str) {
        if name.is_empty() {
            return;
        }
        if let Some(m) = self.project.modulator_mut(id)
            && m.name != name
        {
            let from = std::mem::replace(&mut m.name, name.to_owned());
            self.edits.push(Edit::ModulatorName {
                modulator: id,
                from,
                to: name.to_owned(),
            });
        }
    }

    /// Sets parameter `n` of the modulator with id `id` to `value`, kept in
    /// its range and to its steps ([`Modulator::set`]). Not a number does
    /// nothing, and so do a modulator the project hasn't and a parameter
    /// it hasn't got.
    pub fn set_modulator_param(&mut self, id: ModulatorId, n: u32, value: f64) {
        if value.is_nan() {
            return;
        }
        let Some(modulator) = self.project.modulator_mut(id) else {
            return;
        };
        let Some(from) = modulator.set(n, value) else {
            return;
        };
        let to = modulator
            .value(n)
            .expect("the modulator has the parameter it set");
        if from != to {
            self.edits.push(Edit::ModulatorParam {
                modulator: id,
                param: n,
                from,
                to,
            });
        }
    }

    /// A new control of `kind` for the Macro with id `modulator`, for
    /// [`add_control`](Self::add_control), or `None` if the project hasn't
    /// a Macro with that id: see [`Modulator::new_control`]. Taking an id
    /// isn't an edit, as with notes.
    pub fn new_control(&mut self, modulator: ModulatorId, kind: ControlKind) -> Option<Control> {
        let modulator = self.macro_mut(modulator)?;
        Some(modulator.new_control(kind))
    }

    /// Puts `control` at `index` in the controls of the Macro with id
    /// `modulator`, or at the end if `index` is past them. It's left out if
    /// there's no Macro with that id, if it has a control with the
    /// control's id, or if a file couldn't hold the control: it needs a
    /// name, and a value its kind takes.
    ///
    /// The control's id is one the Macro handed out (see
    /// [`new_control`](Self::new_control)), or else the Macro takes it
    /// then, as a clip takes a note's ([`add_note`](Self::add_note)).
    pub fn add_control(&mut self, modulator: ModulatorId, index: usize, control: Control) {
        if control.name.is_empty() || !control.kind.param().is_valid(control.value) {
            return;
        }
        let Some(after) = control.id.checked_add(1) else {
            return;
        };
        let Some(m) = self.macro_mut(modulator) else {
            return;
        };
        if m.control(control.id).is_some() {
            return;
        }
        m.next_control = m.next_control.max(after);
        let index = index.min(m.controls.len());
        m.controls.insert(index, control.clone());
        self.edits.push(Edit::AddControl {
            modulator,
            index,
            control,
        });
    }

    /// Removes the control with id `control` from the Macro with id
    /// `modulator`, and returns it. What refers to it goes first, each as
    /// an edit of its own: the routings from its output and to its
    /// parameter, and the lane that automates it. Routings it scales lose
    /// their scale.
    pub fn remove_control(&mut self, modulator: ModulatorId, control: u32) -> Option<Control> {
        self.macro_mut(modulator)?.control(control)?;
        let output = Output::new(modulator, control);
        let param = ParamAddress::Modulator(modulator, control);
        self.remove_routings(|_, _, routing| routing.from == output || routing.to == param);
        self.unscale(|scale| scale == output);
        self.remove_lane(param);
        let m = self.macro_mut(modulator)?;
        let index = m.control_index(control)?;
        let control = m.controls.remove(index);
        self.edits.push(Edit::RemoveControl {
            modulator,
            index,
            control: control.clone(),
        });
        Some(control)
    }

    /// Renames the control with id `control` of the Macro with id
    /// `modulator`. An empty name does nothing.
    pub fn set_control_name(&mut self, modulator: ModulatorId, control: u32, name: &str) {
        if name.is_empty() {
            return;
        }
        if let Some(c) = self
            .macro_mut(modulator)
            .and_then(|m| m.control_mut(control))
            && c.name != name
        {
            let from = std::mem::replace(&mut c.name, name.to_owned());
            self.edits.push(Edit::ControlName {
                modulator,
                control,
                from,
                to: name.to_owned(),
            });
        }
    }

    /// The Macro with id `id`, if the project has one.
    fn macro_mut(&mut self, id: ModulatorId) -> Option<&mut Modulator> {
        let modulator = self.project.modulator_mut(id)?;
        (modulator.kind == ModulatorKind::Macro).then_some(modulator)
    }

    /// A new routing from `from` to `to` by `amount`, with the next routing
    /// id, for [`add_routing`](Self::add_routing): see
    /// [`Project::new_routing`].
    pub fn new_routing(&mut self, from: Output, to: ParamAddress, amount: f64) -> Routing {
        self.project.new_routing(from, to, amount)
    }

    /// Puts `routing` at `index` in the routings of `chain`, or at the end
    /// if `index` is past them. It's left out if the chain's track isn't
    /// there, if the project has a routing with its id, or if a file
    /// couldn't hold it: it must go from an output of one of the chain's
    /// modulators, scaled by another of theirs if by any, to a parameter
    /// modulators can move, by an amount that suits it
    /// ([`Project::amount_fits`]), with settings a file can hold
    /// ([`Routing::is_valid`]), and be the chain's only routing from its
    /// output to its parameter. Whether the parameter is in the chain's
    /// reach is the caller's to say: one out of reach waits, unplayed.
    pub fn add_routing(&mut self, chain: Chain, index: usize, routing: Routing) {
        let project = &*self.project;
        let Some(modulation) = project.modulation(chain) else {
            return;
        };
        let fits = routing.is_valid()
            && project.find_routing(routing.id).is_none()
            && project.can_modulate(routing.to)
            && project.amount_fits(routing.to, routing.amount)
            && modulation.has_output(routing.from)
            && routing
                .scale
                .is_none_or(|scale| modulation.has_output(scale))
            && modulation
                .routing_between(routing.from, routing.to)
                .is_none();
        if !fits {
            return;
        }
        let Some(modulation) = self.project.modulation_mut(chain) else {
            return;
        };
        let index = index.min(modulation.routings.len());
        modulation.routings.insert(index, routing);
        self.edits.push(Edit::AddRouting {
            chain,
            index,
            routing,
        });
    }

    /// Removes the routing with id `id` from its channel, and returns it.
    pub fn remove_routing(&mut self, id: RoutingId) -> Option<Routing> {
        let (chain, index, _) = self.project.find_routing(id)?;
        let routing = self.project.modulation_mut(chain)?.routings.remove(index);
        self.edits.push(Edit::RemoveRouting {
            chain,
            index,
            routing,
        });
        Some(routing)
    }

    /// Gives the routing with `routing`'s id `routing`'s settings: its
    /// amount, curve, scale, lag and bypass. Its output and its parameter
    /// are what it is, so a routing that differs in them does nothing, and
    /// so do settings a file couldn't hold, as with
    /// [`add_routing`](Self::add_routing).
    pub fn set_routing(&mut self, routing: Routing) {
        let project = &*self.project;
        let Some((chain, _, old)) = project.find_routing(routing.id) else {
            return;
        };
        let on_chain = |scale| {
            project
                .modulation(chain)
                .is_some_and(|m| m.has_output(scale))
        };
        let fits = (old.from, old.to) == (routing.from, routing.to)
            && routing.is_valid()
            && project.amount_fits(routing.to, routing.amount)
            && routing.scale.is_none_or(on_chain);
        if !fits || *old == routing {
            return;
        }
        let Some(settings) = self.project.routing_mut(routing.id) else {
            return;
        };
        let from = std::mem::replace(settings, routing);
        self.edits.push(Edit::RoutingSettings {
            routing: routing.id,
            from,
            to: routing,
        });
    }

    /// Removes the routings `which` picks, given the project and each
    /// routing's channel, each as an edit of its own.
    fn remove_routings(&mut self, which: impl Fn(&Project, Chain, &Routing) -> bool) {
        let project = &*self.project;
        let routings: Vec<RoutingId> = (project.modulations())
            .flat_map(|(chain, m)| m.routings.iter().map(move |r| (chain, r)))
            .filter(|&(chain, routing)| which(project, chain, routing))
            .map(|(_, routing)| routing.id)
            .collect();
        for routing in routings {
            self.remove_routing(routing);
        }
    }

    /// Takes the outputs `which` picks away from the routings they scale,
    /// each as an edit of its own.
    fn unscale(&mut self, which: impl Fn(Output) -> bool) {
        let unscaled: Vec<Routing> = (self.project.modulations())
            .flat_map(|(_, m)| &m.routings)
            .filter(|routing| routing.scale.is_some_and(&which))
            .map(|&routing| Routing {
                scale: None,
                ..routing
            })
            .collect();
        for routing in unscaled {
            self.set_routing(routing);
        }
    }

    /// Sets the own value of the parameter at `param`: the value it has
    /// where automation doesn't move it. Values that aren't finite do
    /// nothing, and so do parameters the project hasn't and the test
    /// devices'.
    pub fn set_param_value(&mut self, param: ParamAddress, value: f64) {
        if !value.is_finite() {
            return;
        }
        // The mixer keeps its values as `f32`.
        let narrow = value as f32;
        match param {
            ParamAddress::Volume(Chain::Track(track)) => self.set_track_gain(track, narrow),
            ParamAddress::Volume(Chain::Master) => self.set_master_gain(narrow),
            ParamAddress::Pan(Chain::Track(track)) => self.set_track_pan(track, narrow),
            ParamAddress::Pan(Chain::Master) => self.set_master_pan(narrow),
            ParamAddress::Send(send) => {
                if let Some(Chain::Track(track)) = self.project.param_chain(param) {
                    self.set_send_level(track, send, narrow);
                }
            }
            ParamAddress::Device(device, n) => {
                let device_kind = self.project.find_device(device).map(|(_, _, d)| &d.kind);
                if device_kind.is_some_and(|kind| kind.native().is_some()) {
                    self.set_native_param(device, n, value);
                } else {
                    self.set_plugin_param(device, n, Some(value));
                }
            }
            ParamAddress::Bypass(device) => self.set_device_bypass(device, value >= 0.5),
            ParamAddress::Wire(wire) => self.set_wire_gain(wire, narrow),
            ParamAddress::Clip(clip, ClipParam::Gain) => self.set_clip_gain(clip, narrow),
            ParamAddress::Clip(clip, param) => self.set_warp_param(clip, param, value),
            ParamAddress::Modulator(modulator, n) => self.set_modulator_param(modulator, n, value),
        }
    }

    /// Sets the setting of the warped audio clip `clip`'s warp that the
    /// clip parameter `param` is, kept in its range and to its steps (see
    /// [`Warp::set_param`](musicable_model::warp::Warp::set_param)). A clip
    /// that isn't warped is left alone.
    fn set_warp_param(&mut self, clip: ClipId, param: ClipParam, value: f64) {
        let Some((_, audio)) = audio_mut(self.project, clip) else {
            return;
        };
        let Some(warp) = &audio.warp else {
            return;
        };
        let mut warp = warp.clone();
        warp.set_param(param, value);
        let warping = Warping {
            offset: audio.offset,
            warp: Some(warp),
        };
        self.set_clip_warping(clip, warping);
    }

    /// Sets the parameter at `param` by hand, as a control does: automation
    /// it follows is overridden first, so the value holds until automation
    /// is re-enabled. Values that aren't finite do nothing.
    pub fn set_param_by_hand(&mut self, param: ParamAddress, value: f64) {
        if !value.is_finite() {
            return;
        }
        self.override_automation(param);
        self.set_param_value(param, value);
    }

    /// Overrides the automation of `param`, if it follows some: what
    /// touching an automated control does.
    pub fn override_automation(&mut self, param: ParamAddress) {
        if self.project.is_automated(param) {
            self.set_lane_overridden(param, true);
        }
    }

    /// Removes the lanes of the parameters `which` picks, each as an edit of
    /// its own.
    fn remove_lanes(&mut self, which: impl Fn(ParamAddress) -> bool) {
        let params: Vec<ParamAddress> = (self.project.automation.iter())
            .map(|l| l.param)
            .filter(|&param| which(param))
            .collect();
        for param in params {
            self.remove_lane(param);
        }
    }

    fn clip_mut(&mut self, track: TrackId, clip: ClipId) -> Option<&mut Clip> {
        self.project.track_mut(track)?.clip_mut(clip)
    }

    /// Sets the field `field` picks of the track with id `track` to `value`,
    /// and returns what it was if that's different.
    fn replace<T: PartialEq>(
        &mut self,
        track: TrackId,
        field: impl FnOnce(&mut Track) -> &mut T,
        value: T,
    ) -> Option<T> {
        let field = field(self.project.track_mut(track)?);
        (*field != value).then(|| std::mem::replace(field, value))
    }

    /// The same for a field of the track's send with id `send`.
    fn replace_send<T: PartialEq>(
        &mut self,
        track: TrackId,
        send: SendId,
        field: impl FnOnce(&mut Send) -> &mut T,
        value: T,
    ) -> Option<T> {
        let track = self.project.track_mut(track)?;
        let send = track.sends.iter_mut().find(|s| s.id == send)?;
        let field = field(send);
        (*field != value).then(|| std::mem::replace(field, value))
    }

    /// Makes the edits `edit` makes as one change, checking that they
    /// close no loop through an input or a wire once they're all made,
    /// rather than as each is made: what moving several devices takes, as one move can
    /// close a loop that the next opens again. If they end with a loop, or
    /// `edit` gives up, they're all taken back.
    pub fn at_once<R>(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Result<R, Loop>,
    ) -> Result<R, Loop> {
        let start = self.edits.len();
        let checks = std::mem::replace(&mut self.checks_loops, false);
        let mut made = edit(self);
        self.checks_loops = checks;
        if made.is_ok()
            && let Some(route) = self.closed_loop()
        {
            made = Err(Loop(route));
        }
        if made.is_err() {
            self.take_back(start);
        }
        made
    }

    /// The route in a loop, if there is one and loops are checked as edits
    /// are made: see [`at_once`](Self::at_once).
    fn closed_loop(&self) -> Option<Route> {
        self.checks_loops
            .then(|| self.project.route_in_a_loop())
            .flatten()
    }

    /// Takes back every edit the transaction has made, so it leaves no
    /// record: what a command does when it can't finish.
    pub fn cancel(&mut self) {
        self.take_back(0);
    }

    /// Takes back the edits made since the first `count`, newest first.
    fn take_back(&mut self, count: usize) {
        let made = self.edits.split_off(count.min(self.edits.len()));
        for edit in made.iter().rev() {
            self.apply(&edit.inverse());
        }
        self.edits.truncate(count);
    }

    /// Makes the change `edit` describes, through the setter for what it
    /// changes, as undo and redo do: `apply(&edit.inverse())` takes it back.
    /// Undo and redo go back to how the project was, which can be how it
    /// was in the middle of [`at_once`](Self::at_once), so nothing they
    /// make is refused for closing a loop.
    pub fn apply(&mut self, edit: &Edit) {
        let checks = std::mem::replace(&mut self.checks_loops, false);
        self.make(edit);
        self.checks_loops = checks;
    }

    fn make(&mut self, edit: &Edit) {
        match edit {
            Edit::Tempo { to, .. } => self.set_tempo(to.clone()),
            Edit::LoopRegion { to, .. } => self.set_loop_region(*to),
            Edit::Looping { to, .. } => self.set_looping(*to),
            Edit::Metronome { to, .. } => self.set_metronome(*to),
            Edit::PunchIn { to, .. } => self.set_punch_in(*to),
            Edit::PunchOut { to, .. } => self.set_punch_out(*to),
            Edit::AddTrack { index, track } => self.add_track(*index, track.clone()),
            // Undo and redo take back or make again what referred to the
            // track as edits of their own, before this one, so there's
            // nothing left to reroute.
            Edit::RemoveTrack { track, .. } => {
                self.remove_track(track.id);
            }
            Edit::TrackName { track, to, .. } => self.set_track_name(*track, to),
            Edit::TrackGain { track, to, .. } => self.set_track_gain(*track, *to),
            Edit::TrackPan { track, to, .. } => self.set_track_pan(*track, *to),
            Edit::TrackMuted { track, to, .. } => self.set_track_muted(*track, *to),
            Edit::TrackSolo { track, to, .. } => self.set_track_solo(*track, *to),
            Edit::TrackSoloSafe { track, to, .. } => self.set_track_solo_safe(*track, *to),
            Edit::TrackArmed { track, to, .. } => self.set_track_armed(*track, *to),
            Edit::TrackInput { track, to, .. } => {
                let _ = self.set_track_input(*track, to.clone());
            }
            Edit::TrackMonitor { track, to, .. } => self.set_track_monitor(*track, *to),
            Edit::TrackAlwaysLive { track, to, .. } => self.set_track_always_live(*track, *to),
            Edit::TrackOutput { track, to, .. } => {
                let _ = self.set_track_output(*track, *to);
            }
            Edit::TrackClipType { track, to, .. } => self.set_track_clip_type(*track, *to),
            Edit::AddSend { track, index, send } => {
                let _ = self.add_send(*track, *index, *send);
            }
            Edit::RemoveSend { track, send, .. } => self.remove_send(*track, send.id),
            Edit::SendLevel {
                track, send, to, ..
            } => self.set_send_level(*track, *send, *to),
            Edit::SendPreFader {
                track, send, to, ..
            } => self.set_send_pre_fader(*track, *send, *to),
            Edit::MasterGain { to, .. } => self.set_master_gain(*to),
            Edit::MasterPan { to, .. } => self.set_master_pan(*to),
            Edit::MasterClipType { to, .. } => self.set_master_clip_type(*to),
            Edit::AddClip { track, index, clip } => self.add_clip(*track, *index, clip.clone()),
            Edit::RemoveClip { clip, .. } => {
                self.remove_clip(clip.id);
            }
            Edit::ClipTiming { clip, to, .. } => self.set_clip_timing(*clip, *to),
            Edit::ClipFades { clip, to, .. } => self.set_clip_fades(*clip, *to),
            Edit::ClipName { clip, to, .. } => self.set_clip_name(*clip, to),
            Edit::ClipWarping { clip, to, .. } => self.set_clip_warping(*clip, to.clone()),
            Edit::ClipGain { clip, to, .. } => self.set_clip_gain(*clip, *to),
            Edit::AddNote {
                clip, index, note, ..
            } => self.add_note(*clip, *index, note.clone()),
            Edit::RemoveNote { clip, note, .. } => {
                self.remove_note(*clip, note.id);
            }
            Edit::NoteTiming { clip, note, to, .. } => self.set_note_timing(*clip, *note, *to),
            Edit::NoteKey { clip, note, to, .. } => self.set_note_key(*clip, *note, *to),
            Edit::NoteVelocity { clip, note, to, .. } => {
                self.set_note_velocity(*clip, *note, *to);
            }
            Edit::Controller {
                clip,
                controller,
                to,
                ..
            } => self.set_controller(*clip, *controller, to.clone()),
            Edit::AddTake { track, index, take } => self.add_take(*track, *index, take.clone()),
            Edit::RemoveTake { take, .. } => {
                self.remove_take(take.id);
            }
            Edit::TakeName { take, to, .. } => self.set_take_name(*take, to),
            Edit::AddLocator { index, locator } => self.add_locator(*index, locator.clone()),
            Edit::RemoveLocator { locator, .. } => {
                self.remove_locator(locator.id);
            }
            Edit::LocatorAt { locator, to, .. } => self.set_locator_at(*locator, *to),
            Edit::LocatorName { locator, to, .. } => self.set_locator_name(*locator, to),
            Edit::AddDevice {
                chain,
                index,
                device,
            } => self.add_device(*chain, *index, device.clone()),
            Edit::RemoveDevice { device, .. } => {
                self.remove_device(device.id);
            }
            Edit::MoveDevice { device, to, .. } => {
                let _ = self.move_device(*device, to.0, to.1);
            }
            Edit::DeviceBypass { device, to, .. } => self.set_device_bypass(*device, *to),
            Edit::DeviceActive { device, to, .. } => self.set_device_active(*device, *to),
            Edit::PluginParam {
                device, param, to, ..
            } => self.set_plugin_param(*device, *param, *to),
            Edit::PluginSettings { device, to, .. } => {
                self.set_plugin_settings(*device, to.clone())
            }
            Edit::PluginInputs { device, to, .. } => self.put_plugin_inputs(*device, to.clone()),
            Edit::PluginNoteOutput { device, to, .. } => self.put_plugin_note_output(*device, *to),
            Edit::PluginNoteTiming { device, to, .. } => self.put_plugin_note_timing(*device, *to),
            Edit::NativeParam {
                device, param, to, ..
            } => self.set_native_param(*device, *param, *to),
            Edit::NativeSample { device, to, .. } => self.set_native_sample(*device, to.clone()),
            Edit::AddWire { index, wire } => {
                let _ = self.add_wire(*index, *wire);
            }
            Edit::RemoveWire { wire, .. } => {
                self.remove_wire(wire.id);
            }
            Edit::WireFrom { wire, to, .. } => {
                let _ = self.set_wire_from(*wire, *to);
            }
            Edit::WireGain { wire, to, .. } => self.set_wire_gain(*wire, *to),
            Edit::WireChannel { wire, to, .. } => self.set_wire_channel(*wire, *to),
            Edit::AddLane { index, lane } => self.add_lane(*index, lane.clone()),
            Edit::RemoveLane { lane, .. } => {
                self.remove_lane(lane.param);
            }
            Edit::LanePoints { param, to, .. } => self.set_lane_points(*param, to.clone()),
            Edit::LaneOverridden { param, to, .. } => self.set_lane_overridden(*param, *to),
            Edit::AddModulator {
                chain,
                index,
                modulator,
            } => self.add_modulator(*chain, *index, modulator.clone()),
            // As with tracks, what referred to it is taken back or made
            // again as edits of its own.
            Edit::RemoveModulator { modulator, .. } => {
                self.remove_modulator(modulator.id);
            }
            Edit::MoveModulator { modulator, to, .. } => self.move_modulator(*modulator, *to),
            Edit::ModulatorName { modulator, to, .. } => self.set_modulator_name(*modulator, to),
            Edit::ModulatorParam {
                modulator,
                param,
                to,
                ..
            } => self.set_modulator_param(*modulator, *param, *to),
            Edit::AddControl {
                modulator,
                index,
                control,
            } => self.add_control(*modulator, *index, control.clone()),
            Edit::RemoveControl {
                modulator, control, ..
            } => {
                self.remove_control(*modulator, control.id);
            }
            Edit::ControlName {
                modulator,
                control,
                to,
                ..
            } => self.set_control_name(*modulator, *control, to),
            Edit::AddRouting {
                chain,
                index,
                routing,
            } => self.add_routing(*chain, *index, *routing),
            Edit::RemoveRouting { routing, .. } => {
                self.remove_routing(routing.id);
            }
            Edit::RoutingSettings { to, .. } => self.set_routing(*to),
        }
    }
}

/// Why an edit was refused: it would have closed a loop through this route,
/// a track's input from a tap or a wire. See [`Project::route_in_a_loop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loop(pub Route);

/// `gain` kept from silence to [`Wire::GAIN_MAX`], if it's a finite number.
fn wire_gain(gain: f32) -> Option<f32> {
    gain.is_finite().then(|| gain.clamp(0.0, Wire::GAIN_MAX))
}

/// An audio clip's fades. MIDI clips have none.
fn fades_of(clip: &Clip) -> Option<Fades> {
    match &clip.content {
        ClipContent::Audio(audio) => Some(audio.fades()),
        ClipContent::Midi(_) => None,
    }
}

/// The notes of the MIDI clip with id `clip`, on one of the project's
/// tracks, and the track's id.
/// The audio clip with id `clip` on one of the project's tracks, and the
/// track's id.
fn audio_mut(project: &mut Project, clip: ClipId) -> Option<(TrackId, &mut AudioClip)> {
    let track = project.find_clip(clip)?.0.id;
    match &mut project.track_mut(track)?.clip_mut(clip)?.content {
        ClipContent::Audio(audio) => Some((track, audio)),
        ClipContent::Midi(_) => None,
    }
}

fn midi_mut(project: &mut Project, clip: ClipId) -> Option<(TrackId, &mut MidiClip)> {
    let track = project.find_clip(clip)?.0.id;
    match &mut project.track_mut(track)?.clip_mut(clip)?.content {
        ClipContent::Midi(midi) => Some((track, midi)),
        ClipContent::Audio(_) => None,
    }
}

/// The note with id `note` in the MIDI clip with id `clip`, and the clip's
/// track's id.
fn note_mut(project: &mut Project, clip: ClipId, note: NoteId) -> Option<(TrackId, &mut Note)> {
    let (track, midi) = midi_mut(project, clip)?;
    Some((track, midi.note_mut(note)?))
}

/// Whether a file could hold `note`: it lasts more than zero, its key is a
/// MIDI note number, its velocities are from 0 to 1, and each of its
/// expression curves starts at the note's start or after, is in order, and
/// keeps to its dimension's values.
pub(crate) fn note_fits(note: &Note) -> bool {
    let unit = |v: f32| (0.0..=1.0).contains(&v);
    note.length > Beats::ZERO
        && note.key <= Note::MAX_KEY
        && unit(note.velocity)
        && unit(note.release_velocity)
        && Dimension::ALL.into_iter().all(|dimension| {
            let curve = note.expression.curve(dimension);
            let (low, high) = dimension.range();
            curve.first().is_none_or(|p| p.at >= Beats::ZERO)
                && curve.is_sorted_by_key(|p| p.at)
                && curve.iter().all(|p| (low..=high).contains(&p.value))
        })
}

/// Whether a file could hold a controller's curve: its points are in order,
/// with values from 0 to 1.
fn curve_fits(points: &[Point]) -> bool {
    points.is_sorted_by_key(|p| p.at) && points.iter().all(|p| (0.0..=1.0).contains(&p.value))
}

/// One change to the project, with the value it replaced.
///
/// A new kind of edit needs a setter on [`Transaction`], and an arm in each
/// match below, which say how to make it, take it back and merge two of
/// them. The undo tests need an example of it too.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    Tempo {
        from: TempoMap,
        to: TempoMap,
    },
    LoopRegion {
        from: BeatRange,
        to: BeatRange,
    },
    Looping {
        from: bool,
        to: bool,
    },
    Metronome {
        from: bool,
        to: bool,
    },
    PunchIn {
        from: bool,
        to: bool,
    },
    PunchOut {
        from: bool,
        to: bool,
    },
    /// `track` went in at `index` in the track list.
    AddTrack {
        index: usize,
        track: Track,
    },
    /// `track`, which was at `index` in the track list, went.
    RemoveTrack {
        index: usize,
        track: Track,
    },
    TrackName {
        track: TrackId,
        from: String,
        to: String,
    },
    TrackGain {
        track: TrackId,
        from: f32,
        to: f32,
    },
    TrackPan {
        track: TrackId,
        from: f32,
        to: f32,
    },
    TrackMuted {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackSolo {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackSoloSafe {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackArmed {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackInput {
        track: TrackId,
        from: Option<Input>,
        to: Option<Input>,
    },
    TrackMonitor {
        track: TrackId,
        from: Monitor,
        to: Monitor,
    },
    TrackAlwaysLive {
        track: TrackId,
        from: bool,
        to: bool,
    },
    TrackOutput {
        track: TrackId,
        from: Bus,
        to: Bus,
    },
    TrackClipType {
        track: TrackId,
        from: ClipType,
        to: ClipType,
    },
    /// `send` went in at `index` in the sends of the track with id `track`.
    AddSend {
        track: TrackId,
        index: usize,
        send: Send,
    },
    /// `send`, which was at `index` in the track's sends, went.
    RemoveSend {
        track: TrackId,
        index: usize,
        send: Send,
    },
    SendLevel {
        track: TrackId,
        send: SendId,
        from: f32,
        to: f32,
    },
    SendPreFader {
        track: TrackId,
        send: SendId,
        from: bool,
        to: bool,
    },
    MasterGain {
        from: f32,
        to: f32,
    },
    MasterPan {
        from: f32,
        to: f32,
    },
    MasterClipType {
        from: ClipType,
        to: ClipType,
    },
    /// `clip` went in at `index` in the clips of the track with id `track`.
    AddClip {
        track: TrackId,
        index: usize,
        clip: Clip,
    },
    /// `clip`, which was at `index` in the track's clips, went.
    RemoveClip {
        track: TrackId,
        index: usize,
        clip: Clip,
    },
    /// The clip with id `clip`, on the track with id `track`, moved or was
    /// trimmed.
    ClipTiming {
        track: TrackId,
        clip: ClipId,
        from: Timing,
        to: Timing,
    },
    ClipFades {
        track: TrackId,
        clip: ClipId,
        from: Fades,
        to: Fades,
    },
    ClipName {
        track: TrackId,
        clip: ClipId,
        from: String,
        to: String,
    },
    /// The audio clip with id `clip` was warped or unwarped, or its warp
    /// changed.
    ClipWarping {
        track: TrackId,
        clip: ClipId,
        from: Warping,
        to: Warping,
    },
    /// The audio clip's gain changed, as linear gain.
    ClipGain {
        track: TrackId,
        clip: ClipId,
        from: f32,
        to: f32,
    },
    /// `note` went in at `index` in the notes of the MIDI clip with id
    /// `clip`, on the track with id `track`.
    AddNote {
        track: TrackId,
        clip: ClipId,
        index: usize,
        note: Note,
    },
    /// `note`, which was at `index` in the clip's notes, went.
    RemoveNote {
        track: TrackId,
        clip: ClipId,
        index: usize,
        note: Note,
    },
    /// The note with id `note`, in the clip with id `clip`, moved or changed
    /// length: when it sounds, in the clip's own time.
    NoteTiming {
        track: TrackId,
        clip: ClipId,
        note: NoteId,
        from: BeatRange,
        to: BeatRange,
    },
    NoteKey {
        track: TrackId,
        clip: ClipId,
        note: NoteId,
        from: u8,
        to: u8,
    },
    NoteVelocity {
        track: TrackId,
        clip: ClipId,
        note: NoteId,
        from: f32,
        to: f32,
    },
    /// How the MIDI clip with id `clip` moves `controller` changed: its
    /// curve's points, none for a controller it doesn't move.
    Controller {
        track: TrackId,
        clip: ClipId,
        controller: Controller,
        from: Vec<Point>,
        to: Vec<Point>,
    },
    /// `take` went in at `index` in the takes of the track with id `track`.
    AddTake {
        track: TrackId,
        index: usize,
        take: Take,
    },
    /// `take`, which was at `index` in the track's takes, went.
    RemoveTake {
        track: TrackId,
        index: usize,
        take: Take,
    },
    TakeName {
        track: TrackId,
        take: TakeId,
        from: String,
        to: String,
    },
    /// `locator` went in at `index` in the project's locators.
    AddLocator {
        index: usize,
        locator: Locator,
    },
    /// `locator`, which was at `index` in the project's locators, went.
    RemoveLocator {
        index: usize,
        locator: Locator,
    },
    /// The locator with id `locator` moved.
    LocatorAt {
        locator: LocatorId,
        from: Beats,
        to: Beats,
    },
    LocatorName {
        locator: LocatorId,
        from: String,
        to: String,
    },
    /// `device` went in at `index` in `chain`.
    AddDevice {
        chain: Chain,
        index: usize,
        device: Device,
    },
    /// `device`, which was at `index` in `chain`, went.
    RemoveDevice {
        chain: Chain,
        index: usize,
        device: Device,
    },
    /// The device with id `device` moved from a chain and its place there
    /// to another, or to another place in the same chain. Its place in
    /// `to` counts the chain without it.
    MoveDevice {
        device: DeviceId,
        from: (Chain, usize),
        to: (Chain, usize),
    },
    /// The device with id `device` was bypassed, or brought back.
    DeviceBypass {
        device: DeviceId,
        from: bool,
        to: bool,
    },
    /// The device with id `device` was deactivated, or activated.
    DeviceActive {
        device: DeviceId,
        from: bool,
        to: bool,
    },
    /// A parameter of the plugin device with id `device` changed. `None` is
    /// the value in the plugin's state.
    PluginParam {
        device: DeviceId,
        param: u32,
        from: Option<f64>,
        to: Option<f64>,
    },
    /// The settings of the plugin device with id `device` changed as a
    /// whole.
    PluginSettings {
        device: DeviceId,
        from: PluginSettings,
        to: PluginSettings,
    },
    /// The side inputs of the plugin device with id `device` changed, as
    /// its plugin said.
    PluginInputs {
        device: DeviceId,
        from: Vec<PluginInput>,
        to: Vec<PluginInput>,
    },
    /// Whether the plugin device with id `device` has a note output
    /// changed, as its plugin said.
    PluginNoteOutput {
        device: DeviceId,
        from: bool,
        to: bool,
    },
    /// When the notes the plugin device with id `device` plays come, against
    /// its latency, changed, and whether someone chose it.
    PluginNoteTiming {
        device: DeviceId,
        from: (NoteTiming, bool),
        to: (NoteTiming, bool),
    },
    /// Parameter `param` of the built-in device with id `device` changed.
    NativeParam {
        device: DeviceId,
        param: u32,
        from: f64,
        to: f64,
    },
    /// The sample of the sampler with id `device` changed.
    NativeSample {
        device: DeviceId,
        from: Option<String>,
        to: Option<String>,
    },
    /// `wire` went in at `index` in the project's wires.
    AddWire {
        index: usize,
        wire: Wire,
    },
    /// `wire`, which was at `index` in the project's wires, went.
    RemoveWire {
        index: usize,
        wire: Wire,
    },
    /// The wire with id `wire` took its signal from another tap.
    WireFrom {
        wire: WireId,
        from: Tap,
        to: Tap,
    },
    /// The gain of the wire with id `wire` changed.
    WireGain {
        wire: WireId,
        from: f32,
        to: f32,
    },
    /// The channel the note wire with id `wire` plays on changed.
    WireChannel {
        wire: WireId,
        from: Option<u8>,
        to: Option<u8>,
    },
    /// `lane` went in at `index` in the project's lanes: its parameter is
    /// automated.
    AddLane {
        index: usize,
        lane: Lane,
    },
    /// `lane`, which was at `index` in the project's lanes, went.
    RemoveLane {
        index: usize,
        lane: Lane,
    },
    /// The points of the lane that automates `param` changed. For a clip's
    /// parameter, the curve in the clip, which no points means it hasn't.
    LanePoints {
        param: ParamAddress,
        from: Vec<automation::Point>,
        to: Vec<automation::Point>,
    },
    /// The automation of `param` was overridden, or re-enabled.
    LaneOverridden {
        param: ParamAddress,
        from: bool,
        to: bool,
    },
    /// `modulator` went in at `index` in the modulator strip of `chain`.
    AddModulator {
        chain: Chain,
        index: usize,
        modulator: Modulator,
    },
    /// `modulator`, which was at `index` in the strip of `chain`, went.
    RemoveModulator {
        chain: Chain,
        index: usize,
        modulator: Modulator,
    },
    /// The modulator with id `modulator` moved from one place in its strip
    /// to another. Its place in `to` counts the strip without it.
    MoveModulator {
        modulator: ModulatorId,
        from: usize,
        to: usize,
    },
    ModulatorName {
        modulator: ModulatorId,
        from: String,
        to: String,
    },
    /// Parameter `param` of the modulator with id `modulator` changed.
    ModulatorParam {
        modulator: ModulatorId,
        param: u32,
        from: f64,
        to: f64,
    },
    /// `control` went in at `index` in the controls of the Macro with id
    /// `modulator`.
    AddControl {
        modulator: ModulatorId,
        index: usize,
        control: Control,
    },
    /// `control`, which was at `index` in the Macro's controls, went.
    RemoveControl {
        modulator: ModulatorId,
        index: usize,
        control: Control,
    },
    /// The control with id `control` of the Macro with id `modulator` was
    /// renamed.
    ControlName {
        modulator: ModulatorId,
        control: u32,
        from: String,
        to: String,
    },
    /// `routing` went in at `index` in the routings of `chain`.
    AddRouting {
        chain: Chain,
        index: usize,
        routing: Routing,
    },
    /// `routing`, which was at `index` in the routings of `chain`, went.
    RemoveRouting {
        chain: Chain,
        index: usize,
        routing: Routing,
    },
    /// The settings of the routing with id `routing` changed: its amount,
    /// curve, scale, lag or bypass.
    RoutingSettings {
        routing: RoutingId,
        from: Routing,
        to: Routing,
    },
}

impl Edit {
    /// The edit that takes this one back.
    pub fn inverse(&self) -> Edit {
        match self.clone() {
            Edit::Tempo { from, to } => Edit::Tempo { from: to, to: from },
            Edit::LoopRegion { from, to } => Edit::LoopRegion { from: to, to: from },
            Edit::Looping { from, to } => Edit::Looping { from: to, to: from },
            Edit::Metronome { from, to } => Edit::Metronome { from: to, to: from },
            Edit::PunchIn { from, to } => Edit::PunchIn { from: to, to: from },
            Edit::PunchOut { from, to } => Edit::PunchOut { from: to, to: from },
            Edit::AddTrack { index, track } => Edit::RemoveTrack { index, track },
            Edit::RemoveTrack { index, track } => Edit::AddTrack { index, track },
            Edit::TrackName { track, from, to } => Edit::TrackName {
                track,
                from: to,
                to: from,
            },
            Edit::TrackGain { track, from, to } => Edit::TrackGain {
                track,
                from: to,
                to: from,
            },
            Edit::TrackPan { track, from, to } => Edit::TrackPan {
                track,
                from: to,
                to: from,
            },
            Edit::TrackMuted { track, from, to } => Edit::TrackMuted {
                track,
                from: to,
                to: from,
            },
            Edit::TrackSolo { track, from, to } => Edit::TrackSolo {
                track,
                from: to,
                to: from,
            },
            Edit::TrackSoloSafe { track, from, to } => Edit::TrackSoloSafe {
                track,
                from: to,
                to: from,
            },
            Edit::TrackArmed { track, from, to } => Edit::TrackArmed {
                track,
                from: to,
                to: from,
            },
            Edit::TrackInput { track, from, to } => Edit::TrackInput {
                track,
                from: to,
                to: from,
            },
            Edit::TrackMonitor { track, from, to } => Edit::TrackMonitor {
                track,
                from: to,
                to: from,
            },
            Edit::TrackAlwaysLive { track, from, to } => Edit::TrackAlwaysLive {
                track,
                from: to,
                to: from,
            },
            Edit::TrackOutput { track, from, to } => Edit::TrackOutput {
                track,
                from: to,
                to: from,
            },
            Edit::TrackClipType { track, from, to } => Edit::TrackClipType {
                track,
                from: to,
                to: from,
            },
            Edit::AddSend { track, index, send } => Edit::RemoveSend { track, index, send },
            Edit::RemoveSend { track, index, send } => Edit::AddSend { track, index, send },
            Edit::SendLevel {
                track,
                send,
                from,
                to,
            } => Edit::SendLevel {
                track,
                send,
                from: to,
                to: from,
            },
            Edit::SendPreFader {
                track,
                send,
                from,
                to,
            } => Edit::SendPreFader {
                track,
                send,
                from: to,
                to: from,
            },
            Edit::MasterGain { from, to } => Edit::MasterGain { from: to, to: from },
            Edit::MasterPan { from, to } => Edit::MasterPan { from: to, to: from },
            Edit::MasterClipType { from, to } => Edit::MasterClipType { from: to, to: from },
            Edit::AddClip { track, index, clip } => Edit::RemoveClip { track, index, clip },
            Edit::RemoveClip { track, index, clip } => Edit::AddClip { track, index, clip },
            Edit::ClipTiming {
                track,
                clip,
                from,
                to,
            } => Edit::ClipTiming {
                track,
                clip,
                from: to,
                to: from,
            },
            Edit::ClipFades {
                track,
                clip,
                from,
                to,
            } => Edit::ClipFades {
                track,
                clip,
                from: to,
                to: from,
            },
            Edit::ClipName {
                track,
                clip,
                from,
                to,
            } => Edit::ClipName {
                track,
                clip,
                from: to,
                to: from,
            },
            Edit::ClipWarping {
                track,
                clip,
                from,
                to,
            } => Edit::ClipWarping {
                track,
                clip,
                from: to,
                to: from,
            },
            Edit::ClipGain {
                track,
                clip,
                from,
                to,
            } => Edit::ClipGain {
                track,
                clip,
                from: to,
                to: from,
            },
            Edit::AddNote {
                track,
                clip,
                index,
                note,
            } => Edit::RemoveNote {
                track,
                clip,
                index,
                note,
            },
            Edit::RemoveNote {
                track,
                clip,
                index,
                note,
            } => Edit::AddNote {
                track,
                clip,
                index,
                note,
            },
            Edit::NoteTiming {
                track,
                clip,
                note,
                from,
                to,
            } => Edit::NoteTiming {
                track,
                clip,
                note,
                from: to,
                to: from,
            },
            Edit::NoteKey {
                track,
                clip,
                note,
                from,
                to,
            } => Edit::NoteKey {
                track,
                clip,
                note,
                from: to,
                to: from,
            },
            Edit::NoteVelocity {
                track,
                clip,
                note,
                from,
                to,
            } => Edit::NoteVelocity {
                track,
                clip,
                note,
                from: to,
                to: from,
            },
            Edit::Controller {
                track,
                clip,
                controller,
                from,
                to,
            } => Edit::Controller {
                track,
                clip,
                controller,
                from: to,
                to: from,
            },
            Edit::AddTake { track, index, take } => Edit::RemoveTake { track, index, take },
            Edit::RemoveTake { track, index, take } => Edit::AddTake { track, index, take },
            Edit::TakeName {
                track,
                take,
                from,
                to,
            } => Edit::TakeName {
                track,
                take,
                from: to,
                to: from,
            },
            Edit::AddLocator { index, locator } => Edit::RemoveLocator { index, locator },
            Edit::RemoveLocator { index, locator } => Edit::AddLocator { index, locator },
            Edit::LocatorAt { locator, from, to } => Edit::LocatorAt {
                locator,
                from: to,
                to: from,
            },
            Edit::LocatorName { locator, from, to } => Edit::LocatorName {
                locator,
                from: to,
                to: from,
            },
            Edit::AddDevice {
                chain,
                index,
                device,
            } => Edit::RemoveDevice {
                chain,
                index,
                device,
            },
            Edit::RemoveDevice {
                chain,
                index,
                device,
            } => Edit::AddDevice {
                chain,
                index,
                device,
            },
            Edit::MoveDevice { device, from, to } => Edit::MoveDevice {
                device,
                from: to,
                to: from,
            },
            Edit::DeviceBypass { device, from, to } => Edit::DeviceBypass {
                device,
                from: to,
                to: from,
            },
            Edit::DeviceActive { device, from, to } => Edit::DeviceActive {
                device,
                from: to,
                to: from,
            },
            Edit::PluginParam {
                device,
                param,
                from,
                to,
            } => Edit::PluginParam {
                device,
                param,
                from: to,
                to: from,
            },
            Edit::PluginSettings { device, from, to } => Edit::PluginSettings {
                device,
                from: to,
                to: from,
            },
            Edit::PluginInputs { device, from, to } => Edit::PluginInputs {
                device,
                from: to,
                to: from,
            },
            Edit::PluginNoteOutput { device, from, to } => Edit::PluginNoteOutput {
                device,
                from: to,
                to: from,
            },
            Edit::PluginNoteTiming { device, from, to } => Edit::PluginNoteTiming {
                device,
                from: to,
                to: from,
            },
            Edit::NativeParam {
                device,
                param,
                from,
                to,
            } => Edit::NativeParam {
                device,
                param,
                from: to,
                to: from,
            },
            Edit::NativeSample { device, from, to } => Edit::NativeSample {
                device,
                from: to,
                to: from,
            },
            Edit::AddWire { index, wire } => Edit::RemoveWire { index, wire },
            Edit::RemoveWire { index, wire } => Edit::AddWire { index, wire },
            Edit::WireFrom { wire, from, to } => Edit::WireFrom {
                wire,
                from: to,
                to: from,
            },
            Edit::WireGain { wire, from, to } => Edit::WireGain {
                wire,
                from: to,
                to: from,
            },
            Edit::WireChannel { wire, from, to } => Edit::WireChannel {
                wire,
                from: to,
                to: from,
            },
            Edit::AddLane { index, lane } => Edit::RemoveLane { index, lane },
            Edit::RemoveLane { index, lane } => Edit::AddLane { index, lane },
            Edit::LanePoints { param, from, to } => Edit::LanePoints {
                param,
                from: to,
                to: from,
            },
            Edit::LaneOverridden { param, from, to } => Edit::LaneOverridden {
                param,
                from: to,
                to: from,
            },
            Edit::AddModulator {
                chain,
                index,
                modulator,
            } => Edit::RemoveModulator {
                chain,
                index,
                modulator,
            },
            Edit::RemoveModulator {
                chain,
                index,
                modulator,
            } => Edit::AddModulator {
                chain,
                index,
                modulator,
            },
            Edit::MoveModulator {
                modulator,
                from,
                to,
            } => Edit::MoveModulator {
                modulator,
                from: to,
                to: from,
            },
            Edit::ModulatorName {
                modulator,
                from,
                to,
            } => Edit::ModulatorName {
                modulator,
                from: to,
                to: from,
            },
            Edit::ModulatorParam {
                modulator,
                param,
                from,
                to,
            } => Edit::ModulatorParam {
                modulator,
                param,
                from: to,
                to: from,
            },
            Edit::AddControl {
                modulator,
                index,
                control,
            } => Edit::RemoveControl {
                modulator,
                index,
                control,
            },
            Edit::RemoveControl {
                modulator,
                index,
                control,
            } => Edit::AddControl {
                modulator,
                index,
                control,
            },
            Edit::ControlName {
                modulator,
                control,
                from,
                to,
            } => Edit::ControlName {
                modulator,
                control,
                from: to,
                to: from,
            },
            Edit::AddRouting {
                chain,
                index,
                routing,
            } => Edit::RemoveRouting {
                chain,
                index,
                routing,
            },
            Edit::RemoveRouting {
                chain,
                index,
                routing,
            } => Edit::AddRouting {
                chain,
                index,
                routing,
            },
            Edit::RoutingSettings { routing, from, to } => Edit::RoutingSettings {
                routing,
                from: to,
                to: from,
            },
        }
    }

    /// Folds `next`, the edit made right after this one, into it if both
    /// change the same thing. Returns whether it did.
    pub(crate) fn absorb(&mut self, next: &Edit) -> bool {
        match (self, next) {
            (Edit::Tempo { to, .. }, Edit::Tempo { to: next, .. }) => to.clone_from(next),
            (Edit::LoopRegion { to, .. }, Edit::LoopRegion { to: next, .. }) => *to = *next,
            (Edit::Looping { to, .. }, Edit::Looping { to: next, .. }) => *to = *next,
            (Edit::Metronome { to, .. }, Edit::Metronome { to: next, .. })
            | (Edit::PunchIn { to, .. }, Edit::PunchIn { to: next, .. })
            | (Edit::PunchOut { to, .. }, Edit::PunchOut { to: next, .. }) => *to = *next,
            (
                Edit::TrackName { track, to, .. },
                Edit::TrackName {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => to.clone_from(next),
            (
                Edit::TrackGain { track, to, .. },
                Edit::TrackGain {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackPan { track, to, .. },
                Edit::TrackPan {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::TrackMuted { track, to, .. },
                Edit::TrackMuted {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackSolo { track, to, .. },
                Edit::TrackSolo {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackSoloSafe { track, to, .. },
                Edit::TrackSoloSafe {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackArmed { track, to, .. },
                Edit::TrackArmed {
                    track: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::TrackAlwaysLive { track, to, .. },
                Edit::TrackAlwaysLive {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::TrackInput { track, to, .. },
                Edit::TrackInput {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => to.clone_from(next),
            (
                Edit::TrackMonitor { track, to, .. },
                Edit::TrackMonitor {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::TrackOutput { track, to, .. },
                Edit::TrackOutput {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::TrackClipType { track, to, .. },
                Edit::TrackClipType {
                    track: other,
                    to: next,
                    ..
                },
            ) if track == other => *to = *next,
            (
                Edit::SendLevel {
                    track, send, to, ..
                },
                Edit::SendLevel {
                    track: other,
                    send: other_send,
                    to: next,
                    ..
                },
            ) if track == other && send == other_send => *to = *next,
            (
                Edit::SendPreFader {
                    track, send, to, ..
                },
                Edit::SendPreFader {
                    track: other,
                    send: other_send,
                    to: next,
                    ..
                },
            ) if track == other && send == other_send => *to = *next,
            (Edit::MasterGain { to, .. }, Edit::MasterGain { to: next, .. })
            | (Edit::MasterPan { to, .. }, Edit::MasterPan { to: next, .. }) => *to = *next,
            (Edit::MasterClipType { to, .. }, Edit::MasterClipType { to: next, .. }) => {
                *to = *next;
            }
            (
                Edit::ClipTiming { clip, to, .. },
                Edit::ClipTiming {
                    clip: other,
                    to: next,
                    ..
                },
            ) if clip == other => *to = *next,
            (
                Edit::ClipFades { clip, to, .. },
                Edit::ClipFades {
                    clip: other,
                    to: next,
                    ..
                },
            ) if clip == other => *to = *next,
            (
                Edit::ClipName { clip, to, .. },
                Edit::ClipName {
                    clip: other,
                    to: next,
                    ..
                },
            ) if clip == other => to.clone_from(next),
            (
                Edit::ClipWarping { clip, to, .. },
                Edit::ClipWarping {
                    clip: other,
                    to: next,
                    ..
                },
            ) if clip == other => to.clone_from(next),
            (
                Edit::ClipGain { clip, to, .. },
                Edit::ClipGain {
                    clip: other,
                    to: next,
                    ..
                },
            ) if clip == other => *to = *next,
            (
                Edit::NoteTiming { clip, note, to, .. },
                Edit::NoteTiming {
                    clip: other,
                    note: other_note,
                    to: next,
                    ..
                },
            ) if clip == other && note == other_note => *to = *next,
            (
                Edit::NoteKey { clip, note, to, .. },
                Edit::NoteKey {
                    clip: other,
                    note: other_note,
                    to: next,
                    ..
                },
            ) if clip == other && note == other_note => *to = *next,
            (
                Edit::NoteVelocity { clip, note, to, .. },
                Edit::NoteVelocity {
                    clip: other,
                    note: other_note,
                    to: next,
                    ..
                },
            ) if clip == other && note == other_note => *to = *next,
            (
                Edit::Controller {
                    clip,
                    controller,
                    to,
                    ..
                },
                Edit::Controller {
                    clip: other,
                    controller: other_controller,
                    to: next,
                    ..
                },
            ) if clip == other && controller == other_controller => to.clone_from(next),
            (
                Edit::TakeName { take, to, .. },
                Edit::TakeName {
                    take: other,
                    to: next,
                    ..
                },
            ) if take == other => to.clone_from(next),
            (
                Edit::LocatorAt { locator, to, .. },
                Edit::LocatorAt {
                    locator: other,
                    to: next,
                    ..
                },
            ) if locator == other => *to = *next,
            (
                Edit::LocatorName { locator, to, .. },
                Edit::LocatorName {
                    locator: other,
                    to: next,
                    ..
                },
            ) if locator == other => to.clone_from(next),
            (
                Edit::DeviceBypass { device, to, .. },
                Edit::DeviceBypass {
                    device: other,
                    to: next,
                    ..
                },
            )
            | (
                Edit::DeviceActive { device, to, .. },
                Edit::DeviceActive {
                    device: other,
                    to: next,
                    ..
                },
            ) if device == other => *to = *next,
            (
                Edit::PluginParam {
                    device, param, to, ..
                },
                Edit::PluginParam {
                    device: other,
                    param: other_param,
                    to: next,
                    ..
                },
            ) if device == other && param == other_param => *to = *next,
            (
                Edit::PluginSettings { device, to, .. },
                Edit::PluginSettings {
                    device: other,
                    to: next,
                    ..
                },
            ) if device == other => to.clone_from(next),
            (
                Edit::PluginInputs { device, to, .. },
                Edit::PluginInputs {
                    device: other,
                    to: next,
                    ..
                },
            ) if device == other => to.clone_from(next),
            (
                Edit::PluginNoteOutput { device, to, .. },
                Edit::PluginNoteOutput {
                    device: other,
                    to: next,
                    ..
                },
            ) if device == other => *to = *next,
            (
                Edit::PluginNoteTiming { device, to, .. },
                Edit::PluginNoteTiming {
                    device: other,
                    to: next,
                    ..
                },
            ) if device == other => *to = *next,
            (
                Edit::NativeParam {
                    device, param, to, ..
                },
                Edit::NativeParam {
                    device: other,
                    param: other_param,
                    to: next,
                    ..
                },
            ) if device == other && param == other_param => *to = *next,
            (
                Edit::NativeSample { device, to, .. },
                Edit::NativeSample {
                    device: other,
                    to: next,
                    ..
                },
            ) if device == other => to.clone_from(next),
            (
                Edit::WireGain { wire, to, .. },
                Edit::WireGain {
                    wire: other,
                    to: next,
                    ..
                },
            ) if wire == other => *to = *next,
            (
                Edit::LanePoints { param, to, .. },
                Edit::LanePoints {
                    param: other,
                    to: next,
                    ..
                },
            ) if param == other => to.clone_from(next),
            (
                Edit::LaneOverridden { param, to, .. },
                Edit::LaneOverridden {
                    param: other,
                    to: next,
                    ..
                },
            ) if param == other => *to = *next,
            (
                Edit::ModulatorName { modulator, to, .. },
                Edit::ModulatorName {
                    modulator: other,
                    to: next,
                    ..
                },
            ) if modulator == other => to.clone_from(next),
            (
                Edit::ModulatorParam {
                    modulator,
                    param,
                    to,
                    ..
                },
                Edit::ModulatorParam {
                    modulator: other,
                    param: other_param,
                    to: next,
                    ..
                },
            ) if modulator == other && param == other_param => *to = *next,
            (
                Edit::ControlName {
                    modulator,
                    control,
                    to,
                    ..
                },
                Edit::ControlName {
                    modulator: other,
                    control: other_control,
                    to: next,
                    ..
                },
            ) if modulator == other && control == other_control => to.clone_from(next),
            (
                Edit::RoutingSettings { routing, to, .. },
                Edit::RoutingSettings {
                    routing: other,
                    to: next,
                    ..
                },
            ) if routing == other => *to = *next,
            // Adding, removing and moving never merge: each is a step of its own.
            (
                Edit::Tempo { .. }
                | Edit::LoopRegion { .. }
                | Edit::Looping { .. }
                | Edit::Metronome { .. }
                | Edit::PunchIn { .. }
                | Edit::PunchOut { .. }
                | Edit::AddTrack { .. }
                | Edit::RemoveTrack { .. }
                | Edit::TrackName { .. }
                | Edit::TrackGain { .. }
                | Edit::TrackPan { .. }
                | Edit::TrackMuted { .. }
                | Edit::TrackSolo { .. }
                | Edit::TrackSoloSafe { .. }
                | Edit::TrackArmed { .. }
                | Edit::TrackInput { .. }
                | Edit::TrackMonitor { .. }
                | Edit::TrackAlwaysLive { .. }
                | Edit::TrackOutput { .. }
                | Edit::TrackClipType { .. }
                | Edit::AddSend { .. }
                | Edit::RemoveSend { .. }
                | Edit::SendLevel { .. }
                | Edit::SendPreFader { .. }
                | Edit::MasterGain { .. }
                | Edit::MasterPan { .. }
                | Edit::MasterClipType { .. }
                | Edit::AddClip { .. }
                | Edit::RemoveClip { .. }
                | Edit::ClipTiming { .. }
                | Edit::ClipFades { .. }
                | Edit::ClipName { .. }
                | Edit::ClipWarping { .. }
                | Edit::ClipGain { .. }
                | Edit::AddNote { .. }
                | Edit::RemoveNote { .. }
                | Edit::NoteTiming { .. }
                | Edit::NoteKey { .. }
                | Edit::NoteVelocity { .. }
                | Edit::Controller { .. }
                | Edit::AddTake { .. }
                | Edit::RemoveTake { .. }
                | Edit::TakeName { .. }
                | Edit::AddLocator { .. }
                | Edit::RemoveLocator { .. }
                | Edit::LocatorAt { .. }
                | Edit::LocatorName { .. }
                | Edit::AddDevice { .. }
                | Edit::RemoveDevice { .. }
                | Edit::MoveDevice { .. }
                | Edit::DeviceBypass { .. }
                | Edit::DeviceActive { .. }
                | Edit::PluginParam { .. }
                | Edit::PluginSettings { .. }
                | Edit::PluginInputs { .. }
                | Edit::PluginNoteOutput { .. }
                | Edit::PluginNoteTiming { .. }
                | Edit::NativeParam { .. }
                | Edit::NativeSample { .. }
                | Edit::AddWire { .. }
                | Edit::RemoveWire { .. }
                | Edit::WireFrom { .. }
                | Edit::WireGain { .. }
                | Edit::WireChannel { .. }
                | Edit::AddLane { .. }
                | Edit::RemoveLane { .. }
                | Edit::LanePoints { .. }
                | Edit::LaneOverridden { .. }
                | Edit::AddModulator { .. }
                | Edit::RemoveModulator { .. }
                | Edit::MoveModulator { .. }
                | Edit::ModulatorName { .. }
                | Edit::ModulatorParam { .. }
                | Edit::AddControl { .. }
                | Edit::RemoveControl { .. }
                | Edit::ControlName { .. }
                | Edit::AddRouting { .. }
                | Edit::RemoveRouting { .. }
                | Edit::RoutingSettings { .. },
                _,
            ) => return false,
        }
        true
    }

    /// Whether the edit changes nothing, as when two merged edits cancel out.
    pub(crate) fn is_noop(&self) -> bool {
        match self {
            Edit::Tempo { from, to } => from == to,
            Edit::LoopRegion { from, to } => from == to,
            Edit::Looping { from, to } => from == to,
            Edit::Metronome { from, to }
            | Edit::PunchIn { from, to }
            | Edit::PunchOut { from, to } => from == to,
            Edit::AddTrack { .. }
            | Edit::RemoveTrack { .. }
            | Edit::AddSend { .. }
            | Edit::RemoveSend { .. }
            | Edit::AddClip { .. }
            | Edit::RemoveClip { .. }
            | Edit::AddNote { .. }
            | Edit::RemoveNote { .. }
            | Edit::AddTake { .. }
            | Edit::RemoveTake { .. }
            | Edit::AddLocator { .. }
            | Edit::RemoveLocator { .. }
            | Edit::AddDevice { .. }
            | Edit::RemoveDevice { .. }
            | Edit::AddWire { .. }
            | Edit::RemoveWire { .. }
            | Edit::AddLane { .. }
            | Edit::RemoveLane { .. }
            | Edit::AddModulator { .. }
            | Edit::RemoveModulator { .. }
            | Edit::AddControl { .. }
            | Edit::RemoveControl { .. }
            | Edit::AddRouting { .. }
            | Edit::RemoveRouting { .. } => false,
            Edit::TrackName { from, to, .. } => from == to,
            Edit::TrackGain { from, to, .. }
            | Edit::TrackPan { from, to, .. }
            | Edit::SendLevel { from, to, .. }
            | Edit::WireGain { from, to, .. }
            | Edit::MasterGain { from, to }
            | Edit::MasterPan { from, to } => from == to,
            Edit::TrackMuted { from, to, .. }
            | Edit::TrackSolo { from, to, .. }
            | Edit::TrackSoloSafe { from, to, .. }
            | Edit::TrackArmed { from, to, .. }
            | Edit::TrackAlwaysLive { from, to, .. }
            | Edit::SendPreFader { from, to, .. }
            | Edit::DeviceBypass { from, to, .. }
            | Edit::DeviceActive { from, to, .. } => from == to,
            Edit::TrackOutput { from, to, .. } => from == to,
            Edit::WireFrom { from, to, .. } => from == to,
            Edit::WireChannel { from, to, .. } => from == to,
            Edit::MoveDevice { from, to, .. } => from == to,
            Edit::TrackInput { from, to, .. } => from == to,
            Edit::TrackMonitor { from, to, .. } => from == to,
            Edit::TrackClipType { from, to, .. } | Edit::MasterClipType { from, to } => from == to,
            Edit::ClipTiming { from, to, .. } => from == to,
            Edit::ClipFades { from, to, .. } => from == to,
            Edit::ClipWarping { from, to, .. } => from == to,
            Edit::ClipGain { from, to, .. } => from == to,
            Edit::NoteTiming { from, to, .. } => from == to,
            Edit::NoteKey { from, to, .. } => from == to,
            Edit::NoteVelocity { from, to, .. } => from == to,
            Edit::Controller { from, to, .. } => from == to,
            Edit::ClipName { from, to, .. }
            | Edit::TakeName { from, to, .. }
            | Edit::LocatorName { from, to, .. } => from == to,
            Edit::LocatorAt { from, to, .. } => from == to,
            Edit::PluginParam { from, to, .. } => from == to,
            Edit::PluginSettings { from, to, .. } => from == to,
            Edit::PluginInputs { from, to, .. } => from == to,
            Edit::PluginNoteOutput { from, to, .. } => from == to,
            Edit::PluginNoteTiming { from, to, .. } => from == to,
            Edit::NativeParam { from, to, .. } => from == to,
            Edit::NativeSample { from, to, .. } => from == to,
            Edit::LanePoints { from, to, .. } => from == to,
            Edit::LaneOverridden { from, to, .. } => from == to,
            Edit::MoveModulator { from, to, .. } => from == to,
            Edit::ModulatorName { from, to, .. } | Edit::ControlName { from, to, .. } => from == to,
            Edit::ModulatorParam { from, to, .. } => from == to,
            Edit::RoutingSettings { from, to, .. } => from == to,
        }
    }
}

/// A transaction that changed the project.
#[derive(Debug, Clone, PartialEq)]
pub struct Committed {
    /// What the Edit menu calls it. Undo and redo commit theirs as "Undo
    /// Tempo Up" and so on.
    pub name: String,
    /// In the order they were made.
    pub edits: Vec<Edit>,
}

#[cfg(test)]
mod tests {
    use musicable_model::plugin::{PluginDevice, PluginKind, PluginParam, PluginState};
    use musicable_model::time::Beats;
    use musicable_model::{PluginFormat, TrackKind};

    use super::*;

    #[test]
    fn transactions_record_their_edits() {
        let mut doc = Document::new(Project::new());
        assert!(!doc.is_modified());
        let region = BeatRange::new(Beats::new(4), Beats::new(8));
        doc.transact("Loop Bar 2", |tx| {
            tx.set_loop_region(region);
            tx.set_looping(true);
            assert!(tx.project().looping);
        });
        assert_eq!(doc.project().loop_region, region);
        assert!(doc.project().looping);
        assert!(doc.is_modified());

        let committed = doc.take_committed();
        assert_eq!(
            committed,
            [Committed {
                name: "Loop Bar 2".into(),
                edits: vec![
                    Edit::LoopRegion {
                        from: Project::new().loop_region,
                        to: region,
                    },
                    Edit::Looping {
                        from: false,
                        to: true,
                    },
                ],
            }]
        );
        assert_eq!(doc.take_committed(), []);
        doc.mark_saved();
        assert!(!doc.is_modified());
    }

    #[test]
    fn changes_the_project_follows_are_no_steps_in_the_undo_history() {
        let mut project = Project::new();
        let track = project.add_track("Synth", TrackKind::Midi);
        let synth = project.new_device(DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.example.synth".into(),
            name: "Synth".into(),
            kind: PluginKind::Instrument,
            inputs: Vec::new(),
            note_output: false,
            note_timing: musicable_model::plugin::NoteTiming::WithSound,
            note_timing_chosen: false,
            settings: PluginSettings::default(),
        }));
        project
            .track_mut(track)
            .unwrap()
            .devices
            .push(synth.clone());
        let mut doc = Document::new(project);
        doc.transact("Metronome On", |tx| tx.set_metronome(true));
        assert!(doc.undo());
        doc.take_committed();

        // The plugin saves a state of its own as it starts, say. The engine
        // follows it, but undo doesn't, and redo still can.
        let own = PluginSettings {
            state: Some(PluginState::new(*b"own")),
            params: Vec::new(),
        };
        doc.follow("Change Synth", |tx| {
            tx.set_plugin_settings(synth.id, own.clone());
        });
        let settings = |doc: &Document| {
            let (_, _, device) = doc.project().find_device(synth.id).unwrap();
            device.kind.plugin().unwrap().settings.clone()
        };
        assert_eq!(settings(&doc), own);
        let committed = doc.take_committed();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].name, "Change Synth");
        assert_eq!(doc.undo_name(), None);
        assert_eq!(doc.redo_name(), Some("Metronome On"));
        assert!(!doc.is_modified());
        assert!(doc.redo());
        assert!(doc.project().metronome);
        assert_eq!(settings(&doc), own);
    }

    #[test]
    fn a_transaction_that_changes_nothing_leaves_no_record() {
        let mut doc = Document::new(Project::new());
        doc.transact("Nothing", |tx| {
            let tempo = tx.project().tempo.clone();
            tx.set_tempo(tempo);
            tx.set_metronome(false);
        });
        assert_eq!(doc.take_committed(), []);
        assert!(!doc.is_modified());
        assert_eq!(doc.undo_name(), None);
    }

    #[test]
    fn edits_keep_the_values_they_replaced() {
        let mut doc = Document::new(Project::new());
        let mut tempo = TempoMap::default();
        tempo.set_tempo(90.0);
        doc.transact("Tempo", |tx| {
            tx.set_metronome(true);
            tx.set_tempo(tempo.clone());
            tx.set_metronome(false);
        });
        let edits = &doc.take_committed()[0].edits;
        assert_eq!(
            edits,
            &[
                Edit::Metronome {
                    from: false,
                    to: true,
                },
                Edit::Tempo {
                    from: TempoMap::default(),
                    to: tempo,
                },
                Edit::Metronome {
                    from: true,
                    to: false,
                },
            ]
        );
    }

    #[test]
    fn devices_go_in_once_with_settings_a_file_can_hold() {
        let mut project = Project::new();
        let track = project.add_track("Synth", TrackKind::Midi);
        let mut doc = Document::new(project);
        let synth = DeviceKind::Plugin(PluginDevice {
            format: PluginFormat::Clap,
            id: "org.example.synth".into(),
            name: "Synth".into(),
            kind: PluginKind::Instrument,
            inputs: Vec::new(),
            note_output: false,
            note_timing: musicable_model::plugin::NoteTiming::WithSound,
            note_timing_chosen: false,
            settings: PluginSettings {
                state: Some(PluginState::new(*b"state")),
                params: Vec::new(),
            },
        });
        let (synth, gain) = doc.transact("Add Devices", |tx| {
            let synth = tx.new_device(synth);
            tx.add_device(Chain::Track(track), 9, synth.clone());
            // Once only, and on a track that's there.
            tx.add_device(Chain::Master, 0, synth.clone());
            let gain = tx.new_device(DeviceKind::TestGain { gain: 0.5 });
            tx.add_device(Chain::Track(TrackId(7)), 0, gain.clone());
            tx.add_device(Chain::Master, 0, gain.clone());
            (synth.id, gain.id)
        });
        let project = doc.project();
        assert_eq!(project.tracks[0].devices[0].id, synth);
        assert_eq!(project.master.devices[0].id, gain);
        assert_eq!(project.find_device(synth).unwrap().0, Chain::Track(track));

        doc.transact("Change Synth", |tx| {
            tx.set_plugin_param(synth, 4, Some(0.25));
            tx.set_plugin_param(synth, 4, Some(f64::NAN));
            tx.set_plugin_param(synth, 2, Some(1.0));
            tx.set_plugin_param(synth, 2, None);
            // Not a plugin.
            tx.set_plugin_param(gain, 1, Some(1.0));
            let unordered = PluginSettings {
                state: None,
                params: vec![
                    PluginParam { id: 2, value: 0.0 },
                    PluginParam { id: 1, value: 0.0 },
                ],
            };
            tx.set_plugin_settings(synth, unordered);
        });
        let edits = &doc.take_committed()[1].edits;
        assert_eq!(edits.len(), 3, "{edits:?}");
        let plugin = doc.project().tracks[0].devices[0].kind.plugin().unwrap();
        assert_eq!(plugin.settings.params, [PluginParam { id: 4, value: 0.25 }]);

        let removed = doc.transact("Delete Synth", |tx| tx.remove_device(synth));
        assert_eq!(removed.map(|d| d.id), Some(synth));
        assert!(doc.project().tracks[0].devices.is_empty());
        assert!(doc.undo());
        let plugin = doc.project().tracks[0].devices[0].kind.plugin().unwrap();
        assert_eq!(plugin.settings.params, [PluginParam { id: 4, value: 0.25 }]);
    }
}
