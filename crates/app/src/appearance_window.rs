//! The appearance window: the theme Musicable draws with, what was wrong
//! with its file, and where theme files go.

use std::time::{Duration, Instant};

use eframe::egui::{self, Button, Label, RichText, Ui};
use musicable_ui::Base;

use crate::theme_file::{Choice, UserTheme};

/// How often the window lists the themes folder again while it's open, to
/// offer files as they're added.
const LIST_EVERY: Duration = Duration::from_secs(1);

/// What the user asked for in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// Draw with this theme from now on.
    Choose(Choice),
    /// Ask where to export the theme in use, and export it.
    Export,
    /// Show the themes folder in the system's file manager.
    OpenFolder,
    /// Read the theme file and its fonts again.
    Reload,
}

/// The appearance window. A theme picked in it is used at once.
#[derive(Default)]
pub struct AppearanceWindow {
    open: bool,
    /// The theme files in the themes folder, as last listed.
    files: Vec<String>,
    /// When they were listed.
    listed: Option<Instant>,
}

impl AppearanceWindow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&mut self) {
        self.open = true;
        self.listed = None;
    }

    /// Shows the window, if it's open, for the theme in use. Returns what
    /// the user asked for.
    pub fn show(&mut self, ctx: &egui::Context, theme: &UserTheme) -> Option<Asked> {
        if !self.open {
            return None;
        }
        if self.listed.is_none_or(|at| at.elapsed() >= LIST_EVERY) {
            self.files = theme.files();
            self.listed = Some(Instant::now());
        }
        let mut open = true;
        let mut asked = None;
        egui::Window::new("Appearance")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| asked = contents(ui, theme, &self.files));
        self.open = open;
        if asked.is_some() {
            // An export or a look at the folder may have added a file.
            self.listed = None;
        }
        asked
    }
}

/// The themes on offer, with the one in use picked, what was wrong with its
/// file, and what can be done with theme files.
fn contents(ui: &mut Ui, theme: &UserTheme, files: &[String]) -> Option<Asked> {
    let mut asked = None;
    let mut pick = |ui: &mut Ui, choice: Choice| {
        let chosen = &choice == theme.choice();
        if ui.radio(chosen, choice.title()).clicked() && !chosen {
            asked = Some(Asked::Choose(choice));
        }
    };
    ui.label(RichText::new("Theme").strong());
    for base in Base::ALL {
        pick(ui, Choice::BuiltIn(base));
    }
    let mut files: Vec<Choice> = files.iter().cloned().map(Choice::File).collect();
    // The theme in use, even when its file is gone.
    if matches!(theme.choice(), Choice::File(_)) && !files.contains(theme.choice()) {
        files.push(theme.choice().clone());
    }
    if !files.is_empty() {
        ui.label(RichText::new("In your themes folder").weak());
    }
    for file in files {
        pick(ui, file);
    }

    let warn = ui.visuals().warn_fg_color;
    if let Choice::File(name) = theme.choice() {
        ui.add_space(4.0);
        if let Some(error) = theme.error() {
            ui.label(RichText::new(error).color(warn));
        } else {
            ui.label(format!("{name} changes the {} theme.", theme.base().name()));
        }
        if !theme.problems().is_empty() {
            let text = format!("{name} has mistakes, which Musicable leaves out:");
            ui.label(RichText::new(text).color(warn));
            for problem in theme.problems() {
                ui.label(RichText::new(format!("\u{2022} {problem}.")).color(warn));
            }
        }
    }
    if let Some(notice) = theme.notice() {
        ui.add_space(4.0);
        ui.label(notice);
    }

    ui.separator();
    match theme.folder() {
        Some(folder) => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Your themes folder:");
                ui.add(
                    Label::new(RichText::new(folder.display().to_string()).monospace())
                        .selectable(true),
                );
            });
        }
        None => {
            ui.label("Musicable found no folder for theme files, so it has only its own themes.");
        }
    }
    ui.horizontal(|ui| {
        let export = ui
            .button("Export Theme...")
            .on_hover_text("Writes the theme in use to a file, to start a theme of your own from");
        if export.clicked() {
            asked = Some(Asked::Export);
        }
        let open = ui
            .add_enabled(theme.folder().is_some(), Button::new("Open Themes Folder"))
            .on_hover_text("Shows the folder of your theme files");
        if open.clicked() {
            asked = Some(Asked::OpenFolder);
        }
        if theme.path().is_some() {
            let reload = ui
                .button("Reload")
                .on_hover_text("Reads the theme file and its fonts again");
            if reload.clicked() {
                asked = Some(Asked::Reload);
            }
        }
    });
    ui.label(
        RichText::new(
            "A theme file holds its changes to a built-in theme. Musicable follows changes to \
             the one in use as you save them.",
        )
        .weak(),
    );
    asked
}
