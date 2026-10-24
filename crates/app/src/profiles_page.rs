//! The Settings window's Profiles page: the user's profiles, to switch
//! between, and to make, rename and delete.

use eframe::egui::{self, Button, Label, RichText, Ui};
use musicable_ui::{check_file_name, checked_name_box, confirm_box};

use crate::profiles::{DEFAULT_PROFILE, Profiles};

/// What the user asked for on the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// Switch to the profile called this.
    Switch(String),
    /// Make a profile called this, with Musicable's own keys, mouse,
    /// theme, layout and behaviour, and switch to it.
    New(String),
    /// Copy the profile in use as one called this, and switch to the copy.
    Duplicate(String),
    /// Call the profile in use this.
    Rename(String),
    /// Delete the profile in use, and switch to Default.
    Delete,
    /// Show the profiles folder in the system's file manager.
    OpenFolder,
    /// List the profiles again, as the page comes into sight, to find those
    /// copied into the profiles folder.
    List,
}

/// What the page asks the user in a box over the window.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Asking {
    /// A name for a new profile, as typed so far.
    New(String),
    /// A name for a copy of the profile in use.
    Duplicate(String),
    /// A new name for the profile in use, or `None` until the box first
    /// shows, with the name it has.
    Rename(Option<String>),
    /// Whether to delete the profile in use.
    Delete,
}

/// What the page keeps.
#[derive(Debug, Default)]
pub struct ProfilesPage {
    asking: Option<Asking>,
}

impl ProfilesPage {
    /// Forgets what the page was asking.
    pub fn reset(&mut self) {
        self.asking = None;
    }

    /// Shows the box that asks for a name, or whether to delete the profile
    /// in use, while one is open. It's drawn before the window, so Enter
    /// and Escape reach it first. Returns what the user asked for, once
    /// they've said.
    pub fn show_box(&mut self, ctx: &egui::Context, profiles: &Profiles) -> Option<Asked> {
        let duplicate = matches!(self.asking, Some(Asking::Duplicate(_)));
        let asking = self.asking.as_mut()?;
        let current = profiles.current_name();
        let names = || profiles.names().iter().map(String::as_str);
        // A name no profile has, or the one in use's, when it's renamed.
        // The profiles folder's hidden folders aren't profiles.
        let check = |text: &str, renaming: bool| {
            if text.trim_start().starts_with('.') {
                return Err("A profile's name can't start with a dot.".to_owned());
            }
            let taken = names().filter(|&name| !renaming || name != current);
            check_file_name(text, "profile", taken)
        };
        let named = |text: &str, renaming| check(text, renaming).ok();
        let done = match asking {
            Asking::New(text) | Asking::Duplicate(text) => {
                let (title, make): (_, fn(String) -> Asked) = match duplicate {
                    false => ("New Profile".to_owned(), Asked::New),
                    true => (format!("Duplicate {current}"), Asked::Duplicate),
                };
                let problem = |text: &str| check(text, false).err();
                checked_name_box(ctx, "profile name", &title, "Create", text, false, problem)
                    .map(|confirmed| confirmed.then(|| named(text, false).map(make)).flatten())
            }
            Asking::Rename(text) => {
                let first = text.is_none();
                let text = text.get_or_insert_with(|| current.to_owned());
                let title = format!("Rename {current}");
                let problem = |text: &str| check(text, true).err();
                checked_name_box(ctx, "profile name", &title, "Rename", text, first, problem).map(
                    |confirmed| {
                        let name = confirmed.then(|| named(text, true)).flatten();
                        name.filter(|name| name != current).map(Asked::Rename)
                    },
                )
            }
            Asking::Delete => confirm_box(
                ctx,
                &format!("Delete the profile {current}?"),
                &format!(
                    "Its folder goes, with its keys, mouse map, workspaces and settings, and \
                     Musicable switches to {DEFAULT_PROFILE}."
                ),
                "Delete",
            )
            .map(|confirmed| confirmed.then_some(Asked::Delete)),
        };
        let asked = done?;
        self.asking = None;
        asked
    }

    /// Draws the page, for `profiles`. `keys` are the keys that switch to
    /// the next profile. `new` says the page came into sight in this frame.
    /// Returns what the user asked for.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        profiles: &Profiles,
        keys: &str,
        new: bool,
    ) -> Option<Asked> {
        let mut asked = None;
        if new {
            self.asking = None;
            asked = Some(Asked::List);
        }
        ui.label(
            RichText::new(
                "A profile keeps your keys, mouse map, theme, workspaces and behaviour together, \
                 so switching to another, such as one with another DAW's habits, changes them all \
                 at once. The audio and MIDI devices, the recording settings, the library's \
                 folders and the plugins stay with this computer, whichever is in use.",
            )
            .weak(),
        );
        ui.add_space(6.0);
        for (index, name) in profiles.names().iter().enumerate() {
            let current = index == profiles.current();
            if ui.radio(current, name).clicked() && !current {
                asked = Some(Asked::Switch(name.clone()));
            }
        }
        ui.add_space(4.0);
        let folder = profiles.folder();
        let default = profiles.current() == 0;
        ui.horizontal(|ui| {
            let mut button = |ui: &mut Ui, enabled: bool, text: &str, asking: Asking| {
                if ui.add_enabled(enabled, Button::new(text)).clicked() {
                    self.asking = Some(asking);
                }
            };
            let kept = folder.is_some();
            button(ui, kept, "New...", Asking::New(String::new()));
            button(ui, kept, "Duplicate...", Asking::Duplicate(String::new()));
            button(ui, kept && !default, "Rename...", Asking::Rename(None));
            button(ui, kept && !default, "Delete...", Asking::Delete);
        });
        let switch = match keys {
            "" => "Options > Switch to Next Profile".to_owned(),
            keys => format!("Options > Switch to Next Profile ({keys})"),
        };
        ui.label(
            RichText::new(format!(
                "New starts with Musicable's own keys, mouse, theme, layout and behaviour, and \
                 Duplicate with the profile in use's. {switch} switches to the next profile, and \
                 from the last back to {DEFAULT_PROFILE}, which can't be renamed or deleted."
            ))
            .weak(),
        );
        ui.separator();
        match folder {
            Some(folder) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Your profiles folder:");
                    ui.add(
                        Label::new(RichText::new(folder.display().to_string()).monospace())
                            .selectable(true),
                    );
                });
                ui.label(
                    RichText::new(format!(
                        "Each profile but {DEFAULT_PROFILE} is a folder there, which can be \
                         copied to another computer's as it is. {DEFAULT_PROFILE}'s files are \
                         the config folder's own."
                    ))
                    .weak(),
                );
                if ui
                    .button("Open Profiles Folder")
                    .on_hover_text("Shows the folder of your profiles")
                    .clicked()
                {
                    asked = Some(Asked::OpenFolder);
                }
            }
            None => {
                ui.label(
                    "Musicable found no folder for profiles, so there's only Default, and \
                     changes last until it closes.",
                );
            }
        }
        asked
    }
}
