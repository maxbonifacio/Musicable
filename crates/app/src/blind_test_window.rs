//! The Blind Test window: a round of pairs (`blind_test.rs`) to judge
//! without knowing which is which. It plays A, B and, in an ABX round, X,
//! through the audio device as previews do, switching between them at the
//! same place, and keeps each answer as it's given. Once every trial is
//! judged, Unseal reads the key and shows how it went.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Button, Grid, ProgressBar, RichText, TextEdit, Ui};
use musicable_engine::Preview;
use musicable_engine::media::{AudioData, Source};
use musicable_io::audio::Decoder;
use serde_json::Value as Json;

use crate::Session;
use crate::blind_test::{Better, Round, Side};
use crate::media::FileReader;

/// Previews the window plays have ids from here up, apart from the
/// browser's.
const FIRST_ID: u64 = 1 << 48;
/// How long a sound asked for can take to start playing.
const WAIT: Duration = Duration::from_secs(1);

/// One of a trial's sounds, loaded: A, B, or X for `None`.
struct Sound {
    which: Option<Side>,
    source: Source,
    frames: u64,
    rate: u32,
}

pub struct BlindTestWindow {
    round: Option<Round>,
    open: bool,
    /// The trial shown, from 0.
    trial: usize,
    /// Its sounds, once loaded.
    sounds: Vec<Sound>,
    loaded: Option<usize>,
    /// The sound playing and its preview's id.
    playing: Option<(Option<Side>, u64)>,
    /// When it was asked to play, until the engine is seen playing it.
    asked: Option<Instant>,
    next_id: u64,
    /// Switching starts the next sound where the last one was.
    same_place: bool,
    problem: Option<String>,
}

impl BlindTestWindow {
    pub fn new() -> Self {
        Self {
            round: None,
            open: false,
            trial: 0,
            sounds: Vec::new(),
            loaded: None,
            playing: None,
            asked: None,
            next_id: FIRST_ID,
            same_place: true,
            problem: None,
        }
    }

    /// Opens the round in `folder`, at its first trial still to judge.
    pub fn open(&mut self, folder: &Path, session: &mut Session) -> Result<&Round, String> {
        let round = Round::open(folder)?;
        self.stop(session);
        self.trial = (0..round.answers.len())
            .find(|&i| !round.judged(i))
            .unwrap_or(0);
        self.round = Some(round);
        self.loaded = None;
        self.sounds.clear();
        self.problem = None;
        self.open = true;
        Ok(self.round.as_ref().expect("just opened"))
    }

    /// The round open, if one is.
    #[cfg(test)]
    pub fn round(&self) -> Option<&Round> {
        self.round.as_ref().filter(|_| self.open)
    }

    fn stop(&mut self, session: &mut Session) {
        if self.playing.take().is_some() {
            session.stop_sound();
        }
    }

    /// Loads the trial shown's sounds, if they aren't.
    fn load(&mut self) {
        let Some(round) = &self.round else {
            return;
        };
        if self.loaded == Some(self.trial) {
            return;
        }
        self.loaded = Some(self.trial);
        self.sounds.clear();
        self.problem = None;
        for which in [Some(Side::A), Some(Side::B), None] {
            let Some(path) = round.sound(self.trial, which) else {
                continue;
            };
            let read = Decoder::open(&path)
                .map_err(std::io::Error::other)
                .and_then(|decoder| {
                    let info = decoder.info();
                    let data = AudioData::read(&mut FileReader(decoder))?;
                    Ok(Sound {
                        which,
                        source: Source::Whole(Arc::new(data)),
                        frames: info.frames,
                        rate: info.sample_rate,
                    })
                });
            match read {
                Ok(sound) => self.sounds.push(sound),
                Err(error) => {
                    self.problem = Some(format!("Couldn't play {}: {error}.", path.display()));
                }
            }
        }
    }

    /// Plays `which`, or stops it if it's playing: from where the one
    /// playing is, if switching keeps the place.
    fn play(&mut self, which: Option<Side>, session: &mut Session) {
        if self.playing.is_some_and(|(playing, _)| playing == which) {
            return self.stop(session);
        }
        let Some(sound) = self.sounds.iter().find(|s| s.which == which) else {
            return;
        };
        let at = match (self.same_place, self.playing, session.sound_at()) {
            (true, Some((_, id)), Some(at)) if at.id == id => at.frame.min(sound.frames),
            _ => 0,
        };
        let id = self.next_id;
        self.next_id += 1;
        let preview = Preview::new(id, sound.source.clone(), session.engine_config());
        if session.play_sound(preview.starting_at(at)) {
            self.playing = Some((which, id));
            self.asked = Some(Instant::now());
        }
    }

    /// Goes to trial `index`.
    fn go(&mut self, index: usize, session: &mut Session) {
        let count = self.round.as_ref().map_or(0, |r| r.file.trials.len());
        if index < count && index != self.trial {
            self.stop(session);
            self.trial = index;
        }
    }

    /// Shows the window, if it's open. Returns what it has to say, such as
    /// an answer that couldn't be kept.
    pub fn show(&mut self, ctx: &egui::Context, session: &mut Session) -> Option<String> {
        if !self.open || self.round.is_none() {
            return None;
        }
        // A sound that played to its end, or that another preview replaced.
        // The engine picks a sound up with its next block, and gives up on
        // one it never got to.
        if let Some((_, id)) = self.playing {
            match session.sound_at() {
                Some(at) if at.id == id => self.asked = None,
                _ if self.asked.is_some_and(|asked| asked.elapsed() < WAIT) => {}
                _ => self.playing = None,
            }
        }
        self.load();
        let mut open = self.open;
        let mut said = None;
        let title = match self.round.as_ref().map(|r| r.file.name.trim()) {
            Some("") | None => "Blind Test".to_owned(),
            Some(name) => format!("Blind Test: {name}"),
        };
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| said = self.contents(ui, session));
        if !open {
            self.stop(session);
            self.open = false;
        }
        if self.playing.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        said
    }

    fn contents(&mut self, ui: &mut Ui, session: &mut Session) -> Option<String> {
        let round = self.round.as_ref()?;
        let count = round.file.trials.len();
        let (judged, abx) = (round.judged_count(), round.file.abx);
        ui.horizontal(|ui| {
            let back = ui.add_enabled(self.trial > 0, Button::new("Previous"));
            ui.label(RichText::new(format!("Trial {} of {count}", self.trial + 1)).strong());
            let next = ui.add_enabled(self.trial + 1 < count, Button::new("Next"));
            ui.label(format!("{judged} of {count} judged"));
            if back.clicked() {
                self.go(self.trial - 1, session);
            }
            if next.clicked() {
                self.go(self.trial + 1, session);
            }
        });
        ui.add_space(6.0);

        // The sounds.
        let mut asked = None;
        ui.horizontal(|ui| {
            let mut sounds = vec![(Some(Side::A), "A"), (Some(Side::B), "B")];
            if abx {
                sounds.push((None, "X"));
            }
            for (which, label) in sounds {
                let playing = self.playing.is_some_and(|(p, _)| p == which);
                let button = Button::new(RichText::new(format!("Play {label}")).size(16.0))
                    .selected(playing)
                    .min_size(egui::vec2(72.0, 32.0));
                let loaded = self.sounds.iter().any(|s| s.which == which);
                if ui.add_enabled(loaded, button).clicked() {
                    asked = Some(which);
                }
            }
            if ui
                .add_enabled(self.playing.is_some(), Button::new("Stop"))
                .clicked()
            {
                self.stop(session);
            }
        });
        if let Some(which) = asked {
            self.play(which, session);
        }
        let place = match (self.playing, session.sound_at()) {
            (Some((which, id)), Some(at)) if at.id == id => {
                let sound = self.sounds.iter().find(|s| s.which == which);
                sound.map(|s| (at.frame, s.frames, s.rate))
            }
            _ => None,
        };
        let (frame, frames, rate) = place.unwrap_or((0, 1, 1));
        let seconds = |frames: u64| frames as f64 / f64::from(rate);
        let text = match place {
            Some(_) => format!("{:.1} s of {:.1} s", seconds(frame), seconds(frames)),
            None => "Stopped".to_owned(),
        };
        ui.add(ProgressBar::new(frame as f32 / frames.max(1) as f32).text(text));
        ui.checkbox(&mut self.same_place, "Switch at the same place");
        if let Some(problem) = &self.problem {
            ui.colored_label(ui.visuals().error_fg_color, problem);
        }
        ui.add_space(6.0);

        // The answer.
        let round = self.round.as_ref()?;
        let sealed = round.results.is_none();
        let mut answer = round.answers[self.trial].clone();
        ui.add_enabled_ui(sealed, |ui| {
            Grid::new("blind_test_answer")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    if abx {
                        ui.label("X is");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut answer.x, Some(Side::A), "A");
                            ui.radio_value(&mut answer.x, Some(Side::B), "B");
                        });
                        ui.end_row();
                    }
                    ui.label("Better");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut answer.better, Some(Better::A), "A");
                        ui.radio_value(&mut answer.better, Some(Better::Same), "Same");
                        ui.radio_value(&mut answer.better, Some(Better::B), "B");
                    });
                    ui.end_row();
                    ui.label("Why");
                    ui.add(
                        TextEdit::singleline(&mut answer.note)
                            .hint_text("What you heard")
                            .desired_width(280.0),
                    );
                    ui.end_row();
                });
        });
        let trial = self.trial;
        let round = self.round.as_mut()?;
        let mut said = round.answer(trial, answer).err();
        ui.add_space(6.0);
        ui.separator();
        match round.results.clone() {
            None => {
                let ready = round.judged_count() == count;
                let unseal = ui
                    .add_enabled(ready, Button::new("Unseal"))
                    .on_disabled_hover_text("Judge every trial first.")
                    .on_hover_text("Read the key and write the results");
                if unseal.clicked() {
                    if let Err(error) = round.unseal() {
                        said = Some(error);
                    }
                    self.stop(session);
                }
            }
            Some(results) => show_results(ui, &results, trial),
        }
        said
    }
}

/// How the round went, and which was which in trial `trial`.
fn show_results(ui: &mut Ui, results: &Json, trial: usize) {
    let (first, second) = (
        results["first"].as_str().unwrap_or("first"),
        results["second"].as_str().unwrap_or("second"),
    );
    if let Some(abx) = results["abx"].as_object() {
        ui.label(format!(
            "X named right in {} of {}: by guessing, a chance of {:.3} of doing as well.",
            abx["correct"],
            abx["trials"],
            abx["p_value"].as_f64().unwrap_or(1.0)
        ));
    }
    let preference = &results["preference"];
    ui.label(format!(
        "Better: {first} {}, {second} {}, the same {} (sign test p = {:.3}).",
        preference["first"],
        preference["second"],
        preference["same"],
        preference["p_value"].as_f64().unwrap_or(1.0)
    ));
    let shown = &results["trials"][trial];
    let file = |side: &str| {
        let sound = &shown[side];
        let from = sound["from"].as_str().unwrap_or_default();
        let name = Path::new(from).file_name().map(|n| n.to_string_lossy());
        format!(
            "{} was {} ({})",
            side.to_uppercase(),
            sound["is"].as_str().unwrap_or_default(),
            name.unwrap_or_default()
        )
    };
    ui.label(format!("This trial: {}, {}.", file("a"), file("b")));
    if let Some(x) = shown["x"].as_str() {
        let right = match shown["correct"].as_bool() {
            Some(true) => "named right",
            _ => "named wrong",
        };
        ui.label(format!("X was {}, {right}.", x.to_uppercase()));
    }
    ui.label(RichText::new("The results are in results.json, in the round's folder.").weak());
}
