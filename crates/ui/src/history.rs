//! The History panel: every step of the undo history by name, the oldest
//! at the top, under a row for the project as it was before them. Clicking
//! a row takes the project back or forward to it, and the steps after the
//! one it's at, which redo can make again, are greyed out under it. While
//! the panel has the focus, the arrow keys step through the list, as undo
//! and redo do.

use egui::{Align2, FontId, Label, Response, RichText, Role, ScrollArea, Sense, Ui, WidgetInfo};
use musicable_commands::{Args, history};

use crate::shell::{Action, View};
use crate::theme::Theme;

/// The panel's own state, kept between frames.
#[derive(Debug, Default)]
pub(crate) struct HistoryPanel {
    /// How many steps were done when the list was last drawn, to keep the
    /// row the project is at in sight when that changes.
    done: Option<usize>,
    /// How far the list was scrolled.
    scrolled: f32,
}

impl HistoryPanel {
    pub fn show(&mut self, ui: &mut Ui, view: &View<'_>, theme: &Theme, actions: &mut Vec<Action>) {
        let steps = view.history;
        let done = steps.done();
        // The project's row, then a row for each step.
        let name = |row: usize| match row {
            0 => view.name,
            row => steps.name(row - 1).unwrap_or_default(),
        };
        let mut go_to = |row: usize, response: Response| {
            if response.clicked() && row != done {
                let args = Args::new().with("step", row as i64);
                actions.push(Action::RunWith(history::GO_TO, args));
            }
        };
        if steps.is_empty() {
            self.done = None;
            go_to(0, step_row(ui, theme, name(0), true, false));
            let text = "Each change to the project is listed here. Click one to go back to it.";
            ui.add(Label::new(RichText::new(text).weak()).wrap());
            return;
        }
        let height = row_height(ui);
        let mut scroll = ScrollArea::vertical().id_salt("history").auto_shrink(false);
        if self.done != Some(done) {
            // As little scrolling as shows the whole row.
            let spacing = ui.spacing().item_spacing.y;
            let top = done as f32 * (height + spacing);
            let shown = ui.available_height();
            let offset = self.scrolled.max(top + height - shown).min(top);
            scroll = scroll.vertical_scroll_offset(offset);
            self.done = Some(done);
        }
        let output = scroll.show_rows(ui, height, steps.len() + 1, |ui, rows| {
            for row in rows {
                let response = step_row(ui, theme, name(row), row == done, row > done);
                go_to(row, response);
            }
        });
        self.scrolled = output.state.clamped_offset().y;
    }
}

/// A row of the list, called `name`: highlighted if it's where the project
/// is, and greyed out if it was undone. It senses clicks.
fn step_row(ui: &mut Ui, theme: &Theme, name: &str, current: bool, undone: bool) -> Response {
    let size = egui::vec2(ui.available_width(), row_height(ui));
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, current, name));
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let painter = ui.painter_at(rect);
    let visuals = ui.style().interact_selectable(&response, current);
    if current || response.hovered() {
        painter.rect_filled(rect, theme.corner, visuals.weak_bg_fill);
    }
    let color = match undone {
        true => ui.visuals().weak_text_color(),
        false => visuals.text_color(),
    };
    let at = egui::pos2(rect.left() + theme.padding, rect.center().y);
    let font = FontId::proportional(theme.text);
    painter.text(at, Align2::LEFT_CENTER, name, font, color);
    response
}

fn row_height(ui: &Ui) -> f32 {
    ui.spacing().interact_size.y
}

#[cfg(test)]
mod tests {
    use egui::accesskit::{Role, Toggled};
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{Focus, edit};
    use musicable_model::Project;

    use super::*;
    use crate::panels::HISTORY;
    use crate::testing::{
        State, click_at, focus_main, harness, key, own_windows, own_windows_harness, with_own,
    };

    type Harness = egui_kittest::Harness<'static, State>;

    /// The rows the History panel lists, and the one the project is at.
    fn rows(harness: &Harness) -> (Vec<String>, Vec<String>) {
        let rows: Vec<_> = harness.get_all_by_role(Role::ListItem).collect();
        let name = |row: &egui_kittest::Node<'_>| row.accesskit_node().label().unwrap_or_default();
        // egui tells a selected row by its being toggled on.
        let current = rows
            .iter()
            .filter(|row| row.accesskit_node().toggled() == Some(Toggled::True));
        (rows.iter().map(name).collect(), current.map(name).collect())
    }

    /// The arguments of the jumps to a step the shell asked for.
    fn jumps(harness: &Harness) -> Vec<i64> {
        (harness.state().actions.iter())
            .filter_map(|action| match action {
                Action::RunWith(id, args) if *id == history::GO_TO => args.get("step").ok(),
                _ => None,
            })
            .collect()
    }

    fn click(harness: &mut Harness, name: &str) {
        let row = harness.get_by_role_and_label(Role::ListItem, name).rect();
        click_at(harness, row.center());
    }

    #[test]
    fn the_history_panel_lists_the_steps_and_goes_to_the_one_clicked() {
        let mut harness = harness(Project::new());
        harness.state_mut().shell.toggle_panel(HISTORY);
        harness.run();
        // Before any change, there's the project as it opened.
        assert_eq!(
            rows(&harness),
            (vec!["Untitled".into()], vec!["Untitled".into()])
        );
        harness
            .get_by_label("Each change to the project is listed here. Click one to go back to it.");
        for name in ["Add Audio Track", "Set Tempo", "Move Clips"] {
            harness.state_mut().step(name);
        }
        harness.run();
        let listed = ["Untitled", "Add Audio Track", "Set Tempo", "Move Clips"].map(String::from);
        assert_eq!(rows(&harness), (listed.to_vec(), vec!["Move Clips".into()]));

        // An earlier step: those after it stay listed, for redo, and the
        // panel has the focus.
        click(&mut harness, "Add Audio Track");
        assert_eq!(jumps(&harness), [1]);
        assert_eq!(rows(&harness).1, ["Add Audio Track"]);
        assert_eq!(harness.state().editor.focus, Focus::HISTORY);
        assert_eq!(harness.state().history.redo_name(), Some("Set Tempo"));
        // The project before them all, then a later step again.
        click(&mut harness, "Untitled");
        click(&mut harness, "Set Tempo");
        assert_eq!(jumps(&harness), [1, 0, 2]);
        assert_eq!(rows(&harness), (listed.to_vec(), vec!["Set Tempo".into()]));
        // The row the project is at asks for nothing.
        click(&mut harness, "Set Tempo");
        assert_eq!(jumps(&harness), [1, 0, 2]);
        // A new step drops those undone.
        harness.state_mut().step("Delete Track");
        harness.run();
        let listed = ["Untitled", "Add Audio Track", "Set Tempo", "Delete Track"];
        assert_eq!(rows(&harness).0, listed);
    }

    #[test]
    fn the_history_panel_keeps_the_step_it_is_at_in_sight() {
        let mut harness = harness(Project::new());
        harness.state_mut().shell.toggle_panel(HISTORY);
        for step in 1..=100 {
            harness.state_mut().step(&format!("Step {step}"));
        }
        harness.run();
        // The list draws the rows in sight.
        let drawn = |harness: &Harness, name: &str| {
            (harness.query_by_role_and_label(Role::ListItem, name)).is_some()
        };
        // The newest step, at the foot of the list.
        assert!(drawn(&harness, "Step 100") && !drawn(&harness, "Step 1"));
        // Back to the first, and the list scrolls up to it.
        assert!(harness.state_mut().history.go_to(1));
        harness.run();
        assert!(drawn(&harness, "Step 1") && !drawn(&harness, "Step 100"));
    }

    #[test]
    fn the_history_panel_has_arrow_keys_of_its_own_in_a_window_of_its_own() {
        let mut harness = own_windows_harness(Project::new());
        for name in ["Add Audio Track", "Set Tempo"] {
            harness.state_mut().step(name);
        }
        let shell = &mut harness.state_mut().shell;
        shell.show_panel(HISTORY);
        shell.move_to_new_window(HISTORY).unwrap();
        harness.run();
        let [window] = &own_windows()[..] else {
            panic!("one window of its own");
        };
        let window = window.id;
        focus_main(&mut harness, false);
        with_own(&mut harness, window, |own| own.info.focused = Some(true));
        harness.run();
        assert_eq!(harness.state().editor.focus, Focus::HISTORY);
        // Up takes a step back, as undo does, and Down makes it again.
        for arrow in [egui::Key::ArrowUp, egui::Key::ArrowDown] {
            with_own(&mut harness, window, |own| own.events.extend(key(arrow)));
            harness.run();
        }
        let actions = [Action::Run(edit::UNDO), Action::Run(edit::REDO)];
        assert_eq!(harness.state().actions, actions);
    }

    #[test]
    fn a_step_clicked_in_a_window_of_its_own_is_gone_to() {
        let mut harness = harness(Project::new());
        for name in ["Add Audio Track", "Set Tempo"] {
            harness.state_mut().step(name);
        }
        let shell = &mut harness.state_mut().shell;
        shell.show_panel(HISTORY);
        shell.move_to_new_window(HISTORY).unwrap();
        harness.run();
        // The tests show the window in the main one.
        harness.get_by_label("History - Untitled - Musicable");
        harness
            .get_by_role_and_label(Role::ListItem, "Add Audio Track")
            .click();
        harness.run();
        assert_eq!(jumps(&harness), [1]);
        assert_eq!(rows(&harness).1, ["Add Audio Track"]);
    }
}
