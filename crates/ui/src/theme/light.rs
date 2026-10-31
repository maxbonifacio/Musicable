//! The light theme: the dark theme's sizes, in light colours.

use egui::{Color32, Stroke};

use super::{Theme, WidgetState, Widgets};

impl Theme {
    /// The light theme, with the dark theme's sizes.
    pub fn light() -> Self {
        let gray = Color32::from_gray;
        let rgb = Color32::from_rgb;
        let alpha = Color32::from_rgba_unmultiplied;
        Self {
            mute: rgb(236, 160, 40),
            solo: rgb(70, 150, 230),
            arm: rgb(228, 70, 60),
            monitor: rgb(80, 180, 150),
            switch: rgb(236, 186, 50),
            ink: gray(24),
            meter_quiet: rgb(70, 175, 95),
            meter_loud: rgb(222, 185, 45),
            meter_over: rgb(222, 60, 50),
            warning: rgb(205, 55, 45),

            lane: gray(222),
            lane_alternate: gray(228),
            lane_selected: rgb(206, 216, 230),
            empty: gray(210),
            bar_shade: alpha(0, 0, 0, 14),
            grid_bar: gray(182),
            grid_beat: gray(202),
            grid_step: gray(214),
            ruler: gray(234),
            ruler_text: gray(70),
            loop_on: rgb(220, 140, 30),
            loop_off: gray(160),
            loop_shade: alpha(220, 140, 30, 20),
            locator: gray(70),
            playhead: gray(20),
            start_marker: rgb(30, 100, 200),
            time_selection: alpha(40, 110, 210, 50),
            clips: [
                rgb(226, 132, 88),
                rgb(214, 184, 84),
                rgb(132, 190, 104),
                rgb(88, 182, 166),
                rgb(100, 156, 222),
                rgb(156, 132, 222),
                rgb(210, 112, 182),
                rgb(176, 176, 176),
            ],
            master: gray(110),
            clip_body: alpha(255, 255, 255, 90),
            clip_text: gray(20),
            clip_selected: gray(20),
            waveform: alpha(20, 20, 20, 170),
            notes: alpha(20, 20, 20, 190),
            fade: alpha(20, 20, 20, 200),
            ghost: alpha(20, 20, 20, 50),
            take_lane: gray(214),
            take_clip: gray(176),
            recording: alpha(228, 70, 60, 110),
            automation_lane: gray(230),
            automation: rgb(214, 60, 50),
            automation_off: gray(140),
            modulation: rgb(20, 150, 95),
            header: gray(234),
            header_selected: rgb(210, 220, 234),
            header_text: gray(25),
            divider: gray(186),
            focus: rgb(40, 110, 210),
            device: gray(248),
            preview_wave: rgb(40, 110, 210),

            white_row: gray(236),
            black_row: gray(224),
            white_key: gray(250),
            black_key: gray(48),
            key_text: gray(90),
            outside_clip: alpha(0, 0, 0, 24),
            area: alpha(40, 110, 210, 40),

            warp_marker: rgb(170, 130, 20),
            warp_marker_selected: rgb(220, 160, 0),
            transient: gray(120),
            pseudo_marker: gray(95),

            widgets: Widgets::light(),
            ..Self::dark()
        }
    }
}

impl Widgets {
    fn light() -> Self {
        let gray = Color32::from_gray;
        let state = |background, fill, outline: Stroke, text: Stroke, corner| WidgetState {
            background: gray(background),
            fill: gray(fill),
            outline: outline.color,
            outline_width: outline.width,
            text: text.color,
            line_width: text.width,
            corner,
            expansion: 0.0,
        };
        let line = |width, value| Stroke::new(width, gray(value));
        Self {
            panel: gray(240),
            window: gray(246),
            window_outline: gray(186),
            shadow: Color32::from_black_alpha(32),
            field: gray(255),
            stripe: gray(230),
            code: gray(226),
            link: Color32::from_rgb(0, 110, 200),
            warning: Color32::from_rgb(200, 90, 0),
            error: Color32::from_rgb(200, 30, 30),
            selection: Color32::from_rgb(150, 200, 245),
            selected_text: Color32::from_rgb(10, 50, 90),
            cursor: Color32::from_rgb(10, 80, 150),
            passive: state(240, 240, line(1.0, 196), line(1.0, 60), 4.0),
            resting: state(222, 222, Stroke::NONE, line(1.0, 30), 6.0),
            hovered: state(210, 210, line(1.0, 140), line(1.5, 0), 6.0),
            pressed: state(190, 186, line(1.0, 60), line(2.0, 0), 6.0),
            open: state(214, 240, line(1.0, 186), line(1.0, 0), 6.0),
            ..Self::dark()
        }
    }
}
