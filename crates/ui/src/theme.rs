//! The colours and sizes the views draw with.
//!
//! Views draw from a [`Theme`], never from colours or sizes of their own, so
//! a theme can change how all of Musicable looks. egui's own widgets, such
//! as buttons and menus, follow egui's visuals instead.

use egui::{Color32, Vec2, vec2};

/// How Musicable looks.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    // The colours of mute, solo and arm when they're on, wherever they are,
    // and of the text on them.
    pub mute: Color32,
    pub solo: Color32,
    pub arm: Color32,
    /// Input monitoring's buttons, when they're on.
    pub monitor: Color32,
    /// Built-in devices' switches, when they're on.
    pub switch: Color32,
    pub ink: Color32,
    /// The meters' colours, from quiet to loud to over full scale.
    pub meter_quiet: Color32,
    pub meter_loud: Color32,
    pub meter_over: Color32,
    /// Errors and warnings, such as dropouts.
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
    pub cpu_meter: Vec2,

    // The arrangement's colours.
    /// The tracks' lanes, every other one a little lighter.
    pub lane: Color32,
    pub lane_alternate: Color32,
    /// The lane of the selected track.
    pub lane_selected: Color32,
    /// Below the last track.
    pub empty: Color32,
    /// The grid's lines: at bars, at beats and between.
    pub grid_bar: Color32,
    pub grid_beat: Color32,
    pub grid_step: Color32,
    pub ruler: Color32,
    pub ruler_text: Color32,
    /// The loop brace while looping, and while not.
    pub loop_on: Color32,
    pub loop_off: Color32,
    /// The loop region's shade over the lanes while looping.
    pub loop_shade: Color32,
    pub locator: Color32,
    pub playhead: Color32,
    pub start_marker: Color32,
    pub time_selection: Color32,
    /// Clips take a colour from these, by their track's place.
    pub clips: [Color32; 8],
    /// Laid over a clip's colour under its title bar, where its waveform or
    /// its notes are.
    pub clip_body: Color32,
    pub clip_text: Color32,
    /// The outline of selected clips.
    pub clip_selected: Color32,
    pub waveform: Color32,
    pub notes: Color32,
    pub fade: Color32,
    /// Where clips land while they're dragged.
    pub ghost: Color32,
    /// The lanes of tracks' takes, under the tracks.
    pub take_lane: Color32,
    /// A take's clips where the track doesn't play them. Where it does,
    /// they take the track's colour.
    pub take_clip: Color32,
    /// What the armed tracks have recorded so far, while they record.
    pub recording: Color32,
    /// Automation lanes, under the tracks, and the lines and points on
    /// them: grey where someone overrode the automation by hand, and for a
    /// parameter's own value where it has none.
    pub automation_lane: Color32,
    pub automation: Color32,
    pub automation_off: Color32,
    pub header: Color32,
    pub header_selected: Color32,
    pub header_text: Color32,
    /// Lines between tracks and around panels.
    pub divider: Color32,
    /// The devices panel's boxes, one for each device.
    pub device: Color32,
    /// The waveform of the sample previewing, in the browser.
    pub preview_wave: Color32,

    // The piano roll's colours. Its notes take their track's colour, and
    // its grid lines, start marker and playhead are the arrangement's.
    /// The rows of the white keys and of the black keys, behind the notes.
    pub white_row: Color32,
    pub black_row: Color32,
    /// The keyboard beside them, and the names of its Cs.
    pub white_key: Color32,
    pub black_key: Color32,
    pub key_text: Color32,
    /// Laid over the rows before the clip's start and after its end, where
    /// notes don't play.
    pub outside_clip: Color32,
    /// The rectangle that selects the notes in it, while it's dragged.
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
    pub dock: DockSizes,
}

/// The sizes of the dock the panels are in, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DockSizes {
    /// The height of the bar of tabs over each group of panels.
    pub tab_bar: f32,
    /// The gap between panels side by side or one over another.
    pub gap: f32,
    /// The smallest a panel gets when the user drags its edge, and what
    /// the arrangement keeps at least when the window's too small for the
    /// panels around it.
    pub min_panel: f32,
}

/// The piano roll's sizes, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrowserSizes {
    /// How far in a folder's rows are from its own, and the space for its
    /// arrow.
    pub indent: f32,
    /// The height of the waveform of the sample previewing.
    pub waveform: f32,
}

impl MixerSizes {
    /// The width of what's in a strip.
    pub fn inner(&self) -> f32 {
        self.strip - 2.0 * self.margin
    }
}

impl Default for Theme {
    /// The dark theme, which goes with egui's dark visuals.
    fn default() -> Self {
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
            dock: DockSizes {
                tab_bar: 22.0,
                gap: 1.0,
                min_panel: 96.0,
            },
        }
    }
}

impl Theme {
    /// The colour of clips on the track at `index` in the track list.
    pub fn clip(&self, index: usize) -> Color32 {
        self.clips[index % self.clips.len()]
    }
}
