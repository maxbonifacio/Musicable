//! An agent using the mouse and keys in a window, while the user lets it:
//! its steps become egui input, frame by frame, at the user's speed, so the
//! system's pointer never moves. It backs off the moment the user touches
//! the mouse or keys in Musicable.
//!
//! An egui plugin feeds the steps into the input of the window they're
//! for. The main window's input goes through the app's raw input hook
//! first, which feeds it there, so the keymap sees the agent's keys as it
//! sees the user's.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, DroppedFile, Event, FullOutput, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2,
    RawInput, Rect, TouchPhase, Vec2, ViewportId,
};
use musicable_commands::{KeyChord, Modifiers};
use musicable_mcp::{Button, Reply, Step, Target};
use serde_json::{Value as Json, json};

use crate::ui_tree::{self, Ask};

/// How long the user must leave Musicable alone before an agent may use
/// the mouse and keys again.
pub(crate) const IDLE: Duration = Duration::from_secs(3);

/// How long a step waits for the window's UI tree, to find a node.
const TREE_WAIT: Duration = Duration::from_secs(2);

/// The most frames a call gives back.
const MOST_FRAMES: usize = 300;

/// The egui plugin that feeds an agent's mouse and keys into the windows.
#[derive(Default)]
pub(crate) struct AgentHands {
    script: Option<Script>,
    /// When the user last used the mouse or keys in Musicable.
    user: Option<Instant>,
    /// Whether the app's raw input hook fed the main window's input this
    /// frame, so the plugin doesn't again.
    fed_main: bool,
    /// The window whose pass is ending, whose output comes next.
    ending: Option<ViewportId>,
    /// Whether that window's pass began with AccessKit on.
    began_on: HashMap<ViewportId, bool>,
}

/// The agent's steps, under way.
struct Script {
    viewport: ViewportId,
    ops: VecDeque<Op>,
    steps: usize,
    done: usize,
    started: Instant,
    /// What's under way.
    current: Option<Current>,
    /// Where the pointer is, in points, once a step has put it somewhere.
    pointer: Option<Pos2>,
    held: Modifiers,
    down: Vec<PointerButton>,
    /// The nodes' places in the window, as of the last frame drawn with
    /// its whole tree, by id.
    places: HashMap<u64, Rect>,
    /// When the tree was first wanted and not there yet.
    wanting: Option<Instant>,
    watch: Option<Ask>,
    frames: Vec<Json>,
    last_frame: Option<Json>,
    /// Frames left to draw once the steps are done, so the window shows
    /// what they did before the answer.
    settling: Option<u8>,
    reply: Reply,
}

/// What a step does, a frame at a time.
#[derive(Debug, Clone)]
enum Op {
    MoveTo(Target, f32),
    Button(PointerButton, bool),
    Key(egui::Key, Modifiers, bool),
    Hold(Modifiers),
    Release(Modifiers),
    Text(String),
    Scroll(Vec2),
    Drop(Vec<PathBuf>),
    Wait(f32),
    /// Waits for the next frame.
    NextFrame,
    /// A step is done.
    StepDone,
}

/// What's under way over several frames.
#[derive(Clone, Copy)]
enum Current {
    Moving {
        from: Pos2,
        to: Pos2,
        start: Instant,
        seconds: f32,
    },
    Waiting(Instant),
    NextFrame,
}

/// Starts the agent's `steps` in the window `viewport`, reading `watch` of
/// it each frame, and answers `reply` once they're done, or says why they
/// can't be.
pub(crate) fn start(
    ctx: &egui::Context,
    viewport: ViewportId,
    steps: Vec<Step>,
    watch: Option<Ask>,
    reply: Reply,
) {
    let hands = ctx.plugin_or_default::<AgentHands>();
    let mut hands = hands.lock();
    if hands.script.is_some() {
        return reply
            .failed("An agent is using the mouse and keys already: wait for it to finish.");
    }
    if hands.user.is_some_and(|user| user.elapsed() < IDLE) {
        return reply.failed(format!(
            "The user is using Musicable: try again once they've left it alone for {} seconds.",
            IDLE.as_secs()
        ));
    }
    let count = steps.len();
    let ops = match ops(steps) {
        Ok(ops) => ops,
        Err(why) => return reply.failed(why),
    };
    let viewport = ui_tree::drawn_in(ctx, viewport);
    ui_tree::hold(ctx, true);
    hands.script = Some(Script {
        viewport,
        ops,
        steps: count,
        done: 0,
        started: Instant::now(),
        current: None,
        pointer: None,
        held: Modifiers::NONE,
        down: Vec::new(),
        places: HashMap::new(),
        wanting: None,
        watch,
        frames: Vec::new(),
        last_frame: None,
        settling: None,
        reply,
    });
    ctx.request_repaint_of(viewport);
}

/// Whether an agent is using the mouse and keys.
pub(crate) fn busy(ctx: &egui::Context) -> bool {
    agent(ctx).is_some()
}

/// The agent using the mouse and keys, as its client names itself, if one
/// is.
pub(crate) fn agent(ctx: &egui::Context) -> Option<String> {
    let hands = ctx.plugin_opt::<AgentHands>()?;
    let hands = hands.lock();
    Some(hands.script.as_ref()?.reply.agent().to_owned())
}

/// Stops the agent using the mouse and keys, if one is, saying `why`.
pub(crate) fn stop(ctx: &egui::Context, why: &str) {
    let Some(hands) = ctx.plugin_opt::<AgentHands>() else {
        return;
    };
    let script = hands.lock().script.take();
    if let Some(script) = script {
        // The buttons it held go up as the user's next move or click comes.
        ui_tree::hold(ctx, false);
        script.reply.failed(why.to_owned());
    }
}

/// Feeds the agent's input for this frame into the main window's `input`,
/// from the app's raw input hook, so the keymap sees its keys.
pub(crate) fn feed_main(ctx: &egui::Context, input: &mut RawInput) {
    let hands = ctx.plugin_or_default::<AgentHands>();
    let mut hands = hands.lock();
    hands.feed(ctx, input);
    hands.fed_main = true;
}

impl egui::Plugin for AgentHands {
    fn debug_name(&self) -> &'static str {
        "Musicable's agent mouse and keys"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut RawInput) {
        if input.viewport_id == ViewportId::ROOT && std::mem::take(&mut self.fed_main) {
            return;
        }
        self.feed(ctx, input);
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx();
        let on = ctx.has_accesskit_node(egui::accesskit_root_id());
        self.began_on.insert(ctx.viewport_id(), on);
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        self.ending = Some(ui.ctx().viewport_id());
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut FullOutput) {
        let Some(viewport) = self.ending.take() else {
            return;
        };
        let Some(script) = &mut self.script else {
            return;
        };
        if script.viewport != viewport {
            return;
        }
        let whole = self.began_on.get(&viewport).copied().unwrap_or(false);
        if whole && let Some(update) = &output.platform_output.accesskit_update {
            script.places = places(update);
            script.record(update);
        }
        if let Some(settling) = &mut script.settling {
            if *settling > 0 {
                *settling -= 1;
            } else if let Some(script) = self.script.take() {
                ui_tree::hold(ctx, false);
                script.finish();
                return;
            }
        }
        ctx.request_repaint_of(viewport);
    }
}

impl AgentHands {
    /// Notes the user's own input in `input`, which stops the agent's, and
    /// adds the agent's for this frame if `input` is for its window.
    fn feed(&mut self, ctx: &egui::Context, input: &mut RawInput) {
        if users(input) {
            self.user = Some(Instant::now());
            if let Some(script) = self.script.take() {
                ui_tree::hold(ctx, false);
                // Let go of what the agent held, before the user's input.
                if input.viewport_id == script.viewport {
                    let lets_go = script.letting_go();
                    input.events.splice(0..0, lets_go);
                }
                let why = format!(
                    "The user took over: they used the mouse or keys in Musicable, so the \
                     agent's input stopped after {} of {} steps. Try again once they've left it \
                     alone for {} seconds.",
                    script.done,
                    script.steps,
                    IDLE.as_secs()
                );
                script.reply.failed(why);
            }
            return;
        }
        let Some(script) = &mut self.script else {
            return;
        };
        if input.viewport_id != script.viewport {
            return;
        }
        if script.reply.is_closed() {
            let script = self.script.take().expect("there's a script");
            input.events.extend(script.letting_go());
            ui_tree::hold(ctx, false);
            return;
        }
        let scale = ctx.input_for(script.viewport, |input| input.pixels_per_point());
        match script.run(scale) {
            Ok((events, dropped)) => {
                input.events.extend(events);
                (input.dropped_files)
                    .extend(dropped.into_iter().map(|file| Arc::new(Dropped(file)) as _));
            }
            Err(why) => {
                let script = self.script.take().expect("there's a script");
                input.events.extend(script.letting_go());
                ui_tree::hold(ctx, false);
                script
                    .reply
                    .failed(format!("Step {}: {why}", script.done + 1));
            }
        }
    }
}

/// Whether `input` has the user's own mouse or key input in Musicable.
/// The mouse's raw motion doesn't count: it comes wherever the pointer is.
fn users(input: &RawInput) -> bool {
    !input.dropped_files.is_empty()
        || !input.hovered_files.is_empty()
        || input.events.iter().any(|event| {
            matches!(
                event,
                Event::PointerMoved(_)
                    | Event::PointerButton { .. }
                    | Event::Key { .. }
                    | Event::Text(_)
                    | Event::MouseWheel { .. }
                    | Event::Zoom(_)
                    | Event::Touch { .. }
                    | Event::Copy
                    | Event::Cut
                    | Event::Paste(_)
            )
        })
}

impl Script {
    /// The input for this frame, events and files dropped, or why the step
    /// can't go on.
    fn run(&mut self, scale: f32) -> Result<(Vec<Event>, Vec<PathBuf>), String> {
        let mut events = Vec::new();
        let mut dropped = Vec::new();
        let now = Instant::now();
        loop {
            match self.current {
                Some(Current::NextFrame) => {
                    self.current = None;
                }
                Some(Current::Waiting(until)) => {
                    if now < until {
                        break;
                    }
                    self.current = None;
                }
                Some(Current::Moving {
                    from,
                    to,
                    start,
                    seconds,
                }) => {
                    let t = (now - start).as_secs_f32() / seconds.max(f32::EPSILON);
                    let at = from.lerp(to, t.min(1.0));
                    self.pointer = Some(at);
                    events.push(Event::PointerMoved(at));
                    if t < 1.0 {
                        break;
                    }
                    self.current = None;
                }
                None => {}
            }
            let Some(op) = self.ops.pop_front() else {
                if self.settling.is_none() {
                    events.extend(self.letting_go());
                    self.settling = Some(2);
                }
                break;
            };
            match op {
                Op::MoveTo(target, seconds) => {
                    let Some(to) = self.place(target, scale)? else {
                        // The tree isn't drawn yet: again next frame.
                        self.ops.push_front(Op::MoveTo(target, seconds));
                        break;
                    };
                    match self.pointer {
                        Some(from) if seconds > 0.0 => {
                            self.current = Some(Current::Moving {
                                from,
                                to,
                                start: now,
                                seconds,
                            });
                        }
                        _ => {
                            self.pointer = Some(to);
                            events.push(Event::PointerMoved(to));
                        }
                    }
                }
                Op::Button(button, pressed) => {
                    let pos = self
                        .pointer
                        .ok_or("the pointer isn't anywhere yet: move it first.")?;
                    if pressed {
                        self.down.push(button);
                    } else {
                        self.down.retain(|down| *down != button);
                    }
                    events.push(Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers: egui_modifiers(self.held),
                    });
                }
                Op::Key(key, mods, pressed) => {
                    events.push(Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed,
                        repeat: false,
                        modifiers: egui_modifiers(self.held | mods),
                    });
                }
                Op::Hold(mods) => {
                    self.held = self.held | mods;
                    events.push(Event::ModifiersChanged(egui_modifiers(self.held)));
                }
                Op::Release(mods) => {
                    self.held = Modifiers {
                        ctrl: self.held.ctrl && !mods.ctrl,
                        alt: self.held.alt && !mods.alt,
                        shift: self.held.shift && !mods.shift,
                        cmd: self.held.cmd && !mods.cmd,
                    };
                    events.push(Event::ModifiersChanged(egui_modifiers(self.held)));
                }
                Op::Text(text) => events.push(Event::Text(text)),
                Op::Scroll(by) => events.push(Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    // The system's wheel counts down as negative.
                    delta: -by / scale,
                    phase: TouchPhase::Move,
                    source: MouseWheelSource::Wheel,
                    modifiers: egui_modifiers(self.held),
                }),
                // Dropped where the pointer is, as the user's are.
                Op::Drop(files) => dropped.extend(files),
                Op::Wait(seconds) => {
                    self.current = Some(Current::Waiting(now + Duration::from_secs_f32(seconds)));
                }
                // The rest waits for the next frame, so a button let go of
                // or a modifier given up there isn't in this frame's input
                // too, which only says how the frame ends.
                Op::NextFrame => {
                    self.current = Some(Current::NextFrame);
                    break;
                }
                Op::StepDone => self.done += 1,
            }
        }
        Ok((events, dropped))
    }

    /// Where `target` is in the window, in points, or `None` until the
    /// window's tree is drawn, or why it's nowhere.
    fn place(&mut self, target: Target, scale: f32) -> Result<Option<Pos2>, String> {
        match target {
            Target::At([x, y]) => Ok(Some(egui::pos2(x, y) / scale)),
            Target::Node(id) => {
                if let Some(rect) = self.places.get(&id) {
                    self.wanting = None;
                    return Ok(Some(rect.center()));
                }
                let wanting = *self.wanting.get_or_insert_with(Instant::now);
                if self.places.is_empty() && wanting.elapsed() < TREE_WAIT {
                    return Ok(None);
                }
                Err(format!(
                    "there's no node {id:x} with a place in the window now: get_ui_tree gives \
                     their ids."
                ))
            }
        }
    }

    /// Lets go of the buttons and modifiers the agent holds.
    fn letting_go(&self) -> Vec<Event> {
        let mut events = Vec::new();
        if let Some(pos) = self.pointer {
            events.extend(self.down.iter().map(|&button| Event::PointerButton {
                pos,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }));
        }
        if self.held != Modifiers::NONE {
            events.push(Event::ModifiersChanged(egui::Modifiers::NONE));
        }
        events
    }

    /// Keeps what the watch reads of the frame `update`, if it changed.
    fn record(&mut self, update: &egui::accesskit::TreeUpdate) {
        let Some(watch) = &self.watch else {
            return;
        };
        let mut read = ui_tree::answer(update, watch);
        let frame = json!({"nodes": read["nodes"].take(), "more": read["more"].take()});
        if self.last_frame.as_ref() == Some(&frame) {
            return;
        }
        self.last_frame = Some(frame.clone());
        if self.frames.len() < MOST_FRAMES {
            let mut frame = frame;
            let time = self.started.elapsed().as_secs_f64();
            frame["time"] = json!((time * 1000.0).round() / 1000.0);
            self.frames.push(frame);
        }
    }

    /// Answers the agent: how many steps, how long, and the frames.
    fn finish(self) {
        let seconds = self.started.elapsed().as_secs_f64();
        let mut answer = json!({
            "steps": self.done,
            "seconds": (seconds * 100.0).round() / 100.0,
        });
        if self.watch.is_some() {
            answer["frames"] = self.frames.into();
        }
        self.reply.done(answer);
    }
}

/// The nodes' places in the tree `update`, in points, by id.
fn places(update: &egui::accesskit::TreeUpdate) -> HashMap<u64, Rect> {
    (update.nodes.iter())
        .filter_map(|(id, node)| {
            let bounds = node.bounds()?;
            let rect = Rect::from_min_max(
                egui::pos2(bounds.x0 as f32, bounds.y0 as f32),
                egui::pos2(bounds.x1 as f32, bounds.y1 as f32),
            );
            Some((id.0, rect))
        })
        .collect()
}

/// What the steps do, a frame at a time, or why they can't be done.
fn ops(steps: Vec<Step>) -> Result<VecDeque<Op>, String> {
    let mut ops = VecDeque::new();
    for (index, step) in steps.into_iter().enumerate() {
        let problem = |why: String| format!("Step {}: {why}", index + 1);
        match step {
            Step::Move { to, seconds } => ops.push_back(Op::MoveTo(to, seconds)),
            Step::Down(button) => {
                ops.extend([Op::Button(pointer_button(button), true), Op::NextFrame]);
            }
            Step::Up(button) => {
                ops.extend([Op::Button(pointer_button(button), false), Op::NextFrame]);
            }
            Step::Click {
                at,
                button,
                count,
                interval,
            } => {
                let button = pointer_button(button);
                ops.extend([Op::MoveTo(at, 0.0), Op::NextFrame]);
                for click in 0..count {
                    if click > 0 {
                        ops.push_back(Op::Wait(interval));
                    }
                    ops.extend([
                        Op::Button(button, true),
                        Op::NextFrame,
                        Op::Button(button, false),
                        Op::NextFrame,
                    ]);
                }
            }
            Step::Drag {
                from,
                to,
                button,
                seconds,
            } => {
                let button = pointer_button(button);
                ops.extend([
                    Op::MoveTo(from, 0.0),
                    Op::NextFrame,
                    Op::Button(button, true),
                    Op::NextFrame,
                    Op::MoveTo(to, seconds),
                    Op::NextFrame,
                    Op::Button(button, false),
                    Op::NextFrame,
                ]);
            }
            Step::Hover { at, seconds } => {
                ops.extend([Op::MoveTo(at, 0.0), Op::Wait(seconds)]);
            }
            Step::Scroll { by, at } => {
                if let Some(at) = at {
                    ops.extend([Op::MoveTo(at, 0.0), Op::NextFrame]);
                }
                ops.extend([Op::Scroll(egui::vec2(by[0], by[1])), Op::NextFrame]);
            }
            Step::Key(chord) => {
                let key = egui_key(chord).map_err(problem)?;
                ops.extend([
                    Op::Key(key, chord.mods, true),
                    Op::NextFrame,
                    Op::Key(key, chord.mods, false),
                    Op::NextFrame,
                ]);
            }
            Step::Hold(mods) => ops.push_back(Op::Hold(mods)),
            Step::Release(mods) => ops.push_back(Op::Release(mods)),
            Step::Text(text) => ops.extend([Op::Text(text), Op::NextFrame]),
            Step::Drop { files, at } => {
                ops.extend([Op::MoveTo(at, 0.0), Op::NextFrame, Op::Drop(files)]);
                ops.push_back(Op::NextFrame);
            }
            Step::Wait(seconds) => ops.push_back(Op::Wait(seconds)),
        }
        ops.push_back(Op::StepDone);
    }
    Ok(ops)
}

fn pointer_button(button: Button) -> PointerButton {
    match button {
        Button::Left => PointerButton::Primary,
        Button::Right => PointerButton::Secondary,
        Button::Middle => PointerButton::Middle,
    }
}

/// egui's key for `chord`'s, or why there isn't one.
fn egui_key(chord: KeyChord) -> Result<egui::Key, String> {
    egui::Key::from_name(chord.key.name())
        .ok_or_else(|| format!("Musicable can't press {} for agents yet.", chord.key))
}

/// egui's modifiers for `mods`. Ctrl is egui's command key, except on
/// macOS, where Cmd is.
fn egui_modifiers(mods: Modifiers) -> egui::Modifiers {
    egui::Modifiers {
        alt: mods.alt,
        ctrl: mods.ctrl,
        shift: mods.shift,
        mac_cmd: mods.cmd,
        command: if cfg!(target_os = "macos") {
            mods.cmd
        } else {
            mods.ctrl
        },
    }
}

/// A file an agent drops on a window.
#[derive(Debug)]
struct Dropped(PathBuf);

impl DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|error| error.to_string())
    }
}
