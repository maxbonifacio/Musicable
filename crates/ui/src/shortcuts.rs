//! The keyboard shortcuts: the Settings window's Keys page, where every
//! command's keys can be changed.
//!
//! Each command shows its keys, a row for each place they work: everywhere,
//! or in one panel while it has the focus, where they win over the keys
//! that work everywhere. Clicking a key changes it, and + adds one: the
//! page waits for the keys to be pressed, lets the user pick where they
//! work, says whether another command has them there, and gives them to the
//! command on Assign. The app saves every change to the user's keymap file.

use egui::{
    Align, Button, Label, Layout, RichText, Role, ScrollArea, TextEdit, Ui, WidgetInfo, vec2,
};
use musicable_commands::{Command, CommandId, KeyChord, Problem, Scope};

use crate::keys::first_press;
use crate::menus::{label, menus};
use crate::panels::Registry;
use crate::search::score;
use crate::shell::{Action, View};

/// How wide the column of command titles is.
const TITLE_WIDTH: f32 = 200.0;
/// How wide the column that says where the keys work is.
const WHERE_WIDTH: f32 = 90.0;

/// The Keys page's own state.
#[derive(Debug, Default)]
pub(crate) struct Shortcuts {
    /// Whether the page was in sight when it was last drawn.
    shown: bool,
    /// What the commands are filtered by.
    search: String,
    /// Set when the page comes into sight, so the search field takes the
    /// focus.
    focus_search: bool,
    /// The command getting new keys, while it does.
    editing: Option<Editing>,
}

/// Keys being given to a command.
#[derive(Debug, Clone, PartialEq)]
struct Editing {
    command: CommandId,
    /// The row whose keys change, or whose + was clicked.
    row: Scope,
    /// Where the new keys work: the row's place, until the user picks
    /// another.
    scope: Scope,
    /// The chord the new keys replace, where it works, or `None` when
    /// adding keys.
    replacing: Option<(Scope, KeyChord)>,
    /// The keys pressed, once they have been. They wait for Assign.
    chord: Option<KeyChord>,
}

impl Shortcuts {
    /// Whether the page waits for the keys to give a command.
    pub(crate) fn capturing(&self) -> bool {
        self.shown && self.editing.as_ref().is_some_and(|e| e.chord.is_none())
    }

    /// Whether the page asks whether to give a command the keys pressed.
    pub(crate) fn asking(&self) -> bool {
        self.shown && self.editing.as_ref().is_some_and(|e| e.chord.is_some())
    }

    /// Whether the page is giving a command keys.
    pub(crate) fn editing(&self) -> bool {
        self.shown && self.editing.is_some()
    }

    /// The keys pressed, while the page waits for them.
    pub(crate) fn captured(&mut self, chord: KeyChord) {
        if let Some(editing) = self.editing.as_mut().filter(|e| e.chord.is_none()) {
            editing.chord = Some(chord);
        }
    }

    /// Stops giving a command keys.
    pub(crate) fn cancel(&mut self) {
        self.editing = None;
    }

    /// Draws the page in `ui`, naming the panels as `panels` does.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        panels: &Registry,
        actions: &mut Vec<Action>,
    ) {
        if !self.shown {
            self.shown = true;
            self.focus_search = true;
        }
        self.contents(ui, view, &Places(panels), actions);
    }

    /// The page went out of sight, so it stops giving a command keys.
    pub(crate) fn hide(&mut self) {
        self.shown = false;
        self.editing = None;
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        places: &Places<'_>,
        actions: &mut Vec<Action>,
    ) {
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
            .show(ui, |ui| self.table(ui, view, places, actions));
        ui.separator();
        file(ui, view, places, actions);
    }

    /// The commands that match the search, by menu, with their keys.
    fn table(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        places: &Places<'_>,
        actions: &mut Vec<Action>,
    ) {
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
                self.rows(ui, view, places, command, actions);
            }
        }
    }

    /// Whether `command`'s name or any of its keys, wherever they work,
    /// match the search.
    fn matches(&self, view: &View<'_>, command: &Command) -> bool {
        let chords: Vec<_> = (view.keymap.bindings_for(&command.id))
            .map(|(_, chord)| chord)
            .collect();
        score(&self.search, &label(command), command.id.as_str(), &chords).is_some()
    }

    /// A command's rows: one for its keys everywhere, then one for each
    /// panel it has keys in. Under the row whose keys change, what to do.
    fn rows(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        places: &Places<'_>,
        command: &Command,
        actions: &mut Vec<Action>,
    ) {
        let id = &command.id;
        let panels = view.keymap.bindings_for(id).map(|(scope, _)| scope);
        let mut scopes: Vec<Scope> = std::iter::once(Scope::Everywhere).chain(panels).collect();
        scopes.dedup();
        let editing = self.editing.as_ref().filter(|e| e.command == *id).cloned();
        // The prompt goes under the last row if its own went.
        let under = editing
            .as_ref()
            .map(|editing| match scopes.contains(&editing.row) {
                true => editing.row,
                false => scopes[scopes.len() - 1],
            });
        for (i, &scope) in scopes.iter().enumerate() {
            self.row(ui, view, places, command, scope, i == 0, actions);
            if let Some(editing) = editing.as_ref().filter(|_| under == Some(scope)) {
                ui.horizontal(|ui| {
                    ui.add_space(TITLE_WIDTH + ui.spacing().item_spacing.x);
                    ui.vertical(|ui| self.prompt(ui, view, places, command, editing, actions));
                });
            }
        }
    }

    /// A row of a command's keys: its title on the first, where the keys
    /// work, the keys, and a reset button if the command's keys aren't the
    /// default ones. A panel's keys that run another command everywhere
    /// else say so.
    #[allow(clippy::too_many_arguments)]
    fn row(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        places: &Places<'_>,
        command: &Command,
        scope: Scope,
        first: bool,
        actions: &mut Vec<Action>,
    ) {
        let id = &command.id;
        let editing = self.editing.as_ref().filter(|e| e.command == *id).cloned();
        let height = ui.spacing().interact_size.y;
        let here = places.suffix(scope);
        ui.horizontal(|ui| {
            let column = Layout::left_to_right(Align::Center);
            ui.allocate_ui_with_layout(vec2(TITLE_WIDTH, height), column, |ui| {
                ui.set_width(TITLE_WIDTH);
                if first {
                    ui.add(Label::new(&command.title).truncate())
                        .on_hover_text(id.as_str());
                }
            });
            ui.allocate_ui_with_layout(vec2(WHERE_WIDTH, height), column, |ui| {
                ui.set_width(WHERE_WIDTH);
                ui.add(Label::new(RichText::new(places.name(scope)).weak()).truncate());
            });
            let chords: Vec<KeyChord> = view.keymap.chords_in(id, scope).collect();
            for chord in chords {
                let changing =
                    (editing.as_ref()).is_some_and(|e| e.replacing == Some((scope, chord)));
                let key =
                    Button::new(RichText::new(chord.to_string()).monospace()).selected(changing);
                // Where else it runs another command, which it doesn't run
                // in this panel.
                let over = match scope {
                    Scope::Panel(_) => view.keymap.lookup(chord).filter(|other| *other != id),
                    Scope::Everywhere => None,
                };
                let hint = match over {
                    Some(other) => format!(
                        "Everywhere else, {chord} runs {}.\nClick, then press the keys to use \
                         instead",
                        title_of(view, other)
                    ),
                    None => "Click, then press the keys to use instead".to_owned(),
                };
                if ui.add(key).on_hover_text(hint).clicked() {
                    self.edit(id, scope, Some(chord));
                }
                let remove = ui.add(Button::new("\u{d7}").small().frame(false));
                let name = format!("Remove {chord}{here}");
                named(&remove, name.clone());
                if remove.on_hover_text(name).clicked() {
                    let mut keymap = view.keymap.clone();
                    keymap.unbind_in(scope, chord);
                    self.editing = None;
                    actions.push(Action::SetKeymap(keymap));
                }
                if let Some(other) = over {
                    let text = format!("over {}", title_of(view, other));
                    ui.label(RichText::new(text).small().weak());
                }
            }
            let adding =
                (editing.as_ref()).is_some_and(|e| e.replacing.is_none() && e.row == scope);
            let add = ui.add(Button::new("+").small().selected(adding));
            named(&add, format!("Add keys to {}{here}", command.title));
            if add.on_hover_text("Add keys").clicked() {
                self.edit(id, scope, None);
            }
            let defaults: Vec<(Scope, KeyChord)> = view.default_keymap.bindings_for(id).collect();
            if first && view.keymap.bindings_for(id).ne(defaults.iter().copied()) {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let reset = ui.small_button("Reset");
                    named(&reset, format!("Reset {}", command.title));
                    let reset = reset.on_hover_ui(|ui| {
                        ui.label(reset_hint(view, places, id, &defaults));
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
    }

    /// Starts giving `command` new keys from the row of `scope`, in place
    /// of `replacing` there if given.
    fn edit(&mut self, command: &CommandId, scope: Scope, replacing: Option<KeyChord>) {
        self.editing = Some(Editing {
            command: command.clone(),
            row: scope,
            scope,
            replacing: replacing.map(|chord| (scope, chord)),
            chord: None,
        });
    }

    /// What to do while a command gets new keys: pick where they work,
    /// press them, then assign them.
    fn prompt(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        places: &Places<'_>,
        command: &Command,
        editing: &Editing,
        actions: &mut Vec<Action>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Where:");
            for scope in places.all() {
                let (name, chosen) = (places.name(scope), editing.scope == scope);
                let choice = ui.selectable_label(chosen, name);
                choice.widget_info(|| {
                    WidgetInfo::selected(Role::RadioButton, true, chosen, format!("Where: {name}"))
                });
                if choice.clicked()
                    && let Some(editing) = self.editing.as_mut()
                {
                    editing.scope = scope;
                }
            }
        });
        ui.horizontal_wrapped(|ui| self.ask(ui, view, places, command, editing, actions));
    }

    /// Asks for the keys, then says what they run where they'd work, and
    /// offers to assign them.
    fn ask(
        &mut self,
        ui: &mut Ui,
        view: &View<'_>,
        places: &Places<'_>,
        command: &Command,
        editing: &Editing,
        actions: &mut Vec<Action>,
    ) {
        let Some(chord) = editing.chord else {
            let text = match editing.replacing {
                Some((_, old)) => format!("Press the keys to use in place of {old}..."),
                None => format!("Press the keys to add to {}...", command.title),
            };
            ui.label(RichText::new(text).strong());
            if ui.button("Cancel").clicked() {
                self.editing = None;
            }
            return;
        };
        let (scope, here) = (editing.scope, places.suffix(editing.scope));
        let title = &command.title;
        ui.label(RichText::new(chord.to_string()).monospace().strong());
        let warn = ui.visuals().warn_fg_color;
        let assign = match runs(view, scope, chord) {
            Runs::Here(owner) if *owner == command.id => {
                ui.label(format!("runs {title}{here} already."));
                None
            }
            Runs::Here(owner) => {
                let text = format!("runs {}{here} now.", title_of(view, owner));
                ui.label(RichText::new(text).color(warn));
                Some("Reassign")
            }
            Runs::Everywhere(owner) if *owner == command.id => {
                ui.label(format!("runs {title} everywhere already."));
                None
            }
            Runs::Everywhere(owner) => {
                let other = title_of(view, owner);
                ui.label(format!(
                    "runs {other} everywhere, and would run {title}{here} instead."
                ));
                Some("Assign")
            }
            Runs::Nothing => {
                // Panels that give the keys to other commands keep them.
                let kept: Vec<String> = (view.keymap.iter())
                    .filter(|&(place, other, owner)| {
                        scope == Scope::Everywhere
                            && place != scope
                            && other == chord
                            && *owner != command.id
                    })
                    .map(|(place, _, owner)| {
                        let panel = places.name(place);
                        format!("the {panel} panel keeps it for {}", title_of(view, owner))
                    })
                    .collect();
                match kept.is_empty() {
                    true => ui.label("isn't used yet."),
                    false => ui.label(format!("isn't used yet, but {}.", joined(&kept))),
                };
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

    /// Gives the command being edited the keys pressed, where they're to
    /// work, taking them from any command that has them there.
    fn assign(&mut self, view: &View<'_>, actions: &mut Vec<Action>) {
        let Some(Editing {
            command,
            scope,
            replacing,
            chord: Some(chord),
            ..
        }) = self.editing.take()
        else {
            return;
        };
        let already = match runs(view, scope, chord) {
            Runs::Here(owner) | Runs::Everywhere(owner) => *owner == command,
            Runs::Nothing => false,
        };
        if already {
            return;
        }
        let mut keymap = view.keymap.clone();
        if let Some((place, old)) =
            replacing.filter(|&(place, old)| keymap.lookup_in(place, old) == Some(&command))
        {
            keymap.unbind_in(place, old);
        }
        keymap.bind_in(scope, chord, command);
        actions.push(Action::SetKeymap(keymap));
    }
}

/// What a chord runs in a place.
enum Runs<'a> {
    /// The place's own command for it.
    Here(&'a CommandId),
    /// The command it runs everywhere, which a panel without its own runs.
    Everywhere(&'a CommandId),
    Nothing,
}

/// What `chord` runs in `scope` now.
fn runs<'a>(view: &View<'a>, scope: Scope, chord: KeyChord) -> Runs<'a> {
    let keymap = view.keymap;
    match (keymap.lookup_in(scope, chord), scope) {
        (Some(owner), _) => Runs::Here(owner),
        (None, Scope::Panel(_)) => keymap.lookup(chord).map_or(Runs::Nothing, Runs::Everywhere),
        (None, Scope::Everywhere) => Runs::Nothing,
    }
}

/// Names the places keys work, by the panels' titles.
struct Places<'a>(&'a Registry);

impl Places<'_> {
    /// "Everywhere", or the panel's title.
    fn name(&self, scope: Scope) -> &'static str {
        match scope {
            Scope::Everywhere => "Everywhere",
            Scope::Panel(id) => self.0.get(id).map_or(id, |panel| panel.title),
        }
    }

    /// Nothing, or " in the Piano Roll panel", to follow what keys run.
    fn suffix(&self, scope: Scope) -> String {
        match scope {
            Scope::Everywhere => String::new(),
            Scope::Panel(_) => format!(" in the {} panel", self.name(scope)),
        }
    }

    /// Everywhere, then each panel, in the order the page places them.
    fn all(&self) -> impl Iterator<Item = Scope> + '_ {
        let panels = self.0.iter().filter_map(|panel| Scope::panel(panel.id));
        std::iter::once(Scope::Everywhere).chain(panels)
    }
}

/// The user's keymap file, what was wrong with it, and what can be done with
/// it.
fn file(ui: &mut Ui, view: &View<'_>, places: &Places<'_>, actions: &mut Vec<Action>) {
    let status = &view.keymap_file;
    let warn = ui.visuals().warn_fg_color;
    if let Some(error) = status.error {
        ui.label(RichText::new(error).color(warn));
    }
    for problem in status.problems {
        match problem {
            Problem::Conflict {
                scope,
                chord,
                commands,
            } => {
                let titles: Vec<_> = commands.iter().map(|id| title_of(view, id)).collect();
                let (here, there) = match scope {
                    Scope::Everywhere => (String::new(), ""),
                    Scope::Panel(_) => (places.suffix(*scope), " there"),
                };
                let text = format!(
                    "Your keymap file gives {chord} to {}{here}, so it runs none of them{there}.",
                    joined(&titles)
                );
                ui.label(RichText::new(text).color(warn));
                ui.horizontal_wrapped(|ui| {
                    for (id, title) in commands.iter().zip(&titles) {
                        if ui.button(format!("Give {chord} to {title}")).clicked() {
                            let mut keymap = view.keymap.clone();
                            keymap.bind_in(*scope, *chord, id.clone());
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
    for (scope, chord, id) in view.keymap.dangling(view.commands) {
        let here = places.suffix(scope);
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{chord} runs {id}{here}, which this version doesn't have."
            ));
            let remove = ui.small_button("Remove");
            named(&remove, format!("Remove {chord}{here}"));
            if remove.clicked() {
                let mut keymap = view.keymap.clone();
                keymap.unbind_in(scope, chord);
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
fn reset_hint(
    view: &View<'_>,
    places: &Places<'_>,
    id: &CommandId,
    defaults: &[(Scope, KeyChord)],
) -> String {
    let mut hint = match defaults {
        [] => "Back to no keys, as by default.".to_owned(),
        _ => {
            let defaults: Vec<_> = (defaults.iter())
                .map(|&(scope, chord)| format!("{chord}{}", places.suffix(scope)))
                .collect();
            format!("Back to the default keys: {}.", defaults.join(", "))
        }
    };
    for &(scope, chord) in defaults {
        let owner = view.keymap.lookup_in(scope, chord);
        if let Some(owner) = owner.filter(|owner| *owner != id) {
            let here = places.suffix(scope);
            hint += &format!("\nTakes {chord}{here} from {}.", title_of(view, owner));
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
    use egui::accesskit::Toggled;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{Keymap, notes, transport};
    use musicable_model::Project;

    use super::*;
    use crate::panels::PIANO_ROLL;
    use crate::testing::{State, harness};

    const ROLL: Scope = Scope::Panel(PIANO_ROLL);

    fn chord(text: &str) -> KeyChord {
        text.parse().unwrap()
    }

    fn keys(keymap: &Keymap, id: &CommandId) -> Vec<String> {
        keymap.chords_for(id).map(|c| c.to_string()).collect()
    }

    /// The shell with the keys page in a window of its own, as the app's
    /// Settings window shows it.
    fn window() -> Harness<'static, State> {
        let mut harness = harness(Project::new());
        harness.state_mut().keys_page = true;
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

    /// Presses `chord` while the page waits for keys.
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
        assert!(!harness.state().shell.giving_keys());
        assert_eq!(harness.state().keymap, Keymap::default_keymap());
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
        // The search field has the focus when the page comes into sight.
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
            scope: Scope::Everywhere,
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
    fn conflicts_in_a_panel_s_table_are_resolved_there() {
        let mut harness = window();
        let l = chord("L");
        harness.state_mut().keymap_problems = vec![Problem::Conflict {
            scope: ROLL,
            chord: l,
            commands: vec![notes::TRANSPOSE_UP, notes::TRANSPOSE_DOWN],
        }];
        harness.run();
        harness.get_by_label(
            "Your keymap file gives L to Transpose Up and Transpose Down in the Piano Roll \
             panel, so it runs none of them there.",
        );
        click(&mut harness, "Give L to Transpose Down");
        let keymap = &harness.state().keymap;
        assert_eq!(keymap.lookup_in(ROLL, l), Some(&notes::TRANSPOSE_DOWN));
        assert_eq!(keymap.lookup(l), None);
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

        // And in a panel's table.
        (harness.state_mut().keymap).bind_in(ROLL, chord("Ctrl+F12"), "later.thing");
        harness.run();
        harness.get_by_label(
            "Ctrl+F12 runs later.thing in the Piano Roll panel, which this version doesn't have.",
        );
        click(&mut harness, "Remove Ctrl+F12 in the Piano Roll panel");
        assert_eq!(harness.state().keymap, Keymap::default_keymap());
    }

    #[test]
    fn each_command_shows_where_its_keys_work() {
        let mut harness = window();
        // The note commands' arrow keys work in the piano roll only, on a
        // row of their own.
        harness
            .get_by_role(Role::TextInput)
            .type_text("transpose up");
        harness.run();
        harness.get_by_label("Transpose Up");
        harness.get_by_label("Everywhere");
        harness.get_by_label("Add keys to Transpose Up");
        harness.get_by_label("Piano Roll");
        harness.get_by_label("Remove Up in the Piano Roll panel");
        harness.get_by_label("Add keys to Transpose Up in the Piano Roll panel");
        // Undo has keys everywhere only.
        let mut harness = window();
        harness.get_by_role(Role::TextInput).type_text("undo");
        harness.run();
        harness.get_by_label("Everywhere");
        harness.get_by_label("Add keys to Undo");
        assert!(harness.query_by_label("Piano Roll").is_none());
    }

    #[test]
    fn keys_can_be_given_to_a_command_in_one_panel() {
        let mut harness = window();
        let metronome = transport::TOGGLE_METRONOME;
        click(&mut harness, "Add keys to Toggle Metronome");
        // They work everywhere, unless another place is picked.
        let everywhere = harness.get_by_label("Where: Everywhere");
        assert_eq!(everywhere.accesskit_node().toggled(), Some(Toggled::True));
        click(&mut harness, "Where: Piano Roll");
        press(&mut harness, "M");
        harness.get_by_label("isn't used yet.");
        click(&mut harness, "Assign");

        let keymap = &harness.state().keymap;
        assert_eq!(keymap.lookup_in(ROLL, chord("M")), Some(&metronome));
        assert_eq!(keymap.lookup(chord("M")), None);
        assert_eq!(keys(keymap, &metronome), ["O"]);
        // A row of its own says where.
        harness.get_by_label("Remove M in the Piano Roll panel");
        harness.get_by_label("Reset Toggle Metronome");
    }

    #[test]
    fn a_panel_s_keys_over_keys_that_work_everywhere_say_so() {
        let mut harness = window();
        let transpose = notes::TRANSPOSE_UP;
        // In the piano roll, O transposes instead of the metronome.
        click(
            &mut harness,
            "Add keys to Transpose Up in the Piano Roll panel",
        );
        press(&mut harness, "O");
        harness.get_by_label(
            "runs Toggle Metronome everywhere, and would run Transpose Up in the Piano Roll \
             panel instead.",
        );
        click(&mut harness, "Assign");
        let keymap = &harness.state().keymap;
        assert_eq!(keymap.lookup_in(ROLL, chord("O")), Some(&transpose));
        assert_eq!(
            keymap.lookup(chord("O")),
            Some(&transport::TOGGLE_METRONOME)
        );
        harness.get_by_label("over Toggle Metronome");

        // Keys the piano roll gives another command can still work
        // everywhere else.
        click(&mut harness, "Add keys to Toggle Loop");
        press(&mut harness, "Up");
        harness.get_by_label("isn't used yet, but the Piano Roll panel keeps it for Transpose Up.");
        click(&mut harness, "Assign");
        let keymap = &harness.state().keymap;
        assert_eq!(keymap.lookup(chord("Up")), Some(&transport::TOGGLE_LOOP));
        assert_eq!(keymap.lookup_in(ROLL, chord("Up")), Some(&transpose));
    }

    #[test]
    fn keys_move_between_places() {
        let mut harness = window();
        let transpose = notes::TRANSPOSE_UP;
        // Up moves from the piano roll to everywhere.
        harness
            .get_by_role(Role::TextInput)
            .type_text("transpose up");
        harness.run();
        click(&mut harness, "Up");
        harness.get_by_label_contains("Press the keys to use in place of Up");
        let roll = harness.get_by_label("Where: Piano Roll");
        assert_eq!(roll.accesskit_node().toggled(), Some(Toggled::True));
        click(&mut harness, "Where: Everywhere");
        press(&mut harness, "Up");
        harness.get_by_label("isn't used yet.");
        click(&mut harness, "Assign");
        let keymap = &harness.state().keymap;
        assert_eq!(keymap.lookup(chord("Up")), Some(&transpose));
        assert_eq!(keymap.lookup_in(ROLL, chord("Up")), None);

        // Reset puts it back.
        harness.get_by_label("Reset Transpose Up").hover();
        harness.run();
        harness.get_by_label_contains("Back to the default keys: Up in the Piano Roll panel.");
        click(&mut harness, "Reset Transpose Up");
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
    fn hiding_the_page_stops_waiting_for_keys() {
        let mut harness = window();
        click(&mut harness, "Add keys to Play");
        let shell = &harness.state().shell;
        assert!(shell.capturing() && shell.giving_keys());
        harness.state_mut().keys_page = false;
        harness.run();
        let shell = &harness.state().shell;
        assert!(!shell.capturing() && !shell.giving_keys());
        // Back in sight, its search field has the focus again.
        harness.state_mut().keys_page = true;
        harness.run();
        assert!(harness.get_by_role(Role::TextInput).is_focused());
        assert!(!harness.state().shell.giving_keys());
    }

    #[test]
    fn opening_the_palette_stops_waiting_for_keys() {
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
