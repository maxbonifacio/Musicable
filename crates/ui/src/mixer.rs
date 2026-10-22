//! The mixer: a channel strip for each track, in the project's order, and
//! the master's on the right.
//!
//! A strip has, from the top: the track's name, where its output goes, a
//! send to each return track, the pan, the fader beside the meter, the gain
//! and the highest peak, mute, solo and arm, and the clip stage. Its
//! controls hand back [`Change`]s for the app to make.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use egui::accesskit::{Action as Request, ActionData};
use egui::{
    Align, Align2, AsIdSalt, Color32, ComboBox, DragValue, FontId, Frame, Label, Layout, Painter,
    Rangef, Rect, Response, RichText, Role, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2,
    WidgetInfo, pos2, vec2,
};
use musicable_commands::mixer::Change;
use musicable_model::mixer::Bus;
use musicable_model::{ClipType, Project, Track, TrackId, TrackKind};

use crate::mouse::{Gesture, MouseAction, MouseMap, Target, modifiers};
use crate::shell::Action;
use crate::theme::{MixerSizes, Theme};
use crate::widgets::{Face, toggle};

/// A channel's meter readings, linear: 1.0 is full scale.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Level {
    /// Left and right, falling back once the signal drops.
    pub peak: [f32; 2],
    /// Left and right, averaged over a fraction of a second.
    pub rms: [f32; 2],
    /// The highest sample since the peaks were last cleared.
    pub highest: f32,
}

/// The readings of the project's meters, as the engine last measured them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Levels {
    pub tracks: BTreeMap<TrackId, Level>,
    pub master: Level,
}

impl Levels {
    /// The readings of the track with `id`: silence if it has no meter.
    pub fn track(&self, id: TrackId) -> Level {
        self.tracks.get(&id).copied().unwrap_or_default()
    }

    /// Whether a meter shows a signal. The meters fall back for a few
    /// seconds after the music stops, and the window has to keep redrawing
    /// until they're down.
    pub fn moving(&self) -> bool {
        self.tracks.values().chain([&self.master]).any(|level| {
            let readings = level.peak.iter().chain(&level.rms);
            readings
                .into_iter()
                .any(|&reading| decibels_of(reading) > FLOOR)
        })
    }
}

/// The quietest level the meters and the gain fields show, in dB. Below
/// it, they read silence.
const FLOOR: f32 = -70.0;
/// The top of the faders and the meters, in dB. Live's faders go as high.
const FADER_TOP: f32 = 6.0;
/// The top of the sends, in dB.
const SEND_TOP: f32 = 0.0;
/// Where the meters turn from green to yellow, in dB. Over full scale, they
/// turn red.
const LOUD: f32 = -6.0;
/// The marks on the meters' scale, in dB.
const MARKS: [f32; 6] = [6.0, 0.0, -6.0, -12.0, -24.0, -48.0];

/// What the mixer shows without tracks.
const NO_TRACKS: &str = "No tracks yet. Track > New Audio Track adds one, and File > New Demo \
                         Project has a dozen test tones.";

/// Shows the mixer for `project`, with the meters reading `levels`, and
/// adds what the user changed to `actions`.
pub(crate) fn show(
    ui: &mut Ui,
    project: &Project,
    levels: &Levels,
    theme: &Theme,
    mouse: &MouseMap,
    actions: &mut Vec<Action>,
) {
    let sizes = &theme.mixer;
    let returns: Vec<&Track> = project
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Return)
        .collect();
    // Every strip has the same rows, so the faders line up, and the faders
    // take the height the rows leave.
    let rows = 6 + returns.len();
    let fixed = rows as f32 * (sizes.row + sizes.gap) + 2.0 * sizes.margin + sizes.scroll_bar;
    let fader = (ui.available_height() - fixed).max(sizes.min_fader);
    let mixer = Mixer {
        project,
        returns,
        fader,
        theme,
        mouse,
    };
    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
        mixer.master_strip(ui, levels.master, actions);
        ui.separator();
        if project.tracks.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(NO_TRACKS).weak());
            });
            return;
        }
        ScrollArea::both()
            .auto_shrink(false)
            .show_viewport(ui, |ui, viewport| {
                ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.x = sizes.gap;
                    // Only the strips in sight are drawn, so a hundred tracks
                    // cost no more than a screenful. The rest take their
                    // room, and each strip's widgets keep their ids.
                    let left = ui.cursor().min.x;
                    for track in &project.tracks {
                        let x = ui.cursor().min.x - left;
                        if x + sizes.strip < viewport.min.x || x > viewport.max.x {
                            ui.allocate_space(vec2(sizes.strip, 0.0));
                            continue;
                        }
                        ui.push_id(track.id, |ui| {
                            mixer.track_strip(ui, track, levels.track(track.id), actions);
                        });
                    }
                });
            });
    });
}

/// What every strip needs to know.
struct Mixer<'a> {
    project: &'a Project,
    /// The return tracks. Each strip has a row of sends with one for each.
    returns: Vec<&'a Track>,
    /// How tall the faders are.
    fader: f32,
    theme: &'a Theme,
    mouse: &'a MouseMap,
}

impl Mixer<'_> {
    fn sizes(&self) -> &MixerSizes {
        &self.theme.mixer
    }

    fn track_strip(&self, ui: &mut Ui, track: &Track, level: Level, actions: &mut Vec<Action>) {
        let id = track.id;
        let name = &track.name;
        self.strip(ui, |ui| {
            self.row(ui, |ui| self.title(ui, name))
                .on_hover_text(about(track))
                .context_menu(|ui| {
                    let mut safe = track.solo_safe;
                    let toggle = ui
                        .checkbox(&mut safe, "Solo-Safe")
                        .on_hover_text("Soloing other tracks doesn't silence this one.");
                    if toggle.changed() {
                        mix(actions, Change::SoloSafe(id, safe));
                    }
                    ui.separator();
                    if ui.button("Delete Track").clicked() {
                        mix(actions, Change::Delete(id));
                        ui.close();
                    }
                });
            self.row(ui, |ui| {
                if let Some(bus) = self.output(ui, track) {
                    mix(actions, Change::Output(id, bus));
                }
            });
            for to in &self.returns {
                self.row(ui, |ui| {
                    if track.kind != TrackKind::Return {
                        self.send(ui, track, to, actions);
                    }
                });
            }
            self.row(ui, |ui| {
                let pan = self.pan_control(ui, track.pan, &format!("{name} Pan"));
                moved(pan, actions, |pan| Change::Pan(id, pan));
            });
            let fader = self.channel(ui, track.gain, level, name);
            moved(fader, actions, |gain| Change::Gain(id, gain));
            self.row(ui, |ui| {
                self.readouts(ui, track.gain, level.highest, name, actions, |gain| {
                    Change::Gain(id, gain)
                });
            });
            self.row(ui, |ui| self.buttons(ui, track, actions));
            self.row(ui, |ui| {
                let label = format!("{name} Clip Type");
                if let Some(clip_type) = self.clip_menu(ui, ("clip", id), track.clip_type, label) {
                    mix(actions, Change::ClipType(id, clip_type));
                }
            });
        });
    }

    fn master_strip(&self, ui: &mut Ui, level: Level, actions: &mut Vec<Action>) {
        let master = &self.project.master;
        self.strip(ui, |ui| {
            self.row(ui, |ui| self.title(ui, "Master")).on_hover_text(
                "The master: every track ends up here, and it plays out of the audio device.",
            );
            // The master has no output to choose, and no sends.
            for _ in 0..1 + self.returns.len() {
                self.row(ui, |_| ());
            }
            self.row(ui, |ui| {
                let pan = self.pan_control(ui, master.pan, "Master Pan");
                moved(pan, actions, Change::MasterPan);
            });
            let fader = self.channel(ui, master.gain, level, "Master");
            moved(fader, actions, Change::MasterGain);
            self.row(ui, |ui| {
                self.readouts(
                    ui,
                    master.gain,
                    level.highest,
                    "Master",
                    actions,
                    Change::MasterGain,
                );
            });
            self.row(ui, |_| ());
            self.row(ui, |ui| {
                let label = "Master Clip Type".to_owned();
                if let Some(clip_type) = self.clip_menu(ui, "master_clip", master.clip_type, label)
                {
                    mix(actions, Change::MasterClipType(clip_type));
                }
            });
        });
    }

    /// Where the track's output goes: a menu of the master and the groups
    /// it can feed. Returns the bus the user picked.
    fn output(&self, ui: &mut Ui, track: &Track) -> Option<Bus> {
        let current = self.project.output_of(track);
        let text = self.bus_name(current);
        let sizes = self.sizes();
        if track.kind == TrackKind::Return {
            ui.add_sized(
                [sizes.inner(), sizes.row],
                Label::new(RichText::new(text).weak()).truncate(),
            )
            .on_hover_text("Return tracks feed the master.");
            return None;
        }
        let mut picked = None;
        let menu = ComboBox::from_id_salt(("output", track.id))
            .width(sizes.inner())
            .truncate()
            .selected_text(text)
            .show_ui(ui, |ui| {
                for bus in self.project.buses_for(track.id) {
                    if ui
                        .selectable_label(bus == current, self.bus_name(bus))
                        .clicked()
                        && bus != current
                    {
                        picked = Some(bus);
                    }
                }
            });
        name_menu(&menu.response, format!("{} Output", track.name), text);
        menu.response.on_hover_text("Where the track's output goes");
        picked
    }

    fn bus_name(&self, bus: Bus) -> &str {
        match bus {
            Bus::Master => "Master",
            Bus::Group(id) => self.project.track(id).map_or("?", |t| t.name.as_str()),
        }
    }
}

/// Asks the app to make `change`.
fn mix(actions: &mut Vec<Action>, change: Change) {
    actions.push(Action::Mixer(change));
}

/// Asks the app for the `change` a control makes, if it was moved, from the
/// control's response and the value it was moved to. Dragging a control is
/// one gesture, so one undo takes the whole drag back.
fn moved<T>(
    (response, value): (Response, Option<T>),
    actions: &mut Vec<Action>,
    change: impl FnOnce(T) -> Change,
) {
    if response.drag_started() {
        actions.push(Action::BeginGesture);
    }
    if let Some(value) = value {
        mix(actions, change(value));
    }
    if response.drag_stopped() {
        actions.push(Action::EndGesture);
    }
}

/// What the strip's name says when hovered.
fn about(track: &Track) -> String {
    let kind = match track.kind {
        TrackKind::Audio => "Audio track",
        TrackKind::Midi => "MIDI track",
        TrackKind::Return => "Return track",
        TrackKind::Group => "Group track",
    };
    let mut text = format!("{}\n{kind}", track.name);
    if let Some(tone) = track.tone {
        text.push_str(&format!(
            "\nTest tone: {:.2} Hz at {}",
            tone.frequency,
            decibels(tone.level)
        ));
    }
    text.push_str("\nRight-click for more.");
    text
}

impl Mixer<'_> {
    /// Lays out a strip: a column a strip wide, in a frame, where the layout
    /// puts the next widget.
    fn strip(&self, ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
        let sizes = self.sizes();
        let size = vec2(sizes.strip, ui.available_height());
        ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| {
            Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(self.theme.corner)
                .inner_margin(sizes.margin)
                .show(ui, |ui| {
                    ui.set_width(sizes.inner());
                    ui.spacing_mut().item_spacing = vec2(sizes.gap, sizes.gap);
                    add(ui);
                });
        });
    }

    /// Lays out one of a strip's rows: a row high and as wide as the strip,
    /// left to right.
    fn row<R>(&self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        let size = vec2(self.sizes().inner(), self.sizes().row);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(size);
            add(ui)
        })
        .inner
    }

    /// A strip's name, which can be right-clicked.
    fn title(&self, ui: &mut Ui, name: &str) -> Response {
        let label = Label::new(RichText::new(name).strong())
            .truncate()
            .sense(Sense::click());
        ui.add_sized([self.sizes().inner(), self.sizes().row], label)
    }

    /// The track's send to the return track `to`: its level, and whether it
    /// takes the signal before the fader.
    fn send(&self, ui: &mut Ui, track: &Track, to: &Track, actions: &mut Vec<Action>) {
        let sizes = self.sizes();
        let send = track.sends.iter().find(|send| send.to == to.id);
        let (level, pre_fader) = send.map_or((0.0, false), |send| (send.level, send.pre_fader));
        let label = format!("{} Send to {}", track.name, to.name);
        let width = sizes.inner() - sizes.gap - sizes.pre_button;
        let control = self.send_control(ui, level, &to.name, &label, width);
        if send.is_some() {
            control.0.context_menu(|ui| {
                if ui.button("Remove Send").clicked() {
                    let remove = Change::RemoveSend {
                        track: track.id,
                        to: to.id,
                    };
                    mix(actions, remove);
                    ui.close();
                }
            });
        }
        moved(control, actions, |level| Change::SendLevel {
            track: track.id,
            to: to.id,
            level,
        });
        let label = format!("Pre-Fader {} Send to {}", track.name, to.name);
        let size = vec2(sizes.pre_button, sizes.row - 2.0);
        let pre = toggle(
            ui,
            Face::Text("Pre"),
            pre_fader,
            self.theme.solo,
            &label,
            size,
            self.theme,
        )
        .on_hover_text("Pre-fader: the send takes the signal before the fader and the pan.");
        if pre.clicked() {
            let pre = Change::SendPreFader {
                track: track.id,
                to: to.id,
                pre_fader: !pre_fader,
            };
            mix(actions, pre);
        }
    }

    /// Mute, solo, and arm for audio and MIDI tracks.
    fn buttons(&self, ui: &mut Ui, track: &Track, actions: &mut Vec<Action>) {
        let id = track.id;
        let name = &track.name;
        let theme = self.theme;
        let sizes = self.sizes();
        let size = vec2((sizes.inner() - 2.0 * sizes.gap) / 3.0, sizes.row - 2.0);
        let mute = toggle(
            ui,
            Face::Text("M"),
            track.muted,
            theme.mute,
            &format!("Mute {name}"),
            size,
            theme,
        )
        .on_hover_text("Mute");
        if mute.clicked() {
            mix(actions, Change::Muted(id, !track.muted));
        }
        let hint = if track.solo_safe {
            "Solo. Ctrl-click to solo more than one track.\nSolo-safe: soloing other tracks \
             doesn't silence this one."
        } else {
            "Solo. Ctrl-click to solo more than one track."
        };
        let solo = toggle(
            ui,
            Face::Text("S"),
            track.solo,
            theme.solo,
            &format!("Solo {name}"),
            size,
            theme,
        )
        .on_hover_text(hint);
        if solo.clicked() {
            let solo = !track.solo;
            // As in Live, a click solos the track alone, and Ctrl-click
            // (Cmd-click on a Mac) solos it along with the others.
            let mods = modifiers(ui.input(|input| input.modifiers));
            let action = self.mouse.action(Target::SoloButton, Gesture::Click, mods);
            let solo = Change::Solo {
                track: id,
                solo,
                exclusive: solo && action != Some(MouseAction::AddSolo),
            };
            mix(actions, solo);
        }
        if matches!(track.kind, TrackKind::Audio | TrackKind::Midi) {
            let arm = toggle(
                ui,
                Face::Dot,
                track.armed,
                theme.arm,
                &format!("Arm {name}"),
                size,
                theme,
            )
            .on_hover_text("Arm for recording");
            if arm.clicked() {
                mix(actions, Change::Armed(id, !track.armed));
            }
        }
    }

    /// The fader beside the meter and its scale. Returns the fader, and the
    /// gain it was moved to.
    fn channel(&self, ui: &mut Ui, gain: f32, level: Level, name: &str) -> (Response, Option<f32>) {
        let sizes = self.sizes();
        let size = vec2(sizes.inner(), self.fader);
        ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Min), |ui| {
            ui.set_min_size(size);
            let gap = sizes.gap - 1.0;
            ui.spacing_mut().item_spacing.x = gap;
            let width = sizes.cap.x + sizes.meter + sizes.scale + 2.0 * gap;
            ui.add_space(((sizes.inner() - width) / 2.0).max(0.0));
            let moved = self.fader(ui, gain, &format!("{name} Volume"));
            self.meter(ui, level, &format!("{name} Level"));
            self.scale(ui);
            moved
        })
        .inner
    }

    /// The gain field and the highest peak under the fader. A gain typed or
    /// dragged into the field makes the `change`.
    fn readouts(
        &self,
        ui: &mut Ui,
        gain: f32,
        highest: f32,
        name: &str,
        actions: &mut Vec<Action>,
        change: impl FnOnce(f32) -> Change,
    ) {
        let sizes = self.sizes();
        let size = vec2((sizes.inner() - sizes.gap) / 2.0, sizes.row - 2.0);
        moved(gain_field(ui, gain, size), actions, change);
        let peak = self.peak_readout(ui, highest, &format!("{name} Highest Peak"), size);
        if peak.clicked() {
            actions.push(Action::ClearPeaks);
        }
    }

    /// A vertical fader for `gain`, from silence at the bottom to +6 dB at
    /// the top. Returns it, and the gain the user moved it to.
    fn fader(&self, ui: &mut Ui, gain: f32, label: &str) -> (Response, Option<f32>) {
        let cap = self.sizes().cap;
        let size = vec2(cap.x, self.fader);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let db = decibels_of(gain).max(FLOOR);
        let range = f64::from(FLOOR)..=f64::from(FADER_TOP);
        let mut moved =
            slider_info(ui, &response, label, f64::from(db), range).map(|db| gain_of(db as f32));
        let at = height_of(gain, FADER_TOP);
        let (bottom, top) = self.travel(rect);
        if let Some(to) = self.dragged(ui, &response, at, bottom - top, true) {
            moved = Some(gain_at(to, FADER_TOP));
        }
        if response.double_clicked() {
            moved = Some(1.0);
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let visuals = ui.style().interact(&response);
            let x = rect.center().x;
            let groove = Rect::from_x_y_ranges(x - 2.0..=x + 2.0, top..=bottom);
            painter.rect_filled(groove, 2.0, ui.visuals().extreme_bg_color);
            let unity = self.y_at(rect, height_of(1.0, FADER_TOP));
            let mark = ui.visuals().widgets.noninteractive.fg_stroke;
            painter.hline(rect.x_range(), unity, Stroke::new(1.0, mark.color));
            let cap = Rect::from_center_size(pos2(x, self.y_at(rect, at)), cap);
            painter.rect(
                cap,
                self.theme.corner,
                visuals.bg_fill,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
            painter.hline(
                cap.shrink2(vec2(5.0, 0.0)).x_range(),
                cap.center().y,
                visuals.fg_stroke,
            );
        }
        let response = response.on_hover_text(format!(
            "Volume: {}\nDrag, or Ctrl-drag to move finely. Double-click for 0 dB.",
            decibels(gain)
        ));
        (response, moved.filter(|&new| new != gain))
    }

    /// A stereo meter: the peak in light colours, the RMS in solid ones.
    fn meter(&self, ui: &mut Ui, level: Level, label: &str) {
        let width = self.sizes().meter;
        let (rect, response) = ui.allocate_exact_size(vec2(width, self.fader), Sense::hover());
        let loudest = level.peak[0].max(level.peak[1]);
        response.widget_info(|| WidgetInfo {
            value: Some(f64::from(decibels_of(loudest).max(FLOOR))),
            ..WidgetInfo::labeled(Role::Meter, true, label)
        });
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter();
        let (bottom, top) = self.travel(rect);
        let width = (width - 1.0) / 2.0;
        for c in 0..2 {
            let left = rect.left() + c as f32 * (width + 1.0);
            let x = Rangef::new(left, left + width);
            let bar = Rect::from_x_y_ranges(x, top..=bottom);
            painter.rect_filled(bar, 1.0, ui.visuals().extreme_bg_color);
            let (bottom, top) = (self.y_at(rect, 0.0), self.y_at(rect, 1.0));
            let column = Rect::from_x_y_ranges(x, top..=bottom);
            paint_level(painter, column, level.peak[c], 0.4, self.theme);
            paint_level(painter, column, level.rms[c], 1.0, self.theme);
            let peak = meter_height(level.peak[c]);
            if peak > 0.0 {
                let colour = meter_colour(peak, self.theme);
                painter.hline(x, self.y_at(rect, peak), Stroke::new(1.5, colour));
            }
        }
    }

    /// The meters' scale in dB, beside a meter.
    fn scale(&self, ui: &mut Ui) {
        let size = vec2(self.sizes().scale, self.fader);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter();
        let color = ui.visuals().weak_text_color();
        for db in MARKS {
            let y = self.y_at(rect, height_of(gain_of(db), FADER_TOP));
            let text = if db > 0.0 {
                format!("+{db}")
            } else {
                format!("{db}")
            };
            painter.text(
                pos2(rect.left(), y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(self.sizes().scale_text),
                color,
            );
        }
    }

    /// The y coordinates of the bottom and the top of a fader's travel in
    /// `rect`. A meter beside it reads on the same scale.
    fn travel(&self, rect: Rect) -> (f32, f32) {
        let cap = self.sizes().cap;
        (rect.bottom() - cap.y / 2.0, rect.top() + cap.y / 2.0)
    }

    /// The y coordinate `height` of the way up the travel in `rect`.
    fn y_at(&self, rect: Rect, height: f32) -> f32 {
        let (bottom, top) = self.travel(rect);
        bottom + (top - bottom) * height
    }

    /// A send's level, from silence at the left to 0 dB at the right, with
    /// the return track's name `to` on it. Returns the level the user moved
    /// it to.
    fn send_control(
        &self,
        ui: &mut Ui,
        level: f32,
        to: &str,
        label: &str,
        width: f32,
    ) -> (Response, Option<f32>) {
        let size = vec2(width, self.sizes().row - 4.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let db = decibels_of(level).max(FLOOR);
        let range = f64::from(FLOOR)..=f64::from(SEND_TOP);
        let mut moved =
            slider_info(ui, &response, label, f64::from(db), range).map(|db| gain_of(db as f32));
        let at = height_of(level, SEND_TOP);
        if let Some(to) = self.dragged(ui, &response, at, rect.width(), false) {
            moved = Some(gain_at(to, SEND_TOP));
        }
        if response.double_clicked() {
            moved = Some(0.0);
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter_at(rect);
            let visuals = ui.style().interact(&response);
            painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
            let mut filled = rect;
            filled.set_width(rect.width() * at);
            painter.rect_filled(filled, 2.0, ui.visuals().selection.bg_fill);
            let font = FontId::proportional(self.sizes().small_text);
            let color = visuals.text_color();
            let value = painter.text(
                rect.right_center() - vec2(3.0, 0.0),
                Align2::RIGHT_CENTER,
                db_text(decibels_of(level)),
                font.clone(),
                color,
            );
            let room = Rect::from_x_y_ranges(rect.left()..=value.left() - 2.0, rect.y_range());
            painter.with_clip_rect(room).text(
                rect.left_center() + vec2(3.0, 0.0),
                Align2::LEFT_CENTER,
                to,
                font,
                color,
            );
            painter.rect_stroke(rect, 2.0, visuals.bg_stroke, StrokeKind::Inside);
        }
        let response = response.on_hover_text(format!(
            "Send to {to}: {}\nDrag, or Ctrl-drag to move finely. Double-click to silence it.",
            decibels(level)
        ));
        (response, moved.filter(|&new| new != level))
    }

    /// The pan, from hard left to hard right. Returns it, and the pan the
    /// user moved it to.
    fn pan_control(&self, ui: &mut Ui, pan: f32, label: &str) -> (Response, Option<f32>) {
        let size = vec2(self.sizes().inner(), self.sizes().row - 4.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        // Screen readers set it from 50 left to 50 right, as it reads.
        let range = -50.0..=50.0;
        let mut moved = slider_info(ui, &response, label, f64::from(pan * 50.0), range)
            .map(|pan| pan as f32 / 50.0);
        let at = (pan + 1.0) / 2.0;
        if let Some(to) = self.dragged(ui, &response, at, rect.width(), false) {
            moved = Some(to * 2.0 - 1.0);
        }
        if response.double_clicked() {
            moved = Some(0.0);
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let visuals = ui.style().interact(&response);
            painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
            let centre = rect.center().x;
            let x = rect.left() + rect.width() * at.clamp(0.0, 1.0);
            let filled =
                Rect::from_x_y_ranges(Rangef::new(centre.min(x), centre.max(x)), rect.y_range());
            painter.rect_filled(filled, 0.0, ui.visuals().selection.bg_fill);
            painter.vline(
                centre,
                rect.y_range(),
                Stroke::new(1.0, visuals.fg_stroke.color),
            );
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                pan_text(pan),
                FontId::proportional(self.sizes().small_text),
                visuals.text_color(),
            );
            painter.rect_stroke(rect, 2.0, visuals.bg_stroke, StrokeKind::Inside);
        }
        let response = response.on_hover_text(format!(
            "Pan: {}\nDrag, or Ctrl-drag to move finely. Double-click to centre it.",
            pan_text(pan)
        ));
        (response, moved.filter(|&new| new != pan))
    }

    /// The highest peak since the peaks were cleared, red once it's over
    /// full scale. Clicking it clears them.
    fn peak_readout(&self, ui: &mut Ui, highest: f32, label: &str, size: Vec2) -> Response {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let text = db_text(decibels_of(highest));
        response.widget_info(|| WidgetInfo {
            current_text_value: Some(text.clone()),
            ..WidgetInfo::labeled(Role::Button, true, label)
        });
        if ui.is_rect_visible(rect) {
            let over = highest > 1.0;
            let visuals = ui.style().interact(&response);
            let fill = if over {
                self.theme.meter_over
            } else {
                ui.visuals().extreme_bg_color
            };
            let ink = if over {
                self.theme.ink
            } else {
                visuals.text_color()
            };
            let painter = ui.painter();
            painter.rect(rect, 2.0, fill, visuals.bg_stroke, StrokeKind::Inside);
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                &text,
                FontId::monospace(self.sizes().readout_text),
                ink,
            );
        }
        response.on_hover_text(
            "The highest peak since the peaks were cleared. Red: the channel went over full \
             scale.\nClick to clear every channel's peak.",
        )
    }

    /// The clip stage's type: a menu of them. Returns the type the user
    /// picked.
    fn clip_menu(
        &self,
        ui: &mut Ui,
        id: impl AsIdSalt,
        clip_type: ClipType,
        label: String,
    ) -> Option<ClipType> {
        let text = format!("Clip: {}", clip_name(clip_type));
        let mut picked = None;
        let menu = ComboBox::from_id_salt(id)
            .width(self.sizes().inner())
            .selected_text(text.as_str())
            .show_ui(ui, |ui| {
                for option in [ClipType::Off, ClipType::Hard, ClipType::Soft] {
                    let item = ui
                        .selectable_label(option == clip_type, clip_name(option))
                        .on_hover_text(clip_hint(option));
                    if item.clicked() && option != clip_type {
                        picked = Some(option);
                    }
                }
            });
        name_menu(&menu.response, label, &text);
        menu.response.on_hover_text(
            "The clip stage after the fader, for a channel that goes over full scale",
        );
        picked
    }

    /// Where the user dragged a control to, from `at` of the way along its
    /// travel, which is `length` points long: up for `vertical` controls,
    /// right for the others. The mouse map's fine drag, Ctrl-drag (Cmd on a
    /// Mac) in Live's, moves it a tenth as far, for small changes.
    fn dragged(
        &self,
        ui: &Ui,
        response: &Response,
        at: f32,
        length: f32,
        vertical: bool,
    ) -> Option<f32> {
        if !response.dragged() || length <= 0.0 {
            return None;
        }
        let delta = response.drag_delta();
        let moved = if vertical { -delta.y } else { delta.x };
        if moved == 0.0 {
            return None;
        }
        let mods = modifiers(ui.input(|input| input.modifiers));
        let action = self.mouse.action(Target::MixerControl, Gesture::Drag, mods);
        let scale = if action == Some(MouseAction::Fine) {
            0.1
        } else {
            1.0
        };
        Some((at + moved / length * scale).clamp(0.0, 1.0))
    }
}

/// Fills the meter bar `column` up to `gain`, in the colours of the scale's
/// zones, `alpha` opaque.
fn paint_level(painter: &Painter, column: Rect, gain: f32, alpha: f32, theme: &Theme) {
    let at = meter_height(gain);
    let zones = [
        (height_of(gain_of(LOUD), FADER_TOP), theme.meter_quiet),
        (height_of(1.0, FADER_TOP), theme.meter_loud),
        (1.0, theme.meter_over),
    ];
    let y_at = |height: f32| column.bottom() - column.height() * height;
    let mut from = 0.0;
    for (to, color) in zones {
        let end = at.min(to);
        if end > from {
            let y = Rangef::new(y_at(end), y_at(from));
            painter.rect_filled(
                Rect::from_x_y_ranges(column.x_range(), y),
                0.0,
                color.gamma_multiply(alpha),
            );
        }
        from = to;
    }
}

/// The colour of a meter at `height` of the way up: the colour of the
/// scale's zone there.
pub(crate) fn meter_colour(height: f32, theme: &Theme) -> Color32 {
    if height < height_of(gain_of(LOUD), FADER_TOP) {
        theme.meter_quiet
    } else if height <= height_of(1.0, FADER_TOP) {
        theme.meter_loud
    } else {
        theme.meter_over
    }
}

/// How far up a meter shows `gain`, from 0.0 to 1.0: nothing below the
/// floor.
pub(crate) fn meter_height(gain: f32) -> f32 {
    if decibels_of(gain) > FLOOR {
        height_of(gain, FADER_TOP)
    } else {
        0.0
    }
}

/// The gain in dB, to drag or type. Returns it, and the gain the user set.
fn gain_field(ui: &mut Ui, gain: f32, size: Vec2) -> (Response, Option<f32>) {
    let before = decibels_of(gain).max(FLOOR);
    let mut db = before;
    let field = ui
        .add_sized(
            size,
            DragValue::new(&mut db)
                .speed(0.1)
                .range(FLOOR..=FADER_TOP)
                .clamp_existing_to_range(false)
                .custom_formatter(|db, _| db_text(db as f32))
                .custom_parser(parse_decibels)
                // A typed gain applies on Enter, not at each digit.
                .update_while_editing(false),
        )
        .on_hover_text("Volume in dB: drag, or click and type");
    let typed = (field.changed() && db != before).then(|| gain_of(db));
    (field, typed)
}

fn clip_name(clip_type: ClipType) -> &'static str {
    match clip_type {
        ClipType::Off => "Off",
        ClipType::Hard => "Hard",
        ClipType::Soft => "Soft",
    }
}

fn clip_hint(clip_type: ClipType) -> &'static str {
    match clip_type {
        ClipType::Off => "No clipping: the signal passes as it is.",
        ClipType::Hard => "Cuts the signal off at full scale.",
        ClipType::Soft => {
            "Rounds off the signal above -6 dB, so it never quite reaches full scale."
        }
    }
}

/// Names the slider `response` for screen readers, and tells them its
/// value in `range`. Returns the value one asked it to take.
fn slider_info(
    ui: &Ui,
    response: &Response,
    label: &str,
    value: f64,
    range: RangeInclusive<f64>,
) -> Option<f64> {
    response.widget_info(|| WidgetInfo {
        value: Some(value),
        ..WidgetInfo::labeled(Role::Slider, true, label)
    });
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(*range.start());
        node.set_max_numeric_value(*range.end());
        node.add_action(Request::SetValue);
    });
    ui.input(|input| {
        input
            .accesskit_action_requests(response.id, Request::SetValue)
            .filter_map(|request| match request.data {
                Some(ActionData::NumericValue(value)) => Some(value),
                _ => None,
            })
            .last()
    })
    .filter(|value| value.is_finite())
    .map(|value| value.clamp(*range.start(), *range.end()))
}

/// Names the combo box `response` for screen readers, showing `value`.
fn name_menu(response: &Response, label: String, value: &str) {
    response.widget_info(|| WidgetInfo {
        current_text_value: Some(value.to_owned()),
        ..WidgetInfo::labeled(Role::ComboBox, true, &label)
    });
}

/// Where `gain` sits on a scale whose top is `top` dB: 0.0 for silence, 1.0
/// at the top. The fourth root spreads out the loud end as a console's
/// faders do: on a fader, 0 dB sits 84% of the way up, -20 dB at 47% and
/// -60 dB at 15%.
fn height_of(gain: f32, top: f32) -> f32 {
    (gain.max(0.0) / gain_of(top)).sqrt().sqrt().min(1.0)
}

/// The gain at `height` on a scale whose top is `top` dB.
fn gain_at(height: f32, top: f32) -> f32 {
    gain_of(top) * height.clamp(0.0, 1.0).powi(4)
}

/// The linear gain of `db` decibels: silence at or below the floor.
fn gain_of(db: f32) -> f32 {
    if db <= FLOOR {
        0.0
    } else {
        10_f32.powf(db / 20.0)
    }
}

/// A linear gain in decibels: negative infinity for silence.
fn decibels_of(gain: f32) -> f32 {
    20.0 * gain.log10()
}

/// A linear gain in decibels: "-6.0 dB".
fn decibels(gain: f32) -> String {
    if gain > 0.0 {
        let db = 20.0 * gain.log10();
        // No "-0.0 dB" for gains a hair under unity.
        format!("{:.1} dB", if db.abs() < 0.05 { 0.0 } else { db })
    } else {
        "-inf dB".to_owned()
    }
}

/// Decibels for the narrow fields of a strip: "-6.0", "+1.5", "0.0", or
/// "-inf" at the floor and below.
fn db_text(db: f32) -> String {
    if db.is_nan() || db <= FLOOR {
        return "-inf".to_owned();
    }
    let rounded = (db * 10.0).round() / 10.0;
    if rounded == 0.0 {
        "0.0".to_owned()
    } else {
        format!("{rounded:+.1}")
    }
}

/// Decibels as typed into a gain field: "-6", "+1.5 dB" or "-inf".
fn parse_decibels(text: &str) -> Option<f64> {
    let text = text.trim().to_ascii_lowercase();
    let text = text.strip_suffix("db").unwrap_or(&text).trim();
    match text {
        "-inf" | "inf" => Some(f64::from(FLOOR)),
        _ => text.parse().ok().filter(|db: &f64| db.is_finite()),
    }
}

/// A pan as Live shows it: "C", or from "50L" to "50R".
fn pan_text(pan: f32) -> String {
    let amount = (pan.abs() * 50.0).round();
    if amount == 0.0 {
        "C".to_owned()
    } else if pan < 0.0 {
        format!("{amount}L")
    } else {
        format!("{amount}R")
    }
}

#[cfg(test)]
mod tests {
    use egui::Modifiers;
    use egui::accesskit::Toggled;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    use musicable_commands::{Document, mixer};

    use super::*;

    /// The mixer, on a document its changes go to, as the app would make
    /// them, and the actions it handed back.
    struct State {
        document: Document,
        levels: Levels,
        actions: Vec<Action>,
    }

    fn harness(project: Project) -> Harness<'static, State> {
        harness_with(project, Levels::default())
    }

    fn harness_with(project: Project, levels: Levels) -> Harness<'static, State> {
        let state = State {
            document: Document::new(project),
            levels,
            actions: Vec::new(),
        };
        Harness::builder().with_size([900.0, 600.0]).build_ui_state(
            |ui, state: &mut State| {
                let mut actions = Vec::new();
                let (theme, mouse) = (Theme::default(), MouseMap::live());
                let project = state.document.project();
                show(ui, project, &state.levels, &theme, &mouse, &mut actions);
                for action in &actions {
                    match action {
                        Action::Mixer(change) => mixer::change(&mut state.document, *change),
                        Action::BeginGesture => state.document.begin_gesture(),
                        Action::EndGesture => state.document.end_gesture(),
                        _ => {}
                    }
                }
                state.actions.extend(actions);
            },
            state,
        )
    }

    /// Lead, an audio track at -6 dB with a send to the reverb; Keys, a MIDI
    /// track feeding the drums group; and a reverb return.
    fn project() -> (Project, [TrackId; 4]) {
        let mut project = Project::new();
        let lead = project.add_track("Lead", TrackKind::Audio);
        let keys = project.add_track("Keys", TrackKind::Midi);
        let drums = project.add_track("Drums", TrackKind::Group);
        let reverb = project.add_track("Reverb", TrackKind::Return);
        let send = project.new_send(reverb);
        let track = project.track_mut(lead).unwrap();
        track.gain = 0.5;
        track.sends.push(send);
        project.track_mut(keys).unwrap().output = Bus::Group(drums);
        (project, [lead, keys, drums, reverb])
    }

    fn mixed(harness: &Harness<'_, State>) -> Vec<Change> {
        let actions = harness.state().actions.iter();
        actions
            .filter_map(|action| match action {
                Action::Mixer(change) => Some(*change),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_track_has_a_strip() {
        let (project, _) = project();
        let harness = harness(project);
        for name in ["Lead", "Keys", "Drums", "Reverb", "Master"] {
            let label = format!("{name} Volume");
            let volume = harness.get_by_role_and_label(Role::Slider, &label);
            let db = volume.accesskit_node().numeric_value().unwrap();
            let expected = if name == "Lead" { -6.02 } else { 0.0 };
            assert!((db - expected).abs() < 0.01, "{name}: {db}");
            let label = format!("{name} Level");
            let meter = harness.get_by_role_and_label(Role::Meter, &label);
            assert_eq!(
                meter.accesskit_node().numeric_value(),
                Some(f64::from(FLOOR))
            );
            let label = format!("{name} Pan");
            let pan = harness.get_by_role_and_label(Role::Slider, &label);
            assert_eq!(pan.accesskit_node().numeric_value(), Some(0.0));
            let label = format!("{name} Clip Type");
            let clip = harness.get_by_role_and_label(Role::ComboBox, &label);
            assert_eq!(clip.value().as_deref(), Some("Clip: Off"));
        }
        // Every track but the return can send to it.
        for name in ["Lead", "Keys", "Drums"] {
            harness.get_by_role_and_label(Role::Slider, &format!("{name} Send to Reverb"));
        }
        assert!(harness.query_by_label("Reverb Send to Reverb").is_none());
        // Audio and MIDI tracks can be armed.
        for (name, armable) in [("Lead", true), ("Keys", true), ("Drums", false)] {
            let label = format!("Arm {name}");
            assert_eq!(harness.query_by_label(&label).is_some(), armable, "{name}");
        }
        assert!(harness.query_by_label("Arm Reverb").is_none());
        // Return tracks feed the master, and the others choose.
        let output = |name: &str| {
            let label = format!("{name} Output");
            harness
                .get_by_role_and_label(Role::ComboBox, &label)
                .value()
        };
        assert_eq!(output("Lead").as_deref(), Some("Master"));
        assert_eq!(output("Keys").as_deref(), Some("Drums"));
        assert!(harness.query_by_label("Reverb Output").is_none());
        assert!(harness.query_by_label_contains("No tracks yet").is_none());
    }

    #[test]
    fn without_tracks_it_says_how_to_add_one() {
        let harness = harness(Project::new());
        harness.get_by_label_contains("No tracks yet");
        harness.get_by_role_and_label(Role::Slider, "Master Volume");
    }

    #[test]
    fn the_controls_change_the_mix() {
        let (project, [lead, keys, drums, reverb]) = project();
        let mut harness = harness(project);
        let slider = |harness: &Harness<'_, State>, label: &str, value: f64| {
            let node = harness.get_by_role_and_label(Role::Slider, label);
            node.set_value_accesskit(value);
        };
        slider(&harness, "Lead Volume", -12.0);
        harness.run();
        slider(&harness, "Keys Pan", -25.0);
        harness.run();
        slider(&harness, "Keys Send to Reverb", 0.0);
        harness.run();
        slider(&harness, "Master Volume", 6.0);
        harness.run();
        for label in ["Mute Lead", "Arm Keys", "Pre-Fader Lead Send to Reverb"] {
            harness.get_by_label(label).click();
            harness.run();
        }
        harness
            .get_by_role_and_label(Role::ComboBox, "Lead Output")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Drums").click();
        harness.run();
        harness
            .get_by_role_and_label(Role::ComboBox, "Reverb Clip Type")
            .click();
        harness.run();
        harness.get_by_role_and_label(Role::Button, "Soft").click();
        harness.run();

        let changes = mixed(&harness);
        let [
            Change::Gain(id, gain),
            Change::Pan(pan_id, pan),
            level,
            Change::MasterGain(master),
        ] = changes[..4]
        else {
            panic!("{changes:?}");
        };
        assert_eq!(id, lead);
        assert!((gain - 0.251).abs() < 0.001, "{gain}");
        assert_eq!((pan_id, pan), (keys, -0.5));
        let full = Change::SendLevel {
            track: keys,
            to: reverb,
            level: 1.0,
        };
        assert_eq!(level, full);
        assert!((master - 1.995).abs() < 0.001, "{master}");
        let pre = Change::SendPreFader {
            track: lead,
            to: reverb,
            pre_fader: true,
        };
        let rest = [
            Change::Muted(lead, true),
            Change::Armed(keys, true),
            pre,
            Change::Output(lead, Bus::Group(drums)),
            Change::ClipType(reverb, ClipType::Soft),
        ];
        assert_eq!(changes[4..], rest);
        // The mixer shows what changed.
        let mute = harness.get_by_label("Mute Lead").accesskit_node().toggled();
        assert_eq!(mute, Some(Toggled::True));
        let keys = harness.get_by_role_and_label(Role::Slider, "Keys Pan");
        assert_eq!(keys.accesskit_node().numeric_value(), Some(-25.0));
    }

    #[test]
    fn dragging_a_fader_is_one_gesture() {
        let (project, [lead, ..]) = project();
        let mut harness = harness(project);
        let fader = harness.get_by_role_and_label(Role::Slider, "Lead Volume");
        let at = fader.rect().center();
        harness.hover_at(at);
        harness.run();
        harness.drag_at(at);
        harness.run();
        for y in 1..=3 {
            harness.hover_at(at - vec2(0.0, 10.0 * y as f32));
            harness.run();
        }
        harness.drop_at(at - vec2(0.0, 30.0));
        harness.run();

        let actions = &harness.state().actions;
        assert_eq!(actions.first(), Some(&Action::BeginGesture));
        assert_eq!(actions.last(), Some(&Action::EndGesture));
        let gains: Vec<f32> = actions[1..actions.len() - 1]
            .iter()
            .map(|action| match action {
                Action::Mixer(Change::Gain(id, gain)) if *id == lead => *gain,
                other => panic!("{other:?} in the middle of a drag"),
            })
            .collect();
        assert!(gains.len() > 1 && gains.is_sorted(), "{gains:?}");
        assert!(gains[0] > 0.5);
        // One undo takes the whole drag back.
        let document = &mut harness.state_mut().document;
        assert_eq!(document.undo_name(), Some("Volume"));
        assert!(document.undo());
        assert_eq!(document.project().track(lead).unwrap().gain, 0.5);
        assert_eq!(document.undo_name(), None);
    }

    #[test]
    fn a_click_solos_one_track_and_ctrl_click_another() {
        let (project, [lead, keys, ..]) = project();
        let mut harness = harness(project);
        harness.get_by_label("Solo Lead").click();
        harness.run();
        harness
            .get_by_label("Solo Keys")
            .click_modifiers(Modifiers::COMMAND);
        harness.run();
        let solo = |track, exclusive| Change::Solo {
            track,
            solo: true,
            exclusive,
        };
        assert_eq!(mixed(&harness), [solo(lead, true), solo(keys, false)]);
        let soloed = |harness: &Harness<'_, State>, track| {
            let project = harness.state().document.project();
            project.track(track).unwrap().solo
        };
        assert!(soloed(&harness, lead) && soloed(&harness, keys));
        // Clicking a soloed track unsolos just that one.
        harness.get_by_label("Solo Lead").click();
        harness.run();
        assert!(!soloed(&harness, lead) && soloed(&harness, keys));
    }

    #[test]
    fn the_strips_menus_delete_tracks_and_sends() {
        let (project, [lead, keys, ..]) = project();
        let mut harness = harness(project);
        harness.get_by_label("Keys").click_secondary();
        harness.run();
        harness.get_by_label("Solo-Safe").click();
        harness.run();
        harness
            .get_by_label("Lead Send to Reverb")
            .click_secondary();
        harness.run();
        harness.get_by_label("Remove Send").click();
        harness.run();
        harness.get_by_label("Lead").click_secondary();
        harness.run();
        harness.get_by_label("Delete Track").click();
        harness.run();
        let reverb = harness.state().document.project().tracks[2].id;
        let expected = [
            Change::SoloSafe(keys, true),
            Change::RemoveSend {
                track: lead,
                to: reverb,
            },
            Change::Delete(lead),
        ];
        assert_eq!(mixed(&harness), expected);
        assert!(harness.query_by_label("Lead").is_none());
    }

    #[test]
    fn the_meters_read_the_levels() {
        let (project, [lead, ..]) = project();
        let mut levels = Levels::default();
        let loud = Level {
            peak: [0.5, 0.25],
            rms: [0.25, 0.125],
            highest: 1.5,
        };
        levels.tracks.insert(lead, loud);
        levels.master.highest = 0.5;
        let mut harness = harness_with(project, levels);
        let meter = harness.get_by_role_and_label(Role::Meter, "Lead Level");
        let db = meter.accesskit_node().numeric_value().unwrap();
        assert!((db + 6.02).abs() < 0.01, "{db}");
        let peak = |harness: &Harness<'_, State>, name: &str| {
            let label = format!("{name} Highest Peak");
            harness.get_by_role_and_label(Role::Button, &label).value()
        };
        assert_eq!(peak(&harness, "Lead").as_deref(), Some("+3.5"));
        assert_eq!(peak(&harness, "Keys").as_deref(), Some("-inf"));
        assert_eq!(peak(&harness, "Master").as_deref(), Some("-6.0"));
        harness.get_by_label("Master Highest Peak").click();
        harness.run();
        assert_eq!(harness.state().actions, [Action::ClearPeaks]);
    }

    #[test]
    fn meters_move_until_every_reading_is_down() {
        let mut levels = Levels::default();
        assert!(!levels.moving());
        levels.master.rms[1] = 0.001;
        assert!(levels.moving());
        levels.master.rms[1] = gain_of(FLOOR - 1.0);
        levels.tracks.insert(TrackId(3), Level::default());
        assert!(!levels.moving());
        // The highest peak holds, so it doesn't count.
        levels.master.highest = 1.0;
        assert!(!levels.moving());
        levels.tracks.get_mut(&TrackId(3)).unwrap().peak[0] = 0.5;
        assert!(levels.moving());
    }

    #[test]
    fn faders_spread_out_the_loud_end() {
        let at = |db: f32| height_of(gain_of(db), FADER_TOP);
        assert!((at(0.0) - 0.841).abs() < 0.001, "{}", at(0.0));
        assert!((at(-20.0) - 0.473).abs() < 0.001, "{}", at(-20.0));
        assert!((at(-60.0) - 0.150).abs() < 0.001, "{}", at(-60.0));
        assert_eq!(at(FADER_TOP), 1.0);
        assert_eq!(at(12.0), 1.0);
        assert_eq!(height_of(0.0, FADER_TOP), 0.0);
        for height in [0.0, 0.1, 0.5, 0.84, 1.0] {
            let back = height_of(gain_at(height, SEND_TOP), SEND_TOP);
            assert!((back - height).abs() < 1e-6, "{height}: {back}");
        }
        assert_eq!(gain_at(1.0, SEND_TOP), 1.0);
        assert_eq!(gain_of(FLOOR), 0.0);
    }

    #[test]
    fn decibels_read_and_parse() {
        assert_eq!(decibels(1.0), "0.0 dB");
        assert_eq!(decibels(0.5), "-6.0 dB");
        assert_eq!(decibels(0.08), "-21.9 dB");
        assert_eq!(decibels(2.0), "6.0 dB");
        assert_eq!(decibels(0.0), "-inf dB");
        assert_eq!(db_text(-6.02), "-6.0");
        assert_eq!(db_text(1.54), "+1.5");
        assert_eq!(db_text(-0.04), "0.0");
        assert_eq!(db_text(FLOOR), "-inf");
        assert_eq!(db_text(f32::NEG_INFINITY), "-inf");
        assert_eq!(db_text(f32::NAN), "-inf");
        assert_eq!(parse_decibels("-6"), Some(-6.0));
        assert_eq!(parse_decibels(" +1.5 dB "), Some(1.5));
        assert_eq!(parse_decibels("-3db"), Some(-3.0));
        assert_eq!(parse_decibels("-INF"), Some(f64::from(FLOOR)));
        assert_eq!(parse_decibels("loud"), None);
        assert_eq!(parse_decibels("NaN"), None);
    }

    #[test]
    fn pans_read_as_live_shows_them() {
        assert_eq!(pan_text(0.0), "C");
        assert_eq!(pan_text(0.005), "C");
        assert_eq!(pan_text(-0.5), "25L");
        assert_eq!(pan_text(1.0), "50R");
    }
}
