//! The controls of Musicable's own devices, in the devices panel. Each
//! parameter has one: a bar to drag for a number, a button for a switch and
//! a menu for a choice. They stand in columns in the order of the device's
//! table, a column for each group of parameters, such as an EQ's band.

use egui::text::{CCursor, CCursorRange};
use egui::{
    Align2, Color32, ComboBox, FontId, Id, Key, Popup, Rangef, Rect, Response, RichText, Sense,
    StrokeKind, TextEdit, Ui, vec2,
};
use musicable_commands::{Args, device};
use musicable_model::native::{NativeDevice, ParamInfo, Unit};
use musicable_model::param::ParamAddress;
use musicable_model::{DeviceId, Project};

use crate::mixer::{automated_value, automation_items, automation_mark, name_menu, slider_info};
use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::shell::{Action, View};
use crate::theme::Theme;
use crate::widgets::{Face, toggle};

/// The most controls in a column: a group with more goes on in the next
/// one.
pub(crate) const ROWS: usize = 5;

/// Each group's name and parameters, by their places in `params`, with the
/// groups in the order they first come in the table. Tables only grow at
/// the end, so a parameter added to an earlier group joins it there.
pub(crate) fn groups(params: &[ParamInfo]) -> Vec<(&'static str, Vec<usize>)> {
    let mut groups: Vec<(&'static str, Vec<usize>)> = Vec::new();
    for (index, info) in params.iter().enumerate() {
        match groups.iter_mut().find(|(group, _)| *group == info.group) {
            Some((_, members)) => members.push(index),
            None => groups.push((info.group, vec![index])),
        }
    }
    groups
}

/// The parameters in each column, by their places in `params`: a group's,
/// at most [`ROWS`] to a column, with the groups in [`groups`]' order.
fn columns(params: &[ParamInfo]) -> Vec<Vec<usize>> {
    let groups = groups(params);
    let columns = groups.iter().flat_map(|(_, members)| members.chunks(ROWS));
    columns.map(<[usize]>::to_vec).collect()
}

/// How wide `device`'s controls are, side by side.
pub(crate) fn width(device: &NativeDevice, theme: &Theme) -> f32 {
    let columns = columns(device.params()).len() as f32;
    let sizes = &theme.devices;
    columns * sizes.control + (columns - 1.0).max(0.0) * sizes.gap
}

/// Shows the controls of `device`, the device with id `id`, which the panel
/// calls `name`, and asks for the changes the user makes to it. A
/// parameter with automation shows the value its lane gives it at the
/// playhead, as the mixer's controls do, until someone overrides it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn controls(
    ui: &mut Ui,
    id: DeviceId,
    name: &str,
    device: &NativeDevice,
    view: &View<'_>,
    theme: &Theme,
    mouse: &MouseMap,
    actions: &mut Vec<Action>,
) {
    let params = device.params();
    // A device without groups needs no headings.
    let headed = params.iter().any(|info| !info.group.is_empty());
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = theme.devices.gap;
        let mut group = None;
        for column in columns(params) {
            // A group's name heads its first column.
            let title = params[column[0]].group;
            let heading = if group == Some(title) { "" } else { title };
            group = Some(title);
            ui.vertical(|ui| {
                ui.set_width(theme.devices.control);
                if headed {
                    ui.label(RichText::new(heading).size(theme.small_text).weak());
                }
                for index in column {
                    let param = ParamAddress::Device(id, index as u32);
                    let playhead = view.playback.playhead;
                    let own = device.values[index];
                    let automated = automated_value(view.project, param, playhead, own);
                    // Between points, a switch or a choice shows its
                    // nearest value.
                    let value = params[index].clamp(automated);
                    let control = Control {
                        device: id,
                        index,
                        info: &params[index],
                        label: format!("{name} {}", params[index].full_name()),
                        project: view.project,
                    };
                    ui.push_id(index, |ui| control.show(ui, value, theme, mouse, actions));
                }
            });
        }
    });
}

/// One parameter's control.
struct Control<'a> {
    device: DeviceId,
    /// The parameter's place in the device's table.
    index: usize,
    info: &'a ParamInfo,
    /// What screen readers call it: "EQ Band 1 Freq".
    label: String,
    /// The project, for the parameter's automation.
    project: &'a Project,
}

impl Control<'_> {
    fn show(
        &self,
        ui: &mut Ui,
        value: f64,
        theme: &Theme,
        mouse: &MouseMap,
        actions: &mut Vec<Action>,
    ) {
        let size = vec2(theme.devices.control, ui.spacing().interact_size.y);
        let info = self.info;
        match info.unit {
            Unit::Switch => {
                let on = value >= 0.5;
                let face = Face::Text(info.name);
                let button = toggle(ui, face, on, theme.switch, &self.label, size, theme)
                    .on_hover_text(format!("{}: {}", info.full_name(), info.format(value)));
                if button.clicked() {
                    actions.push(self.set(if on { 0.0 } else { 1.0 }));
                }
                self.automated(ui, &button, theme, actions);
            }
            Unit::Choice(choices) => {
                let current = info.format(value);
                // With its name, if there's room for it with every choice:
                // "Detector: Peak".
                let font = FontId::proportional(theme.small_text);
                let width = |text: String| {
                    let galley =
                        ui.painter()
                            .layout_no_wrap(text, font.clone(), Color32::PLACEHOLDER);
                    galley.size().x
                };
                let room = size.x - ui.spacing().icon_width - 3.0 * ui.spacing().button_padding.x;
                let named = (choices.iter())
                    .all(|choice| width(format!("{}: {choice}", info.name)) <= room);
                let shown = if named {
                    format!("{}: {current}", info.name)
                } else {
                    current.clone()
                };
                let mut picked = None;
                let menu = ComboBox::from_id_salt(self.index)
                    .width(size.x)
                    .selected_text(RichText::new(shown).size(theme.small_text))
                    .show_ui(ui, |ui| {
                        for (choice, &text) in choices.iter().enumerate() {
                            let choice = choice as f64;
                            if ui.selectable_label(choice == value, text).clicked() {
                                picked = Some(choice);
                            }
                        }
                    });
                name_menu(&menu.response, self.label.clone(), &current);
                let response = menu
                    .response
                    .on_hover_text(format!("{}: {current}", info.full_name()));
                if let Some(choice) = picked.filter(|&choice| choice != value) {
                    actions.push(self.set(choice));
                }
                self.automated(ui, &response, theme, actions);
            }
            _ => {
                let (response, moved) = self.bar(ui, value, size.x, theme, mouse, actions);
                automation_mark(ui, theme, self.project, &response, self.param());
                if response.drag_started() {
                    actions.push(Action::BeginGesture);
                }
                if let Some(value) = moved {
                    actions.push(self.set(value));
                }
                if response.drag_stopped() {
                    actions.push(Action::EndGesture);
                }
            }
        }
    }

    /// Where automation finds the parameter.
    fn param(&self) -> ParamAddress {
        ParamAddress::Device(self.device, self.index as u32)
    }

    /// Marks the control `response` if the parameter has automation, and
    /// gives it a menu of the automation's commands. The menu has an id of
    /// its own, so it leaves a choice's own menu be.
    fn automated(&self, ui: &Ui, response: &Response, theme: &Theme, actions: &mut Vec<Action>) {
        let param = self.param();
        automation_mark(ui, theme, self.project, response, param);
        Popup::context_menu(response)
            .id(response.id.with("automation"))
            .show(|ui| automation_items(ui, self.project, param, actions));
    }

    /// The command that sets the parameter to `value`.
    fn set(&self, value: f64) -> Action {
        let args = Args::new()
            .with("device", self.device)
            .with("param", self.index as i64)
            .with("value", value);
        Action::RunWith(device::SET_PARAM, args)
    }

    /// A bar `width` wide, filled as far as `value` is along the
    /// parameter's range, with its name and its value on it. Dragging it
    /// sideways moves the value, and the mouse map's fine drag, Ctrl-drag in
    /// Live's, a tenth as far. A double-click gives the default, and the
    /// right-click menu takes a typed value, and has the automation's
    /// commands. Returns the bar, and the value the user moved it to.
    fn bar(
        &self,
        ui: &mut Ui,
        value: f64,
        width: f32,
        theme: &Theme,
        mouse: &MouseMap,
        actions: &mut Vec<Action>,
    ) -> (Response, Option<f64>) {
        let info = self.info;
        let size = vec2(width, ui.spacing().interact_size.y);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let mut moved = slider_info(ui, &response, &self.label, value, info.min..=info.max);
        let at = info.to_unit(value) as f32;
        // Where the drag has got to, which a stepped parameter's value
        // lags.
        let dragged_to = response.id.with("at");
        if response.drag_started() {
            ui.data_mut(|data| data.insert_temp(dragged_to, at));
        }
        let delta = response.drag_delta().x;
        if response.dragged() && delta != 0.0 && rect.width() > 0.0 {
            let mods = modifiers(ui.input(|input| input.modifiers));
            let action = mouse.action(Target::DeviceControl, Gesture::Drag, mods);
            let scale = if action == Some(MouseAction::Fine) {
                0.1
            } else {
                1.0
            };
            let from = ui.data(|data| data.get_temp(dragged_to)).unwrap_or(at);
            let to = (from + delta / rect.width() * scale).clamp(0.0, 1.0);
            ui.data_mut(|data| data.insert_temp(dragged_to, to));
            moved = Some(info.from_unit(f64::from(to)));
        }
        if response.double_clicked() {
            moved = Some(info.default);
        }
        if ui.is_rect_visible(rect) {
            self.paint(ui, &response, rect, value, at, theme);
        }
        let response = response.on_hover_text(format!(
            "{}: {}\nDrag, or Ctrl-drag to move finely. Double-click for the default, {}. \
             Right-click to type a value.",
            info.full_name(),
            info.format(value),
            info.format(info.default),
        ));
        let typed_id = response.id.with("typed");
        let menu = response.context_menu(|ui| {
            if let Some(typed) = typed(ui, typed_id, info, value, theme.small_text) {
                moved = Some(typed);
                ui.close();
            }
            ui.separator();
            automation_items(ui, self.project, self.param(), actions);
        });
        // The menu starts from the value each time it opens.
        if menu.is_none() {
            ui.data_mut(|data| data.remove::<String>(typed_id));
        }
        (response, moved.filter(|&new| new != value))
    }

    /// Paints the bar at `rect`, with `value` at `at` of the way along it.
    /// A range either side of zero fills from zero, as a pan does.
    fn paint(&self, ui: &Ui, response: &Response, rect: Rect, value: f64, at: f32, theme: &Theme) {
        let info = self.info;
        let painter = ui.painter_at(rect);
        let visuals = ui.style().interact(response);
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let zero = if info.min < 0.0 && info.max > 0.0 {
            info.to_unit(0.0) as f32
        } else {
            0.0
        };
        let x = |unit: f32| rect.left() + rect.width() * unit.clamp(0.0, 1.0);
        let filled = Rect::from_x_y_ranges(
            Rangef::new(x(zero.min(at)), x(zero.max(at))),
            rect.y_range(),
        );
        painter.rect_filled(filled, 2.0, ui.visuals().selection.bg_fill);
        let font = FontId::proportional(theme.small_text);
        let color = visuals.text_color();
        let text = painter.text(
            rect.right_center() - vec2(3.0, 0.0),
            Align2::RIGHT_CENTER,
            info.format(value),
            font.clone(),
            color,
        );
        let room = Rect::from_x_y_ranges(rect.left()..=text.left() - 2.0, rect.y_range());
        painter.with_clip_rect(room).text(
            rect.left_center() + vec2(3.0, 0.0),
            Align2::LEFT_CENTER,
            info.name,
            font,
            color,
        );
        painter.rect_stroke(rect, 2.0, visuals.bg_stroke, StrokeKind::Inside);
    }
}

/// The right-click menu's field for typing a value, kept under `id` while
/// the menu is open, with a hint under it in `small_text`. Returns the
/// value typed, once Enter takes it.
fn typed(ui: &mut Ui, id: Id, info: &ParamInfo, value: f64, small_text: f32) -> Option<f64> {
    let stored = ui.data(|data| data.get_temp::<String>(id));
    let first = stored.is_none();
    let mut text = stored.unwrap_or_else(|| info.format(value));
    let name = ui.label(RichText::new(info.full_name()).weak());
    let width = ui.spacing().text_edit_width / 2.0;
    let field = ui
        .add(TextEdit::singleline(&mut text).desired_width(width))
        .labelled_by(name.id);
    if first {
        // The whole value, ready to be typed over.
        if let Some(mut state) = TextEdit::load_state(ui.ctx(), field.id) {
            let all = CCursorRange::two(CCursor::new(0), CCursor::new(text.chars().count()));
            state.cursor.set_char_range(Some(all));
            state.store(ui.ctx(), field.id);
        }
        field.request_focus();
    }
    let entered = field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));
    let parsed = info.parse(&text);
    if entered && parsed.is_none() {
        field.request_focus();
    }
    let hint = if text.trim().is_empty() || parsed.is_some() {
        "Enter sets it"
    } else {
        "Not a value it takes"
    };
    ui.label(RichText::new(hint).size(small_text).weak());
    ui.data_mut(|data| data.insert_temp(id, text));
    parsed.filter(|_| entered)
}

#[cfg(test)]
mod tests {
    use musicable_model::native::{NativeKind, eq, sampler, utility};

    use super::*;

    #[test]
    fn groups_stand_in_columns_of_at_most_five() {
        let all = |kind: NativeKind| columns(kind.params());
        // A column for each band, and one for the output.
        let eq = all(NativeKind::Eq);
        assert_eq!(eq.len(), eq::BANDS + 1);
        assert_eq!(eq[2], Vec::from_iter(2 * eq::PER_BAND..3 * eq::PER_BAND));
        assert_eq!(eq[eq::BANDS], [eq::OUTPUT]);
        // Seven ungrouped controls take two columns.
        assert_eq!(all(NativeKind::Utility), [vec![0, 1, 2, 3, 4], vec![5, 6]]);
        assert_eq!(utility::DC, 6);
        // The sampler's nine sample controls take two, with the loop's fade,
        // which came later, among them.
        let sampler = all(NativeKind::Sampler);
        let fade = sampler::LOOP_FADE;
        assert_eq!(sampler[..2], [vec![0, 1, 2, 3, 4], vec![5, 6, 7, fade]]);
        assert_eq!(sampler[2][0], sampler::ATTACK);
        for kind in NativeKind::ALL {
            let params = kind.params();
            let columns = all(kind);
            // Each parameter once, a group's in the table's order.
            let mut places = columns.concat();
            places.sort_unstable();
            assert!(places.into_iter().eq(0..params.len()), "{kind:?}");
            for column in &columns {
                assert!(!column.is_empty() && column.len() <= ROWS);
                assert!(column.windows(2).all(|pair| pair[0] < pair[1]));
                let group = params[column[0]].group;
                assert!(column.iter().all(|&i| params[i].group == group));
            }
            // A group's columns stand together.
            let groups: Vec<_> = columns.iter().map(|c| params[c[0]].group).collect();
            let mut seen = Vec::new();
            for (i, group) in groups.iter().enumerate() {
                if i == 0 || groups[i - 1] != *group {
                    assert!(!seen.contains(group), "{kind:?}: {group} twice");
                    seen.push(*group);
                }
            }
        }
    }
}
