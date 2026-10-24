//! The command palette: every command, found by typing part of its name or
//! its keys, and run with Enter.

use egui::{Align2, Button, Id, Key, Modal, Modifiers, RichText, ScrollArea, TextEdit, vec2};
use musicable_commands::{Command, CommandId};

use crate::menus::label;
use crate::search::score;
use crate::shell::{Action, View, with_keys};

/// How many of the commands last run from the palette it lists first.
const RECENT: usize = 5;

/// The command palette's own state.
#[derive(Debug, Default)]
pub(crate) struct Palette {
    open: bool,
    /// What the user typed.
    query: String,
    /// The highlighted command, by its place in the list.
    selected: usize,
    /// The commands last run from the palette, most recent first.
    recent: Vec<CommandId>,
}

impl Palette {
    pub(crate) fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context, view: &View<'_>, actions: &mut Vec<Action>) {
        if !self.open {
            return;
        }
        // The keys that move through the list, before the text field sees
        // them.
        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.count_and_consume_key(Modifiers::NONE, Key::ArrowUp),
                i.count_and_consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let id = Id::unique("command_palette");
        let area = Modal::default_area(id).anchor(Align2::CENTER_TOP, vec2(0.0, 72.0));
        let mut run = None;
        let modal = Modal::new(id).area(area).show(ctx, |ui| {
            ui.set_width(520.0);
            let field = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text("Type the name of a command, or its keys")
                    .desired_width(f32::INFINITY),
            );
            field.request_focus();
            if field.changed() {
                self.selected = 0;
            }
            let matches = self.matches(view);
            let last = matches.len().saturating_sub(1);
            self.selected = (self.selected + down).saturating_sub(up).min(last);
            ui.add_space(4.0);
            ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (i, (command, label)) in matches.iter().enumerate() {
                    let selected = i == self.selected;
                    let button = Button::selectable(selected, label.as_str())
                        .min_size(vec2(ui.available_width(), 0.0));
                    let button = with_keys(button, view, &command.id);
                    // Greyed out when it doesn't apply, as in the menus.
                    let applies = command.when.applies(view.project, view.editor);
                    let response = ui.add_enabled(applies, button);
                    if selected && up + down > 0 {
                        response.scroll_to_me(None);
                    }
                    if response.clicked() {
                        run = Some(command.id.clone());
                    }
                }
                if matches.is_empty() {
                    ui.label(RichText::new("No command matches.").weak());
                }
            });
            if enter && run.is_none() {
                run = matches
                    .get(self.selected)
                    .filter(|(c, _)| c.when.applies(view.project, view.editor))
                    .map(|(c, _)| c.id.clone());
            }
        });
        if let Some(id) = run {
            self.open = false;
            self.recent.retain(|recent| *recent != id);
            self.recent.insert(0, id.clone());
            self.recent.truncate(RECENT);
            actions.push(Action::Run(id));
        } else if escape || modal.backdrop_response.clicked() {
            self.open = false;
        }
    }

    /// The commands that match what the user typed, with their labels: best
    /// first, then the ones last run from the palette, then by name. Those
    /// that need arguments, which the palette can't give, are left out.
    fn matches<'a>(&self, view: &View<'a>) -> Vec<(&'a Command, String)> {
        let mut matches: Vec<_> = view
            .commands
            .iter()
            .filter(|command| !command.needs_arguments())
            .filter_map(|command| {
                let label = label(command);
                let chords = view.keymap.chords_focused(&command.id, view.editor.focus);
                let score = score(&self.query, &label, command.id.as_str(), &chords)?;
                Some((command, label, score))
            })
            .collect();
        let recency = |id: &CommandId| self.recent.iter().position(|r| r == id).unwrap_or(RECENT);
        matches.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| recency(&a.0.id).cmp(&recency(&b.0.id)))
                .then_with(|| a.1.cmp(&b.1))
        });
        matches
            .into_iter()
            .map(|(command, label, _)| (command, label))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use musicable_commands::{Focus, file, transport};
    use musicable_model::Project;

    use super::*;
    use crate::testing::{State, harness};

    /// The shell with the command palette open.
    fn palette() -> Harness<'static, State> {
        let mut harness = harness(Project::new());
        harness.state_mut().shell.open_palette();
        harness.run();
        harness
    }

    fn press(harness: &mut Harness<'static, State>, key: Key) {
        harness.key_press(key);
        harness.run();
    }

    fn type_text(harness: &mut Harness<'static, State>, text: &str) {
        harness.get_by_role(egui::Role::TextInput).type_text(text);
        harness.run();
    }

    fn ran(harness: &Harness<'static, State>) -> Vec<Action> {
        harness.state().actions.clone()
    }

    #[test]
    fn every_command_is_listed_with_its_keys() {
        let harness = palette();
        let state = harness.state();
        for command in state.commands.iter().filter(|c| !c.needs_arguments()) {
            let chords = (state.keymap).chords_focused(&command.id, state.editor.focus);
            let keys: Vec<_> = chords.iter().map(ToString::to_string).collect();
            let row = match keys.join(", ") {
                keys if keys.is_empty() => label(command),
                keys => format!("{} {keys}", label(command)),
            };
            harness.get_by_label(&row);
        }
        harness.get_by_label("File: New Demo Project");
        harness.get_by_label("Transport: Toggle Loop Ctrl+L");
        harness.get_by_label("View: Command Palette... Ctrl+Shift+P");
        // The palette can't say which clips to move.
        assert!(harness.query_by_label_contains("Move Clips").is_none());
        assert!(harness.state().shell.asking(), "keys don't run commands");
    }

    #[test]
    fn the_keys_are_those_that_work_where_the_focus_is() {
        let mut harness = palette();
        // The arrow keys move notes in the piano roll only.
        type_text(&mut harness, "transpose up");
        harness.get_by_label("Notes: Transpose Up");
        harness.state_mut().editor.focus = Focus::PIANO_ROLL;
        harness.run();
        harness.get_by_label("Notes: Transpose Up Up");
        // There, they find what they run.
        let mut harness = palette();
        harness.state_mut().editor.focus = Focus::PIANO_ROLL;
        type_text(&mut harness, "shift+up");
        harness.get_by_label("Notes: Octave Up Shift+Up");
        let mut harness = palette();
        type_text(&mut harness, "shift+up");
        assert!(harness.query_by_label_contains("Octave Up").is_none());
    }

    #[test]
    fn typing_finds_a_command_and_enter_runs_it() {
        let mut harness = palette();
        type_text(&mut harness, "metro");
        assert!(
            harness
                .query_by_label("Transport: Toggle Loop Ctrl+L")
                .is_none()
        );
        press(&mut harness, Key::Enter);
        assert_eq!(ran(&harness), [Action::Run(transport::TOGGLE_METRONOME)]);
        assert!(!harness.state().shell.asking(), "it closes");

        // The keys find their command too.
        let mut harness = palette();
        type_text(&mut harness, "ctrl+s");
        press(&mut harness, Key::Enter);
        assert_eq!(ran(&harness), [Action::Run(file::SAVE)]);

        let mut harness = palette();
        type_text(&mut harness, "nothing like it");
        harness.get_by_label("No command matches.");
        press(&mut harness, Key::Enter);
        assert_eq!(ran(&harness), []);
        assert!(harness.state().shell.asking(), "it stays open");
    }

    #[test]
    fn the_arrows_choose_a_command() {
        let mut harness = palette();
        // "Toggle Punch In", then "Toggle Punch Out".
        type_text(&mut harness, "transport toggle punch");
        press(&mut harness, Key::ArrowDown);
        press(&mut harness, Key::ArrowDown);
        press(&mut harness, Key::ArrowUp);
        press(&mut harness, Key::Enter);
        assert_eq!(ran(&harness), [Action::Run(transport::TOGGLE_PUNCH_IN)]);

        let mut harness = palette();
        type_text(&mut harness, "transport toggle punch");
        press(&mut harness, Key::ArrowUp);
        press(&mut harness, Key::ArrowDown);
        press(&mut harness, Key::ArrowDown);
        press(&mut harness, Key::Enter);
        assert_eq!(ran(&harness), [Action::Run(transport::TOGGLE_PUNCH_OUT)]);
    }

    #[test]
    fn clicking_a_command_runs_it() {
        let mut harness = palette();
        // Narrowed down, so that it's in sight.
        type_text(&mut harness, "play");
        harness.get_by_label("Transport: Play Shift+Space").click();
        harness.run();
        assert_eq!(ran(&harness), [Action::Run(transport::PLAY)]);
        assert!(!harness.state().shell.asking());
    }

    #[test]
    fn escape_or_a_click_outside_closes_it() {
        let mut harness = palette();
        type_text(&mut harness, "loop");
        press(&mut harness, Key::Escape);
        assert!(!harness.state().shell.asking());

        let mut harness = palette();
        harness
            .get_by_role_and_label(egui::Role::Button, "Go to Start")
            .click();
        harness.run();
        assert!(!harness.state().shell.asking());
        assert_eq!(ran(&harness), []);
    }

    #[test]
    fn escape_closes_the_palette_and_not_the_window_under_it() {
        let mut harness = harness(Project::new());
        harness.state_mut().shell.open_shortcuts();
        harness.state_mut().shell.open_palette();
        harness.run();
        press(&mut harness, Key::Escape);
        assert!(!harness.state().shell.asking());
        harness.get_by_label("Keyboard Shortcuts");
        press(&mut harness, Key::Escape);
        assert!(harness.query_by_label("Keyboard Shortcuts").is_none());
    }

    #[test]
    fn the_last_commands_run_come_first() {
        let mut harness = palette();
        type_text(&mut harness, "tempo down");
        press(&mut harness, Key::Enter);
        // Opened again, it lists that command first, and starts empty.
        harness.state_mut().shell.open_palette();
        harness.run();
        assert_eq!(
            harness
                .get_by_role(egui::Role::TextInput)
                .value()
                .as_deref(),
            Some("")
        );
        press(&mut harness, Key::Enter);
        // Among the commands that match equally well too.
        harness.state_mut().shell.open_palette();
        harness.run();
        type_text(&mut harness, "tempo");
        press(&mut harness, Key::Enter);
        let tempo_down = Action::Run(transport::TEMPO_DOWN);
        assert_eq!(
            ran(&harness),
            [tempo_down.clone(), tempo_down.clone(), tempo_down]
        );
    }
}
