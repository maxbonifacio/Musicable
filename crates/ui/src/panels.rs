//! The panels the window docks: the arrangement, the mixer, the devices,
//! the piano roll, the Sample Editor, the browser and the history. Each
//! kind of panel
//! registers its id, its title, where it goes in the default layout and how
//! it's drawn, and the dock lays them out by id: the shell doesn't name
//! them.

use std::fmt;

use egui::{Rect, Ui, UiBuilder};
use musicable_model::Chain;

use crate::arrangement::Arrangement;
use crate::browser::{self, BrowserPanel, Destination};
use crate::devices::DevicesPanel;
use crate::history::HistoryPanel;
use crate::mixer::{self, Choices};
use crate::mouse::MouseMap;
use crate::parts::Parts;
use crate::piano_roll::PianoRoll;
use crate::sample_editor::SampleEditor;
use crate::shell::{Action, View};
use crate::theme::Theme;

// The panels' ids, which the focus and the keymaps' panel tables know them
// by too.
pub use musicable_commands::panel::{
    ARRANGEMENT, BROWSER, DEVICES, HISTORY, MIXER, PIANO_ROLL, SAMPLE_EDITOR,
};

/// Where a panel goes in the default layout, and in a layout that doesn't
/// have it yet, such as one saved before it existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// The middle of the window, which grows and shrinks with it.
    Middle,
    /// Left of everything else, as tall as the window.
    Left,
    /// Under the middle, below the panels already there.
    Below,
    /// In a tab beside the panel with this id.
    With(&'static str),
}

/// A kind of panel.
pub(crate) struct Panel {
    pub id: &'static str,
    /// What its tab and the View menu call it.
    pub title: &'static str,
    pub place: Place,
    /// Whether the default layout shows it.
    pub shown: bool,
    /// Its width, beside other panels, or its height, over or under them,
    /// until the user drags its edge: across for [`Place::Left`], down for
    /// the others. The middle panel takes what the others leave.
    pub size: fn(&Theme) -> f32,
    /// Whether the user can hide it.
    pub hides: bool,
    pub show: fn(&mut Panels, &mut Ui, &mut Cx<'_, '_>),
}

impl Panel {
    /// Whether [`size`](Self::size) is a width.
    pub fn across(&self, registry: &Registry) -> bool {
        match self.place {
            Place::Left => true,
            Place::Middle | Place::Below => false,
            Place::With(other) => (registry.get(other))
                .is_some_and(|other| other.id != self.id && other.place == Place::Left),
        }
    }
}

/// Every kind of panel, in the order the default layout places them.
pub(crate) struct Registry {
    panels: Vec<Panel>,
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.iter().map(|panel| panel.id))
            .finish()
    }
}

impl Registry {
    /// Musicable's own panels.
    pub fn new() -> Self {
        let mut registry = Self { panels: Vec::new() };
        registry.register(Panel {
            id: BROWSER,
            title: "Browser",
            place: Place::Left,
            // It waits to be asked for.
            shown: false,
            size: |theme| theme.devices.browser,
            hides: true,
            show: |panels, ui, cx| {
                framed(ui, |ui| {
                    let theme = cx.theme;
                    (panels.browser).show(ui, cx.view, cx.chain, theme, cx.actions);
                });
            },
        });
        registry.register(Panel {
            id: ARRANGEMENT,
            title: "Arrangement",
            place: Place::Middle,
            shown: true,
            size: |_| 0.0,
            hides: false,
            show: |panels, ui, cx| {
                let (view, theme, mouse) = (cx.view, cx.theme, cx.mouse);
                (panels.arrangement).show(ui, view, theme, mouse, cx.parts, cx.actions);
            },
        });
        registry.register(Panel {
            id: MIXER,
            title: "Mixer",
            place: Place::Below,
            shown: true,
            size: |theme| theme.mixer.panel,
            hides: true,
            show: |_, ui, cx| {
                let view = cx.view;
                let inputs = Choices {
                    audio: view.inputs,
                    midi: view.midi_inputs,
                };
                framed(ui, |ui| {
                    let (project, levels, at) = (view.project, view.levels, view.playback.playhead);
                    let (theme, mouse, parts) = (cx.theme, cx.mouse, cx.parts);
                    let editor = view.editor;
                    mixer::show(
                        ui, project, editor, levels, inputs, at, theme, mouse, parts, cx.actions,
                    );
                });
            },
        });
        registry.register(Panel {
            id: DEVICES,
            title: "Devices",
            place: Place::Below,
            shown: true,
            size: |theme| theme.devices.panel,
            hides: true,
            show: |panels, ui, cx| {
                let rect = ui.max_rect();
                let browse = framed(ui, |ui| {
                    let (view, theme, mouse) = (cx.view, cx.theme, cx.mouse);
                    (panels.devices).show(ui, view, cx.chain, theme, mouse, cx.actions)
                });
                cx.browse |= browse;
                // Devices and presets dragged from the browser go in the
                // chain the panel shows.
                let target = Destination::Chain(cx.chain);
                let dropped = browser::drop_in(ui.ctx(), rect, &target, cx.theme);
                cx.actions.extend(dropped);
            },
        });
        registry.register(Panel {
            id: PIANO_ROLL,
            title: "Piano Roll",
            // As Live's clip view takes its device view's place.
            place: Place::With(DEVICES),
            // It waits for a MIDI clip to be opened in it.
            shown: false,
            size: |theme| theme.piano_roll.panel,
            hides: true,
            show: |panels, ui, cx| {
                framed(ui, |ui| {
                    let (view, theme, mouse) = (cx.view, cx.theme, cx.mouse);
                    panels.piano_roll.show(ui, view, theme, mouse, cx.actions);
                });
            },
        });
        registry.register(Panel {
            id: SAMPLE_EDITOR,
            title: "Sample Editor",
            // As Live's clip view takes its device view's place.
            place: Place::With(DEVICES),
            // It waits for an audio clip to be opened in it.
            shown: false,
            size: |theme| theme.sample_editor.panel,
            hides: true,
            show: |panels, ui, cx| {
                framed(ui, |ui| {
                    let (view, theme, mouse) = (cx.view, cx.theme, cx.mouse);
                    panels.sample_editor.show(ui, view, theme, mouse, cx.actions);
                });
            },
        });
        registry.register(Panel {
            id: HISTORY,
            title: "History",
            // A list, as the browser is.
            place: Place::With(BROWSER),
            // It waits to be asked for.
            shown: false,
            size: |theme| theme.devices.browser,
            hides: true,
            show: |panels, ui, cx| {
                framed(ui, |ui| {
                    (panels.history).show(ui, cx.view, cx.theme, cx.actions);
                });
            },
        });
        registry
    }

    /// Adds a kind of panel, after those there. A panel with the same id
    /// takes its place.
    pub fn register(&mut self, panel: Panel) {
        match self.panels.iter_mut().find(|p| p.id == panel.id) {
            Some(old) => *old = panel,
            None => self.panels.push(panel),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Panel> {
        self.panels.iter().find(|panel| panel.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Panel> {
        self.panels.iter()
    }

    /// The panel in the middle of the window.
    pub fn middle(&self) -> Option<&Panel> {
        self.iter().find(|panel| panel.place == Place::Middle)
    }
}

/// The panels' own state, kept between frames.
#[derive(Debug, Default)]
pub(crate) struct Panels {
    pub arrangement: Arrangement,
    pub devices: DevicesPanel,
    pub piano_roll: PianoRoll,
    pub sample_editor: SampleEditor,
    pub browser: BrowserPanel,
    pub history: HistoryPanel,
}

/// What the panels draw from, and what they hand back.
pub(crate) struct Cx<'a, 'v> {
    pub view: &'a View<'v>,
    pub theme: &'a Theme,
    pub mouse: &'a MouseMap,
    /// The parts of the transport bar, the track headers and the mixer
    /// strips, as the user has them.
    pub parts: &'a Parts,
    /// The chain the devices panel shows, which the browser adds to.
    pub chain: Option<Chain>,
    pub actions: &'a mut Vec<Action>,
    /// Set when the devices panel's Add Device was clicked, to show the
    /// browser's devices.
    pub browse: bool,
}

/// Draws `add_contents` on the panels' background, inside the margins of
/// egui's side and top panels, as the panels were before they docked.
fn framed<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, ui.visuals().panel_fill);
    let margin = egui::Frame::side_top_panel(ui.style()).inner_margin;
    let inner = Rect::from_min_max(
        rect.min + margin.left_top(),
        rect.max - margin.right_bottom(),
    );
    ui.scope_builder(UiBuilder::new().max_rect(inner), add_contents)
        .inner
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use musicable_commands::panel;

    use super::*;

    #[test]
    fn the_registry_has_the_panels_the_commands_know() {
        let ids: BTreeSet<_> = Registry::new().iter().map(|panel| panel.id).collect();
        assert_eq!(ids, BTreeSet::from(panel::ALL));
    }
}
