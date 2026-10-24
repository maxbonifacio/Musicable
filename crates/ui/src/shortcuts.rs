//! The keyboard shortcuts window, where every command's keys can be changed.
//!
//! Each command shows its keys. Clicking a key changes it, and + adds one:
//! the window waits for the keys to be pressed, says whether another command
//! has them, and gives them to the command on Assign. The app saves every
//! change to the user's keymap file.

use egui::{
    Align, Align2, Button, Event, Label, Layout, RichText, Role, ScrollArea, TextEdit, Ui,
    WidgetInfo, vec2,
};
use musicable_commands::{Command, CommandId, KeyChord, Problem};

use crate::menus::{label, menus};
use crate::search::score;
use crate::shell::{Action, View};

/// How wide the column of command titles is.
const TITLE_WIDTH: f32 = 200.0;

/// The keyboard shortcuts window's own state.
#[derive(Debug, Default)]
pub(crate) struct Shortcuts {
    open: bool,
    /// What the commands are filtered by.
    search: String,
    /// Set when the window opens, so the search field takes the focus.
    focus_search: bool,
    /// The command getting new keys, while it does.
    editing: Option<Editing>,
}

/// Keys being given to a command.
#[derive(Debug, Clone, PartialEq)]
struct Editing {
    command: CommandId,
    /// The chord the new keys replace, or `None` when adding keys.
    replacing: Option<KeyChord>,
    /// The keys pressed, once they have been. They wait for Assign.
    chord: Option<KeyChord>,
}

impl Shortcuts {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.focus_search = true;
    }

    /// Whether the window waits for the keys to give a command.
    pub(crate) fn capturing(&self) -> bool {
        self.open && self.editing.as_ref().is_some_and(|e| e.chord.is_none())
    }

    /// Whether the window asks whether to give a command the keys pressed.
    pub(crate) fn asking(&self) -> bool {
        self.open && self.editing.as_ref().is_some_and(|e| e.chord.is_some())
    }

    /// The keys pressed, while the window waits for them.
    pub(crate) fn captured(&mut self, chord: KeyChord) {
        if let Some(editing) = self.editing.as_mut().filter(|e| e.chord.is_none()) {
            editing.chord = Some(chord);
        }
    }

    /// Stops giving a command keys.
    pub(crate) fn cancel(&mut self) {
        self.editing = None;
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, view: &View<'_>, actions: &mut Vec<Action>) {
        // Escape closes the window, unless it's giving a command keys: then
        // it stops that.
        if self.open && self.editing.is_none() {
            self.open = !ctx.input_mut(|i| first_press(i, egui::Key::Escape));
        }
        let mut open = self.open;
        // It opens below the transport bar, where the command palette does.
        let top = ctx.content_rect().center_top() + vec2(0.0, 72.0);
        let window = egui::Window::new("Keyboard Shortcuts")
            .open(&mut open)
            .pivot(Align2::CENTER_TOP)
            .default_pos(top)
            .default_width(600.0)
            .show(ctx, |ui| self.contents(ui, view, actions));
        // Clicking anywhere else stops the editing, as closing the window does.
        let elsewhere = window.is_some_and(|w| w.response.clicked_elsewhere());
        if !open || elsewhere {
            self.editing = None;
        }
        self.open = open;
    }

    fn contents(&mut self, ui: &mut Ui, view: &View<'_>, actions: &mut Vec<Action>) {
        if self.asking() {
            // Enter assigns the keys pressed, and Escape doesn't. Repeats of
            // the keys just captured don't count.
            let (enter, escape) = ui.input_mut(|i| {
                (
                    first_press(i, egui::Key::Enter),
                    first_press(i, egui::Key::Escape),
                )
            });
            if escape {
                self.editing = None;
            } else if enter {
                self.assign(view, actions);
            }
        }
        let search = ui.add(
            TextEdit::singleline(&mut self.search)
                .hint_text("Search for a command, or keys such as Ctrl+L")
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_search) {
            search.request_focus();
        }
        if search.changed() {
            self.editing = None;
        }
        ui.add_space(6.0);
        ScrollArea::vertical()
            .max_height(380.0)
            .auto_shrink([false, true])
            .show(ui, |ui| self.table(ui, view, actions));
        ui.separator();
        file(ui, view, actions);
    }

    /// The commands that match the search, by menu, with their keys.
    fn table(&mut self, ui: &mut Ui, view: &View<'_>, actions: &mut Vec<Action>) {
        let menus = menus(view.commands);
        let shown: Vec<(&str, Vec<&Command>)> = menus
            .iter()
            .map(|menu| {
                let commands = menu.items.iter().flatten().copied();
                let matching = commands.filter(|c| self.matches(view, c)).collect();
                (menu.title.as_str(), matching)
            })
            .filter(|(_, commands): &(&str, Vec<&Command>)| !commands.is_empty())
            .collect();
        if shown.is_empty() {
            ui.label(
                RichText::new(format!("No command matches \"{}\".", self.search.trim())).weak(),
            );
            return;
        }
        for (i, (title, commands)) in shown.into_iter().enumerate() {
            if i > 0 {
                ui.add_space(8.0);
            }
            ui.label(RichText::new(title).strong());
            for command in commands {
                self.row(ui, view, command, actions);
            }
        }
    }

    fn matches(&self, view: &View<'_>, command: &Command) -> bool {
        let chords: Vec<_> = view.keymap.chords_for(&command.id).collect();
        score(&self.search, &label(command), command.id.as_str(), &chords).is_some()
    }

    /// A command's row: its title, its keys, and a reset button if its keys
    /// aren't the default ones. Under it, while it gets new keys, what to do.
    fn row(&mut self, ui: &mut Ui, view: &View<'_>, command: &Command, actions: &mut Vec<Action>) {
        let id = &command.id;
        let editing = self.editing.as_ref().filter(|e| e.command == *id).cloned();
        let height = ui.spacing().interact_size.y;
        ui.horizontal(|ui| {
            let title = Layout::left_to_right(Align::Center);
            ui.allocate_ui_with_layout(vec2(TITLE_WIDTH, height), title, |ui| {
                ui.set_width(TITLE_WIDTH);
                ui.add(Label::new(&command.title).truncate())
                    .on_hover_text(id.as_str());
            });
            for chord in view.keymap.chords_for(id) {
                let changing = editing.as_ref().is_some_and(|e| e.replacing == Some(chord));
                let key =
                    Button::new(RichText::new(chord.to_string()).monospace()).selected(changing);
                if ui
                    .add(key)
                    .on_hover_text("Click, then press the keys to use instead")
                    .clicked()
                {
                    self.edit(id, Some(chord));
                }
                let remove = ui.add(Button::new("\u{d7}").small().frame(false));
                named(&remove, format!("Remove {chord}"));
                if remove.on_hover_text(format!("Remove {chord}")).clicked() {
                    let mut keymap = view.keymap.clone();
                    keymap.unbind(chord);
                    self.editing = None;
                    actions.push(Action::SetKeymap(keymap));
                }
            }
            let adding = editing.as_ref().is_some_and(|e| e.replacing.is_none());
            let add = ui.add(Button::new("+").small().selected(adding));
            named(&add, format!("Add keys to {}", command.title));
            if add.on_hover_text("Add keys").clicked() {
                self.edit(id, None);
            }
            let defaults: Vec<KeyChord> = view.default_keymap.chords_for(id).collect();
            if view.keymap.chords_for(id).ne(defaults.iter().copied()) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let reset = ui.small_button("Reset");
                    named(&reset, format!("Reset {}", command.title));
                    let reset = reset.on_hover_ui(|ui| {
                        ui.label(reset_hint(view, id, &defaults));
                    });
                    if reset.clicked() {
                        let mut keymap = view.keymap.clone();
                        keymap.reset_command(id, view.default_keymap);
                        self.editing = None;
                        actions.push(Action::SetKeymap(keymap));
                    }
                });
            }
        });
        if let Some(editing) = editing {
            ui.horizontal(|ui| {
                ui.add_space(TITLE_WIDTH + ui.spacing().item_spacing.x);
                ui.horizontal_wrapped(|ui| self.prompt(ui, view, command, &editing, actions));
            });
        }
    }

    /// Starts giving `command` new keys, in place of `replacing` if given.
    fn edit(&mut self, command: &CommandId, replacing: Option<KeyChord>) {
        self.editing = Some(Editing {
            command: command.clone(),
            replacing,
            chord: None,
        });
    }

    /// What to do while a command gets new keys: press them, then assign
    /// them.
    fn prompt(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        command: &Command,
        editing: &Editing,
        actions: &mut Vec<Action>,
    ) {
        let Some(chord) = editing.chord else {
            let text = match editing.replacing {
                Some(old) => format!("Press the keys to use in place of {old}..."),
                None => format!("Press the keys to add to {}...", command.title),
            };
            ui.label(RichText::new(text).strong());
            if ui.button("Cancel").clicked() {
                self.editing = None;
            }
            return;
        };
        let chord_text = RichText::new(chord.to_string()).monospace().strong();
        let assign = match view.keymap.lookup(chord) {
            Some(owner) if *owner == command.id => {
                ui.label(chord_text);
                ui.label(format!("runs {} already.", command.title));
                None
            }
            Some(owner) => {
                ui.label(chord_text);
                let warn = ui.visuals().warn_fg_color;
                ui.label(RichText::new(format!("runs {} now.", title_of(view, owner))).color(warn));
                Some("Reassign")
            }
            None => {
                ui.label(chord_text);
                ui.label("isn't used yet.");
                Some("Assign")
            }
        };
        if let Some(text) = assign {
            let hint = match text {
                "Reassign" => "Takes the keys from the other command (Enter)",
                _ => "Enter",
            };
            if ui.button(text).on_hover_text(hint).clicked() {
                self.assign(view, actions);
            }
        }
        if ui.button("Try Other Keys").clicked() {
            self.editing = Some(Editing {
                chord: None,
                ..editing.clone()
            });
        }
        if ui.button("Cancel").on_hover_text("Escape").clicked() {
            self.editing = None;
        }
    }

    /// Gives the command being edited the keys pressed, taking them from any
    /// command that has them.
    fn assign(&mut self, view: &View<'_>, actions: &mut Vec<Action>) {
        let Some(Editing {
            command,
            replacing,
            chord: Some(chord),
        }) = self.editing.take()
        else {
            return;
        };
        if view.keymap.lookup(chord) == Some(&command) {
            return;
        }
        let mut keymap = view.keymap.clone();
        if let Some(old) = replacing.filter(|&old| keymap.lookup(old) == Some(&command)) {
            keymap.unbind(old);
        }
        keymap.bind(chord, command);
        actions.push(Action::SetKeymap(keymap));
    }
}

/// The user's keymap file, what was wrong with it, and what can be done with
/// it.
fn file(ui: &mut Ui, view: &View<'_>, actions: &mut Vec<Action>) {
    let status = &view.keymap_file;
    let warn = ui.visuals().warn_fg_color;
    if let Some(error) = status.error {
        ui.label(RichText::new(error).color(warn));
    }
    for problem in status.problems {
        match problem {
            Problem::Conflict { chord, commands } => {
                let titles: Vec<_> = commands.iter().map(|id| title_of(view, id)).collect();
                let text = format!(
                    "Your keymap file gives {chord} to {}, so it runs none of them.",
                    joined(&titles)
                );
                ui.label(RichText::new(text).color(warn));
                ui.horizontal_wrapped(|ui| {
                    for (id, title) in commands.iter().zip(&titles) {
                        if ui.button(format!("Give {chord} to {title}")).clicked() {
                            let mut keymap = view.keymap.clone();
                            keymap.bind(*chord, id.clone());
                            actions.push(Action::SetKeymap(keymap));
                        }
                    }
                });
            }
            problem => {
                ui.label(RichText::new(problem.to_string()).color(warn));
            }
        }
    }
    for (chord, id) in view.keymap.dangling(view.commands) {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{chord} runs {id}, which this version doesn't have."
            ));
            let remove = ui.small_button("Remove");
            named(&remove, format!("Remove {chord}"));
            if remove.clicked() {
                let mut keymap = view.keymap.clone();
                keymap.unbind(chord);
                actions.push(Action::SetKeymap(keymap));
            }
        });
    }
    if let Some(notice) = status.notice {
        ui.label(notice);
    }

    match status.path {
        Some(path) => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Your keymap file:");
                ui.add(
                    egui::Label::new(RichText::new(path.display().to_string()).monospace())
                        .selectable(true),
                );
            });
        }
        None => {
            ui.label(
                "Musicable found no folder for your keymap file, so changes last until it closes.",
            );
        }
    }
    ui.horizontal(|ui| {
        if ui
            .button("Import...")
            .on_hover_text("Uses the keys in a keymap file in place of yours")
            .clicked()
        {
            actions.push(Action::ImportKeymap);
        }
        if ui
            .button("Export...")
            .on_hover_text("Saves a copy of your keymap file")
            .clicked()
        {
            actions.push(Action::ExportKeymap);
        }
        let changed = view.keymap != view.default_keymap
            || !status.problems.is_empty()
            || status.error.is_some();
        let reset = ui
            .add_enabled(changed, Button::new("Reset All"))
            .on_hover_text("Gives every command its default keys again");
        if reset.clicked() {
            actions.push(Action::ResetKeymap);
        }
        if let Some(path) = status.path {
            if path.exists() {
                let reload = ui
                    .button("Reload")
                    .on_hover_text("Reads the file again, after editing it");
                if reload.clicked() {
                    actions.push(Action::ReloadKeymap);
                }
            } else if ui
                .button("Create It")
                .on_hover_text("Creates the file with no changes in it yet, to edit")
                .clicked()
            {
                actions.push(Action::CreateKeymap);
            }
        }
    });
}

/// What resetting a command's keys does, for the reset button's tooltip.
fn reset_hint(view: &View<'_>, id: &CommandId, defaults: &[KeyChord]) -> String {
    let mut hint = match defaults {
        [] => "Back to no keys, as by default.".to_owned(),
        _ => {
            let defaults: Vec<_> = defaults.iter().map(KeyChord::to_string).collect();
            format!("Back to the default keys: {}.", defaults.join(", "))
        }
    };
    for &chord in defaults {
        if let Some(owner) = view.keymap.lookup(chord).filter(|owner| *owner != id) {
            hint += &format!("\nTakes {chord} from {}.", title_of(view, owner));
        }
    }
    hint
}

/// A command's title, or its id for a command this version doesn't have.
fn title_of(view: &View<'_>, id: &CommandId) -> String {
    view.commands
        .get(id)
        .map_or_else(|| id.to_string(), |c| c.title.clone())
}

/// Names a widget for screen readers, and the tests, where its text doesn't
/// say enough.
fn named(response: &egui::Response, name: String) {
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
}

/// Takes the first press of `key` without modifiers out of the input, if it
/// isn't a repeat.
pub(crate) fn first_press(input: &mut egui::InputState, key: egui::Key) -> bool {
    let at = input.events.iter().position(|event| {
        matches!(event, Event::Key { key: k, pressed: true, repeat: false, modifiers, .. }
            if *k == key && modifiers.is_none())
    });
    at.map(|at| input.events.remove(at)).is_some()
}

/// "A", "A and B", "A, B and C".
fn joined(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use musicable_commands::{Keymap, transport};
    use musicable_model::Project;

    use super::*;
    use crate::testing::{State, harness};

    fn chord(text: &str) -> KeyChord {
        text.parse().unwrap()
    }

    fn keys(keymap: &Keymap, id: &CommandId) -> Vec<String> {
        keymap.chords_for(id).map(|c| c.to_string()).collect()
    }

    /// The shell with the keyboard shortcuts window open.
    fn window() -> Harness<'static, State> {
        let mut harness = harness(Project::new());
        harness.state_mut().shell.open_shortcuts();
        harness.run();
        harness
    }

    /// Scrolls the widget called `label` into view, and clicks it.
    fn click(harness: &mut Harness<'static, State>, label: &str) {
        harness.get_by_label(label).scroll_to_me();
        harness.run();
        harness.get_by_label(label).click();
        harness.run();
    }

    /// Presses `chord` while the window waits for keys.
    fn press(harness: &mut Harness<'static, State>, chord: &str) {
        assert!(harness.state().shell.capturing());
        harness.state_mut().shell.captured(self::chord(chord));
        harness.run();
        assert!(harness.state().shell.asking());
    }

    #[test]
    fn a_command_gets_new_keys() {
        let mut harness = window();
        let metronome = transport::TOGGLE_METRONOME;
        click(&mut harness, "Add keys to Toggle Metronome");
        harness.get_by_label_contains("Press the keys to add to Toggle Metronome");
        press(&mut harness, "M");
        harness.get_by_label("isn't used yet.");
        click(&mut harness, "Assign");

        let state = harness.state();
        assert!(!state.shell.capturing() && !state.shell.asking());
        assert_eq!(keys(&state.keymap, &metronome), ["M", "O"]);
        let mut expected = Keymap::default_keymap();
        expected.bind(chord("M"), metronome);
        assert_eq!(state.actions, [Action::SetKeymap(expected)]);
        harness.get_by_label("Reset Toggle Metronome");
    }

    #[test]
    fn keys_another_command_has_are_reassigned() {
        let mut harness = window();
        // Click the metronome's O, and press the loop's Ctrl+L in its place.
        click(&mut harness, "O");
        harness.get_by_label_contains("Press the keys to use in place of O");
        press(&mut harness, "Ctrl+L");
        harness.get_by_label("runs Toggle Loop now.");
        // Enter is Reassign.
        harness.key_press(egui::Key::Enter);
        harness.run();

        let state = harness.state();
        assert!(!state.shell.asking());
        assert_eq!(
            keys(&state.keymap, &transport::TOGGLE_METRONOME),
            ["Ctrl+L"]
        );
        assert_eq!(
            keys(&state.keymap, &transport::TOGGLE_LOOP),
            Vec::<String>::new()
        );
        harness.get_by_label("Reset Toggle Loop");
    }

    #[test]
    fn keys_can_be_tried_again_or_cancelled() {
        let mut harness = window();
        click(&mut harness, "Add keys to Play");
        // Any keys can be pressed, Escape too.
        press(&mut harness, "Escape");
        click(&mut harness, "Try Other Keys");
        press(&mut harness, "Ctrl+Period");
        harness.get_by_label("runs Stop now.");
        // Escape cancels, once the keys are pressed.
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(!harness.state().shell.asking());

        // Keys a command has already change nothing.
        click(&mut harness, "Add keys to Play");
        press(&mut harness, "Shift+Space");
        harness.get_by_label("runs Play already.");
        assert!(harness.query_by_label("Assign").is_none());
        click(&mut harness, "Cancel");
        // So does clicking elsewhere, while it waits for keys.
        click(&mut harness, "Add keys to Play");
        harness
            .get_by_role_and_label(Role::Button, "Go to Start")
            .click();
        harness.run();
        assert!(!harness.state().shell.capturing());
        assert_eq!(harness.state().keymap, Keymap::default_keymap());
        assert_eq!(
            harness.state().actions,
            [Action::Run(transport::GO_TO_START)]
        );
    }

    #[test]
    fn keys_are_removed_and_reset() {
        let mut harness = window();
        let looping = transport::TOGGLE_LOOP;
        click(&mut harness, "Remove Ctrl+L");
        assert_eq!(
            keys(&harness.state().keymap, &looping),
            Vec::<String>::new()
        );

        // Give its keys to the metronome, then reset the loop: it takes them
        // back.
        click(&mut harness, "Add keys to Toggle Metronome");
        press(&mut harness, "Ctrl+L");
        click(&mut harness, "Assign");
        harness.get_by_label("Reset Toggle Loop").hover();
        harness.run();
        harness.get_by_label_contains("Takes Ctrl+L from Toggle Metronome.");
        click(&mut harness, "Reset Toggle Loop");
        let state = harness.state();
        assert_eq!(keys(&state.keymap, &looping), ["Ctrl+L"]);
        assert_eq!(keys(&state.keymap, &transport::TOGGLE_METRONOME), ["O"]);
        assert_eq!(state.keymap, Keymap::default_keymap());
        assert!(harness.query_by_label_contains("Reset Toggle").is_none());
    }

    #[test]
    fn the_search_finds_commands_by_name_and_keys() {
        let mut harness = window();
        let shown = |harness: &Harness<'static, State>| {
            [
                "Toggle Loop",
                "Toggle Metronome",
                "Save Project",
                "Command Palette...",
            ]
            .into_iter()
            .filter(|title| harness.query_by_label(title).is_some())
            .collect::<Vec<_>>()
        };
        assert_eq!(shown(&harness).len(), 4);
        // The search field has the focus when the window opens.
        harness.get_by_role(Role::TextInput).type_text("metro");
        harness.run();
        assert_eq!(shown(&harness), ["Toggle Metronome"]);

        let mut harness = window();
        harness.get_by_role(Role::TextInput).type_text("ctrl+s");
        harness.run();
        assert_eq!(shown(&harness), ["Save Project"]);

        let mut harness = window();
        harness.get_by_role(Role::TextInput).type_text("tl");
        harness.run();
        assert_eq!(shown(&harness), ["Toggle Loop"]);

        let mut harness = window();
        harness
            .get_by_role(Role::TextInput)
            .type_text("nothing like it");
        harness.run();
        assert!(shown(&harness).is_empty());
        harness.get_by_label("No command matches \"nothing like it\".");
    }

    #[test]
    fn conflicts_in_the_keymap_file_are_resolved_here() {
        let mut harness = window();
        let l = chord("L");
        harness.state_mut().keymap_problems = vec![Problem::Conflict {
            chord: l,
            commands: vec![transport::TOGGLE_LOOP, transport::TOGGLE_METRONOME],
        }];
        harness.run();
        harness.get_by_label(
            "Your keymap file gives L to Toggle Loop and Toggle Metronome, so it runs none of \
             them.",
        );
        click(&mut harness, "Give L to Toggle Metronome");
        let state = harness.state();
        assert_eq!(state.keymap.lookup(l), Some(&transport::TOGGLE_METRONOME));
        assert!(
            harness
                .query_by_label_contains("Your keymap file gives")
                .is_none()
        );
    }

    #[test]
    fn keys_for_commands_this_version_lacks_are_shown() {
        let mut harness = window();
        harness
            .state_mut()
            .keymap
            .bind(chord("Ctrl+F12"), "later.thing");
        harness.run();
        harness.get_by_label("Ctrl+F12 runs later.thing, which this version doesn't have.");
        click(&mut harness, "Remove Ctrl+F12");
        assert_eq!(harness.state().keymap, Keymap::default_keymap());
    }

    #[test]
    fn the_file_buttons_ask_the_app() {
        let mut harness = window();
        // Nothing to reset yet, and nowhere to keep the file.
        assert!(harness.query_by_label("Reload").is_none());
        harness.get_by_label_contains("found no folder for your keymap file");
        click(&mut harness, "Reset All");
        assert_eq!(harness.state().actions, []);

        let path = std::env::temp_dir().join("musicable-no-such-folder/keymap.toml");
        harness.state_mut().keymap_path = Some(path);
        harness.state_mut().keymap.unbind(chord("Space"));
        harness.run();
        for button in ["Import...", "Export...", "Reset All", "Create It"] {
            click(&mut harness, button);
        }
        assert_eq!(
            harness.state().actions,
            [
                Action::ImportKeymap,
                Action::ExportKeymap,
                Action::ResetKeymap,
                Action::CreateKeymap
            ]
        );
    }

    #[test]
    fn escape_closes_the_window() {
        let mut harness = window();
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.query_by_label("Keyboard Shortcuts").is_none());
    }

    #[test]
    fn closing_the_window_stops_waiting_for_keys() {
        let mut harness = window();
        click(&mut harness, "Add keys to Play");
        assert!(harness.state().shell.capturing());
        harness.state_mut().shell.open_palette();
        harness.run();
        assert!(!harness.state().shell.capturing());
        assert!(harness.state().shell.asking(), "the palette is open");
    }

    #[test]
    fn lists_read_naturally() {
        let items =
            |items: &[&str]| -> Vec<String> { items.iter().map(|s| s.to_string()).collect() };
        assert_eq!(joined(&items(&[])), "");
        assert_eq!(joined(&items(&["A"])), "A");
        assert_eq!(joined(&items(&["A", "B"])), "A and B");
        assert_eq!(joined(&items(&["A", "B", "C"])), "A, B and C");
    }
}
