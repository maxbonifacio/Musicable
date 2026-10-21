//! A work-stealing deque with a fixed capacity.
//!
//! Each thread pushes the nodes it makes ready onto its own deque and pops
//! them from the same end, newest first. Threads that run out of work steal
//! from the other end of someone else's, oldest first. This is the Chase-Lev
//! deque, with the memory orderings from "Correct and Efficient Work-Stealing
//! for Weak Memory Models" (Lê, Pop, Cohen and Zappa Nardelli, PPoPP 2013),
//! minus the resizing: the capacity is fixed when the deque is built, so
//! nothing allocates on the audio thread. The renderer only shares graphs
//! with fewer nodes than that, and each node is pushed at most once a block,
//! so a deque never fills up.
//!
//! The index thieves take from only grows, so a thief working from indices
//! that are out of date fails its compare-and-swap rather than taking an item
//! twice, even if it was held up across blocks.

use std::sync::atomic::Ordering::{Acquire, Relaxed, Release, SeqCst};
use std::sync::atomic::{AtomicI64, AtomicU32, fence};

use super::CachePadded;

pub(crate) struct Deque {
    /// The next item to steal. Thieves and the owner's last pop move it.
    top: CachePadded<AtomicI64>,
    /// One past the newest item. Only the owner writes it.
    bottom: CachePadded<AtomicI64>,
    /// A ring of `mask + 1` slots, a power of two.
    slots: Box<[AtomicU32]>,
    mask: usize,
}

/// What a steal found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Steal {
    Empty,
    /// Another thread got there first. There may be more.
    Retry,
    Item(u32),
}

impl Deque {
    /// A deque that holds up to `capacity` items, rounded up to a power of
    /// two.
    pub fn new(capacity: usize) -> Self {
        let len = capacity.max(1).next_power_of_two();
        Self {
            top: CachePadded(AtomicI64::new(0)),
            bottom: CachePadded(AtomicI64::new(0)),
            slots: (0..len).map(|_| AtomicU32::new(0)).collect(),
            mask: len - 1,
        }
    }

    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    fn slot(&self, index: i64) -> &AtomicU32 {
        &self.slots[index as usize & self.mask]
    }

    /// Adds `item` at the owner's end. Only the owner may call this, and
    /// never on a full deque.
    pub fn push(&self, item: u32) {
        let b = self.bottom.load(Relaxed);
        let t = self.top.load(Acquire);
        debug_assert!(b - t < self.slots.len() as i64, "the deque is full");
        self.slot(b).store(item, Relaxed);
        fence(Release);
        self.bottom.store(b + 1, Relaxed);
    }

    /// Takes the newest item. Only the owner may call this.
    pub fn pop(&self) -> Option<u32> {
        let b = self.bottom.load(Relaxed) - 1;
        self.bottom.store(b, Relaxed);
        fence(SeqCst);
        let t = self.top.load(Relaxed);
        if t > b {
            // Empty.
            self.bottom.store(b + 1, Relaxed);
            return None;
        }
        let item = self.slot(b).load(Relaxed);
        if t < b {
            return Some(item);
        }
        // The last item: thieves may be after it too.
        let won = self.top.compare_exchange(t, t + 1, SeqCst, Relaxed).is_ok();
        self.bottom.store(b + 1, Relaxed);
        won.then_some(item)
    }

    /// Takes the oldest item, from any thread.
    pub fn steal(&self) -> Steal {
        let t = self.top.load(Acquire);
        fence(SeqCst);
        let b = self.bottom.load(Acquire);
        if t >= b {
            return Steal::Empty;
        }
        let item = self.slot(t).load(Relaxed);
        match self.top.compare_exchange(t, t + 1, SeqCst, Relaxed) {
            Ok(_) => Steal::Item(item),
            Err(_) => Steal::Retry,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::thread;

    use super::*;

    #[test]
    fn the_owner_pops_newest_first_and_thieves_take_oldest() {
        let d = Deque::new(4);
        assert_eq!(d.pop(), None);
        assert_eq!(d.steal(), Steal::Empty);
        for i in 1..=3 {
            d.push(i);
        }
        assert_eq!(d.pop(), Some(3));
        assert_eq!(d.steal(), Steal::Item(1));
        assert_eq!(d.pop(), Some(2));
        assert_eq!(d.pop(), None);
        assert_eq!(d.steal(), Steal::Empty);
    }

    #[test]
    fn the_ring_wraps_around() {
        let d = Deque::new(3);
        assert_eq!(d.capacity(), 4);
        for round in 0..100 {
            for i in 0..4 {
                d.push(round * 4 + i);
            }
            for i in 0..2 {
                assert_eq!(d.steal(), Steal::Item(round * 4 + i));
            }
            assert_eq!(d.pop(), Some(round * 4 + 3));
            assert_eq!(d.pop(), Some(round * 4 + 2));
            assert_eq!(d.pop(), None);
        }
    }

    #[test]
    fn each_item_is_taken_exactly_once() {
        // The owner pushes in bursts and pops, while thieves steal. Every item
        // must come out once, whoever takes it.
        let (items, thieves, burst) = if cfg!(miri) {
            (200, 2, 4)
        } else {
            (200_000, 3, 64)
        };
        let d = Arc::new(Deque::new(burst));
        let taken: Arc<Vec<AtomicBool>> =
            Arc::new((0..items).map(|_| AtomicBool::new(false)).collect());
        let count = Arc::new(AtomicUsize::new(0));
        let take = {
            let taken = taken.clone();
            let count = count.clone();
            move |item: u32| {
                let twice = taken[item as usize].swap(true, Relaxed);
                assert!(!twice, "item {item} was taken twice");
                count.fetch_add(1, Relaxed);
            }
        };
        let handles: Vec<_> = (0..thieves)
            .map(|_| {
                let d = d.clone();
                let count = count.clone();
                let take = take.clone();
                thread::spawn(move || {
                    while count.load(Relaxed) < items {
                        match d.steal() {
                            Steal::Item(item) => take(item),
                            Steal::Retry | Steal::Empty => std::hint::spin_loop(),
                        }
                    }
                })
            })
            .collect();
        let mut next = 0;
        while next < items as u32 {
            for _ in 0..burst.min(items - next as usize) {
                d.push(next);
                next += 1;
            }
            // Pop about half of each burst, leaving the rest to the thieves.
            for _ in 0..burst / 2 {
                if let Some(item) = d.pop() {
                    take(item);
                }
            }
            // Wait for room before the next burst.
            while d.bottom.load(Relaxed) - d.top.load(Relaxed) > 0 {
                if let Some(item) = d.pop() {
                    take(item);
                }
            }
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(count.load(Relaxed), items);
        assert!(taken.iter().all(|t| t.load(Relaxed)));
    }
}
