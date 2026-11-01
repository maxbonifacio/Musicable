//! Agents, such as Claude Code, controlling the app over MCP: the server,
//! while the user lets them in, and the window's answers to their calls,
//! which run commands as a menu would. `docs/agents.md` describes them.

use std::path::{Path, PathBuf};

use eframe::egui;
use musicable_commands::{
    Args, BlindRound, Editor, ExportOptions, Keymap, Registry, Request, Steps, arrange,
};
use musicable_engine::Lead;
use musicable_mcp::pipe::Address;
use musicable_mcp::{
    CpuPart, CpuRow, Exporting, Host, Performance, PluginState, Readout, RenderingAhead, Reply,
    Run, Status,
};
use musicable_model::param::ParamSpec;
use musicable_model::time::Beats;
use musicable_model::{DeviceId, Project, TrackId};
use musicable_ui::Row;
use serde_json::json;

use super::{App, with_extension};
use crate::audio_files::clip_path;
use crate::blind_test;
use crate::export::{Report, Stage};
use crate::renders::RenderCache;

/// What an agent's command hears while the window asks the user something.
const ASKING: &str = "Musicable is waiting for the user to answer a question in its window. Try \
                      again once they have.";

/// The agents' side of the app.
pub(super) struct Agents {
    /// The server, while the user lets agents in.
    host: Option<Host>,
    /// Why Musicable can't let agents in, if it can't.
    problem: Option<String>,
    /// What agents last changed, for the status bar: the step their last
    /// command made, or what it did.
    change: Option<String>,
    /// The agent's New or Open that waits for the user to say whether to
    /// save their changes first.
    asking: Option<Reply>,
    /// The agent's export that's under way.
    exporting: Option<Reply>,
    /// The agent's measurement of a device's response that's under way.
    measuring: Option<Reply>,
    /// The command that adds Musicable to Claude Code.
    claude_command: String,
}

impl Agents {
    pub(super) fn new() -> Self {
        Self {
            host: None,
            problem: None,
            change: None,
            asking: None,
            exporting: None,
            measuring: None,
            claude_command: claude_command(),
        }
    }

    /// The agents connected, as their clients name themselves.
    pub(super) fn connected(&self) -> Vec<String> {
        self.host.as_ref().map(Host::agents).unwrap_or_default()
    }

    /// What the status bar says while agents are connected: "Agent: Move
    /// Notes", their last change, "Agent connected" before they make one,
    /// or "Agent using the mouse and keys" while `using_hands`.
    pub(super) fn status(&self, using_hands: bool) -> Option<String> {
        let who = match self.connected().len() {
            0 => return None,
            1 => "Agent".to_owned(),
            n => format!("{n} agents"),
        };
        if using_hands {
            return Some(format!("{who}: using the mouse and keys"));
        }
        Some(match &self.change {
            Some(change) => format!("{who}: {change}"),
            None => format!("{who} connected"),
        })
    }

    /// How the Agents page of the Settings window shows them.
    pub(super) fn page(
        &self,
        settings: &crate::settings::AgentSettings,
        using_hands: bool,
    ) -> crate::settings_window::Agents<'_> {
        crate::settings_window::Agents {
            enabled: settings.enabled,
            mouse_and_keys: settings.mouse_and_keys,
            using_hands,
            listening: self.host.as_ref().map(|host| host.address().to_string()),
            connected: self.connected(),
            problem: self.problem.as_deref(),
            claude_command: &self.claude_command,
        }
    }

    /// Forgets agents' last change once none is connected, so the next
    /// one starts afresh.
    pub(super) fn tick(&mut self) {
        if self.change.is_some() && self.connected().is_empty() {
            self.change = None;
        }
    }
}

impl App {
    /// Lets agents in from now on, or stops letting them in, and keeps that
    /// in the settings.
    pub(super) fn let_agents_in(&mut self, on: bool, ctx: &egui::Context) {
        self.settings.agents.enabled = on;
        self.save_settings();
        self.follow_agent_settings(ctx);
    }

    /// Lets agents use the mouse and keys from now on, or stops letting
    /// them, and keeps that in the settings.
    pub(super) fn let_agents_use_hands(&mut self, on: bool, ctx: &egui::Context) {
        self.settings.agents.mouse_and_keys = on;
        self.save_settings();
        self.follow_agent_settings(ctx);
    }

    /// Listens for agents while the settings let them in, and stops when
    /// they don't. Calls still waiting for an answer then get none.
    pub(super) fn follow_agent_settings(&mut self, ctx: &egui::Context) {
        let agents = &mut self.agents;
        if !(self.settings.agents.enabled && self.settings.agents.mouse_and_keys) {
            crate::agent_hands::stop(
                ctx,
                "The user stopped letting agents use the mouse and keys.",
            );
        }
        if !self.settings.agents.enabled {
            agents.host = None;
            agents.problem = None;
            agents.change = None;
            agents.asking = None;
            agents.exporting = None;
            agents.measuring = None;
            return;
        }
        if agents.host.is_some() {
            return;
        }
        let Some(dir) = &self.config_dir else {
            agents.problem = Some(
                "Musicable found no folder for its settings, so agents can't reach it.".to_owned(),
            );
            return;
        };
        let ctx = ctx.clone();
        let wake = move || ctx.request_repaint();
        let commands = self.session.commands();
        match Address::new(dir).and_then(|address| Host::start(address, commands, wake)) {
            Ok(host) => {
                agents.host = Some(host);
                agents.problem = None;
            }
            Err(error) => {
                let problem = format!("Agents can't control this Musicable: {error}.");
                self.messages.push(problem.clone());
                agents.problem = Some(problem);
            }
        }
    }

    /// Answers the calls agents made since the last frame, in order, after
    /// what the user asked for before them. It runs while the window is
    /// minimized too.
    pub(super) fn answer_agents(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        self.agents.tick();
        let Some(host) = &self.agents.host else {
            return;
        };
        let calls = host.calls();
        if calls.is_empty() {
            return;
        }
        self.handle_requests(ctx, frame);
        // The plugins' parameters, as of now.
        self.update_plugin_status();
        for call in calls {
            let mut agent = Agent {
                app: self,
                ctx,
                frame,
            };
            musicable_mcp::answer(call, &mut agent);
        }
    }

    /// Runs what an agent asked for, as a menu or a key runs it, and
    /// answers: at once, or once the user has said whether to save their
    /// changes, or once an export has finished. The command log says the
    /// agent ran it.
    fn run_for_agent(
        &mut self,
        run: Run,
        reply: Reply,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        let agent = reply.agent().to_owned();
        self.session.command_log_mut().set_by(Some(agent));
        self.run_as_agent(run, reply, ctx, frame);
        self.log_hands(ctx);
    }

    fn run_as_agent(&mut self, run: Run, reply: Reply, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.pending.is_some() {
            return reply.failed(ASKING);
        }
        let revision = self.session.revision();
        let stepping = run.steps_through_history();
        let done = match &run {
            Run::One(id, _) => (self.session.commands().get(id))
                .map(|command| command.title.trim_end_matches("...").to_owned()),
            Run::Batch(name, _) => Some(name.clone()),
        };
        match run {
            Run::One(id, args) => {
                let result = self.session.run_with(&id, &args);
                let ran = result.is_ok();
                let mut reply = Some(reply);
                for request in self.session.take_requests() {
                    if answers(&request) {
                        if let Some(reply) = reply.take() {
                            self.answer_request(request, reply, ctx, frame);
                        }
                    } else if request.for_agents() {
                        self.carry_out(request, ctx, frame);
                    }
                }
                if let Some(reply) = reply {
                    reply.ran(result, self.step_since(revision, stepping).as_deref());
                }
                if ran {
                    self.note_change(revision, stepping, done);
                }
            }
            Run::Batch(name, commands) => {
                let result = self.session.run_batch(&name, &commands);
                let ran = result.is_ok();
                // Commands that run alone, as those that ask for files or
                // change settings do, can't be part of a batch.
                for request in self.session.take_requests() {
                    if request.for_agents() && !answers(&request) {
                        self.carry_out(request, ctx, frame);
                    }
                }
                reply.ran_batch(result, self.step_since(revision, stepping).as_deref());
                if ran {
                    self.note_change(revision, stepping, done);
                }
            }
        }
    }

    /// The name of the step in the undo history that the commands run
    /// since the session's `revision` made, if they made one. Stepping
    /// through the history makes none.
    fn step_since(&self, revision: u64, stepping: bool) -> Option<String> {
        let made = self.session.revision() != revision && !stepping;
        made.then(|| self.session.undo_name().map(str::to_owned))
            .flatten()
    }

    /// Keeps what an agent just did for the status bar: the step it made,
    /// or else what it ran, `done`.
    fn note_change(&mut self, revision: u64, stepping: bool, done: Option<String>) {
        if let Some(change) = self.step_since(revision, stepping).or(done) {
            self.agents.change = Some(change);
        }
    }

    /// Carries out a request an agent's command made that answers it: for
    /// a file, or to change a setting.
    fn answer_request(
        &mut self,
        request: Request,
        reply: Reply,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        let answer = match request {
            Request::SetSetting { key, value } => self.set_setting(&key, &value, ctx, frame),
            Request::RecordOutput(file) if !file.is_absolute() => Err(format!(
                "{} isn't a full path: give the file's full path, from the root of its drive.",
                file.display()
            )),
            Request::RecordOutput(file) => self.session.record_output(&file),
            Request::StopRecordingOutput => self.session.stop_recording_output(),
            Request::MakeBlindTest(round) => match whole_paths(&round) {
                Some(path) => Err(format!(
                    "{} isn't a full path: give each file's full path, from the root of its \
                     drive.",
                    path.display()
                )),
                None => blind_test::make(&super::making(round)),
            },
            Request::OpenBlindTest(None) => Err("Give the round's folder.".into()),
            Request::OpenBlindTest(Some(folder)) if !folder.is_absolute() => Err(format!(
                "{} isn't a full path: give the folder's full path, from the root of its drive.",
                folder.display()
            )),
            Request::OpenBlindTest(Some(folder)) => {
                (self.blind_test.open(&folder, &mut self.session)).map(|round| {
                    json!({
                        "folder": folder,
                        "name": round.file.name,
                        "trials": round.file.trials.len(),
                        "judged": round.judged_count(),
                        "unsealed": round.results.is_some(),
                    })
                })
            }
            Request::MeasureResponse(probing) => {
                match self.measure_response(probing, ctx) {
                    Ok(()) => self.agents.measuring = Some(reply),
                    Err(why) => reply.failed(full_stop(why)),
                }
                return;
            }
            Request::SetAudioDevice(device) => self.set_audio_device(device),
            Request::SetKeys { command, keys } => self.set_keys(&command, keys),
            Request::ResetKeys(command) => self.reset_keys(&command),
            Request::SaveWorkspaceAs(Some(name)) => {
                (self.save_workspace_as(&name)).map(|()| json!({ "workspace": name }))
            }
            Request::RenameWorkspace(Some(name)) => {
                (self.rename_workspace(&name)).map(|()| json!({ "workspace": name }))
            }
            Request::SaveWorkspaceAs(None) | Request::RenameWorkspace(None) => {
                Err("Give the workspace its name.".into())
            }
            request => return self.file_for_agent(request, reply, ctx),
        };
        match answer {
            Ok(answer) => reply.done(json!({ "result": answer })),
            Err(why) => reply.failed(full_stop(why)),
        }
    }

    /// Carries out a request for a file an agent's command made, and
    /// answers. New and Open ask whether to save the changes first, in the
    /// window, and answer once the user says.
    fn file_for_agent(&mut self, request: Request, reply: Reply, ctx: &egui::Context) {
        if let Some(path) = relative_path(&request) {
            return reply.failed(format!(
                "{} isn't a full path: give the file's full path, from the root of its drive.",
                path.display()
            ));
        }
        match request {
            Request::New | Request::NewDemo | Request::Open(Some(_))
                if self.session.is_modified() =>
            {
                self.pending = Some(request);
                self.agents.asking = Some(reply);
                // The question waits in the window, which may be behind
                // others or minimized.
                let attention = egui::UserAttentionType::Informational;
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(attention));
            }
            Request::New | Request::NewDemo | Request::Open(Some(_)) => {
                self.replace_for_agent(request, reply)
            }
            Request::Save => match self.path.clone() {
                Some(path) => saved(reply, self.save_to(path)),
                None => reply.failed(
                    "The project has no file yet: save it with file.save_as, giving a file.",
                ),
            },
            Request::SaveAs(Some(path)) => saved(reply, self.save_to(with_extension(path))),
            Request::ImportAudio {
                files: Some(files),
                track,
                at,
            } => self.import_for_agent(files, track, at, reply),
            Request::ExportAudio {
                file: Some(file),
                options,
            } => self.export_for_agent(file, &options, reply, ctx),
            // The file commands' arguments for files are ones agents must
            // give.
            _ => reply.failed("Give the command its file."),
        }
    }

    /// Starts a new project, or opens one, for an agent, once the changes
    /// are saved or the user let them go, and answers.
    pub(super) fn replace_for_agent(&mut self, request: Request, reply: Reply) {
        match request {
            Request::New => self.open_project(Project::new(), None),
            Request::NewDemo => self.open_project(crate::demo_project(), None),
            Request::Open(Some(path)) => {
                if let Err(why) = self.open_file(path.clone()) {
                    return reply.failed(full_stop(why));
                }
                return reply.done(json!({ "result": path }));
            }
            _ => return reply.failed("Give the command its file."),
        }
        reply.done(json!({ "result": null }));
    }

    /// Reads the audio files at `paths`, and puts them on `track` from
    /// `at`, as Import Audio does, for an agent, which hears the clips
    /// made: all of them, or none if a file can't be read.
    fn import_for_agent(
        &mut self,
        paths: Vec<PathBuf>,
        track: Option<TrackId>,
        at: Option<Beats>,
        reply: Reply,
    ) {
        let folder = self.project_folder().map(Path::to_owned);
        let mut files = Vec::new();
        for path in paths {
            let name = clip_path(&path, folder.as_deref());
            match self.audio_files.read_now(&name, &path) {
                Ok(file) => {
                    self.session.editor_mut().files.insert(name.clone(), file);
                    files.push(name);
                }
                Err(error) => {
                    return reply.failed(full_stop(format!(
                        "Couldn't read the audio file {}: {error}. Nothing changed",
                        path.display()
                    )));
                }
            }
        }
        let mut args = Args::new().with("files", files);
        if let Some(track) = track {
            args.set("track", track);
        }
        if let Some(at) = at {
            args.set("at", at);
        }
        let revision = self.session.revision();
        let result = self.session.run_with(&arrange::INSERT_AUDIO, &args);
        reply.ran(result, self.step_since(revision, false).as_deref());
    }

    /// Renders the mix to `file` for an agent, as a script would: see
    /// [`App::export_mix_to`]. The agent hears the file once it's written.
    fn export_for_agent(
        &mut self,
        file: PathBuf,
        options: &ExportOptions,
        reply: Reply,
        ctx: &egui::Context,
    ) {
        match self.export_audio_to(&file, options, ctx) {
            Ok(()) => self.agents.exporting = Some(reply),
            Err(why) => reply.failed(why),
        }
    }

    /// Answers the agent whose measurement of a device's response just
    /// finished with `result`, if an agent asked for it. Returns whether
    /// one did.
    pub(super) fn measured_for_agent(
        &mut self,
        result: &Result<serde_json::Value, String>,
    ) -> bool {
        let Some(reply) = self.agents.measuring.take() else {
            return false;
        };
        match result {
            Ok(result) => reply.done(json!({ "result": result })),
            Err(why) => reply.failed(full_stop(why.clone())),
        }
        true
    }

    /// Answers the agent whose export just finished, as `report` says it
    /// went, if an agent asked for it.
    pub(super) fn exported_for_agent(&mut self, report: &Report) {
        let Some(reply) = self.agents.exporting.take() else {
            return;
        };
        if let Some(failed) = &report.failed {
            return reply.failed(full_stop(format!("The export stopped: {failed}")));
        }
        if report.cancelled {
            return reply.failed("The user cancelled the export.");
        }
        let files: Vec<_> = report.files.iter().map(|file| &file.path).collect();
        let mut answer = json!({ "result": files });
        if !report.problems.is_empty() {
            answer["problems"] = json!(report.problems);
        }
        reply.done(answer);
    }

    /// Answers the agent whose New or Open waited for the user to say
    /// whether to save their changes, if one did: carried out if
    /// `go_ahead`. Returns whether one did.
    pub(super) fn answered_for_agent(&mut self, request: Request, go_ahead: bool) -> bool {
        let Some(reply) = self.agents.asking.take() else {
            return false;
        };
        match go_ahead {
            true => self.replace_for_agent(request, reply),
            false => reply.failed("The user kept the project open, with its unsaved changes."),
        }
        true
    }
}

/// The window as the tools see it, while it answers agents' calls.
struct Agent<'a> {
    app: &'a mut App,
    ctx: &'a egui::Context,
    frame: &'a eframe::Frame,
}

impl musicable_mcp::Window for Agent<'_> {
    fn commands(&self) -> &Registry {
        self.app.session.commands()
    }

    fn keymap(&self) -> &Keymap {
        self.app.session.keymap()
    }

    fn project(&self) -> &Project {
        self.app.session.project()
    }

    fn editor(&self) -> &Editor {
        self.app.session.editor()
    }

    fn history(&self) -> Steps<'_> {
        self.app.session.history()
    }

    fn file(&self) -> Option<&Path> {
        self.app.path.as_deref()
    }

    fn is_modified(&self) -> bool {
        self.app.session.is_modified()
    }

    fn plugin_params(&self, device: DeviceId) -> Option<Vec<(u32, ParamSpec)>> {
        let listed = self.app.plugin_params.get(&device)?;
        Some(
            (listed.params.iter())
                .map(|param| (param.id, param.spec.clone()))
                .collect(),
        )
    }

    fn performance(&self) -> Performance {
        let (output, figures) = (&self.app.output, self.app.performance.performance());
        let rows = (figures.busiest().into_iter())
            .map(|(row, cpu)| CpuRow {
                part: match row {
                    Row::Device(device) => CpuPart::Device(device),
                    Row::Clips(track) => CpuPart::Clips(track),
                    Row::Modulators(chain) => CpuPart::Modulators(chain),
                    Row::Mixer(chain) => CpuPart::Mixer(chain),
                    Row::Metronome => CpuPart::Metronome,
                },
                average: cpu.average,
                longest: cpu.longest,
                ahead: cpu.ahead,
            })
            .collect();
        let render_ahead =
            output
                .lead_lately()
                .zip(output.info())
                .and_then(|((setting, lately), info)| {
                    let setting = match setting {
                        Lead::Off => return None,
                        Lead::Short => "short",
                        Lead::Normal => "normal",
                        Lead::Long => "long",
                    };
                    let ms = |frames| (frames as f64 * 1000.0 / f64::from(info.sample_rate)) as f32;
                    Some(RenderingAhead {
                        setting,
                        ahead: lately.ahead,
                        lead_ms: ms(lately.lead),
                        target_ms: ms(lately.target),
                        load: figures.ahead,
                        short_blocks: output.short_blocks(),
                    })
                });
        Performance {
            playing: output.info().is_some(),
            audio: output.summary(),
            load: output.load(),
            dropouts: output.dropouts(),
            render_ahead,
            rows,
            total: figures.total,
        }
    }

    fn status(&self) -> Status {
        let app = &self.app;
        let session = &app.session;
        let heard = session.heard_position(app.output.latency());
        let rate = session.engine_config().sample_rate;
        let takes = (super::recordings(session).into_iter())
            .map(|recording| (recording.track, recording.range))
            .collect();
        let question = (app.pending.as_ref()).map(|request| {
            let before = match request {
                Request::New | Request::NewDemo => " before starting a new project",
                Request::Open(_) => " before opening another project",
                Request::Quit => " before quitting",
                _ => "",
            };
            format!("Save the changes to {}{before}?", app.name())
        });
        let export = (app.export.as_ref()).map(|export| {
            let stage = export.stage();
            let (name, pass, passes, speed) = match stage {
                Stage::Preparing { pass, passes } => ("preparing", pass, passes, None),
                Stage::Rendering {
                    pass,
                    passes,
                    speed,
                    ..
                } => ("rendering", pass, passes, Some(speed)),
                Stage::Normalising { pass, passes, .. } => ("normalising", pass, passes, None),
                Stage::Finished => ("finished", 0, 0, None),
            };
            Exporting {
                stage: name,
                pass,
                passes,
                done: stage.fraction(),
                speed,
            }
        });
        let library = session.library();
        let folders = |folders: Vec<(PathBuf, usize)>| {
            (folders.into_iter())
                .map(|(folder, files)| (folder.display().to_string(), files))
                .collect()
        };
        Status {
            playing: session.is_playing(),
            recording: session.is_recording(),
            playhead: session.beats_at(heard),
            seconds: heard as f64 / f64::from(rate),
            takes,
            question,
            analysing: app.analyses.analysing(),
            rendering: RenderCache::global().pending(),
            compiling: session.compiling(),
            export,
            scanning_plugins: app.plugin_scan.is_running(),
            scanning_folders: folders(library.scanning()),
            reading_folders: folders(library.reading()),
            finding_presets: library.is_finding_presets(),
            revision: session.revision(),
        }
    }

    fn plugin_state(&self, device: DeviceId) -> Option<PluginState> {
        let status = self.app.plugin_status.get(&device)?;
        let readouts = match self.app.session.plugin(device) {
            Some(instance) => (instance.params().iter())
                .filter(|param| param.read_only && !param.hidden)
                .filter_map(|param| {
                    let value = instance.param_value(param.id)?;
                    Some(Readout {
                        param: param.id,
                        name: param.name.clone(),
                        value,
                        text: instance.value_to_text(param.id, value),
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        Some(PluginState {
            loaded: status.loaded,
            problem: status.problem.clone(),
            has_window: status.has_window,
            window_open: status.window_open,
            readouts,
        })
    }

    fn plugins(&self, filter: Option<&str>) -> serde_json::Value {
        self.app.plugins_for_agent(filter)
    }

    fn command_log(&self, since: Option<u64>, last: usize) -> serde_json::Value {
        self.app.session.command_log().read(since, last)
    }

    fn layout(&self) -> serde_json::Value {
        self.app.layout_for_agent()
    }

    fn levels(&self) -> serde_json::Value {
        self.app.levels_for_agent()
    }

    fn settings(&self) -> serde_json::Value {
        self.app.settings_for_agent()
    }

    fn audio_devices(&mut self) -> serde_json::Value {
        self.app.audio_devices_for_agent()
    }

    fn messages(&self) -> Vec<String> {
        self.app.messages.clone()
    }

    fn clear_messages(&mut self) {
        self.app.messages.clear();
    }

    fn run(&mut self, run: Run, reply: Reply) {
        self.app.run_for_agent(run, reply, self.ctx, self.frame);
    }

    fn windows(&self) -> serde_json::Value {
        self.app.windows_for_agent(self.ctx)
    }

    fn screenshot(&mut self, shot: musicable_mcp::Shot, reply: Reply) {
        self.app.screenshot_for_agent(shot, reply, self.ctx);
    }

    fn ui_tree(&mut self, tree: musicable_mcp::Tree, reply: Reply) {
        self.app.ui_tree_for_agent(tree, reply, self.ctx);
    }

    fn use_mouse_and_keys(&mut self, hands: musicable_mcp::Hands, reply: Reply) {
        self.app.hands_for_agent(hands, reply, self.ctx);
    }

    fn play_midi(&mut self, messages: Vec<(f64, [u8; 3])>, reply: Reply) {
        // Off the window's thread, which it would hold for as long as it
        // plays.
        std::thread::spawn(move || match musicable_audio::play_agent_midi(&messages) {
            Ok(()) => reply.done(serde_json::json!({
                "messages": messages.len(),
                "seconds": messages.last().map_or(0.0, |(at, _)| *at),
            })),
            Err(why) => reply.failed(why),
        });
    }
}

/// Whether the app answers the agent whose command made `request`, rather
/// than the command: it's for a file, one an agent's command gives, where
/// the user's would ask for one, or it changes a setting, which says what
/// it was.
fn answers(request: &Request) -> bool {
    matches!(
        request,
        Request::New
            | Request::NewDemo
            | Request::Open(_)
            | Request::Save
            | Request::SaveAs(_)
            | Request::ImportAudio { .. }
            | Request::ExportAudio { .. }
            | Request::SetSetting { .. }
            | Request::SetAudioDevice(_)
            | Request::SetKeys { .. }
            | Request::ResetKeys(_)
            | Request::SaveWorkspaceAs(_)
            | Request::RenameWorkspace(_)
            | Request::RecordOutput(_)
            | Request::StopRecordingOutput
            | Request::MeasureResponse(_)
            | Request::MakeBlindTest(_)
            | Request::OpenBlindTest(_)
    )
}

/// The first path in `request` that isn't a full one, if there's one:
/// agents' paths can't depend on where Musicable was started from.
fn relative_path(request: &Request) -> Option<&Path> {
    let paths: Vec<&Path> = match request {
        Request::Open(Some(path))
        | Request::SaveAs(Some(path))
        | Request::ExportAudio {
            file: Some(path), ..
        } => {
            vec![path]
        }
        Request::ImportAudio {
            files: Some(files), ..
        } => files.iter().map(PathBuf::as_path).collect(),
        _ => Vec::new(),
    };
    paths.into_iter().find(|path| !path.is_absolute())
}

/// The first of a round's paths that isn't a full one, if there's one.
fn whole_paths(round: &BlindRound) -> Option<&Path> {
    let folder = std::iter::once(&round.folder);
    let mut paths = folder.chain(&round.first).chain(&round.second);
    paths.find(|path| !path.is_absolute()).map(PathBuf::as_path)
}

/// Answers `reply` with the file the project was saved to, or why it
/// wasn't.
fn saved(reply: Reply, saved: Result<PathBuf, String>) {
    match saved {
        Ok(path) => reply.done(json!({ "result": path })),
        Err(why) => reply.failed(full_stop(why)),
    }
}

/// `text`, ending with a full stop.
fn full_stop(mut text: String) -> String {
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

/// The command that adds Musicable to Claude Code, with this binary's
/// path, and the config folder if `MUSICABLE_CONFIG_DIR` picks it.
fn claude_command() -> String {
    let exe = std::env::current_exe().ok();
    let exe = exe.map_or("musicable".to_owned(), |exe| {
        format!("\"{}\"", exe.display())
    });
    let env = std::env::var_os("MUSICABLE_CONFIG_DIR").filter(|dir| !dir.is_empty());
    let env = env.map_or(String::new(), |dir| {
        format!("-e \"MUSICABLE_CONFIG_DIR={}\" ", Path::new(&dir).display())
    });
    format!("claude mcp add musicable {env}-- {exe} mcp")
}
