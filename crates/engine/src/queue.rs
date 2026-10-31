//! The queues between a graph's two halves: what the nodes that render
//! ahead send the nodes that render live ([`Graph::prepare`]).
//!
//! A node renders live if a live source reaches it, through sound, notes or
//! modulation, and ahead otherwise, so what passes between the halves only
//! ever goes from the ahead half to the live one. Each way it can go has a
//! pair of nodes here: one at the end of the ahead half that keeps what it
//! gets, at its engine time, and one at the start of the live half that
//! hands it on at the same engine time. So a queue adds no latency: the live
//! node reads each frame as it would have had the node before it rendered
//! live too.
//!
//! - Sound: [`SoundOut`] and [`SoundIn`] share a ring of frames, as the
//!   engine's output ring is shared ([`Ring`]): engine time `t` at place
//!   `t % capacity`, written once the live side has played the place and
//!   read once the ahead side has rendered it.
//! - Notes, those a node plays and those it makes as it renders:
//!   [`NoteOut`] keeps the notes it gets, and [`NoteIn`] plays them on.
//! - Modulation: [`ModulationOut`] keeps what each port of a modulator that
//!   goes to the live half sends, and [`ModulationIn`] sends it on through
//!   ports of its own.
//!
//! The notes and the modulation wait in an [`EventRing`], which the engine
//! makes sure has room before it renders a block ahead.
//!
//! [`Graph::prepare`]: crate::Graph::prepare

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use crate::ahead::Ring;
use crate::parallel::CachePadded;
use crate::{AudioMut, Controls, Inputs, Node, NodeEventKind, Notes, ProcessContext};

/// Events waiting to cross from the ahead half to the live one, each with
/// the engine time it's for, in order of time: one thread pushes them and
/// another pops them, each a block at a time.
pub(crate) struct EventRing<T> {
    slots: Box<[Slot<T>]>,
    /// How many have been popped.
    head: CachePadded<AtomicUsize>,
    /// How many have been pushed.
    tail: CachePadded<AtomicUsize>,
}

/// A place in an [`EventRing`]: an event and its engine time, once pushed.
type Slot<T> = UnsafeCell<MaybeUninit<(u64, T)>>;

// SAFETY: the pusher writes only slots the popper has popped, and the
// popper reads only slots the pusher has pushed, as `head` and `tail` say,
// with release and acquire on both sides. One thread at a time pushes, and
// one pops: a node renders on one thread at a time, each block after the
// last.
unsafe impl<T: Send> Sync for EventRing<T> {}

impl<T: Copy> EventRing<T> {
    /// Room for `capacity` events. Allocates.
    pub(crate) fn new(capacity: usize) -> Self {
        let slots = std::iter::repeat_with(|| UnsafeCell::new(MaybeUninit::uninit()))
            .take(capacity.max(1))
            .collect();
        Self {
            slots,
            head: CachePadded(AtomicUsize::new(0)),
            tail: CachePadded(AtomicUsize::new(0)),
        }
    }

    /// Queues `event` for engine time `time`, after those queued before.
    /// Returns whether there was room. Real-time safe.
    fn push(&self, time: u64, event: T) -> bool {
        let tail = self.tail.load(Relaxed);
        if tail - self.head.load(Acquire) == self.slots.len() {
            return false;
        }
        // SAFETY: the popper is done with this slot, as `head` says.
        unsafe { (*self.slots[tail % self.slots.len()].get()).write((time, event)) };
        self.tail.store(tail + 1, Release);
        true
    }

    /// Takes the next event if it's for a time before `end`. Real-time
    /// safe.
    fn pop_before(&self, end: u64) -> Option<(u64, T)> {
        let head = self.head.load(Relaxed);
        if head == self.tail.load(Acquire) {
            return None;
        }
        // SAFETY: the pusher has written this slot, as `tail` says, and
        // won't again until `head` passes it.
        let event = unsafe { (*self.slots[head % self.slots.len()].get()).assume_init() };
        if event.0 >= end {
            return None;
        }
        self.head.store(head + 1, Release);
        Some(event)
    }

    /// How many more it can take.
    fn free(&self) -> usize {
        let queued = self.tail.load(Acquire) - self.head.load(Acquire);
        self.slots.len() - queued
    }
}

/// A queue the engine checks has room before it renders a block ahead: an
/// [`EventRing`], with the most a block can put into it.
#[derive(Clone)]
pub(crate) struct Room {
    free: Arc<dyn Fn() -> usize + Send + Sync>,
    block: usize,
}

impl Room {
    fn of<T: Copy + Send + 'static>(ring: &Arc<EventRing<T>>, block: usize) -> Self {
        let ring = Arc::clone(ring);
        Self {
            free: Arc::new(move || ring.free()),
            block,
        }
    }

    /// Whether the queue can take another block's events.
    pub(crate) fn has_room(&self) -> bool {
        (self.free)() >= self.block
    }
}

/// The ends of a queue for the sound of a node with `channels` channels,
/// with room for `frames` frames. Allocates.
pub(crate) fn sound(channels: usize, frames: usize) -> (SoundOut, SoundIn) {
    let ring = Arc::new(Ring::new(channels, frames));
    let out = SoundOut {
        ring: Arc::clone(&ring),
    };
    (out, SoundIn { ring, channels })
}

/// The ends of a queue for the notes a node plays or makes, with room for
/// `capacity` of them, and the room a block needs, `block`. Allocates.
pub(crate) fn notes(capacity: usize, block: usize) -> (NoteOut, NoteIn, Room) {
    let ring = Arc::new(EventRing::new(capacity));
    let room = Room::of(&ring, block);
    let out = NoteOut {
        ring: Arc::clone(&ring),
    };
    (out, NoteIn { ring }, room)
}

/// The ends of a queue for what a modulator's ports send to the live half,
/// with room for `capacity` events, and the room a block needs, `block`.
/// Allocates.
pub(crate) fn modulation(capacity: usize, block: usize) -> (ModulationOut, ModulationIn, Room) {
    let ring = Arc::new(EventRing::new(capacity));
    let room = Room::of(&ring, block);
    let out = ModulationOut {
        ring: Arc::clone(&ring),
    };
    (out, ModulationIn { ring }, room)
}

/// Keeps the sound of the node before it, at the end of the ahead half.
pub(crate) struct SoundOut {
    ring: Arc<Ring>,
}

impl Node for SoundOut {
    fn channels(&self) -> usize {
        1
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, inputs: Inputs<'_>, _: &mut AudioMut<'_>) {
        // SAFETY: the engine renders a block ahead only where the live side
        // has played its places, and before it knows the block is rendered,
        // and renders the live half only where the ahead half has rendered.
        // One thread renders this node at a time.
        unsafe { self.ring.write(ctx.time, ctx.frames, inputs.get(0)) };
    }
}

/// Plays the sound a [`SoundOut`] kept, at the start of the live half.
pub(crate) struct SoundIn {
    ring: Arc<Ring>,
    channels: usize,
}

impl Node for SoundIn {
    fn channels(&self) -> usize {
        self.channels
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, output: &mut AudioMut<'_>) {
        // SAFETY: as for `SoundOut`: the ahead half has rendered these
        // frames, and the live side hasn't played them.
        unsafe { self.ring.read(ctx.time, output) };
    }
}

/// Keeps the notes it gets, at the end of the ahead half: those a node
/// there plays or makes for nodes in the live half.
pub(crate) struct NoteOut {
    ring: Arc<EventRing<NodeEventKind>>,
}

impl Node for NoteOut {
    fn channels(&self) -> usize {
        1
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, _: &mut AudioMut<'_>) {
        for event in ctx.events.iter().filter(|e| e.kind.is_played()) {
            let pushed = self.ring.push(ctx.time + event.offset as u64, event.kind);
            debug_assert!(
                pushed,
                "the engine renders ahead only with room for a block"
            );
        }
    }
}

/// Plays the notes a [`NoteOut`] kept on to the nodes in the live half
/// they're for, each on its frame.
pub(crate) struct NoteIn {
    ring: Arc<EventRing<NodeEventKind>>,
}

impl Node for NoteIn {
    fn channels(&self) -> usize {
        1
    }

    fn play_notes(&mut self, ctx: &ProcessContext<'_>, notes: &mut Notes<'_>) {
        let end = ctx.time + ctx.frames as u64;
        // A live block can span several blocks rendered ahead, with more
        // notes than it has room for: those it hasn't room for wait in the
        // ring for the next, rather than go.
        while notes.room() > 0
            && let Some((time, kind)) = self.ring.pop_before(end)
        {
            let offset = time.saturating_sub(ctx.time) as usize;
            notes.send_played(offset, kind);
        }
    }

    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, _: &mut AudioMut<'_>) {}
}

/// Keeps what a modulator in the ahead half sends through its ports that go
/// to the live half: each such port goes to this node's parameter with the
/// number the [`ModulationIn`] port has.
pub(crate) struct ModulationOut {
    ring: Arc<EventRing<(u32, f64)>>,
}

impl Node for ModulationOut {
    fn channels(&self) -> usize {
        1
    }

    fn process(&mut self, ctx: &ProcessContext<'_>, _: Inputs<'_>, _: &mut AudioMut<'_>) {
        for event in ctx.events {
            if let NodeEventKind::Modulation { param, amount, .. } = event.kind {
                let time = ctx.time + event.offset as u64;
                let pushed = self.ring.push(time, (param.0 as u32, amount));
                debug_assert!(
                    pushed,
                    "the engine renders ahead only with room for a block"
                );
            }
        }
    }
}

/// Sends on what a [`ModulationOut`] kept, each through its port, on its
/// frame.
pub(crate) struct ModulationIn {
    ring: Arc<EventRing<(u32, f64)>>,
}

impl Node for ModulationIn {
    fn channels(&self) -> usize {
        1
    }

    fn control(&mut self, ctx: &ProcessContext<'_>, controls: &mut Controls<'_>) {
        let end = ctx.time + ctx.frames as u64;
        // As for notes: what it hasn't room for waits for the next block.
        while controls.room() > 0
            && let Some((time, (port, amount))) = self.ring.pop_before(end)
        {
            let offset = time.saturating_sub(ctx.time) as usize;
            controls.send(port as usize, offset, amount);
        }
    }

    fn process(&mut self, _: &ProcessContext<'_>, _: Inputs<'_>, _: &mut AudioMut<'_>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_wait_for_their_time_and_room_comes_back_as_they_go() {
        let ring = EventRing::<u32>::new(3);
        assert!(ring.push(10, 1));
        assert!(ring.push(20, 2));
        assert!(ring.push(20, 3));
        assert!(!ring.push(30, 4), "full");
        assert_eq!(ring.free(), 0);
        assert_eq!(ring.pop_before(10), None);
        assert_eq!(ring.pop_before(11), Some((10, 1)));
        assert_eq!(ring.pop_before(11), None);
        assert_eq!(ring.free(), 1);
        assert!(ring.push(30, 4));
        let rest: Vec<_> = std::iter::from_fn(|| ring.pop_before(u64::MAX)).collect();
        assert_eq!(rest, [(20, 2), (20, 3), (30, 4)]);
        assert_eq!(ring.free(), 3);
    }
}
