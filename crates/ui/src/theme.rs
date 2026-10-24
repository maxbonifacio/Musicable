//! The colours, sizes and fonts the views draw with.
//!
//! Views draw from a [`Theme`], never from colours or sizes of their own, so
//! a theme can change how all of Musicable looks. Its [`widgets`] section
//! does the same for egui's own widgets, such as buttons, menus and text
//! fields, through egui's style ([`apply`]), and its [`fonts`] section names
//! the fonts. Musicable has a dark theme and a light one built in
//! ([`Base`]), and theme files change one of them ([`ThemeFile`]).
//!
//! [`widgets`]: Theme::widgets
//! [`fonts`]: Theme::fonts

mod file;
mod light;
mod style;
mod values;

use egui::{Color32, Stroke, Vec2, vec2};
use serde::{Deserialize, Serialize};

pub use file::{ThemeFile, ThemeFileError, ThemeProblem};
pub use style::apply;

/// How Musicable looks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    // The colours of mute, solo and arm when they're on, wherever they are,
    // and of the text on them.
    #[serde(with = "values::colour")]
    pub mute: Color32,
    #[serde(with = "values::colour")]
    pub solo: Color32,
    #[serde(with = "values::colour")]
    pub arm: Color32,
    /// Input monitoring's buttons, when they're on.
    #[serde(with = "values::colour")]
    pub monitor: Color32,
    /// Built-in devices' switches, when they're on.
    #[serde(with = "values::colour")]
    pub switch: Color32,
    #[serde(with = "values::colour")]
    pub ink: Color32,
    /// The meters' colours, from quiet to loud to over full scale.
    #[serde(with = "values::colour")]
    pub meter_quiet: Color32,
    #[serde(with = "values::colour")]
    pub meter_loud: Color32,
    #[serde(with = "values::colour")]
    pub meter_over: Color32,
    /// Errors and warnings, such as dropouts.
    #[serde(with = "values::colour")]
    pub warning: Color32,
    /// How round clips', buttons' and strips' corners are.
    pub corner: f32,
    /// The size of the letters on buttons, such as mute and solo.
    pub button_text: f32,

    // The transport bar's sizes, in points.
    /// The width of play, stop and go to start, and the size of their
    /// icons, from the middle to the edge.
    pub icon_button: f32,
    pub icon: f32,
    /// The size of the playhead's position.
    pub position_text: f32,
    /// The CPU meter.
    #[serde(with = "values::pair")]
    pub cpu_meter: Vec2,

    // The arrangement's colours.
    /// The tracks' lanes, every other one a little lighter.
    #[serde(with = "values::colour")]
    pub lane: Color32,
    #[serde(with = "values::colour")]
    pub lane_alternate: Color32,
    /// The lane of the selected track.
    #[serde(with = "values::colour")]
    pub lane_selected: Color32,
    /// Below the last track.
    #[serde(with = "values::colour")]
    pub empty: Color32,
    /// The grid's lines: at bars, at beats and between.
    #[serde(with = "values::colour")]
    pub grid_bar: Color32,
    #[serde(with = "values::colour")]
    pub grid_beat: Color32,
    #[serde(with = "values::colour")]
    pub grid_step: Color32,
    #[serde(with = "values::colour")]
    pub ruler: Color32,
    #[serde(with = "values::colour")]
    pub ruler_text: Color32,
    /// The loop brace while looping, and while not.
    #[serde(with = "values::colour")]
    pub loop_on: Color32,
    #[serde(with = "values::colour")]
    pub loop_off: Color32,
    /// The loop region's shade over the lanes while looping.
    #[serde(with = "values::colour")]
    pub loop_shade: Color32,
    #[serde(with = "values::colour")]
    pub locator: Color32,
    #[serde(with = "values::colour")]
    pub playhead: Color32,
    #[serde(with = "values::colour")]
    pub start_marker: Color32,
    #[serde(with = "values::colour")]
    pub time_selection: Color32,
    /// Clips take a colour from these, by their track's place.
    #[serde(with = "values::colours")]
    pub clips: [Color32; 8],
    /// Laid over a clip's colour under its title bar, where its waveform or
    /// its notes are.
    #[serde(with = "values::colour")]
    pub clip_body: Color32,
    #[serde(with = "values::colour")]
    pub clip_text: Color32,
    /// The outline of selected clips.
    #[serde(with = "values::colour")]
    pub clip_selected: Color32,
    #[serde(with = "values::colour")]
    pub waveform: Color32,
    #[serde(with = "values::colour")]
    pub notes: Color32,
    #[serde(with = "values::colour")]
    pub fade: Color32,
    /// Where clips land while they're dragged.
    #[serde(with = "values::colour")]
    pub ghost: Color32,
    /// The lanes of tracks' takes, under the tracks.
    #[serde(with = "values::colour")]
    pub take_lane: Color32,
    /// A take's clips where the track doesn't play them. Where it does,
    /// they take the track's colour.
    #[serde(with = "values::colour")]
    pub take_clip: Color32,
    /// What the armed tracks have recorded so far, while they record.
    #[serde(with = "values::colour")]
    pub recording: Color32,
    /// Automation lanes, under the tracks, and the lines and points on
    /// them: grey where someone overrode the automation by hand, and for a
    /// parameter's own value where it has none.
    #[serde(with = "values::colour")]
    pub automation_lane: Color32,
    #[serde(with = "values::colour")]
    pub automation: Color32,
    #[serde(with = "values::colour")]
    pub automation_off: Color32,
    #[serde(with = "values::colour")]
    pub header: Color32,
    #[serde(with = "values::colour")]
    pub header_selected: Color32,
    #[serde(with = "values::colour")]
    pub header_text: Color32,
    /// Lines between tracks and around panels.
    #[serde(with = "values::colour")]
    pub divider: Color32,
    /// The devices panel's boxes, one for each device.
    #[serde(with = "values::colour")]
    pub device: Color32,
    /// The waveform of the sample previewing, in the browser.
    #[serde(with = "values::colour")]
    pub preview_wave: Color32,

    // The piano roll's colours. Its notes take their track's colour, and
    // its grid lines, start marker and playhead are the arrangement's.
    /// The rows of the white keys and of the black keys, behind the notes.
    #[serde(with = "values::colour")]
    pub white_row: Color32,
    #[serde(with = "values::colour")]
    pub black_row: Color32,
    /// The keyboard beside them, and the names of its Cs.
    #[serde(with = "values::colour")]
    pub white_key: Color32,
    #[serde(with = "values::colour")]
    pub black_key: Color32,
    #[serde(with = "values::colour")]
    pub key_text: Color32,
    /// Laid over the rows before the clip's start and after its end, where
    /// notes don't play.
    #[serde(with = "values::colour")]
    pub outside_clip: Color32,
    /// The rectangle that selects the notes in it, while it's dragged.
    #[serde(with = "values::colour")]
    pub area: Color32,

    // The arrangement's sizes, in points.
    /// The ruler over the tracks, and the strip at its top for the loop
    /// brace and the locators.
    pub ruler_height: f32,
    pub ruler_strip: f32,
    /// The track headers' column.
    pub header_width: f32,
    /// A track's height until it's changed.
    pub track_height: f32,
    /// A take lane's height.
    pub take_height: f32,
    /// An automation lane's height.
    pub automation_height: f32,
    /// The radius of an automation lane's points, and how near a point or
    /// the line through them a press takes it.
    pub point: f32,
    pub point_grab: f32,
    /// The bar with a clip's name, when the clip is tall enough for one.
    pub clip_title: f32,
    /// How near a clip's edge a drag trims it.
    pub edge_grab: f32,
    /// The square handles that drag fades.
    pub fade_handle: f32,
    /// How near a track's bottom edge a drag changes its height.
    pub height_grab: f32,
    /// The least room between the ruler's bar numbers.
    pub label_spacing: f32,
    /// The start marker's and the locators' triangles.
    pub marker: f32,
    /// Room around what's in a track's header.
    pub padding: f32,
    /// The colour strip down the left of a track's header.
    pub colour_strip: f32,
    /// Mute, solo and arm in a track's header, and its meter's width.
    #[serde(with = "values::pair")]
    pub button: Vec2,
    pub header_meter: f32,
    /// The size of tracks' names and the ruler's numbers, and of clips'
    /// and locators' names.
    pub text: f32,
    pub small_text: f32,

    pub mixer: MixerSizes,
    pub devices: DeviceSizes,
    pub piano_roll: PianoRollSizes,
    pub browser: BrowserSizes,
    pub widgets: Widgets,
    pub fonts: Fonts,
}

/// The piano roll's sizes, in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PianoRollSizes {
    /// The piano roll panel's height until the user drags its edge.
    pub panel: f32,
    /// The keyboard's width, beside the rows of keys.
    pub keys: f32,
    /// The ruler over the rows.
    pub ruler: f32,
    /// The lane under the rows, of the notes' velocities or a controller.
    pub lane: f32,
    /// How near a note's start or end a drag trims it.
    pub edge_grab: f32,
    /// How far a drag up or down a note goes to change its velocity from
    /// the softest to the hardest.
    pub velocity_drag: f32,
}

/// The mixer's sizes, in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MixerSizes {
    /// The mixer panel's height until the user drags its edge.
    pub panel: f32,
    /// A strip's width.
    pub strip: f32,
    /// The space around what's in a strip, and between its controls.
    pub margin: f32,
    pub gap: f32,
    /// The height of a strip's rows of controls.
    pub row: f32,
    /// The shortest a fader gets, however small the window.
    pub min_fader: f32,
    /// Room left under the strips for a scroll bar.
    pub scroll_bar: f32,
    /// A fader's cap, a meter's width and the width of the scale beside it.
    #[serde(with = "values::pair")]
    pub cap: Vec2,
    pub meter: f32,
    pub scale: f32,
    /// The pre-fader buttons beside the sends.
    pub pre_button: f32,
    /// The size of the readouts' digits, the sends' and the pan's text, and
    /// the scale's numbers.
    pub readout_text: f32,
    pub small_text: f32,
    pub scale_text: f32,
}

/// The devices panel's and the browser's sizes, in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DeviceSizes {
    /// The devices panel's height until the user drags its edge.
    pub panel: f32,
    /// A plugin device's box's width.
    pub device: f32,
    /// The width of a built-in device's controls, and of their columns.
    pub control: f32,
    /// The space around what's in a device's box, and between the boxes.
    pub margin: f32,
    pub gap: f32,
    /// The browser's width until the user drags its edge.
    pub browser: f32,
}

/// The browser's sizes, in points. Its width is in [`DeviceSizes`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BrowserSizes {
    /// How far in a folder's rows are from its own, and the space for its
    /// arrow.
    pub indent: f32,
    /// The height of the waveform of the sample previewing.
    pub waveform: f32,
}

/// How egui's own widgets look: the menus, buttons, checkboxes, text fields
/// and sliders, the windows and dialogs, and the panels the views sit in.
/// [`apply`] hands them to egui as its style.
///
/// A theme is dark or light by its panels' colour, which decides the window's
/// title bar and how egui draws text.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Widgets {
    /// Behind panels, such as the menu bar and the transport bar.
    #[serde(with = "values::colour")]
    pub panel: Color32,
    /// Behind windows, menus and tooltips, and around them.
    #[serde(with = "values::colour")]
    pub window: Color32,
    #[serde(with = "values::colour")]
    pub window_outline: Color32,
    pub window_outline_width: f32,
    /// The shadow under windows and menus.
    #[serde(with = "values::colour")]
    pub shadow: Color32,
    /// Behind text fields, and the CPU meter.
    #[serde(with = "values::colour")]
    pub field: Color32,
    /// Behind every other row of striped lists.
    #[serde(with = "values::colour")]
    pub stripe: Color32,
    /// Behind code.
    #[serde(with = "values::colour")]
    pub code: Color32,
    #[serde(with = "values::colour")]
    pub link: Color32,
    /// Warnings and errors in windows, such as a device that didn't open.
    #[serde(with = "values::colour")]
    pub warning: Color32,
    #[serde(with = "values::colour")]
    pub error: Color32,
    /// Behind selected text and buttons that are on, such as Loop while it
    /// loops, and the text on them.
    #[serde(with = "values::colour")]
    pub selection: Color32,
    #[serde(with = "values::colour")]
    pub selected_text: Color32,
    /// The cursor in text fields.
    #[serde(with = "values::colour")]
    pub cursor: Color32,
    /// How opaque faint text is, such as "Ready" in the status bar, from 0
    /// to 1.
    pub weak_text: f32,
    /// How round windows' and menus' corners are.
    pub window_corner: f32,
    pub menu_corner: f32,

    /// What can't be clicked: labels, separators and frames. Their text is
    /// most of the text in windows and menus.
    pub passive: WidgetState,
    /// Buttons, checkboxes, sliders and the rest, at rest, hovered, pressed
    /// or dragged, and with their menu open, as a combo box's is.
    pub resting: WidgetState,
    pub hovered: WidgetState,
    pub pressed: WidgetState,
    pub open: WidgetState,

    // Their sizes, in points.
    /// The room between widgets, across and down.
    #[serde(with = "values::pair")]
    pub item_spacing: Vec2,
    /// The room around buttons' text.
    #[serde(with = "values::pair")]
    pub button_padding: Vec2,
    /// The room inside windows and menus.
    pub window_margin: f32,
    pub menu_margin: f32,
    /// How far in nested things are.
    pub indent: f32,
    /// The least width and height of what can be clicked, such as number
    /// fields.
    #[serde(with = "values::pair")]
    pub interact_size: Vec2,
    /// The widths of sliders, combo boxes and text fields, until a window
    /// makes them wider.
    pub slider_width: f32,
    pub combo_width: f32,
    pub text_edit_width: f32,
    /// The boxes of checkboxes and radio buttons, and the room after them.
    pub icon_width: f32,
    pub icon_spacing: f32,
    /// How wide scroll bars are while the mouse is over them.
    pub scroll_bar: f32,
}

/// How egui's widgets look in one state, such as hovered.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WidgetState {
    /// Behind buttons, which can go without, and behind checkboxes, radio
    /// buttons and sliders, which can't.
    #[serde(with = "values::colour")]
    pub background: Color32,
    #[serde(with = "values::colour")]
    pub fill: Color32,
    /// The line around them, and how wide it is.
    #[serde(with = "values::colour")]
    pub outline: Color32,
    pub outline_width: f32,
    /// Their text, and the lines of the marks drawn on them, such as ticks
    /// and arrows, and how wide those lines are.
    #[serde(with = "values::colour")]
    pub text: Color32,
    pub line_width: f32,
    /// How round their corners are.
    pub corner: f32,
    /// How far they grow past their place.
    pub expansion: f32,
}

/// The fonts egui's widgets and Musicable's views write with, and the sizes
/// of egui's text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fonts {
    /// The font of all text that isn't monospaced: a font file beside the
    /// theme file, such as `"Inter.ttf"`, or one of egui's fonts,
    /// `"Ubuntu-Light"` or `"Hack"`. Empty for egui's, Ubuntu Light. Text in
    /// another script that it can't write falls back to egui's fonts.
    pub text: String,
    /// The font of monospaced text, such as the playhead's position. Empty
    /// for egui's, Hack.
    pub monospace: String,
    pub sizes: FontSizes,
}

/// The sizes of egui's text, in points. Musicable's views have sizes of
/// their own, such as [`Theme::text`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FontSizes {
    /// Labels, menus and text fields.
    pub body: f32,
    pub button: f32,
    pub small: f32,
    /// Headings, such as dialogs' questions.
    pub heading: f32,
    pub monospace: f32,
}

/// A theme Musicable has built in, which theme files start from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Base {
    #[default]
    Dark,
    Light,
}

impl Base {
    pub const ALL: [Base; 2] = [Base::Dark, Base::Light];

    /// The theme.
    pub fn theme(self) -> Theme {
        match self {
            Self::Dark => Theme::dark(),
            Self::Light => Theme::light(),
        }
    }

    /// What theme files call it, as their `base`: `dark` or `light`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// The base whose [`name`](Self::name) is `name`, in any case.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|base| base.name().eq_ignore_ascii_case(name))
    }

    /// What windows call it: "Dark" or "Light".
    pub fn title(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

impl MixerSizes {
    /// The width of what's in a strip.
    pub fn inner(&self) -> f32 {
        self.strip - 2.0 * self.margin
    }
}

impl Default for Theme {
    /// The dark theme.
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// The dark theme, whose widgets look as egui's dark ones do.
    pub fn dark() -> Self {
        Self {
            mute: Color32::from_rgb(232, 165, 50),
            solo: Color32::from_rgb(80, 160, 235),
            arm: Color32::from_rgb(230, 75, 65),
            monitor: Color32::from_rgb(95, 190, 160),
            switch: Color32::from_rgb(236, 190, 60),
            ink: Color32::from_gray(24),
            meter_quiet: Color32::from_rgb(90, 190, 110),
            meter_loud: Color32::from_rgb(230, 200, 70),
            meter_over: Color32::from_rgb(230, 70, 60),
            warning: Color32::from_rgb(235, 90, 80),
            corner: 3.0,
            button_text: 12.0,

            icon_button: 32.0,
            icon: 5.5,
            position_text: 18.0,
            cpu_meter: vec2(56.0, 10.0),

            lane: Color32::from_gray(38),
            lane_alternate: Color32::from_gray(42),
            lane_selected: Color32::from_gray(52),
            empty: Color32::from_gray(30),
            grid_bar: Color32::from_gray(78),
            grid_beat: Color32::from_gray(58),
            grid_step: Color32::from_gray(48),
            ruler: Color32::from_gray(34),
            ruler_text: Color32::from_gray(170),
            loop_on: Color32::from_rgb(215, 160, 60),
            loop_off: Color32::from_gray(95),
            loop_shade: Color32::from_rgba_unmultiplied(215, 160, 60, 14),
            locator: Color32::from_rgb(200, 200, 205),
            playhead: Color32::from_rgb(240, 240, 240),
            start_marker: Color32::from_rgb(150, 200, 255),
            time_selection: Color32::from_rgba_unmultiplied(150, 200, 255, 44),
            clips: [
                Color32::from_rgb(214, 120, 76),
                Color32::from_rgb(196, 168, 72),
                Color32::from_rgb(120, 176, 92),
                Color32::from_rgb(76, 168, 152),
                Color32::from_rgb(84, 140, 210),
                Color32::from_rgb(140, 116, 210),
                Color32::from_rgb(196, 98, 168),
                Color32::from_rgb(160, 160, 160),
            ],
            clip_body: Color32::from_rgba_unmultiplied(255, 255, 255, 70),
            clip_text: Color32::from_gray(20),
            clip_selected: Color32::from_rgb(250, 250, 250),
            waveform: Color32::from_rgba_unmultiplied(20, 20, 20, 170),
            notes: Color32::from_rgba_unmultiplied(20, 20, 20, 190),
            fade: Color32::from_rgba_unmultiplied(20, 20, 20, 210),
            ghost: Color32::from_rgba_unmultiplied(250, 250, 250, 60),
            take_lane: Color32::from_gray(33),
            take_clip: Color32::from_gray(96),
            recording: Color32::from_rgba_unmultiplied(230, 75, 65, 110),
            automation_lane: Color32::from_gray(35),
            automation: Color32::from_rgb(240, 95, 85),
            automation_off: Color32::from_gray(125),
            header: Color32::from_gray(46),
            header_selected: Color32::from_gray(62),
            header_text: Color32::from_gray(225),
            divider: Color32::from_gray(24),
            device: Color32::from_gray(46),
            preview_wave: Color32::from_rgb(150, 200, 255),

            white_row: Color32::from_gray(44),
            black_row: Color32::from_gray(36),
            white_key: Color32::from_gray(205),
            black_key: Color32::from_gray(40),
            key_text: Color32::from_gray(60),
            outside_clip: Color32::from_rgba_unmultiplied(0, 0, 0, 90),
            area: Color32::from_rgba_unmultiplied(150, 200, 255, 36),

            ruler_height: 40.0,
            ruler_strip: 18.0,
            header_width: 190.0,
            track_height: 60.0,
            take_height: 36.0,
            automation_height: 48.0,
            point: 3.0,
            point_grab: 5.0,
            clip_title: 14.0,
            edge_grab: 6.0,
            fade_handle: 7.0,
            height_grab: 4.0,
            label_spacing: 44.0,
            marker: 7.0,
            padding: 4.0,
            colour_strip: 4.0,
            button: vec2(20.0, 16.0),
            header_meter: 7.0,
            text: 12.0,
            small_text: 10.0,

            mixer: MixerSizes {
                panel: 300.0,
                strip: 96.0,
                margin: 4.0,
                gap: 4.0,
                row: 20.0,
                min_fader: 96.0,
                scroll_bar: 10.0,
                cap: vec2(24.0, 12.0),
                meter: 11.0,
                scale: 18.0,
                pre_button: 26.0,
                readout_text: 11.0,
                small_text: 10.0,
                scale_text: 9.0,
            },
            devices: DeviceSizes {
                panel: 190.0,
                device: 170.0,
                control: 96.0,
                margin: 4.0,
                gap: 6.0,
                browser: 260.0,
            },
            browser: BrowserSizes {
                indent: 14.0,
                waveform: 44.0,
            },
            piano_roll: PianoRollSizes {
                panel: 320.0,
                keys: 56.0,
                ruler: 22.0,
                lane: 60.0,
                edge_grab: 4.0,
                velocity_drag: 127.0,
            },
            widgets: Widgets::dark(),
            fonts: Fonts::default(),
        }
    }

    /// The colour of clips on the track at `index` in the track list.
    pub fn clip(&self, index: usize) -> Color32 {
        self.clips[index % self.clips.len()]
    }
}

impl Widgets {
    /// egui's dark widgets, as they look without a theme.
    fn dark() -> Self {
        let state = |background, fill, outline: Stroke, text: Stroke, corner| WidgetState {
            background: Color32::from_gray(background),
            fill: Color32::from_gray(fill),
            outline: outline.color,
            outline_width: outline.width,
            text: text.color,
            line_width: text.width,
            corner,
            expansion: 0.0,
        };
        let line = |width, gray| Stroke::new(width, Color32::from_gray(gray));
        Self {
            panel: Color32::from_gray(27),
            window: Color32::from_gray(27),
            window_outline: Color32::from_gray(60),
            window_outline_width: 1.0,
            shadow: Color32::from_black_alpha(96),
            field: Color32::from_gray(10),
            // egui adds a little light here, which looks the same on the
            // panels and windows.
            stripe: Color32::from_gray(32),
            code: Color32::from_gray(64),
            link: Color32::from_rgb(90, 170, 255),
            warning: Color32::from_rgb(255, 143, 0),
            error: Color32::from_rgb(255, 0, 0),
            selection: Color32::from_rgb(0, 92, 128),
            selected_text: Color32::from_rgb(192, 222, 255),
            cursor: Color32::from_rgb(192, 222, 255),
            weak_text: 0.6,
            window_corner: 6.0,
            menu_corner: 6.0,
            passive: state(27, 27, line(1.0, 60), line(1.0, 140), 4.0),
            resting: state(50, 60, Stroke::NONE, line(1.0, 215), 6.0),
            hovered: state(64, 70, line(1.0, 150), line(1.5, 245), 6.0),
            pressed: state(40, 55, line(1.0, 255), line(2.0, 255), 6.0),
            open: state(45, 27, line(1.0, 60), line(1.0, 210), 6.0),
            item_spacing: vec2(8.0, 3.0),
            button_padding: vec2(8.0, 2.0),
            window_margin: 6.0,
            menu_margin: 6.0,
            indent: 18.0,
            interact_size: vec2(40.0, 20.0),
            slider_width: 100.0,
            combo_width: 100.0,
            text_edit_width: 280.0,
            icon_width: 14.0,
            icon_spacing: 4.0,
            scroll_bar: 10.0,
        }
    }

    /// Whether the theme is dark, by its panels' colour: egui draws text a
    /// little bolder on dark backgrounds, and the window's title bar
    /// follows.
    pub fn is_dark(&self) -> bool {
        let [r, g, b, _] = self.panel.to_srgba_unmultiplied();
        // Rec. 601's weights, near enough to how light a colour looks.
        299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b) < 128_000
    }
}

impl Default for Fonts {
    /// egui's fonts, at egui's sizes.
    fn default() -> Self {
        Self {
            text: String::new(),
            monospace: String::new(),
            sizes: FontSizes {
                body: 13.0,
                button: 13.0,
                small: 9.0,
                heading: 18.0,
                monospace: 13.0,
            },
        }
    }
}
