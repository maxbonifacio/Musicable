//! The Settings window: a page for each area of Musicable's settings,
//! listed down its left side, as Live's Preferences has a tab for each.
//! Changes apply at once, and the app keeps them: in the settings file, in
//! the profile in use's keymap, mouse map, workspaces and settings, or in
//! the theme's file. The window isn't a panel, so no workspace has it. Each
//! page is registered with its id, its title and what draws it, so a new
//! page goes in one place.

use std::path::Path;

use eframe::egui::{
    self, Align, Align2, Checkbox, DragValue, Key, Label, Layout, RichText, Role, ScrollArea,
    Slider, Ui, WidgetInfo, vec2,
};
use musicable_audio::StreamRequest;
use musicable_commands::editor::{Behaviour, FollowMode, FollowPlace, MiddleC, PlayFrom};
use musicable_commands::{options, workspace};
use musicable_ui::{Action, MAX_PREVIEW_VOLUME, MIN_PREVIEW_VOLUME, Shell, View, first_press};

use crate::Output;
use crate::appearance_page::{self, AppearancePage};
use crate::audio_page::AudioPage;
use crate::mouse_file::UserMouse;
use crate::mouse_page;
use crate::profiles::Profiles;
use crate::profiles_page::{self, ProfilesPage};
use crate::settings::{ProfileSettings, RenderAhead, RenderCacheSettings, Settings};
use crate::theme_file::UserTheme;

/// The device to play through, and how.
pub const AUDIO: &str = "audio";
/// How long Capture holds the inputs, and how late recordings land.
pub const RECORDING: &str = "recording";
/// Every command's keys.
pub const KEYS: &str = "keys";
/// What the mouse does where.
pub const MOUSE: &str = "mouse";
/// How editing behaves where people's habits differ.
pub const BEHAVIOUR: &str = "behaviour";
/// The theme.
pub const APPEARANCE: &str = "appearance";
/// The user's workspaces.
pub const WORKSPACES: &str = "workspaces";
/// How the browser previews samples, and where presets go.
pub const BROWSER: &str = "browser";
/// How much the render cache keeps.
pub const FILES: &str = "files";
/// The user's profiles.
pub const PROFILES: &str = "profiles";
/// Whether agents, such as Claude Code, can control Musicable.
pub const AGENTS: &str = "agents";

/// A page of the window.
struct Page {
    /// What commands name it by.
    id: &'static str,
    /// What the list down the window's side calls it.
    title: &'static str,
    /// Whether it scrolls its own contents, as the keys' long list does.
    /// The others scroll in the window.
    scrolls: bool,
    /// Draws it, with what the pages keep. The flag says it came into
    /// sight in this frame, so what it keeps starts afresh. Returns what
    /// the user changed or asked for, for the app to carry out.
    show: fn(&mut Pages, &mut Ui, &mut Cx<'_>, bool) -> Option<Asked>,
}

/// The pages, in the order the window lists them. A new page goes here.
const PAGES: [Page; 11] = [
    Page {
        id: AUDIO,
        title: "Audio",
        scrolls: false,
        show: |pages, ui, cx, new| {
            if new {
                pages.audio.reset(cx.output);
            }
            let render_ahead = cx.settings.audio.render_ahead;
            pages.audio.show(ui, cx.output, render_ahead)
        },
    },
    Page {
        id: RECORDING,
        title: "Recording",
        scrolls: false,
        show: |pages, ui, cx, _| pages.recording.show(ui, cx),
    },
    Page {
        id: KEYS,
        title: "Keys",
        scrolls: true,
        show: |_, ui, cx, _| {
            let actions = cx.shell.show_keys(ui, cx.view);
            cx.actions.extend(actions);
            None
        },
    },
    Page {
        id: MOUSE,
        title: "Mouse",
        scrolls: true,
        show: |_, ui, cx, _| mouse_page::show(ui, cx.shell.mouse_map(), cx.mouse).map(Asked::Mouse),
    },
    Page {
        id: BEHAVIOUR,
        title: "Behaviour",
        scrolls: false,
        show: |_, ui, cx, _| behaviour(ui, cx),
    },
    Page {
        id: APPEARANCE,
        title: "Appearance",
        scrolls: false,
        show: |pages, ui, cx, new| {
            if new {
                pages.appearance.reset();
            }
            pages.appearance.show(ui, cx.theme).map(Asked::Appearance)
        },
    },
    Page {
        id: WORKSPACES,
        title: "Workspaces",
        scrolls: false,
        show: |_, ui, cx, _| workspaces(ui, cx),
    },
    Page {
        id: BROWSER,
        title: "Browser",
        scrolls: false,
        show: |_, ui, cx, _| browser(ui, cx),
    },
    Page {
        id: FILES,
        title: "Files",
        scrolls: false,
        show: |pages, ui, cx, _| pages.files.show(ui, cx),
    },
    Page {
        id: PROFILES,
        title: "Profiles",
        scrolls: false,
        show: |pages, ui, cx, new| {
            let keys: Vec<String> = (cx.view.keymap.chords_for(&options::NEXT_PROFILE))
                .map(|chord| chord.to_string())
                .collect();
            let page = &mut pages.profiles;
            page.show(ui, cx.profiles, &keys.join(", "), new)
                .map(Asked::Profiles)
        },
    },
    Page {
        id: AGENTS,
        title: "Agents",
        scrolls: false,
        show: |_, ui, cx, _| agents(ui, &cx.agents),
    },
];

/// How big the window is at first, in points.
const SIZE: [f32; 2] = [840.0, 560.0];
/// How wide the list of pages is.
const LIST_WIDTH: f32 = 110.0;
/// The furthest a recording offset goes either way, in frames.
const MAX_OFFSET: i64 = 100_000;
/// The longest Capture holds the inputs for, in minutes.
const MAX_CAPTURE_MINUTES: u32 = 120;

/// What the user changed or asked for on a page, for the app to carry out.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked {
    /// Play through the device the Audio page has, as it has it.
    Stream(StreamRequest),
    /// The engine renders this far ahead of the device from now on.
    RenderAhead(RenderAhead),
    /// Recordings through the device playing land this many frames late.
    RecordingOffset(i64),
    /// Capture holds this many minutes of the inputs from now on.
    CaptureMinutes(u32),
    /// Editing behaves so from now on.
    Behaviour(Behaviour),
    /// Analysis leaves the Beat This! network out from now on, or doesn't.
    FastAnalysis(bool),
    /// What the Mouse page asked for.
    Mouse(mouse_page::Asked),
    /// What the Profiles page asked for.
    Profiles(profiles_page::Asked),
    /// What the Appearance page asked for.
    Appearance(appearance_page::Asked),
    /// Ask for a folder to save presets in.
    ChoosePresetsFolder,
    /// Save presets in the folder Musicable picks again.
    DefaultPresetsFolder,
    /// Show the workspaces folder in the system's file manager.
    OpenWorkspacesFolder,
    /// The render cache holds this much from now on.
    RenderCache(RenderCacheSettings),
    /// Show the render cache's folder in the system's file manager.
    OpenRenderCacheFolder,
    /// Let agents control Musicable from now on, or stop letting them.
    Agents(bool),
    /// Let agents use the mouse and keys from now on, or stop letting them.
    AgentsMouseAndKeys(bool),
}

/// What the pages show, from the app.
pub struct Cx<'a> {
    /// What the shell shows in this frame, which the Keys page shows too.
    pub view: &'a View<'a>,
    /// The shell, which draws the Keys page and has the mouse map.
    pub shell: &'a mut Shell,
    /// Where the pages put what they ask the shell's way, as the Keys page
    /// and the workspace buttons do, with the rest of the shell's actions.
    pub actions: &'a mut Vec<Action>,
    pub output: &'a Output,
    /// The device playing's recording offset, in frames.
    pub offset: i64,
    /// The user's mouse map file.
    pub mouse: &'a UserMouse,
    pub theme: &'a UserTheme,
    /// The settings file's settings: the computer's, and Default's.
    pub settings: &'a Settings,
    /// The user's profiles, and the one in use.
    pub profiles: &'a Profiles,
    /// The settings of the profile in use.
    pub profile: &'a ProfileSettings,
    /// Where presets go, if Musicable has a folder for them.
    pub presets_folder: Option<&'a Path>,
    /// Where the workspaces' files go, if there's a config folder.
    pub workspaces_folder: Option<&'a Path>,
    /// Where the render cache keeps what it renders, if there's a cache
    /// folder.
    pub render_folder: Option<&'a Path>,
    pub agents: Agents<'a>,
}

/// Agents, as the Agents page shows them.
pub struct Agents<'a> {
    /// Whether the user lets agents control Musicable.
    pub enabled: bool,
    /// Whether the user lets agents use the mouse and keys.
    pub mouse_and_keys: bool,
    /// Whether an agent is using them now.
    pub using_hands: bool,
    /// Where Musicable listens for them, while it does.
    pub listening: Option<String>,
    /// The agents connected, as their clients name themselves.
    pub connected: Vec<String>,
    /// Why Musicable can't let agents in, if it can't.
    pub problem: Option<&'a str>,
    /// The command that adds Musicable to Claude Code.
    pub claude_command: &'a str,
}

/// What the pages keep from frame to frame.
#[derive(Default)]
struct Pages {
    audio: AudioPage,
    recording: RecordingPage,
    files: FilesPage,
    appearance: AppearancePage,
    profiles: ProfilesPage,
}

/// The Settings window.
pub struct SettingsWindow {
    open: bool,
    /// The page in sight, or that was when the window closed.
    page: &'static str,
    /// The page drawn in the last frame, to tell when one comes into sight.
    drawn: Option<&'static str>,
    pages: Pages,
}

impl Default for SettingsWindow {
    fn default() -> Self {
        Self {
            open: false,
            page: AUDIO,
            drawn: None,
            pages: Pages::default(),
        }
    }
}

impl SettingsWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows the window at the page with id `page`, or at the page it
    /// showed last.
    pub fn open(&mut self, page: Option<&'static str>) {
        self.open = true;
        if let Some(page) = page {
            self.page = page;
        }
    }

    /// Whether the window is open.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The page in sight, or that was when the window closed.
    #[cfg(test)]
    pub(crate) fn page(&self) -> &'static str {
        self.page
    }

    /// Says how applying the Audio page's settings went.
    pub fn applied(&mut self, outcome: Result<String, String>) {
        self.pages.audio.applied(outcome);
    }

    /// Shows the window, if it's open. Returns what the user changed or
    /// asked for that the app carries out.
    pub fn show(&mut self, ctx: &egui::Context, mut cx: Cx<'_>) -> Option<Asked> {
        // The Profiles page's box over the window, which Enter and Escape
        // go to first.
        let boxed = match self.open {
            true => (self.pages.profiles.show_box(ctx, cx.profiles)).map(Asked::Profiles),
            false => {
                self.pages.profiles.reset();
                None
            }
        };
        // Escape closes the window, unless the Keys page is giving a command
        // keys, or a menu is open: then it stops that.
        if self.open && !cx.shell.giving_keys() && !egui::Popup::is_any_open(ctx) {
            self.open = !ctx.input_mut(|i| first_press(i, Key::Escape));
        }
        let mut asked = None;
        let mut drawn = None;
        if self.open {
            let mut open = true;
            // It opens below the transport bar, where the command palette
            // does.
            let top = ctx.content_rect().center_top() + vec2(0.0, 72.0);
            let window = egui::Window::new("Settings")
                .open(&mut open)
                .collapsible(false)
                .pivot(Align2::CENTER_TOP)
                .default_pos(top)
                .default_size(SIZE)
                .show(ctx, |ui| asked = self.contents(ui, &mut cx, &mut drawn));
            // Clicking anywhere else stops giving a command keys.
            if window.is_some_and(|w| w.response.clicked_elsewhere()) {
                cx.shell.stop_giving_keys();
            }
            self.open = open;
        }
        if drawn != Some(KEYS) {
            cx.shell.hide_keys();
        }
        self.drawn = drawn;
        boxed.or(asked)
    }

    /// The list of pages, and the page in sight, whose id goes in `drawn`.
    fn contents(
        &mut self,
        ui: &mut Ui,
        cx: &mut Cx<'_>,
        drawn: &mut Option<&'static str>,
    ) -> Option<Asked> {
        let mut asked = None;
        ui.horizontal_top(|ui| {
            // Each page's title takes the list's width, as a menu's items do.
            let list = Layout::top_down_justified(Align::LEFT);
            ui.allocate_ui_with_layout(vec2(LIST_WIDTH, 0.0), list, |ui| {
                ui.set_width(LIST_WIDTH);
                for page in &PAGES {
                    let on = self.page == page.id;
                    let tab = ui.selectable_label(on, page.title);
                    // Named as the panels' tabs are.
                    let name = format!("{} page", page.title);
                    tab.widget_info(|| WidgetInfo::selected(Role::Tab, true, on, &name));
                    if tab.clicked() {
                        self.page = page.id;
                    }
                }
            });
            ui.separator();
            ui.vertical(|ui| {
                let Some(page) = PAGES.iter().find(|page| page.id == self.page) else {
                    return;
                };
                let new = self.drawn != Some(page.id);
                *drawn = Some(page.id);
                let pages = &mut self.pages;
                if page.scrolls {
                    asked = (page.show)(pages, ui, cx, new);
                } else {
                    ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| asked = (page.show)(pages, ui, cx, new));
                }
            });
        });
        asked
    }
}

/// What the Recording page keeps.
#[derive(Default)]
struct RecordingPage {
    /// Capture's length as the user drags it, until they let go.
    capture_drag: Option<u32>,
    /// The recording offset as the user drags it, until they let go.
    offset_drag: Option<i64>,
}

impl RecordingPage {
    /// How long Capture holds the inputs, and how late recordings through
    /// the device playing land.
    fn show(&mut self, ui: &mut Ui, cx: &Cx<'_>) -> Option<Asked> {
        let mut asked = None;
        ui.label(RichText::new("Capture").strong());
        ui.horizontal(|ui| {
            ui.label("Holds the last");
            let saved = cx.settings.recording.capture_minutes;
            let mut minutes = self.capture_drag.unwrap_or(saved);
            let field = ui
                .add(
                    DragValue::new(&mut minutes)
                        .range(0..=MAX_CAPTURE_MINUTES)
                        .clamp_existing_to_range(false)
                        .suffix(" min")
                        // A typed length applies on Enter, not at each digit.
                        .update_while_editing(false),
                )
                .on_hover_text(
                    "How much of the inputs of armed tracks, and of those monitoring In, \
                     Capture holds in memory, to make into takes after you played. 0 turns \
                     Capture off.",
                );
            field.widget_info(|| WidgetInfo {
                value: Some(f64::from(minutes)),
                ..WidgetInfo::labeled(Role::SpinButton, true, "Capture Length")
            });
            if field.dragged() {
                self.capture_drag = Some(minutes);
            } else if field.changed() || field.drag_stopped() {
                self.capture_drag = None;
                if minutes != saved {
                    asked = Some(Asked::CaptureMinutes(minutes));
                }
            }
            ui.label("of the inputs.");
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Recording offset").strong());
        match cx.output.info() {
            Some(info) => {
                if let Some(frames) = self.offset_row(ui, cx.offset, info.sample_rate) {
                    asked = Some(Asked::RecordingOffset(frames));
                }
            }
            None => {
                ui.label("No device is playing.");
            }
        }
        asked
    }

    /// The device playing's recording offset, `offset` frames at `rate`.
    /// Returns a new one, once the user has typed it, or let go of it.
    fn offset_row(&mut self, ui: &mut Ui, offset: i64, rate: u32) -> Option<i64> {
        let mut frames = self.offset_drag.unwrap_or(offset);
        let mut set = None;
        ui.horizontal(|ui| {
            ui.label("Recordings through the device playing land");
            let field = ui
                .add(
                    DragValue::new(&mut frames)
                        .range(-MAX_OFFSET..=MAX_OFFSET)
                        .suffix(" frames")
                        // A typed offset applies on Enter, not at each digit.
                        .update_while_editing(false),
                )
                .on_hover_text(
                    "How late recordings through this device land, in this mode and at this \
                     rate, beyond the latency its driver reports: what a cable from one of its \
                     outputs to one of its inputs measures. Every audio take starts that much \
                     later in what its input captured. The record round trip example measures \
                     it.",
                );
            field.widget_info(|| WidgetInfo {
                value: Some(frames as f64),
                ..WidgetInfo::labeled(Role::SpinButton, true, "Recording Offset")
            });
            if field.dragged() {
                self.offset_drag = Some(frames);
            } else if field.changed() || field.drag_stopped() {
                self.offset_drag = None;
                set = (frames != offset).then_some(frames);
            }
            let ms = frames as f64 * 1000.0 / f64::from(rate);
            ui.label(format!("late ({ms:.2} ms)."));
        });
        set
    }
}

/// What the Files page keeps.
#[derive(Default)]
struct FilesPage {
    /// The render cache's limits as the user drags one, until they let go.
    drag: Option<RenderCacheSettings>,
}

impl FilesPage {
    /// How much the render cache keeps, and where.
    fn show(&mut self, ui: &mut Ui, cx: &Cx<'_>) -> Option<Asked> {
        let mut asked = None;
        let saved = cx.settings.render_cache;
        let mut limits = self.drag.unwrap_or(saved);
        ui.label(RichText::new("Render cache").strong());
        ui.label(
            "Clips in Spectral play their audio stretched ahead of time. The render cache keeps \
             it, so it's ready the next time any project plays it.",
        );
        let field = |ui: &mut Ui, value: &mut u32, name: &str, hover: &str| {
            let field = ui
                .add(
                    DragValue::new(value)
                        .range(1..=RenderCacheSettings::MAX_GB)
                        .suffix(" GB")
                        // A typed size applies on Enter, not at each digit.
                        .update_while_editing(false),
                )
                .on_hover_text(hover);
            let shown = f64::from(*value);
            field.widget_info(|| WidgetInfo {
                value: Some(shown),
                ..WidgetInfo::labeled(Role::SpinButton, true, name)
            });
            field
        };
        let mut changed = false;
        let mut dragging = false;
        ui.horizontal(|ui| {
            ui.label("Holds up to");
            let size = field(
                ui,
                &mut limits.max_size_gb,
                "Render Cache Size",
                "The most the render cache holds. Past it, what was used longest ago goes first.",
            );
            ui.label("and leaves at least");
            let free = field(
                ui,
                &mut limits.min_free_gb,
                "Render Cache Free Space",
                "How much of its disk the render cache always leaves free, letting go of what \
                 was used longest ago to keep it so.",
            );
            ui.label("of the disk free.");
            for field in [size, free] {
                dragging |= field.dragged();
                changed |= field.changed() || field.drag_stopped();
            }
        });
        if dragging {
            self.drag = Some(limits);
        } else if changed {
            self.drag = None;
            if limits != saved {
                asked = Some(Asked::RenderCache(limits));
            }
        }
        match cx.render_folder {
            Some(folder) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label("It keeps it in");
                    ui.add(
                        Label::new(RichText::new(folder.display().to_string()).monospace())
                            .selectable(true),
                    );
                });
                if ui
                    .button("Show Folder")
                    .on_hover_text("Opens the render cache's folder in the file manager")
                    .clicked()
                {
                    asked = Some(Asked::OpenRenderCacheFolder);
                }
            }
            None => {
                ui.label(
                    "Musicable found no folder for the render cache, so it keeps what it renders \
                     until it closes.",
                );
            }
        }
        asked
    }
}

/// How editing behaves where people's habits differ, which goes with the
/// profile, and how analysis finds beats, which is the computer's. Returns
/// what the user changed.
fn behaviour(ui: &mut Ui, cx: &Cx<'_>) -> Option<Asked> {
    let behaviour = cx.profile.behaviour;
    let mut new = behaviour;
    ui.label(RichText::new("Play/Stop plays from").strong());
    ui.radio_value(
        &mut new.play_from,
        PlayFrom::StartMarker,
        "The start marker, as in Live",
    )
    .on_hover_text("Playing again starts where the last play did, or where you put the marker");
    ui.radio_value(
        &mut new.play_from,
        PlayFrom::Playhead,
        "Where the playhead stopped",
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Middle C, MIDI note 60, is").strong());
    let names = [
        (MiddleC::C3, "C3, as in Live, Cubase, Logic and Bitwig"),
        (
            MiddleC::C4,
            "C4, as in Reaper and scientific pitch notation",
        ),
        (MiddleC::C5, "C5, as in FL Studio"),
    ];
    for (middle_c, name) in names {
        ui.radio_value(&mut new.middle_c, middle_c, name);
    }
    ui.add_space(8.0);
    ui.label(RichText::new("Follow").strong());
    ui.radio_value(
        &mut new.follow_mode,
        FollowMode::Page,
        "Turns a page as the playhead leaves the view, as in Live",
    );
    ui.radio_value(
        &mut new.follow_mode,
        FollowMode::Continuous,
        "Keeps the playhead still, and moves the song under it",
    )
    .on_hover_text("As Pro Tools' and Bitwig's continuous scrolling does");
    ui.add_enabled_ui(new.follow_mode == FollowMode::Continuous, |ui| {
        ui.horizontal(|ui| {
            ui.label("The playhead stays");
            ui.radio_value(
                &mut new.follow_place,
                FollowPlace::Third,
                "a third of the way in",
            );
            ui.radio_value(&mut new.follow_place, FollowPlace::Middle, "in the middle");
        });
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Track heights").strong());
    ui.horizontal_wrapped(|ui| {
        let sizes = [
            (&mut new.small_tracks, "Small"),
            (&mut new.medium_tracks, "Medium"),
            (&mut new.large_tracks, "Large"),
            (&mut new.huge_tracks, "Huge"),
        ];
        for (size, name) in sizes {
            ui.label(name);
            ui.add(DragValue::new(size).range(24..=400).suffix(" pt"))
                .on_hover_text(format!("The {name} size, in points"));
        }
    });
    ui.checkbox(
        &mut new.snap_track_heights,
        "Dragging a track's edge snaps to these sizes",
    )
    .on_hover_text("As Cubase's Snap Track Heights does");
    ui.checkbox(
        &mut new.auto_zoom_tracks,
        "Auto Zoom: the selected track is Large while it's selected",
    )
    .on_hover_text(
        "As in Bitwig, Cubase and Logic. The heights it gives are never saved, and turning it \
         off leaves the heights you set.",
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Auto-warp").strong());
    let rules = [
        (
            &mut new.warp_recordings,
            "Recordings follow the tempo",
            "Each keeps to the tempo it was played to, sample for sample until the tempo changes",
        ),
        (
            &mut new.warp_loops,
            "Loops warp to their whole bars",
            "One marker at the loop's start, at the tempo its length gives",
        ),
        (
            &mut new.warp_steady,
            "Files at a steady tempo warp from their first downbeat",
            "One marker on the first downbeat, which lands on the bar line nearest the drop",
        ),
        (
            &mut new.warp_drifting,
            "Files whose tempo drifts lock to the grid",
            "A marker on every downbeat, and on each beat that strays, so they keep their feel",
        ),
        (
            &mut new.sync_one_shots,
            "Sounds without a pulse land on their hit",
            "One-shots, effects and speech stay unwarped, and play their first hit, or a riser's \
             peak, where they're dropped",
        ),
        (
            &mut new.tempo_from_first_file,
            "The first file sets the song's tempo",
            "The first file with a pulse dropped into an empty song gives it its tempo and meter, \
             rather than being stretched",
        ),
    ];
    for (on, text, hover) in rules {
        ui.checkbox(on, text).on_hover_text(hover);
    }
    ui.label(
        RichText::new(
            "Audio files dropped into the song are warped by what analysis finds in them, by the \
             rule for their kind. A file analysis is unsure of comes in as it is, and Warp warps \
             it at the best guess at its tempo.",
        )
        .weak(),
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Devices").strong());
    ui.checkbox(
        &mut new.copies_take_automation,
        "Copies of devices take their automation",
    )
    .on_hover_text(
        "A device copied, duplicated or pasted gets lanes of its own with the original's \
         points, as in Live. Without it, copies start without automation.",
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Analysis").strong());
    let mut fast = cx.settings.analysis.fast;
    if ui
        .checkbox(&mut fast, "Fast analysis")
        .on_hover_text("Quicker on slower computers, but less accurate")
        .changed()
    {
        return Some(Asked::FastAnalysis(fast));
    }
    ui.label(
        RichText::new(
            "Analysis finds beats with the Beat This! network, and checks them with a classic \
             beat tracker. Fast analysis uses the classic tracker alone. It's this computer's \
             setting, the same in every profile.",
        )
        .weak(),
    );
    (new != behaviour).then_some(Asked::Behaviour(new))
}

/// The user's workspaces, by slot, with the one in use picked, and what
/// can be done with them, which runs the workspace commands.
fn workspaces(ui: &mut Ui, cx: &mut Cx<'_>) -> Option<Asked> {
    let view = cx.view;
    let actions = &mut *cx.actions;
    let listed = view.workspaces;
    for ((slot, name), switch) in listed.slots.iter().enumerate().zip(workspace::SWITCH) {
        let Some(name) = name else {
            continue;
        };
        ui.horizontal(|ui| {
            let picked = ui.radio(slot == listed.current, name);
            if picked.clicked() && slot != listed.current {
                actions.push(Action::Run(switch.clone()));
            }
            let chords: Vec<String> = (view.keymap.chords_for(&switch))
                .map(|chord| chord.to_string())
                .collect();
            ui.label(RichText::new(chords.join(", ")).weak());
        });
    }
    ui.add_space(4.0);
    let default = listed.current == 0;
    ui.horizontal(|ui| {
        let mut run = |ui: &mut Ui, enabled: bool, text: &str, id| {
            if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
                actions.push(Action::Run(id));
            }
        };
        run(ui, true, "Save As...", workspace::SAVE_AS);
        run(ui, !default, "Rename...", workspace::RENAME);
        run(ui, !default, "Delete...", workspace::DELETE);
        run(ui, true, "Reset", workspace::RESET);
    });
    ui.label(
        RichText::new(
            "A workspace is a layout of the panels, with where the window is and the parts of the \
             transport bar, the track headers and the mixer strips. What you change goes into the \
             one in use. Default can't be renamed or deleted.",
        )
        .weak(),
    );
    ui.separator();
    let mut asked = None;
    match cx.workspaces_folder {
        Some(folder) => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Your workspaces folder:");
                ui.add(
                    Label::new(RichText::new(folder.display().to_string()).monospace())
                        .selectable(true),
                );
            });
            if ui
                .button("Open Workspaces Folder")
                .on_hover_text("Shows the folder of your workspace files")
                .clicked()
            {
                asked = Some(Asked::OpenWorkspacesFolder);
            }
        }
        None => {
            ui.label(
                "Musicable found no folder for workspace files, so changes last until it closes.",
            );
        }
    }
    asked
}

/// How the browser previews samples, and where Musicable's presets go.
fn browser(ui: &mut Ui, cx: &mut Cx<'_>) -> Option<Asked> {
    let settings = &cx.settings.browser;
    let actions = &mut *cx.actions;
    ui.label(RichText::new("Previews").strong());
    let mut on = settings.auto_preview;
    if ui
        .checkbox(&mut on, "Preview samples as they're picked")
        .changed()
    {
        actions.push(Action::SetAutoPreview(on));
    }
    ui.horizontal(|ui| {
        ui.label("Volume");
        let mut volume = settings.preview_volume;
        let slider = Slider::new(&mut volume, MIN_PREVIEW_VOLUME..=MAX_PREVIEW_VOLUME)
            .suffix(" dB")
            .clamping(egui::SliderClamping::Always);
        let slider = ui.add(slider).on_hover_text("How loud previews play");
        let value = f64::from(volume);
        slider.widget_info(|| WidgetInfo::slider(true, value, "Preview Volume"));
        if slider.changed() {
            actions.push(Action::SetPreviewVolume(volume));
        }
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Presets").strong());
    let mut asked = None;
    match cx.presets_folder {
        Some(folder) => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Musicable saves its presets in");
                ui.add(
                    Label::new(RichText::new(folder.display().to_string()).monospace())
                        .selectable(true),
                );
            });
        }
        None => {
            ui.label("Musicable found no folder for presets: choose one to save them in.");
        }
    }
    ui.horizontal(|ui| {
        if ui
            .button("Choose Folder...")
            .on_hover_text("Saves presets in another folder from now on")
            .clicked()
        {
            asked = Some(Asked::ChoosePresetsFolder);
        }
        let chosen = settings.presets_folder.is_some();
        if ui
            .add_enabled(chosen, egui::Button::new("Use the Default"))
            .on_hover_text(
                "Saves presets in the Presets folder of the Musicable folder in your music folder",
            )
            .clicked()
        {
            asked = Some(Asked::DefaultPresetsFolder);
        }
    });
    asked
}

/// Whether agents can control Musicable, how they're doing, and how to add
/// Musicable to Claude Code.
fn agents(ui: &mut Ui, agents: &Agents<'_>) -> Option<Asked> {
    let mut asked = None;
    let mut on = agents.enabled;
    if ui
        .checkbox(&mut on, "Let agents control Musicable")
        .on_hover_text(
            "Listens for agents on a pipe that only your account can open, never on the network",
        )
        .changed()
    {
        asked = Some(Asked::Agents(on));
    }
    ui.label(
        RichText::new(
            "Agents, such as Claude Code, can then run Musicable's commands and read the project \
             while you watch, also with the window minimized. Each change they make is a step in \
             the undo history, as yours are. They connect from this computer only.",
        )
        .weak(),
    );
    let mut hands = agents.mouse_and_keys;
    if ui
        .add_enabled(
            agents.enabled,
            Checkbox::new(&mut hands, "Let agents use the mouse and keys"),
        )
        .on_hover_text(
            "Agents can then click, drag and type in Musicable's windows, to check what the mouse \
             and keys do",
        )
        .changed()
    {
        asked = Some(Asked::AgentsMouseAndKeys(hands));
    }
    ui.label(
        RichText::new(
            "Their clicks, drags and keys go into Musicable's input, so your mouse pointer never \
             moves, and the status bar says while they're at it. They stop the moment you use \
             the mouse or keys in Musicable.",
        )
        .weak(),
    );
    ui.add_space(8.0);
    ui.label(RichText::new("Status").strong());
    match (agents.enabled, agents.problem, &agents.listening) {
        (false, ..) => {
            ui.label("Off: Musicable doesn't listen for agents.");
        }
        (true, Some(problem), _) => {
            ui.colored_label(ui.visuals().warn_fg_color, problem);
        }
        (true, None, listening) => {
            match agents.connected.as_slice() {
                [] => ui.label("Listening for agents. None is connected."),
                connected => ui.label(format!("Connected: {}.", connected.join(", "))),
            };
            if agents.using_hands {
                ui.label("An agent is using the mouse and keys.");
            }
            if let Some(address) = listening {
                ui.add(
                    Label::new(RichText::new(address).monospace().weak())
                        .selectable(true)
                        .wrap(),
                );
            }
        }
    }
    ui.add_space(8.0);
    ui.label(RichText::new("Claude Code").strong());
    ui.label("To add Musicable to Claude Code, run this in a terminal:");
    ui.horizontal_wrapped(|ui| {
        let command = RichText::new(agents.claude_command).monospace();
        ui.add(Label::new(command).selectable(true));
        if ui
            .button("Copy")
            .on_hover_text("Copies the command")
            .clicked()
        {
            ui.ctx().copy_text(agents.claude_command.to_owned());
        }
    });
    ui.label(
        RichText::new(
            "Other agents' clients start the same program, with mcp after it: it speaks MCP, the \
             Model Context Protocol, over its standard input and output.",
        )
        .weak(),
    );
    asked
}

#[cfg(test)]
mod tests {
    use eframe::egui::vec2;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// A device's recording offset, as the Recording page shows it, with
    /// the offsets it asked for.
    struct Offset {
        page: RecordingPage,
        frames: i64,
        asked: Vec<i64>,
    }

    #[test]
    fn a_recording_offset_applies_once_typed_or_let_go_of() {
        let offset = Offset {
            page: RecordingPage::default(),
            frames: 0,
            asked: Vec::new(),
        };
        let mut harness = Harness::new_ui_state(
            |ui, offset: &mut Offset| {
                if let Some(frames) = offset.page.offset_row(ui, offset.frames, 48_000) {
                    offset.asked.push(frames);
                    offset.frames = frames;
                }
            },
            offset,
        );
        harness.get_by_label("late (0.00 ms).");
        harness
            .get_by_label("Recording Offset")
            .set_value_accesskit(480.0);
        harness.run();
        assert_eq!(harness.state().asked, [480]);
        harness.get_by_label("late (10.00 ms).");

        // Dragged, it shows the offset as it goes, and asks for it once let
        // go of.
        let at = harness.get_by_label("Recording Offset").rect().center();
        harness.hover_at(at);
        harness.run();
        harness.drag_at(at);
        harness.run();
        for step in 1..=4 {
            harness.hover_at(at + vec2(10.0 * step as f32, 0.0));
            harness.run();
        }
        assert_eq!(harness.state().asked, [480]);
        let dragged = harness.state().page.offset_drag;
        assert!(dragged.is_some_and(|frames| frames > 480), "{dragged:?}");
        harness.drop_at(at + vec2(40.0, 0.0));
        harness.run();
        let asked = &harness.state().asked;
        assert!(asked.len() == 2 && asked[1] > 480, "{asked:?}");
        assert_eq!(harness.state().page.offset_drag, None);
    }
}
