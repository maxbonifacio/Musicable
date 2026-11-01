//! The mouse map: what a mouse gesture does, from where it starts and the
//! modifiers held.
//!
//! The arrangement, the piano roll, the Sample Editor, the mixer and the
//! devices panel look gestures up here instead of deciding for themselves,
//! so the user can make the mouse work as another DAW's does, as a keymap
//! does for keys. [`MouseMap::live`], the map Musicable starts with, follows
//! Ableton Live on Windows, with Cmd standing in for Ctrl on a Mac. A
//! [`MouseMapFile`] holds the user's changes to it, which the Settings
//! window's Mouse page makes, from the actions each place can do
//! ([`Target::actions`]).

mod file;

use std::collections::BTreeMap;

use musicable_commands::Modifiers;

pub use file::{MouseFileError, MouseMapFile, MouseProblem};

/// Each modifier on its own, in the order key chords write them.
const ONE: [Modifiers; 4] = [
    Modifiers::CTRL,
    Modifiers::ALT,
    Modifiers::SHIFT,
    Modifiers::CMD,
];

/// Where a gesture starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    /// A clip's title bar, along its top. A clip too short for a body is
    /// all title bar.
    ClipTitle,
    /// The rest of a clip, under its title bar.
    ClipBody,
    /// Near a clip's start or end.
    ClipEdge,
    /// The handle of an audio clip's fade in or fade out.
    FadeHandle,
    /// A track's lane, where it has no clip.
    Lane,
    /// Under the last track.
    Empty,
    /// The ruler's bar numbers, over the arrangement or the piano roll.
    Ruler,
    /// The strip along the top of the ruler, which has the loop brace and
    /// the locators.
    Strip,
    LoopBrace,
    /// Near the loop brace's start or end.
    LoopEdge,
    Locator,
    /// A track's header, or its strip in the mixer, where the strip has no
    /// control.
    TrackHeader,
    /// A track's bottom edge, in its header, or the master track's top
    /// edge: it's pinned along the bottom.
    TrackEdge,
    /// A take's lane, under its track's while the track shows its takes.
    TakeLane,
    /// The header beside a take's lane, with the take's name.
    TakeHeader,
    /// A point on an automation lane, under its track's lanes.
    AutomationPoint,
    /// Near the line through an automation lane's points, or through a
    /// parameter's own value on a lane without automation.
    AutomationLine,
    /// The rest of an automation lane.
    AutomationLane,
    /// The header beside an automation lane, with its parameter's name.
    AutomationHeader,
    /// A track's solo button, in its header or its mixer strip.
    SoloButton,
    /// A fader, a pan control or a send in the mixer.
    MixerControl,
    /// A built-in device's control, in the devices panel.
    DeviceControl,
    /// The Bypass button in a device's title bar, in the devices panel.
    BypassButton,
    /// The rest of a device's title bar, with its name, in the devices
    /// panel.
    DeviceTitle,
    /// The arrangement's tracks and ruler, and the piano roll's notes and
    /// ruler, for the wheel.
    Timeline,
    /// A note in the piano roll.
    Note,
    /// Near a note's start or end.
    NoteEdge,
    /// The piano roll's rows of keys, where there's no note.
    NoteGrid,
    /// A note while Draw Mode is on.
    DrawNote,
    /// The rows of keys, where there's no note, while Draw Mode is on.
    DrawGrid,
    /// A key of the piano roll's keyboard, beside the rows.
    PianoKey,
    /// The lane of the notes' velocities, under the rows.
    VelocityLane,
    /// The lane under the rows while it shows a controller.
    ControllerLane,
    /// A warp marker in the Sample Editor: its handle, along the top of the
    /// waveform, or its line down the waveform's top half.
    WarpMarker,
    /// A transient of a warped clip's file, in the Sample Editor: near its
    /// tick in the waveform's top half, away from the warp markers, where
    /// its pseudo marker shows.
    Transient,
    /// The top half of the Sample Editor's waveform, where the warp markers
    /// are, away from them and the transients.
    WaveformTop,
    /// The bottom half of the Sample Editor's waveform.
    Waveform,
}

/// What the user does with the mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Gesture {
    Click,
    DoubleClick,
    Drag,
    Wheel,
}

/// What a gesture does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseAction {
    /// Selects just the clip, the track, the note or the device.
    Select,
    /// Adds the clip, the track, the note or the device to the selection,
    /// or takes it out.
    ToggleSelect,
    /// Selects the clip and puts the start marker where it was clicked, so
    /// Split splits it there.
    SelectAndMark,
    /// Moves the selected clips or notes, snapping to the grid, or the
    /// selected devices, to where they're let go.
    Move,
    /// Copies the selected clips or notes, snapping to the grid, or the
    /// selected devices, to where they're let go.
    Copy,
    /// Moves the selected clips without snapping.
    MoveFree,
    /// Moves a clip's start or end, or the selected notes', snapping to the
    /// grid.
    Trim,
    TrimFree,
    /// Makes a fade longer or shorter.
    Fade,
    /// Clears the selection, selects the track, and puts the start marker
    /// there. In the piano roll, clears the notes' selection.
    PlaceMarker,
    /// Selects the time dragged over, on the tracks dragged over: in the
    /// Sample Editor, of the clip's own time, with its warp markers and
    /// transients.
    SelectTime,
    /// Puts the start marker there, and the playhead with it.
    Locate,
    /// Drags up or down to zoom, and sideways to scroll, as Live's ruler
    /// does.
    ScrubZoom,
    /// Draws a new loop region.
    DrawLoop,
    ToggleLoop,
    MoveLoop,
    /// Moves the loop region's start or end.
    ResizeLoop,
    MoveLocator,
    Rename,
    ResizeTrack,
    /// Copies a stretch of a take to its track, replacing what the track
    /// has there: the stretch dragged over, snapping to the grid, or the
    /// take's clip that was clicked.
    Comp,
    /// Copies the stretch dragged over without snapping.
    CompFree,
    /// Adds a point to the automation, snapping to the grid: on the line
    /// where the line was clicked, so nothing changes until it's moved,
    /// and elsewhere at the pointer's height.
    AddPoint,
    DeletePoint,
    /// Moves a point of the automation, snapping to the grid. It stays
    /// between the points either side.
    MovePoint,
    MovePointFree,
    /// Moves the line between two points of the automation up or down,
    /// with the points.
    MoveSegment,
    /// Draws the automation where the pointer goes, a value for each step
    /// of the grid, in place of what was there.
    Draw,
    /// Draws it as the pointer goes, without the grid's steps.
    DrawFree,
    /// Solos the track alone, or unsolos it.
    ExclusiveSolo,
    /// Solos the track along with the tracks soloed already.
    AddSolo,
    /// Moves a control as far as the mouse moves.
    Coarse,
    /// Moves a control a tenth as far, for small changes.
    Fine,
    /// Scrolls the way the wheel turns: up and down, or sideways on a
    /// trackpad.
    Scroll,
    /// Scrolls sideways as the wheel turns.
    ScrollSideways,
    /// Zooms in and out around the pointer.
    Zoom,
    /// Adds a note there, a step of the grid long.
    AddNote,
    DeleteNote,
    /// Adds notes a step of the grid long along the drag, on the keys it
    /// passes over: Live's Draw Mode.
    DrawNotes,
    /// Selects the notes in the rectangle dragged over.
    SelectArea,
    /// Drags up or down to change how hard the note starts, or the
    /// selected notes do, if it's one of them.
    Velocity,
    /// Selects every note on the key.
    SelectKey,
    /// Adds every note on the key to the selection.
    AddKey,
    /// Makes the keys' rows taller or shorter, around the pointer.
    ZoomKeys,
    /// Shows the clip in the piano roll, or an audio clip in the Sample
    /// Editor, and gives it the focus.
    Open,
    /// Draws the controller's curve along the drag, a step of the grid at a
    /// time, at the heights the drag passes.
    DrawController,
    /// Erases the controller's curve along the drag, so what came before
    /// holds there.
    EraseController,
    /// Stretches a warped clip, changing its tempo, so its audio fills it
    /// as it plays: Live's Shift-drag of a clip's edge.
    Stretch,
    /// Adds a warp marker there, snapping to the grid, pinning the audio
    /// that plays there: on a transient, at the transient.
    AddMarker,
    DeleteMarker,
    /// Moves the warp marker, or the selected ones with it, snapping to the
    /// grid: the audio either side stretches. On a transient, adds a marker
    /// there and moves it.
    MoveMarker,
    MoveMarkerFree,
    /// Moves the audio under the warp marker, which stays where it is.
    MoveAudio,
    /// Moves the transient to another moment of the file, between the
    /// transients either side.
    MoveTransient,
    /// Bypasses the device, or brings it back. A deactivated device, it
    /// activates.
    ToggleBypass,
    /// Deactivates the device, taking its latency out too, or activates
    /// it.
    ToggleActive,
    /// Makes the track under the pointer taller or shorter, or the selected
    /// tracks, if it's one of them, as the wheel turns. In the piano roll,
    /// the keys' rows.
    TrackHeight,
    /// Makes every track taller or shorter together, as Live's Alt-drag of
    /// a track's edge does.
    ResizeAllTracks,
    /// Gives the track its default height back.
    ResetHeight,
    /// Zooms to the selection, or with nothing selected to the whole song,
    /// as Live's double click on its ruler does.
    ZoomToFit,
}

impl Target {
    /// Every place a gesture starts, in the order the Mouse page and mouse
    /// map files list them.
    pub const ALL: [Target; 37] = [
        Target::ClipTitle,
        Target::ClipBody,
        Target::ClipEdge,
        Target::FadeHandle,
        Target::Lane,
        Target::Empty,
        Target::TrackHeader,
        Target::TrackEdge,
        Target::Ruler,
        Target::Strip,
        Target::LoopBrace,
        Target::LoopEdge,
        Target::Locator,
        Target::TakeLane,
        Target::TakeHeader,
        Target::AutomationLane,
        Target::AutomationLine,
        Target::AutomationPoint,
        Target::AutomationHeader,
        Target::Note,
        Target::NoteEdge,
        Target::NoteGrid,
        Target::DrawNote,
        Target::DrawGrid,
        Target::PianoKey,
        Target::VelocityLane,
        Target::ControllerLane,
        Target::WarpMarker,
        Target::Transient,
        Target::WaveformTop,
        Target::Waveform,
        Target::SoloButton,
        Target::MixerControl,
        Target::DeviceControl,
        Target::BypassButton,
        Target::DeviceTitle,
        Target::Timeline,
    ];

    /// Its name in mouse map files.
    pub const fn name(self) -> &'static str {
        match self {
            Target::ClipTitle => "clip_title",
            Target::ClipBody => "clip_body",
            Target::ClipEdge => "clip_edge",
            Target::FadeHandle => "fade_handle",
            Target::Lane => "lane",
            Target::Empty => "empty",
            Target::Ruler => "ruler",
            Target::Strip => "strip",
            Target::LoopBrace => "loop_brace",
            Target::LoopEdge => "loop_edge",
            Target::Locator => "locator",
            Target::TrackHeader => "track_header",
            Target::TrackEdge => "track_edge",
            Target::TakeLane => "take_lane",
            Target::TakeHeader => "take_header",
            Target::AutomationPoint => "automation_point",
            Target::AutomationLine => "automation_line",
            Target::AutomationLane => "automation_lane",
            Target::AutomationHeader => "automation_header",
            Target::SoloButton => "solo_button",
            Target::MixerControl => "mixer_control",
            Target::DeviceControl => "device_control",
            Target::BypassButton => "bypass_button",
            Target::DeviceTitle => "device_title",
            Target::Timeline => "timeline",
            Target::Note => "note",
            Target::NoteEdge => "note_edge",
            Target::NoteGrid => "note_grid",
            Target::DrawNote => "draw_note",
            Target::DrawGrid => "draw_grid",
            Target::PianoKey => "piano_key",
            Target::VelocityLane => "velocity_lane",
            Target::ControllerLane => "controller_lane",
            Target::WarpMarker => "warp_marker",
            Target::Transient => "transient",
            Target::WaveformTop => "waveform_top",
            Target::Waveform => "waveform",
        }
    }

    /// What the Mouse page calls it.
    pub const fn title(self) -> &'static str {
        match self {
            Target::ClipTitle => "A clip's title bar",
            Target::ClipBody => "A clip's body",
            Target::ClipEdge => "A clip's start or end",
            Target::FadeHandle => "A fade handle",
            Target::Lane => "A track's lane",
            Target::Empty => "Under the last track",
            Target::Ruler => "The ruler",
            Target::Strip => "The ruler's strip",
            Target::LoopBrace => "The loop brace",
            Target::LoopEdge => "The loop brace's start or end",
            Target::Locator => "A locator",
            Target::TrackHeader => "A track's header or strip",
            Target::TrackEdge => "A track's edge",
            Target::TakeLane => "A take's lane",
            Target::TakeHeader => "A take's header",
            Target::AutomationPoint => "An automation point",
            Target::AutomationLine => "An automation lane's line",
            Target::AutomationLane => "An automation lane",
            Target::AutomationHeader => "An automation lane's header",
            Target::SoloButton => "Solo, in a header or a strip",
            Target::MixerControl => "A fader, pan or send",
            Target::DeviceControl => "A built-in device's control",
            Target::BypassButton => "A device's Bypass button",
            Target::DeviceTitle => "A device's title bar",
            Target::Timeline => "Over the tracks or the notes",
            Target::Note => "A note",
            Target::NoteEdge => "A note's start or end",
            Target::NoteGrid => "The rows, where there's no note",
            Target::DrawNote => "A note, in Draw Mode",
            Target::DrawGrid => "The rows, in Draw Mode",
            Target::PianoKey => "The piano roll's keys",
            Target::VelocityLane => "The velocity lane",
            Target::ControllerLane => "A controller's lane",
            Target::WarpMarker => "A warp marker",
            Target::Transient => "A transient",
            Target::WaveformTop => "The waveform's top half",
            Target::Waveform => "The waveform's bottom half",
        }
    }

    /// The place a mouse map file names, ignoring case.
    pub fn from_name(name: &str) -> Option<Target> {
        Self::ALL
            .into_iter()
            .find(|target| target.name().eq_ignore_ascii_case(name.trim()))
    }

    /// What `gesture` can do here: the actions the arrangement, the piano
    /// roll, the Sample Editor, the mixer and the devices carry out for it,
    /// in the order the Mouse page offers them. None for a gesture that
    /// does nothing here.
    ///
    /// A target they share, such as the ruler, has the actions all of them
    /// carry out.
    pub fn actions(self, gesture: Gesture) -> &'static [MouseAction] {
        use Gesture::{Click, DoubleClick, Drag, Wheel};
        use MouseAction as A;
        use Target as T;
        match (self, gesture) {
            // The arrangement's, where a double click does what a click can.
            (T::ClipTitle | T::ClipBody | T::ClipEdge | T::FadeHandle, Click | DoubleClick) => &[
                A::Select,
                A::ToggleSelect,
                A::SelectAndMark,
                A::Rename,
                A::Open,
            ],
            (T::ClipTitle | T::ClipBody, Drag) => &[A::Move, A::Copy, A::MoveFree, A::SelectTime],
            (T::ClipEdge, Drag) => &[
                A::Trim,
                A::TrimFree,
                A::Stretch,
                A::Move,
                A::Copy,
                A::MoveFree,
            ],
            (T::FadeHandle, Drag) => &[A::Fade, A::Move, A::Copy, A::MoveFree],
            (T::Lane | T::Empty, Click | DoubleClick) => &[A::PlaceMarker, A::Locate],
            (T::Lane | T::Empty, Drag) => &[A::SelectTime],
            (T::TrackHeader, Click | DoubleClick) => &[A::Select, A::ToggleSelect, A::Rename],
            (T::TrackEdge, Drag) => &[A::ResizeTrack, A::ResizeAllTracks],
            (T::TrackEdge, Click | DoubleClick) => &[A::ResetHeight],
            (T::Ruler, Click | DoubleClick) => &[A::Locate, A::ZoomToFit],
            (T::Ruler, Drag) => &[A::ScrubZoom],
            (T::Strip, Click | DoubleClick) => &[A::Locate, A::ToggleLoop],
            (T::Strip, Drag) => &[A::DrawLoop, A::ScrubZoom],
            (T::LoopBrace | T::LoopEdge, Click | DoubleClick) => &[A::ToggleLoop, A::Locate],
            (T::LoopBrace, Drag) => &[A::MoveLoop, A::DrawLoop],
            (T::LoopEdge, Drag) => &[A::ResizeLoop, A::MoveLoop],
            (T::Locator, Click | DoubleClick) => &[A::Locate, A::Rename],
            (T::Locator, Drag) => &[A::MoveLocator],
            (T::TakeLane, Click | DoubleClick) => &[A::Comp],
            (T::TakeLane, Drag) => &[A::Comp, A::CompFree],
            (T::TakeHeader, Click | DoubleClick) => &[A::Rename],
            (T::AutomationLane, Click | DoubleClick) => &[A::PlaceMarker, A::AddPoint, A::Locate],
            (T::AutomationLane, Drag) => &[A::Draw, A::DrawFree, A::SelectTime],
            (T::AutomationLine, Click | DoubleClick) => &[A::AddPoint, A::PlaceMarker],
            (T::AutomationLine, Drag) => &[A::MoveSegment, A::Draw, A::DrawFree],
            (T::AutomationPoint, Click | DoubleClick) => &[A::DeletePoint, A::PlaceMarker],
            (T::AutomationPoint, Drag) => &[A::MovePoint, A::MovePointFree, A::Draw, A::DrawFree],
            (T::AutomationHeader, Click | DoubleClick) => &[A::Select],
            // The piano roll's.
            (T::Note | T::NoteEdge | T::DrawNote, Click | DoubleClick) => {
                &[A::Select, A::ToggleSelect, A::DeleteNote]
            }
            (T::Note | T::DrawNote, Drag) => &[A::Move, A::Copy, A::Velocity],
            (T::NoteEdge, Drag) => &[A::Trim, A::TrimFree, A::Move, A::Copy, A::Velocity],
            (T::NoteGrid | T::DrawGrid, Click | DoubleClick) => &[A::PlaceMarker, A::AddNote],
            (T::NoteGrid | T::DrawGrid, Drag) => &[A::SelectArea, A::DrawNotes],
            (T::PianoKey, Click | DoubleClick) => &[A::SelectKey, A::AddKey],
            (T::PianoKey, Wheel) => &[
                A::Scroll,
                A::ScrollSideways,
                A::Zoom,
                A::ZoomKeys,
                A::TrackHeight,
            ],
            (T::VelocityLane, Drag) => &[A::Velocity],
            (T::ControllerLane, Click | Drag) => &[A::DrawController, A::EraseController],
            // The Sample Editor's. Its rulers are the ruler's, and its gain
            // lane is an automation lane.
            (T::WarpMarker, Click | DoubleClick) => {
                &[A::Select, A::ToggleSelect, A::DeleteMarker, A::PlaceMarker]
            }
            (T::WarpMarker, Drag) => &[A::MoveMarker, A::MoveMarkerFree, A::MoveAudio],
            (T::Transient, Click | DoubleClick) => &[A::AddMarker, A::PlaceMarker],
            (T::Transient, Drag) => &[
                A::MoveMarker,
                A::MoveMarkerFree,
                A::MoveTransient,
                A::SelectTime,
            ],
            (T::WaveformTop, Click | DoubleClick) => &[A::AddMarker, A::PlaceMarker],
            (T::Waveform, Click | DoubleClick) => &[A::PlaceMarker, A::AddMarker],
            (T::WaveformTop | T::Waveform, Drag) => &[A::SelectTime],
            // The mixer's and the devices'.
            (T::SoloButton, Click) => &[A::ExclusiveSolo, A::AddSolo],
            (T::MixerControl | T::DeviceControl, Drag) => &[A::Coarse, A::Fine],
            (T::BypassButton, Click) => &[A::ToggleBypass, A::ToggleActive],
            (T::DeviceTitle, Click) => &[A::Select, A::ToggleSelect],
            (T::DeviceTitle, Drag) => &[A::Move, A::Copy],
            (T::Timeline, Wheel) => &[A::Scroll, A::ScrollSideways, A::Zoom, A::TrackHeight],
            _ => &[],
        }
    }

    /// The gestures that can do something here, in order.
    pub fn gestures(self) -> impl Iterator<Item = Gesture> {
        Gesture::ALL
            .into_iter()
            .filter(move |&gesture| !self.actions(gesture).is_empty())
    }
}

impl Gesture {
    /// Every gesture, in the order the Mouse page and mouse map files list
    /// them.
    pub const ALL: [Gesture; 4] = [
        Gesture::Click,
        Gesture::DoubleClick,
        Gesture::Drag,
        Gesture::Wheel,
    ];

    /// Its name in mouse map files and on the Mouse page.
    pub const fn name(self) -> &'static str {
        match self {
            Gesture::Click => "Click",
            Gesture::DoubleClick => "Double-click",
            Gesture::Drag => "Drag",
            Gesture::Wheel => "Wheel",
        }
    }

    /// The gesture a mouse map file names, ignoring case, spaces and
    /// hyphens, so `DoubleClick` and `double click` are double clicks too.
    pub fn from_name(name: &str) -> Option<Gesture> {
        let plain = |text: &str| -> String {
            (text.chars())
                .filter(|c| !matches!(c, '-' | ' ' | '_'))
                .flat_map(char::to_lowercase)
                .collect()
        };
        let name = plain(name);
        Self::ALL
            .into_iter()
            .find(|gesture| plain(gesture.name()) == name)
    }

    /// The gesture with `mods` held, as mouse map files and the Mouse page
    /// write it: their names and its, joined by `+` as in key chords, such
    /// as `Ctrl+Drag`.
    pub fn with(self, mods: Modifiers) -> String {
        match mods {
            Modifiers::NONE => self.name().to_owned(),
            mods => format!("{mods}+{}", self.name()),
        }
    }

    /// Reads a gesture with modifiers, written as [`with`](Self::with)
    /// writes it. The error says what's wrong.
    pub fn parse(text: &str) -> Result<(Gesture, Modifiers), String> {
        let (mods, name) = Modifiers::split_off(text).map_err(|error| error.to_string())?;
        let gesture = Self::from_name(name).ok_or_else(|| match name {
            "" => "there's no gesture after the modifiers".to_owned(),
            name => format!(
                "there's no gesture called \"{name}\" (they are Click, Double-click, Drag and \
                 Wheel)"
            ),
        })?;
        Ok((gesture, mods))
    }
}

impl MouseAction {
    /// Every action, which mouse map files name.
    pub const ALL: [MouseAction; 60] = [
        MouseAction::Select,
        MouseAction::ToggleSelect,
        MouseAction::SelectAndMark,
        MouseAction::Move,
        MouseAction::Copy,
        MouseAction::MoveFree,
        MouseAction::Trim,
        MouseAction::TrimFree,
        MouseAction::Fade,
        MouseAction::PlaceMarker,
        MouseAction::SelectTime,
        MouseAction::Locate,
        MouseAction::ScrubZoom,
        MouseAction::DrawLoop,
        MouseAction::ToggleLoop,
        MouseAction::MoveLoop,
        MouseAction::ResizeLoop,
        MouseAction::MoveLocator,
        MouseAction::Rename,
        MouseAction::ResizeTrack,
        MouseAction::Comp,
        MouseAction::CompFree,
        MouseAction::AddPoint,
        MouseAction::DeletePoint,
        MouseAction::MovePoint,
        MouseAction::MovePointFree,
        MouseAction::MoveSegment,
        MouseAction::Draw,
        MouseAction::DrawFree,
        MouseAction::ExclusiveSolo,
        MouseAction::AddSolo,
        MouseAction::Coarse,
        MouseAction::Fine,
        MouseAction::Scroll,
        MouseAction::ScrollSideways,
        MouseAction::Zoom,
        MouseAction::AddNote,
        MouseAction::DeleteNote,
        MouseAction::DrawNotes,
        MouseAction::SelectArea,
        MouseAction::Velocity,
        MouseAction::SelectKey,
        MouseAction::AddKey,
        MouseAction::ZoomKeys,
        MouseAction::Open,
        MouseAction::DrawController,
        MouseAction::EraseController,
        MouseAction::Stretch,
        MouseAction::AddMarker,
        MouseAction::DeleteMarker,
        MouseAction::MoveMarker,
        MouseAction::MoveMarkerFree,
        MouseAction::MoveAudio,
        MouseAction::MoveTransient,
        MouseAction::ToggleBypass,
        MouseAction::ToggleActive,
        MouseAction::TrackHeight,
        MouseAction::ResizeAllTracks,
        MouseAction::ResetHeight,
        MouseAction::ZoomToFit,
    ];

    /// Its name in mouse map files.
    pub const fn name(self) -> &'static str {
        match self {
            MouseAction::Select => "select",
            MouseAction::ToggleSelect => "toggle_select",
            MouseAction::SelectAndMark => "select_and_mark",
            MouseAction::Move => "move",
            MouseAction::Copy => "copy",
            MouseAction::MoveFree => "move_free",
            MouseAction::Trim => "trim",
            MouseAction::TrimFree => "trim_free",
            MouseAction::Fade => "fade",
            MouseAction::PlaceMarker => "place_marker",
            MouseAction::SelectTime => "select_time",
            MouseAction::Locate => "locate",
            MouseAction::ScrubZoom => "scrub_zoom",
            MouseAction::DrawLoop => "draw_loop",
            MouseAction::ToggleLoop => "toggle_loop",
            MouseAction::MoveLoop => "move_loop",
            MouseAction::ResizeLoop => "resize_loop",
            MouseAction::MoveLocator => "move_locator",
            MouseAction::Rename => "rename",
            MouseAction::ResizeTrack => "resize_track",
            MouseAction::Comp => "comp",
            MouseAction::CompFree => "comp_free",
            MouseAction::AddPoint => "add_point",
            MouseAction::DeletePoint => "delete_point",
            MouseAction::MovePoint => "move_point",
            MouseAction::MovePointFree => "move_point_free",
            MouseAction::MoveSegment => "move_segment",
            MouseAction::Draw => "draw",
            MouseAction::DrawFree => "draw_free",
            MouseAction::ExclusiveSolo => "exclusive_solo",
            MouseAction::AddSolo => "add_solo",
            MouseAction::Coarse => "coarse",
            MouseAction::Fine => "fine",
            MouseAction::Scroll => "scroll",
            MouseAction::ScrollSideways => "scroll_sideways",
            MouseAction::Zoom => "zoom",
            MouseAction::AddNote => "add_note",
            MouseAction::DeleteNote => "delete_note",
            MouseAction::DrawNotes => "draw_notes",
            MouseAction::SelectArea => "select_area",
            MouseAction::Velocity => "velocity",
            MouseAction::SelectKey => "select_key",
            MouseAction::AddKey => "add_key",
            MouseAction::ZoomKeys => "zoom_keys",
            MouseAction::Open => "open",
            MouseAction::DrawController => "draw_controller",
            MouseAction::EraseController => "erase_controller",
            MouseAction::Stretch => "stretch",
            MouseAction::AddMarker => "add_marker",
            MouseAction::DeleteMarker => "delete_marker",
            MouseAction::MoveMarker => "move_marker",
            MouseAction::MoveMarkerFree => "move_marker_free",
            MouseAction::MoveAudio => "move_audio",
            MouseAction::MoveTransient => "move_transient",
            MouseAction::ToggleBypass => "toggle_bypass",
            MouseAction::ToggleActive => "toggle_active",
            MouseAction::TrackHeight => "track_height",
            MouseAction::ResizeAllTracks => "resize_all_tracks",
            MouseAction::ResetHeight => "reset_height",
            MouseAction::ZoomToFit => "zoom_to_fit",
        }
    }

    /// What the Mouse page calls it.
    pub const fn title(self) -> &'static str {
        match self {
            MouseAction::Select => "Select",
            MouseAction::ToggleSelect => "Add to the selection or take out",
            MouseAction::SelectAndMark => "Select, and set the start marker",
            MouseAction::Move => "Move",
            MouseAction::Copy => "Copy",
            MouseAction::MoveFree => "Move off the grid",
            MouseAction::Trim => "Trim",
            MouseAction::TrimFree => "Trim off the grid",
            MouseAction::Fade => "Lengthen or shorten the fade",
            MouseAction::PlaceMarker => "Put the start marker there",
            MouseAction::SelectTime => "Select time",
            MouseAction::Locate => "Put the playhead there",
            MouseAction::ScrubZoom => "Zoom and scroll",
            MouseAction::DrawLoop => "Draw a new loop",
            MouseAction::ToggleLoop => "Turn looping on or off",
            MouseAction::MoveLoop => "Move the loop",
            MouseAction::ResizeLoop => "Move the loop's start or end",
            MouseAction::MoveLocator => "Move the locator",
            MouseAction::Rename => "Rename",
            MouseAction::ResizeTrack => "Make the track taller or shorter",
            MouseAction::Comp => "Comp, on the grid",
            MouseAction::CompFree => "Comp off the grid",
            MouseAction::AddPoint => "Add a point",
            MouseAction::DeletePoint => "Delete the point",
            MouseAction::MovePoint => "Move the point",
            MouseAction::MovePointFree => "Move the point off the grid",
            MouseAction::MoveSegment => "Move the line up or down",
            MouseAction::Draw => "Draw, on the grid",
            MouseAction::DrawFree => "Draw off the grid",
            MouseAction::ExclusiveSolo => "Solo the track alone",
            MouseAction::AddSolo => "Solo the track with the others",
            MouseAction::Coarse => "Move it as far as the mouse",
            MouseAction::Fine => "Move it a tenth as far",
            MouseAction::Scroll => "Scroll",
            MouseAction::ScrollSideways => "Scroll sideways",
            MouseAction::Zoom => "Zoom",
            MouseAction::AddNote => "Add a note",
            MouseAction::DeleteNote => "Delete the note",
            MouseAction::DrawNotes => "Draw notes",
            MouseAction::SelectArea => "Select the notes in a rectangle",
            MouseAction::Velocity => "Change the velocity",
            MouseAction::SelectKey => "Select the key's notes",
            MouseAction::AddKey => "Add the key's notes to the selection",
            MouseAction::ZoomKeys => "Make the keys taller or shorter",
            MouseAction::Open => "Open in the piano roll or the Sample Editor",
            MouseAction::DrawController => "Draw the controller",
            MouseAction::EraseController => "Erase the controller",
            MouseAction::Stretch => "Stretch, changing a warped clip's tempo",
            MouseAction::AddMarker => "Add a warp marker",
            MouseAction::DeleteMarker => "Delete the warp marker",
            MouseAction::MoveMarker => "Move the warp marker",
            MouseAction::MoveMarkerFree => "Move the warp marker off the grid",
            MouseAction::MoveAudio => "Move the audio under the warp marker",
            MouseAction::MoveTransient => "Move the transient",
            MouseAction::ToggleBypass => "Bypass the device or bring it back",
            MouseAction::ToggleActive => "Deactivate the device or activate it",
            MouseAction::TrackHeight => "Make the track or the keys taller or shorter",
            MouseAction::ResizeAllTracks => "Make every track taller or shorter",
            MouseAction::ResetHeight => "Give the track its default height",
            MouseAction::ZoomToFit => "Zoom to the selection, or to the whole song",
        }
    }

    /// The action a mouse map file names, ignoring case.
    pub fn from_name(name: &str) -> Option<MouseAction> {
        Self::ALL
            .into_iter()
            .find(|action| action.name().eq_ignore_ascii_case(name.trim()))
    }

    /// The drag it's a version of, on the grid and moving rather than
    /// copying: Move for Copy and Move off the grid, Trim for Trim off the
    /// grid, and so on, and itself for the rest.
    pub const fn plain(self) -> MouseAction {
        use MouseAction as A;
        match self {
            A::Copy | A::MoveFree => A::Move,
            A::TrimFree => A::Trim,
            A::CompFree => A::Comp,
            A::MovePointFree => A::MovePoint,
            A::DrawFree => A::Draw,
            A::MoveMarkerFree => A::MoveMarker,
            action => action,
        }
    }

    /// The drags it switches between part of the way through, as modifiers
    /// are pressed or let go ([`MouseMap::during`]): its versions
    /// ([`plain`](Self::plain)), and for a clip's edge trimming and
    /// stretching, as Shift switches them. It's the plain drag for the
    /// rest, and Trim for those.
    pub const fn family(self) -> MouseAction {
        match self {
            MouseAction::Stretch => MouseAction::Trim,
            action => action.plain(),
        }
    }

    /// Whether it's the version of a drag that keeps off the grid.
    pub const fn off_grid(self) -> bool {
        use MouseAction as A;
        matches!(
            self,
            A::MoveFree
                | A::TrimFree
                | A::CompFree
                | A::MovePointFree
                | A::DrawFree
                | A::MoveMarkerFree
        )
    }

    /// Whether it's a drag that puts what it moves, trims, draws or selects
    /// on the grid while Snap to Grid is on, so modifiers can keep it off.
    /// Drawing notes and a controller's curve, a step of the grid at a
    /// time, aren't among them.
    pub const fn snaps(self) -> bool {
        use MouseAction as A;
        matches!(
            self.plain(),
            A::Move
                | A::Trim
                | A::Stretch
                | A::SelectTime
                | A::DrawLoop
                | A::MoveLoop
                | A::ResizeLoop
                | A::MoveLocator
                | A::Comp
                | A::MovePoint
                | A::Draw
                | A::MoveMarker
        )
    }
}

/// Every set of modifiers, in the order the Mouse page and mouse map files
/// list gestures with them: none, then fewer before more, and Ctrl, Alt,
/// Shift and Cmd in that order, as key chords write them.
pub fn modifier_sets() -> Vec<Modifiers> {
    let mut sets: Vec<Modifiers> = (0..16u8)
        .map(|bits| {
            (ONE.iter().enumerate())
                .filter(|&(i, _)| bits & (1 << i) != 0)
                .fold(Modifiers::NONE, |mods, (_, &one)| mods | one)
        })
        .collect();
    sets.sort_by_key(|&mods| rank(mods));
    sets
}

/// Where gestures with `mods` held go in [`modifier_sets`]' order.
fn rank(mods: Modifiers) -> (usize, [bool; 4]) {
    let held = [mods.ctrl, mods.alt, mods.shift, mods.cmd];
    let count = held.iter().filter(|&&held| held).count();
    (count, held.map(|held| !held))
}

/// Gestures and what they do. See the [module docs](self).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MouseMap {
    bindings: BTreeMap<(Target, Gesture, Modifiers), MouseAction>,
}

impl MouseMap {
    /// A mouse map where no gesture does anything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `gesture` on `target` with exactly `mods` held do `action`, and
    /// returns what it did before.
    pub fn bind(
        &mut self,
        target: Target,
        gesture: Gesture,
        mods: Modifiers,
        action: MouseAction,
    ) -> Option<MouseAction> {
        self.bindings.insert((target, gesture, mods), action)
    }

    /// Makes `gesture` on `target` with exactly `mods` held do nothing of
    /// its own, and returns what it did.
    pub fn unbind(
        &mut self,
        target: Target,
        gesture: Gesture,
        mods: Modifiers,
    ) -> Option<MouseAction> {
        self.bindings.remove(&(target, gesture, mods))
    }

    /// What `gesture` on `target` does with `mods` held: what's bound to
    /// those modifiers, or else what's bound to none, so a modifier that
    /// means nothing there is ignored.
    pub fn action(&self, target: Target, gesture: Gesture, mods: Modifiers) -> Option<MouseAction> {
        let action = |mods| self.bindings.get(&(target, gesture, mods)).copied();
        action(mods).or_else(|| action(Modifiers::NONE))
    }

    /// What's bound to `gesture` on `target` with exactly `mods` held,
    /// without falling back to what's bound to none.
    pub fn bound(&self, target: Target, gesture: Gesture, mods: Modifiers) -> Option<MouseAction> {
        self.bindings.get(&(target, gesture, mods)).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (Target, Gesture, Modifiers, MouseAction)> + '_ {
        self.bindings
            .iter()
            .map(|(&(target, gesture, mods), &action)| (target, gesture, mods, action))
    }

    /// The gestures bound on `target`, with their modifiers and actions, by
    /// gesture and then in [`modifier_sets`]' order.
    pub fn on(&self, target: Target) -> Vec<(Gesture, Modifiers, MouseAction)> {
        let mut bound: Vec<_> = (self.iter())
            .filter(|&(on, ..)| on == target)
            .map(|(_, gesture, mods, action)| (gesture, mods, action))
            .collect();
        bound.sort_by_key(|&(gesture, mods, _)| (gesture, rank(mods)));
        bound
    }

    /// What a drag that started on `target` as `action` does while `mods`
    /// are held, part of the way through it, and whether it keeps off the
    /// grid.
    ///
    /// The modifiers count as if the drag started with them held: when
    /// they're bound to a drag of the same family there
    /// ([`MouseAction::family`]), such as copying rather than moving,
    /// trimming off the grid, or stretching a clip's edge rather than
    /// trimming it, the drag does that from then on, and letting go of them
    /// goes back to what it does without them. Modifiers held together that
    /// aren't bound together there count one by one, so Ctrl and Alt copy
    /// clips off the grid, and those bound to another drag, such as Alt on
    /// a note, which changes its velocity, change nothing.
    ///
    /// A drag that snaps with no version off the grid there, such as moving
    /// notes, whose Alt-drag changes their velocity, or the loop, keeps off
    /// the grid while the modifiers that move clips off the grid are held,
    /// unless they give a version of it there: Alt in Live's map, which
    /// takes any drag off the grid while it's held.
    pub fn during(
        &self,
        target: Target,
        action: MouseAction,
        mods: Modifiers,
    ) -> (MouseAction, bool) {
        let family = action.family();
        let drags = |on: Target| {
            (self.iter()).filter(move |&(at, gesture, ..)| at == on && gesture == Gesture::Drag)
        };
        // The drag of the family `mods` give there, if they give one.
        let version = |mods| {
            (self.bound(target, Gesture::Drag, mods)).filter(|bound| bound.family() == family)
        };
        let held = |some: Modifiers| (some | mods) == mods;
        let given: Vec<MouseAction> = match version(mods) {
            Some(version) => vec![version],
            None => (ONE.into_iter().filter(|&one| held(one)))
                .filter_map(version)
                .collect(),
        };
        // Copying or stretching over keeping off the grid, so Ctrl and Alt
        // copy off the grid, and Shift and Alt stretch off it.
        let action = (given.iter().copied())
            .find(|&version| version != family && !version.off_grid())
            .or_else(|| (given.iter().copied()).find(|version| version.off_grid()))
            .or_else(|| given.first().copied())
            .or_else(|| version(Modifiers::NONE))
            .unwrap_or(family);
        let plain = action.plain();
        let off_grid_there =
            drags(target).any(|(.., bound)| bound.plain() == plain && bound.off_grid());
        let off_grid_as_clips = drags(Target::ClipTitle).any(|(_, _, grid, bound)| {
            bound == MouseAction::MoveFree
                && grid != Modifiers::NONE
                && held(grid)
                && version(grid).is_none_or(|version| version == plain || version.off_grid())
        });
        let free = given.iter().any(|version| version.off_grid())
            || (action.snaps() && !off_grid_there && off_grid_as_clips);
        (action, free)
    }

    /// Ableton Live's mouse.
    pub fn live() -> Self {
        use Gesture::{Click, DoubleClick, Drag, Wheel};
        use MouseAction as A;
        use Target as T;
        let none = Modifiers::NONE;
        let shift = Modifiers::SHIFT;
        let alt = Modifiers::ALT;
        // Ctrl, and Cmd on a Mac.
        let ctrl = [Modifiers::CTRL, Modifiers::CMD];
        let mut map = Self::new();
        let mut bind = |target, gesture, mods: &[Modifiers], action| {
            for &mods in mods {
                map.bind(target, gesture, mods, action);
            }
        };
        let toggle = [Modifiers::CTRL, Modifiers::CMD, Modifiers::SHIFT];
        bind(T::ClipTitle, Click, &[none], A::Select);
        bind(T::ClipTitle, Click, &toggle, A::ToggleSelect);
        bind(T::ClipTitle, DoubleClick, &[none], A::Rename);
        bind(T::ClipTitle, Drag, &[none, shift], A::Move);
        bind(T::ClipTitle, Drag, &ctrl, A::Copy);
        bind(T::ClipTitle, Drag, &[alt], A::MoveFree);
        bind(T::ClipBody, Click, &[none], A::SelectAndMark);
        bind(T::ClipBody, Click, &toggle, A::ToggleSelect);
        bind(T::ClipBody, DoubleClick, &[none], A::Open);
        bind(T::ClipBody, Drag, &[none], A::SelectTime);
        bind(T::ClipEdge, Drag, &[none], A::Trim);
        bind(T::ClipEdge, Drag, &[alt], A::TrimFree);
        bind(T::ClipEdge, Drag, &[shift], A::Stretch);
        bind(T::FadeHandle, Drag, &[none], A::Fade);
        bind(T::Lane, Click, &[none], A::PlaceMarker);
        bind(T::Lane, Drag, &[none], A::SelectTime);
        bind(T::Empty, Click, &[none], A::PlaceMarker);
        bind(T::Ruler, Click, &[none], A::Locate);
        bind(T::Ruler, DoubleClick, &[none], A::ZoomToFit);
        bind(T::Ruler, Drag, &[none], A::ScrubZoom);
        bind(T::Strip, Click, &[none], A::Locate);
        bind(T::Strip, Drag, &[none], A::DrawLoop);
        bind(T::LoopBrace, Click, &[none], A::ToggleLoop);
        bind(T::LoopBrace, Drag, &[none], A::MoveLoop);
        bind(T::LoopEdge, Drag, &[none], A::ResizeLoop);
        bind(T::Locator, Click, &[none], A::Locate);
        bind(T::Locator, DoubleClick, &[none], A::Rename);
        bind(T::Locator, Drag, &[none], A::MoveLocator);
        bind(T::TrackHeader, Click, &[none], A::Select);
        bind(T::TrackHeader, Click, &toggle, A::ToggleSelect);
        bind(T::TrackHeader, DoubleClick, &[none], A::Rename);
        bind(T::TrackEdge, Drag, &[none], A::ResizeTrack);
        bind(T::TrackEdge, Drag, &[alt], A::ResizeAllTracks);
        bind(T::TrackEdge, DoubleClick, &[none], A::ResetHeight);
        bind(T::TakeLane, Click, &[none], A::Comp);
        bind(T::TakeLane, Drag, &[none], A::Comp);
        bind(T::TakeLane, Drag, &[alt], A::CompFree);
        bind(T::TakeHeader, DoubleClick, &[none], A::Rename);
        // Live's breakpoint editing, and drawing as its Draw Mode does.
        bind(T::AutomationPoint, Click, &[none], A::DeletePoint);
        bind(T::AutomationPoint, Drag, &[none], A::MovePoint);
        bind(T::AutomationPoint, Drag, &[alt], A::MovePointFree);
        bind(T::AutomationLine, Click, &[none], A::AddPoint);
        bind(T::AutomationLine, Drag, &[none], A::MoveSegment);
        bind(T::AutomationLane, Click, &[none], A::PlaceMarker);
        bind(T::AutomationLane, DoubleClick, &[none], A::AddPoint);
        bind(T::AutomationLane, Drag, &[none], A::Draw);
        bind(T::AutomationLane, Drag, &[alt], A::DrawFree);
        bind(T::AutomationHeader, Click, &[none], A::Select);
        bind(T::SoloButton, Click, &[none], A::ExclusiveSolo);
        bind(T::SoloButton, Click, &ctrl, A::AddSolo);
        bind(T::MixerControl, Drag, &[none], A::Coarse);
        bind(T::MixerControl, Drag, &ctrl, A::Fine);
        bind(T::DeviceControl, Drag, &[none], A::Coarse);
        bind(T::DeviceControl, Drag, &ctrl, A::Fine);
        // Live's power button bypasses, and Alt-click deactivates, as in
        // Cubase. In Live it switches the devices beside it in a rack.
        bind(T::BypassButton, Click, &[none], A::ToggleBypass);
        bind(T::BypassButton, Click, &[alt], A::ToggleActive);
        // Live's device title bars select as its clips do, and drag along
        // the chain or onto tracks. Ctrl-dragging copies, as for clips.
        bind(T::DeviceTitle, Click, &[none], A::Select);
        bind(T::DeviceTitle, Click, &toggle, A::ToggleSelect);
        bind(T::DeviceTitle, Drag, &[none], A::Move);
        bind(T::DeviceTitle, Drag, &ctrl, A::Copy);
        bind(T::Timeline, Wheel, &[none], A::Scroll);
        bind(T::Timeline, Wheel, &[shift], A::ScrollSideways);
        // Alt and the wheel make tracks taller or shorter, as in Live, and
        // the piano roll's keys.
        bind(T::Timeline, Wheel, &ctrl, A::Zoom);
        bind(T::Timeline, Wheel, &[alt], A::TrackHeight);
        // The MIDI note editor's. Live bypasses the grid with Alt while a
        // drag goes, but Alt-dragging a note changes its velocity, so here
        // Snap to Grid turns the grid off.
        bind(T::Note, Click, &[none], A::Select);
        bind(T::Note, Click, &toggle, A::ToggleSelect);
        bind(T::Note, DoubleClick, &[none], A::DeleteNote);
        bind(T::Note, Drag, &[none, shift], A::Move);
        bind(T::Note, Drag, &ctrl, A::Copy);
        bind(T::Note, Drag, &[alt], A::Velocity);
        bind(T::NoteEdge, Drag, &[none], A::Trim);
        bind(T::NoteEdge, Drag, &[alt], A::TrimFree);
        bind(T::NoteGrid, Click, &[none], A::PlaceMarker);
        bind(T::NoteGrid, DoubleClick, &[none], A::AddNote);
        bind(T::NoteGrid, Drag, &[none], A::SelectArea);
        bind(T::DrawNote, Click, &[none], A::DeleteNote);
        bind(T::DrawNote, Drag, &[none, shift], A::Move);
        bind(T::DrawNote, Drag, &ctrl, A::Copy);
        bind(T::DrawNote, Drag, &[alt], A::Velocity);
        bind(T::DrawGrid, Click, &[none], A::AddNote);
        bind(T::DrawGrid, Drag, &[none], A::DrawNotes);
        bind(T::PianoKey, Click, &[none], A::SelectKey);
        bind(T::PianoKey, Click, &toggle, A::AddKey);
        bind(T::PianoKey, Wheel, &[none], A::Scroll);
        bind(T::PianoKey, Wheel, &[ctrl[0], ctrl[1], alt], A::ZoomKeys);
        bind(T::VelocityLane, Drag, &[none], A::Velocity);
        for gesture in [Click, Drag] {
            bind(T::ControllerLane, gesture, &[none], A::DrawController);
            bind(T::ControllerLane, gesture, &[alt], A::EraseController);
        }
        // The Sample Editor's: Live's warp markers.
        bind(T::WarpMarker, Click, &[none], A::Select);
        bind(T::WarpMarker, Click, &toggle, A::ToggleSelect);
        bind(T::WarpMarker, DoubleClick, &[none], A::DeleteMarker);
        bind(T::WarpMarker, Drag, &[none], A::MoveMarker);
        bind(T::WarpMarker, Drag, &[alt], A::MoveMarkerFree);
        bind(T::WarpMarker, Drag, &[shift], A::MoveAudio);
        // Its pseudo markers, on the transients.
        bind(T::Transient, Click, &[none], A::PlaceMarker);
        bind(T::Transient, DoubleClick, &[none], A::AddMarker);
        bind(T::Transient, Drag, &[none], A::MoveMarker);
        bind(T::Transient, Drag, &[alt], A::MoveMarkerFree);
        bind(T::Transient, Drag, &[shift], A::MoveTransient);
        for target in [T::WaveformTop, T::Waveform] {
            bind(target, Click, &[none], A::PlaceMarker);
            bind(target, Drag, &[none], A::SelectTime);
        }
        bind(T::WaveformTop, DoubleClick, &[none], A::AddMarker);
        map
    }
}

/// The modifiers egui reports, as keymaps and mouse maps name them. egui's
/// "command" without Cmd is Ctrl, as it is off a Mac.
pub(crate) fn modifiers(modifiers: egui::Modifiers) -> Modifiers {
    Modifiers {
        ctrl: modifiers.ctrl || (modifiers.command && !modifiers.mac_cmd),
        alt: modifiers.alt,
        shift: modifiers.shift,
        cmd: modifiers.mac_cmd,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn modifiers_that_mean_nothing_there_are_ignored() {
        let map = MouseMap::live();
        let drag = |mods| map.action(Target::ClipTitle, Gesture::Drag, mods);
        assert_eq!(drag(Modifiers::NONE), Some(MouseAction::Move));
        assert_eq!(drag(Modifiers::CTRL), Some(MouseAction::Copy));
        assert_eq!(drag(Modifiers::CMD), Some(MouseAction::Copy));
        assert_eq!(drag(Modifiers::ALT), Some(MouseAction::MoveFree));
        // Nothing's bound to Ctrl+Alt, so it drags as nothing held does.
        assert_eq!(
            drag(Modifiers::CTRL | Modifiers::ALT),
            Some(MouseAction::Move)
        );
        assert_eq!(
            map.action(Target::Lane, Gesture::Wheel, Modifiers::NONE),
            None
        );
        // egui's Ctrl, Cmd, and "command" off a Mac.
        assert_eq!(modifiers(egui::Modifiers::CTRL), Modifiers::CTRL);
        assert_eq!(modifiers(egui::Modifiers::MAC_CMD), Modifiers::CMD);
        assert_eq!(modifiers(egui::Modifiers::COMMAND), Modifiers::CTRL);
    }

    #[test]
    fn names_read_back_as_they_are_written() {
        for target in Target::ALL {
            assert_eq!(Target::from_name(target.name()), Some(target));
        }
        let names: BTreeSet<_> = Target::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names.len(), Target::ALL.len());
        for action in MouseAction::ALL {
            assert_eq!(MouseAction::from_name(action.name()), Some(action));
        }
        let names: BTreeSet<_> = MouseAction::ALL.iter().map(|a| a.name()).collect();
        assert_eq!(names.len(), MouseAction::ALL.len());
        for gesture in Gesture::ALL {
            assert_eq!(Gesture::from_name(gesture.name()), Some(gesture));
        }
        assert_eq!(
            Gesture::from_name("double click"),
            Some(Gesture::DoubleClick)
        );
        assert_eq!(Target::from_name("Clip_Title"), Some(Target::ClipTitle));
        let mods = Modifiers::CTRL | Modifiers::SHIFT;
        assert_eq!(Gesture::DoubleClick.with(mods), "Ctrl+Shift+Double-click");
        assert_eq!(
            Gesture::parse("shift+ctrl+DoubleClick"),
            Ok((Gesture::DoubleClick, mods))
        );
        assert_eq!(
            Gesture::parse("Ctrl+"),
            Err("there's no gesture after the modifiers".to_owned())
        );
    }

    #[test]
    fn each_place_offers_what_live_does_there() {
        let live = MouseMap::live();
        for (target, gesture, mods, action) in live.iter() {
            assert!(Target::ALL.contains(&target), "{target:?}");
            let can = target.actions(gesture);
            assert!(
                can.contains(&action),
                "{target:?} {mods:?}+{gesture:?}: {action:?}"
            );
        }
        // Every place can do something, and every action is listed.
        for target in Target::ALL {
            assert!(target.gestures().next().is_some(), "{target:?}");
            for gesture in Gesture::ALL {
                for action in target.actions(gesture) {
                    assert!(MouseAction::ALL.contains(action), "{action:?}");
                }
            }
        }
        let rows: Vec<String> = (live.on(Target::ClipTitle).into_iter())
            .map(|(gesture, mods, _)| gesture.with(mods))
            .collect();
        assert_eq!(
            rows,
            [
                "Click",
                "Ctrl+Click",
                "Shift+Click",
                "Cmd+Click",
                "Double-click",
                "Drag",
                "Ctrl+Drag",
                "Alt+Drag",
                "Shift+Drag",
                "Cmd+Drag"
            ]
        );
        let sets: Vec<String> = (modifier_sets().into_iter().take(6))
            .map(|mods| Gesture::Drag.with(mods))
            .collect();
        assert_eq!(
            sets,
            [
                "Drag",
                "Ctrl+Drag",
                "Alt+Drag",
                "Shift+Drag",
                "Cmd+Drag",
                "Ctrl+Alt+Drag"
            ]
        );
        assert_eq!(modifier_sets().len(), 16);
    }

    #[test]
    fn modifiers_part_of_the_way_through_a_drag_switch_its_versions() {
        use MouseAction as A;
        let live = MouseMap::live();
        let (none, ctrl, alt, shift) = (
            Modifiers::NONE,
            Modifiers::CTRL,
            Modifiers::ALT,
            Modifiers::SHIFT,
        );
        let during = |target, action, mods| live.during(target, action, mods);
        // Clips copy while Ctrl is held and keep off the grid while Alt is,
        // both together too, whatever the drag started as.
        for started in [A::Move, A::Copy, A::MoveFree] {
            let title = |mods| during(Target::ClipTitle, started, mods);
            assert_eq!(title(none), (A::Move, false));
            assert_eq!(title(alt), (A::MoveFree, true));
            assert_eq!(title(ctrl), (A::Copy, false));
            assert_eq!(title(ctrl | alt), (A::Copy, true));
            assert_eq!(title(shift | alt), (A::MoveFree, true));
        }
        // Shift switches a clip's edge between trimming and stretching,
        // and Alt keeps either off the grid.
        let edge = |action, mods| during(Target::ClipEdge, action, mods);
        for started in [A::Trim, A::TrimFree, A::Stretch] {
            assert_eq!(edge(started, none), (A::Trim, false));
            assert_eq!(edge(started, alt), (A::TrimFree, true));
            assert_eq!(edge(started, shift), (A::Stretch, false));
            assert_eq!(edge(started, shift | alt), (A::Stretch, true));
        }
        // So do moving notes, whose Alt-drag changes their velocity, the
        // loop and locators.
        assert_eq!(during(Target::Note, A::Move, alt), (A::Move, true));
        assert_eq!(during(Target::Note, A::Move, ctrl | alt), (A::Copy, true));
        assert_eq!(
            during(Target::LoopBrace, A::MoveLoop, alt),
            (A::MoveLoop, true)
        );
        assert_eq!(
            during(Target::Locator, A::MoveLocator, none),
            (A::MoveLocator, false)
        );
        // Drags that don't snap don't change, nor does what Alt starts.
        let velocity = during(Target::Note, A::Velocity, none);
        assert_eq!(velocity, (A::Velocity, false));
        let erase = during(Target::ControllerLane, A::EraseController, none);
        assert_eq!(erase, (A::EraseController, false));
        let draw = during(Target::ControllerLane, A::DrawController, alt);
        assert_eq!(draw, (A::DrawController, false));
        let resize = during(Target::TrackEdge, A::ResizeTrack, alt);
        assert_eq!(resize, (A::ResizeTrack, false));

        // With Shift moving clips off the grid and Alt copying them, as
        // a user might have it, those count part of the way through, for
        // notes and the loop too.
        let mut map = MouseMap::live();
        map.bind(Target::ClipTitle, Gesture::Drag, shift, A::MoveFree);
        map.bind(Target::ClipTitle, Gesture::Drag, alt, A::Copy);
        let title = |mods| map.during(Target::ClipTitle, A::Move, mods);
        assert_eq!(title(alt), (A::Copy, false));
        assert_eq!(title(shift), (A::MoveFree, true));
        assert_eq!(map.during(Target::Note, A::Move, shift), (A::Move, true));
        assert_eq!(map.during(Target::Note, A::Move, alt), (A::Move, false));
        let moved = map.during(Target::LoopBrace, A::MoveLoop, shift);
        assert_eq!(moved, (A::MoveLoop, true));
        // A clip's edge still trims off the grid with Alt, its own.
        assert_eq!(
            map.during(Target::ClipEdge, A::Trim, alt),
            (A::TrimFree, true)
        );
        // With Shift and Alt bound together to stretching, Alt still keeps
        // a stretch off the grid, as it trims off it.
        let mut map = MouseMap::live();
        map.bind(Target::ClipEdge, Gesture::Drag, shift | alt, A::Stretch);
        let stretch = map.during(Target::ClipEdge, A::Trim, shift | alt);
        assert_eq!(stretch, (A::Stretch, true));
    }

    #[test]
    fn gestures_can_be_bound_again() {
        let mut map = MouseMap::live();
        let click = (Target::ClipTitle, Gesture::Click, Modifiers::NONE);
        let was = map.bind(click.0, click.1, click.2, MouseAction::SelectAndMark);
        assert_eq!(was, Some(MouseAction::Select));
        assert_eq!(
            map.action(click.0, click.1, click.2),
            Some(MouseAction::SelectAndMark)
        );
        assert_eq!(
            map.unbind(click.0, click.1, click.2),
            Some(MouseAction::SelectAndMark)
        );
        assert_eq!(map.action(click.0, click.1, click.2), None);
        // Shift-click toggles still: it has a binding of its own.
        let shift = map.action(Target::ClipTitle, Gesture::Click, Modifiers::SHIFT);
        assert_eq!(shift, Some(MouseAction::ToggleSelect));
        assert!(map.iter().count() > 30);
        assert_eq!(MouseMap::new().iter().count(), 0);
    }
}
