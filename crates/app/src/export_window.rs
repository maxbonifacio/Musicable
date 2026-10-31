//! The export window: what to render to audio files and how, how the export
//! under way is going, and how the last one went. The manual's Exporting
//! audio page (`manual/src/using/exporting.md`) describes it.

use std::collections::BTreeSet;

use eframe::egui::{
    self, Button, ComboBox, Grid, ProgressBar, Response, RichText, Role, ScrollArea, Ui, WidgetInfo,
};
use musicable_commands::Editor;
use musicable_io::audio::SampleFormat;
use musicable_model::time::BeatRange;
use musicable_model::{Project, TrackId};
use musicable_ui::{clock, position};

use crate::export::{Bits, Export, ExportSettings, FileFormat, Report, Span, Stage};
use crate::output::kilohertz;

/// The sample rates on offer, besides the engine's.
const SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// What the user asked of the window.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked {
    /// To export `range` with `settings`: the app asks for the file, then
    /// starts. `tracks` are the ones wanted on their own, if the settings
    /// want stems.
    Export {
        range: BeatRange,
        settings: ExportSettings,
        tracks: BTreeSet<TrackId>,
    },
    /// To stop the export under way.
    Cancel,
}

/// The export window. Its settings start as the last export's, and the
/// stretch to render as the selection, or else the song.
pub struct ExportWindow {
    open: bool,
    span: Span,
    settings: ExportSettings,
    /// The tracks to export on their own, when the settings want stems.
    tracks: BTreeSet<TrackId>,
    /// How the last export went, or why it couldn't start.
    report: Option<Report>,
}

impl ExportWindow {
    pub fn new() -> Self {
        Self {
            open: false,
            span: Span::Song,
            settings: ExportSettings::default(),
            tracks: BTreeSet::new(),
            report: None,
        }
    }

    /// Whether the window is open.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the window with `settings`, on `project` with `editor`'s
    /// selection, every track ticked. While an export runs, it opens on
    /// that.
    pub fn open(
        &mut self,
        settings: &ExportSettings,
        project: &Project,
        editor: &Editor,
        exporting: bool,
    ) {
        if self.open || exporting {
            self.open = true;
            return;
        }
        self.open = true;
        self.settings = *settings;
        self.span = Span::default_for(project, editor);
        self.tracks = project.tracks.iter().map(|track| track.id).collect();
        self.report = None;
    }

    /// Says how the export went.
    pub fn finished(&mut self, report: Report) {
        self.report = Some(report);
    }

    /// Shows the window, if it's open, with the progress of `export` if one
    /// is under way. `rate` is the engine's sample rate. Returns what the
    /// user asked for.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        editor: &Editor,
        rate: u32,
        export: Option<&Export>,
    ) -> Option<Asked> {
        let mut open = self.open;
        let mut asked = None;
        egui::Window::new("Export Audio")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.add_enabled_ui(export.is_none(), |ui| {
                    Grid::new("export_settings")
                        .num_columns(2)
                        .spacing([14.0, 8.0])
                        .show(ui, |ui| {
                            self.span_row(ui, project, editor);
                            self.tracks_row(ui, project);
                            self.format_row(ui);
                            self.rate_row(ui, rate);
                            self.options_row(ui);
                        });
                });
                ui.add_space(4.0);
                match export {
                    Some(export) => {
                        if progress(ui, export) {
                            asked = Some(Asked::Cancel);
                        }
                    }
                    None => {
                        let range = self.span.range(project, editor);
                        let stems = self.settings.stems && !self.tracks.is_empty();
                        let wanted = self.settings.mixdown || stems;
                        let button = ui
                            .add_enabled(range.is_some() && wanted, Button::new("Export..."))
                            .on_disabled_hover_text(match range {
                                None => "There's nothing to render here",
                                Some(_) => "Choose the mix or some tracks",
                            });
                        if let Some(range) = range
                            && button.clicked()
                        {
                            asked = Some(Asked::Export {
                                range,
                                settings: self.settings,
                                tracks: self.tracks.clone(),
                            });
                        }
                        if let Some(report) = &self.report {
                            ui.separator();
                            show_report(ui, report);
                        }
                    }
                }
            });
        self.open = open;
        asked
    }

    fn span_row(&mut self, ui: &mut Ui, project: &Project, editor: &Editor) {
        ui.label("Render");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                for span in Span::ALL {
                    let offered = span.range(project, editor).is_some();
                    let button = egui::RadioButton::new(self.span == span, span.name());
                    let clicked = ui
                        .add_enabled(offered, button)
                        .on_disabled_hover_text(match span {
                            Span::Song => "The song has no clips yet",
                            Span::Loop => "The loop region is empty",
                            Span::Selection => "Nothing is selected",
                        })
                        .clicked();
                    if clicked {
                        self.span = span;
                    }
                }
            });
            let text = match self.span.range(project, editor) {
                Some(range) => {
                    let tempo = &project.tempo;
                    // In samples at one a second: in seconds.
                    let seconds = tempo.sample_at(range.end, 1) - tempo.sample_at(range.start, 1);
                    format!(
                        "{} to {}, {}",
                        position(tempo, range.start),
                        position(tempo, range.end),
                        clock(seconds)
                    )
                }
                None => "Nothing to render".to_owned(),
            };
            ui.label(RichText::new(text).weak());
        });
        ui.end_row();
    }

    fn tracks_row(&mut self, ui: &mut Ui, project: &Project) {
        ui.label("Files");
        ui.vertical(|ui| {
            ui.checkbox(&mut self.settings.mixdown, "The mix")
                .on_hover_text("The master's output, as it's heard: mute and solo too");
            ui.checkbox(&mut self.settings.stems, "Each track on its own")
                .on_hover_text(
                    "A file for each track ticked, after its fader and before mute and solo, \
                     named after the mix's file and the track",
                );
            if !self.settings.stems {
                return;
            }
            ui.horizontal(|ui| {
                if ui.small_button("All").clicked() {
                    self.tracks = project.tracks.iter().map(|track| track.id).collect();
                }
                if ui.small_button("None").clicked() {
                    self.tracks.clear();
                }
            });
            ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                for track in &project.tracks {
                    let mut ticked = self.tracks.contains(&track.id);
                    if ui.checkbox(&mut ticked, &track.name).changed() {
                        match ticked {
                            true => self.tracks.insert(track.id),
                            false => self.tracks.remove(&track.id),
                        };
                    }
                }
            });
        });
        ui.end_row();
    }

    fn format_row(&mut self, ui: &mut Ui) {
        ui.label("Format");
        ui.horizontal(|ui| {
            let settings = &mut self.settings;
            let format = settings.format.name();
            let menu = ComboBox::from_id_salt("export_format")
                .selected_text(format)
                .show_ui(ui, |ui| {
                    for format in FileFormat::ALL {
                        ui.selectable_value(&mut settings.format, format, format.name());
                    }
                });
            name_menu(&menu.response, "Format", format);
            let flac = settings.format == FileFormat::Flac;
            // Exports write 16- and 24-bit integers and 32-bit floats only.
            let shown = match settings.sample_format() {
                SampleFormat::Int16 => Bits::Int16,
                SampleFormat::Int24 => Bits::Int24,
                _ => Bits::Float32,
            };
            let menu = ComboBox::from_id_salt("export_bits")
                .selected_text(shown.name())
                .show_ui(ui, |ui| {
                    for bits in Bits::ALL {
                        let offered = !(flac && bits == Bits::Float32);
                        let button = egui::Button::selectable(shown == bits, bits.name());
                        let clicked = ui
                            .add_enabled(offered, button)
                            .on_disabled_hover_text("FLAC files hold integers only")
                            .clicked();
                        if clicked {
                            settings.bits = bits;
                        }
                    }
                });
            name_menu(&menu.response, "Bits", shown.name());
            menu.response.on_hover_text(
                "24-bit for files to mix or master further, 16-bit for CDs, and 32-bit \
                     float to keep what goes over full scale",
            );
        });
        ui.end_row();
    }

    fn rate_row(&mut self, ui: &mut Ui, rate: u32) {
        ui.label("Sample rate");
        let label = |chosen: Option<u32>| match chosen {
            Some(chosen) => kilohertz(chosen),
            None => format!("{}, the engine's", kilohertz(rate)),
        };
        let mut chosen = self.settings.sample_rate;
        let menu = ComboBox::from_id_salt("export_rate")
            .selected_text(label(chosen))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, label(None));
                for rate in SAMPLE_RATES.into_iter().filter(|&r| r != rate) {
                    ui.selectable_value(&mut chosen, Some(rate), kilohertz(rate));
                }
            });
        name_menu(&menu.response, "Sample rate", &label(chosen));
        menu.response.on_hover_text(
            "The song renders at this rate, its plugins too: audio files at other rates \
                 are converted as they play",
        );
        self.settings.sample_rate = chosen.filter(|&chosen| chosen != rate);
        ui.end_row();
    }

    fn options_row(&mut self, ui: &mut Ui) {
        ui.label("");
        ui.vertical(|ui| {
            let floats = self.settings.sample_format() == SampleFormat::Float32;
            ui.add_enabled(
                !floats,
                egui::Checkbox::new(&mut self.settings.dither, "Dither"),
            )
            .on_hover_text(
                "Adds a little noise as the samples are rounded, so quiet sounds fade out \
                 smoothly instead of breaking up",
            )
            .on_disabled_hover_text("Float files aren't rounded, so there's nothing to dither");
            ui.checkbox(&mut self.settings.normalize, "Normalise")
                .on_hover_text("Raises or lowers each file so its loudest sample is at 0 dBFS");
            ui.checkbox(&mut self.settings.tail, "Render the tail")
                .on_hover_text(
                    "Renders on after the end until everything has been quiet for two seconds, so \
                 reverbs and releases ring out, for 30 seconds at most",
                );
        });
        ui.end_row();
    }
}

impl Default for ExportWindow {
    fn default() -> Self {
        Self::new()
    }
}

/// Names the combo box `response` for screen readers, showing `value`.
fn name_menu(response: &Response, label: &str, value: &str) {
    response.widget_info(|| WidgetInfo {
        current_text_value: Some(value.to_owned()),
        ..WidgetInfo::labeled(Role::ComboBox, true, label)
    });
}

/// Shows how `export` is going. Returns whether the user cancelled it.
fn progress(ui: &mut Ui, export: &Export) -> bool {
    let stage = export.stage();
    let rate = export.job().sample_rate();
    let bar = ProgressBar::new(stage.fraction())
        .text(stage_text(stage, rate))
        .animate(true);
    ui.add(bar);
    ui.button("Cancel").clicked()
}

/// What an export at `stage` says it's doing, its files at `rate`.
fn stage_text(stage: Stage, rate: u32) -> String {
    let of = |pass: usize, passes: usize| match passes {
        1 => String::new(),
        _ => format!("Pass {pass} of {passes}: "),
    };
    let time = |frames: u64| clock(frames as f64 / f64::from(rate));
    match stage {
        Stage::Preparing { pass, passes } => format!("{}Getting ready", of(pass, passes)),
        Stage::Rendering {
            pass,
            passes,
            done,
            length,
            tail,
            speed,
        } => {
            let what = match tail {
                true => "Rendering the tail".to_owned(),
                false => format!("Rendering {} of {}", time(done), time(length)),
            };
            let speed = match speed {
                speed if speed >= 1.0 => format!(", {speed:.0}× real time"),
                speed if speed > 0.0 => format!(", {speed:.1}× real time"),
                _ => String::new(),
            };
            format!("{}{what}{speed}", of(pass, passes))
        }
        Stage::Normalising {
            pass,
            passes,
            done,
            files,
        } => format!(
            "{}Normalising file {} of {files}",
            of(pass, passes),
            (done + 1).min(files)
        ),
        Stage::Finished => "Finished".to_owned(),
    }
}

/// Shows how an export went: the files it wrote, how loud they are, and
/// what went wrong.
fn show_report(ui: &mut Ui, report: &Report) {
    let warn = ui.visuals().warn_fg_color;
    if let Some(failed) = &report.failed {
        ui.label(RichText::new(format!("The export stopped: {failed}.")).color(warn));
    } else if report.cancelled {
        ui.label("The export was cancelled.");
    } else {
        let took = report.took.as_secs_f64();
        ui.label(format!("Exported in {took:.1} seconds."));
    }
    for file in &report.files {
        let name = file
            .path
            .file_name()
            .map_or(String::new(), |name| name.to_string_lossy().into_owned());
        let mut text = format!("{name}: peak {}", decibels(file.peak));
        if let Some(gain) = file.gain {
            text.push_str(&format!(", normalised by {}", gain_decibels(gain)));
        }
        ui.label(text)
            .on_hover_text(file.path.display().to_string());
        if file.over > 0 {
            let over = format!(
                "{} samples go over full scale, and clip where they're played",
                file.over
            );
            ui.label(RichText::new(over).color(warn));
        }
    }
    for problem in &report.problems {
        ui.label(RichText::new(problem).color(warn));
    }
}

/// A peak level, in decibels below full scale.
fn decibels(peak: f32) -> String {
    match peak {
        0.0 => "silent".to_owned(),
        peak => format!("{:.1} dBFS", 20.0 * peak.log10()),
    }
}

/// A gain, in decibels.
fn gain_decibels(gain: f32) -> String {
    format!("{:+.1} dB", 20.0 * gain.log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_say_what_the_export_does() {
        let rendering = Stage::Rendering {
            pass: 1,
            passes: 1,
            done: 48_000,
            length: 96_000,
            tail: false,
            speed: 12.4,
        };
        assert_eq!(
            stage_text(rendering, 48_000),
            "Rendering 0:01.000 of 0:02.000, 12× real time"
        );
        let tail = Stage::Rendering {
            pass: 2,
            passes: 2,
            done: 96_000,
            length: 96_000,
            tail: true,
            speed: 0.5,
        };
        assert_eq!(
            stage_text(tail, 48_000),
            "Pass 2 of 2: Rendering the tail, 0.5× real time"
        );
        let normalising = Stage::Normalising {
            pass: 1,
            passes: 1,
            done: 2,
            files: 2,
        };
        assert_eq!(stage_text(normalising, 48_000), "Normalising file 2 of 2");
    }

    #[test]
    fn levels_are_in_decibels() {
        assert_eq!(decibels(1.0), "0.0 dBFS");
        assert_eq!(decibels(0.5), "-6.0 dBFS");
        assert_eq!(decibels(0.0), "silent");
        assert_eq!(gain_decibels(2.0), "+6.0 dB");
    }
}
