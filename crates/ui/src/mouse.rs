//! The mouse map: what a mouse gesture does, from where it starts and the
//! modifiers held.
//!
//! The arrangement, the piano roll and the mixer look gestures up here
//! instead of deciding for themselves, so a profile can make the mouse work
//! as another DAW's does, as a keymap does for keys. [`MouseMap::live`], the map Musicable
//! starts with, follows Ableton Live on Windows, with Cmd standing in for
//! Ctrl on a Mac.

use std::collections::BTreeMap;

use musicable_commands::Modifiers;

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
    TrackHeader,
    /// A track's bottom edge, in its header.
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
    /// Selects just the clip, the track or the note.
    Select,
    /// Adds the clip, the track or the note to the selection, or takes it
    /// out.
    ToggleSelect,
    /// Selects the clip and puts the start marker where it was clicked, so
    /// Split splits it there.
    SelectAndMark,
    /// Moves the selected clips or notes, snapping to the grid.
    Move,
    /// Copies the selected clips or notes, snapping to the grid.
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
    /// Selects the time dragged over, on the tracks dragged over.
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
    /// Shows the MIDI clip in the piano roll, and gives it the focus.
    Open,
    /// Draws the controller's curve along the drag, a step of the grid at a
    /// time, at the heights the drag passes.
    DrawController,
    /// Erases the controller's curve along the drag, so what came before
    /// holds there.
    EraseController,
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

    pub fn iter(&self) -> impl Iterator<Item = (Target, Gesture, Modifiers, MouseAction)> + '_ {
        self.bindings
            .iter()
            .map(|(&(target, gesture, mods), &action)| (target, gesture, mods, action))
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
        bind(T::FadeHandle, Drag, &[none], A::Fade);
        bind(T::Lane, Click, &[none], A::PlaceMarker);
        bind(T::Lane, Drag, &[none], A::SelectTime);
        bind(T::Empty, Click, &[none], A::PlaceMarker);
        bind(T::Ruler, Click, &[none], A::Locate);
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
        bind(T::Timeline, Wheel, &[none], A::Scroll);
        bind(T::Timeline, Wheel, &[shift], A::ScrollSideways);
        bind(T::Timeline, Wheel, &[ctrl[0], ctrl[1], alt], A::Zoom);
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
