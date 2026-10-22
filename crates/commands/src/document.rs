//! The open project, and the transactions that change it.
//!
//! Commands change the project only through [`Document::transact`]. A
//! transaction records each [`Edit`] it makes along with the value it
//! replaced, so everything that has to follow the project hooks in here: the
//! engine and the undo history now, ARA change notifications in Tier 2.

use musicable_model::clip::{Clip, ClipContent, ClipId, Fades, Timing};
use musicable_model::mixer::Bus;
use musicable_model::native::NativeKind;
use musicable_model::plugin::PluginSettings;
use musicable_model::time::{BeatRange, Beats, TempoMap};
use musicable_model::{
    Chain, ClipType, Device, DeviceId, DeviceKind, Input, Locator, LocatorId, Monitor, Project,
    Send, SendId, Take, TakeId, Track, TrackId, TrackKind,
};

use crate::editor::Editor;
use crate::history::History;

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
}

impl Document {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            editor: Editor::default(),
            committed: Vec::new(),
            history: History::default(),
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

    /// The name of the step [`undo`](Self::undo) would take back, for the
    /// Edit menu, or `None` if there's nothing to undo.
    pub fn undo_name(&self) -> Option<&str> {
        self.history.undo_name()
    }

    /// The name of the step [`redo`](Self::redo) would make again, or `None`.
    pub fn redo_name(&self) -> Option<&str> {
        self.history.redo_name()
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
}

impl<'a> Transaction<'a> {
    fn new(project: &'a mut Project, editor: &'a Editor) -> Self {
        Self {
            project,
            editor,
            edits: Vec::new(),
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
    pub fn remove_device(&mut self, device: DeviceId) -> Option<Device> {
        let (chain, index, _) = self.project.find_device(device)?;
        let device = self.project.devices_mut(chain)?.remove(index);
        self.edits.push(Edit::RemoveDevice {
            chain,
            index,
            device: device.clone(),
        });
        Some(device)
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
    /// first, each as an edit of its own: tracks feeding it feed the master
    /// instead, and sends to it go.
    pub fn remove_track(&mut self, id: TrackId) -> Option<Track> {
        let index = self.project.track_index(id)?;
        let feeding: Vec<TrackId> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.output == Bus::Group(id))
            .map(|t| t.id)
            .collect();
        for track in feeding {
            self.set_track_output(track, Bus::Master);
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
    /// the track, any input on a bus, and one a file couldn't hold (a stereo
    /// audio input from the last channel there can be, a MIDI channel past
    /// 15) do nothing.
    pub fn set_track_input(&mut self, track: TrackId, input: Option<Input>) {
        let Some(kind) = self.project.track(track).map(|t| t.kind) else {
            return;
        };
        let fits = match &input {
            None => true,
            Some(Input::Audio(audio)) => {
                kind == TrackKind::Audio && !(audio.stereo && audio.channel == u16::MAX)
            }
            Some(Input::Midi(midi)) => {
                kind == TrackKind::Midi && midi.channel.is_none_or(|channel| channel <= 15)
            }
        };
        if !fits {
            return;
        }
        if let Some(from) = self.replace(track, |t| &mut t.input, input.clone()) {
            self.edits.push(Edit::TrackInput {
                track,
                from,
                to: input,
            });
        }
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

    /// Feeds the track's output to `output`, if the track can feed it: see
    /// [`Project::can_feed`]. Otherwise it does nothing.
    pub fn set_track_output(&mut self, track: TrackId, output: Bus) {
        if !self.project.can_feed(track, output) {
            return;
        }
        if let Some(from) = self.replace(track, |t| &mut t.output, output) {
            self.edits.push(Edit::TrackOutput {
                track,
                from,
                to: output,
            });
        }
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
    /// [`Project::can_send`]. Otherwise it does nothing.
    pub fn add_send(&mut self, track: TrackId, index: usize, send: Send) {
        if !self.project.can_send(track, send.to) {
            return;
        }
        let Some(t) = self.project.track_mut(track) else {
            return;
        };
        let index = index.min(t.sends.len());
        t.sends.insert(index, send);
        self.edits.push(Edit::AddSend { track, index, send });
    }

    pub fn remove_send(&mut self, track: TrackId, send: SendId) {
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

    /// Makes the change `edit` describes, through the setter for what it
    /// changes, as undo and redo do: `apply(&edit.inverse())` takes it back.
    pub fn apply(&mut self, edit: &Edit) {
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
            Edit::TrackInput { track, to, .. } => self.set_track_input(*track, to.clone()),
            Edit::TrackMonitor { track, to, .. } => self.set_track_monitor(*track, *to),
            Edit::TrackOutput { track, to, .. } => self.set_track_output(*track, *to),
            Edit::TrackClipType { track, to, .. } => self.set_track_clip_type(*track, *to),
            Edit::AddSend { track, index, send } => self.add_send(*track, *index, *send),
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
            Edit::PluginParam {
                device, param, to, ..
            } => self.set_plugin_param(*device, *param, *to),
            Edit::PluginSettings { device, to, .. } => {
                self.set_plugin_settings(*device, to.clone())
            }
            Edit::NativeParam {
                device, param, to, ..
            } => self.set_native_param(*device, *param, *to),
            Edit::NativeSample { device, to, .. } => self.set_native_sample(*device, to.clone()),
        }
    }
}

/// An audio clip's fades. MIDI clips have none.
fn fades_of(clip: &Clip) -> Option<Fades> {
    match &clip.content {
        ClipContent::Audio(audio) => Some(audio.fades()),
        ClipContent::Midi(_) => None,
    }
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
            // Adding and removing never merge: each is a step of its own.
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
                | Edit::AddTake { .. }
                | Edit::RemoveTake { .. }
                | Edit::TakeName { .. }
                | Edit::AddLocator { .. }
                | Edit::RemoveLocator { .. }
                | Edit::LocatorAt { .. }
                | Edit::LocatorName { .. }
                | Edit::AddDevice { .. }
                | Edit::RemoveDevice { .. }
                | Edit::PluginParam { .. }
                | Edit::PluginSettings { .. }
                | Edit::NativeParam { .. }
                | Edit::NativeSample { .. },
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
            | Edit::AddTake { .. }
            | Edit::RemoveTake { .. }
            | Edit::AddLocator { .. }
            | Edit::RemoveLocator { .. }
            | Edit::AddDevice { .. }
            | Edit::RemoveDevice { .. } => false,
            Edit::TrackName { from, to, .. } => from == to,
            Edit::TrackGain { from, to, .. }
            | Edit::TrackPan { from, to, .. }
            | Edit::SendLevel { from, to, .. }
            | Edit::MasterGain { from, to }
            | Edit::MasterPan { from, to } => from == to,
            Edit::TrackMuted { from, to, .. }
            | Edit::TrackSolo { from, to, .. }
            | Edit::TrackSoloSafe { from, to, .. }
            | Edit::TrackArmed { from, to, .. }
            | Edit::SendPreFader { from, to, .. } => from == to,
            Edit::TrackOutput { from, to, .. } => from == to,
            Edit::TrackInput { from, to, .. } => from == to,
            Edit::TrackMonitor { from, to, .. } => from == to,
            Edit::TrackClipType { from, to, .. } | Edit::MasterClipType { from, to } => from == to,
            Edit::ClipTiming { from, to, .. } => from == to,
            Edit::ClipFades { from, to, .. } => from == to,
            Edit::ClipName { from, to, .. }
            | Edit::TakeName { from, to, .. }
            | Edit::LocatorName { from, to, .. } => from == to,
            Edit::LocatorAt { from, to, .. } => from == to,
            Edit::PluginParam { from, to, .. } => from == to,
            Edit::PluginSettings { from, to, .. } => from == to,
            Edit::NativeParam { from, to, .. } => from == to,
            Edit::NativeSample { from, to, .. } => from == to,
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
    use musicable_model::plugin::{PluginDevice, PluginParam, PluginState};
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
            instrument: true,
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
