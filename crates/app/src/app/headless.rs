//! `musicable --headless`: the app with no window, for agents. It's the
//! same app, with the same session, commands and tools, its frames run as
//! eframe would run them but drawn nowhere, as the app's tests run it. It
//! lets agents in whatever the settings say, and has no user to ask: an
//! agent answers the window's questions with `answer_question`, and quits
//! it with `quit_app`. `docs/agents.md` describes it.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::App as _;
use eframe::egui::{self, Pos2, RawInput, Rect, ViewportCommand, ViewportEvent, ViewportId};
use musicable_mcp::pipe::Address;

use super::App;
use crate::log_file;

/// The screen headless Musicable lays its window out on, as a laptop's.
const SCREEN: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1440.0, 900.0));
/// How long it waits for something to do before running a frame anyway,
/// as the window does while nothing changes.
const IDLE: Duration = Duration::from_secs(1);

/// When the next frame is due, which the app's requests to repaint bring
/// forward, and what wakes the loop for it.
#[derive(Default)]
struct Due {
    at: Mutex<Option<Instant>>,
    changed: Condvar,
}

impl Due {
    /// Brings the next frame forward to `at`, if that's sooner.
    fn by(&self, at: Instant) {
        let mut due = self.at.lock().unwrap_or_else(PoisonError::into_inner);
        if due.is_none_or(|due| at < due) {
            *due = Some(at);
            self.changed.notify_one();
        }
    }

    /// Waits until the next frame is due, or for [`IDLE`] if none is.
    fn wait(&self) {
        let mut due = self.at.lock().unwrap_or_else(PoisonError::into_inner);
        let idle = Instant::now() + IDLE;
        loop {
            let now = Instant::now();
            let until = due.map_or(idle, |at| at.min(idle));
            if until <= now {
                break;
            }
            due = (self.changed.wait_timeout(due, until - now))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *due = None;
    }
}

/// Runs Musicable headless, with the project at `path` open, or a new one,
/// playing through the audio device the settings name if `audio`, until an
/// agent quits it. Fails at once if it can't let agents in.
pub fn run(path: Option<PathBuf>, audio: bool) -> ExitCode {
    let config_dir = crate::config_dir();
    // Before the log file, which the Musicable running writes to.
    if let Some(dir) = &config_dir
        && let Ok(address) = Address::new(dir)
        && musicable_mcp::client::answers(&address)
    {
        eprintln!(
            "musicable: another Musicable lets agents in with the settings in {}, so this one \
             can't. Give it a folder of its own for its settings with MUSICABLE_CONFIG_DIR.",
            dir.display()
        );
        return ExitCode::FAILURE;
    }
    log_file::start(config_dir.as_deref());
    let ctx = egui::Context::default();
    let due = Arc::new(Due::default());
    let woken = Arc::clone(&due);
    ctx.set_request_repaint_callback(move |info| woken.by(Instant::now() + info.delay));
    let mut app = match App::headless(path, audio, &ctx) {
        Ok(app) => app,
        Err(why) => {
            log::error!("Musicable couldn't run headless: {why}");
            eprintln!("musicable: {why}");
            return ExitCode::FAILURE;
        }
    };
    let address = (config_dir.as_deref())
        .and_then(|dir| Address::new(dir).ok())
        .map(|address| address.to_string())
        .unwrap_or_default();
    log::info!("Musicable runs headless, and agents reach it at {address}");
    eprintln!(
        "Musicable runs headless, process {}, and agents reach it at {address}. quit_app quits \
         it.",
        std::process::id()
    );
    let mut frame = eframe::Frame::_new_kittest();
    let started = Instant::now();
    let mut closing = false;
    let mut last = Instant::now();
    loop {
        due.wait();
        let now = Instant::now();
        let mut input = RawInput {
            screen_rect: Some(SCREEN),
            time: Some(started.elapsed().as_secs_f64()),
            predicted_dt: (now - last).as_secs_f32().clamp(1.0 / 240.0, 1.0),
            focused: true,
            ..RawInput::default()
        };
        last = now;
        if closing {
            let root = input.viewports.entry(ViewportId::ROOT).or_default();
            root.events.push(ViewportEvent::Close);
        }
        app.raw_input_hook(&ctx, &mut input);
        let mut output = ctx.run_ui(input, |ui| {
            app.logic(ui.ctx(), &mut frame);
            app.ui(ui, &mut frame);
        });
        // Nothing draws them.
        output.textures_delta.clear();
        let root = output.viewport_output.get(&ViewportId::ROOT);
        let commands = root
            .map(|root| root.commands.as_slice())
            .unwrap_or_default();
        let cancelled = commands.contains(&ViewportCommand::CancelClose);
        if closing && !cancelled {
            break;
        }
        closing = commands.contains(&ViewportCommand::Close);
        if closing {
            due.by(Instant::now());
        } else if let Some(root) = root {
            due.by(Instant::now() + root.repaint_delay.min(IDLE));
        }
    }
    app.on_exit();
    drop(app);
    log::info!("Musicable closed");
    ExitCode::SUCCESS
}
