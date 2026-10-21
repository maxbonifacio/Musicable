//! Real-time safety checks.
//!
//! Code on the audio thread must not allocate or free memory, lock, block or
//! do I/O. [`scope`] marks the calling thread as real-time while it runs a
//! closure. In debug builds with [`CheckedAlloc`] as the global allocator,
//! every allocation or free inside a scope is recorded, and the outermost
//! scope panics when it ends, so a test fails at the render that allocated.
//! Release builds compile the checks out.
//!
//! Every binary and test crate that runs the engine installs the allocator:
//!
//! ```ignore
//! #[cfg(debug_assertions)]
//! #[global_allocator]
//! static ALLOC: musicable_engine::rt::CheckedAlloc = musicable_engine::rt::CheckedAlloc;
//! ```
//!
//! Locks can't be caught this way: an uncontended Rust lock never leaves user
//! space, so there is nothing to intercept. Clippy bans lock types, sleeping
//! and printing in this crate instead (`crates/engine/clippy.toml`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::backtrace::{Backtrace, BacktraceStatus};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

/// The system allocator, plus a record of every allocator call made inside a
/// [`scope`].
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckedAlloc;

static INSTALLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// How many scopes the thread is inside.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// Set while a violation is being recorded, which may allocate itself.
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    /// Allocator calls inside the current outermost scope.
    static COUNT: Cell<u32> = const { Cell::new(0) };
    /// The first of them, and where it came from.
    static FIRST: RefCell<Option<(Call, Backtrace)>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, Copy)]
enum Call {
    Alloc(usize),
    Realloc(usize),
    Free(usize),
}

impl fmt::Display for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Alloc(size) => write!(f, "an allocation of {size} bytes"),
            Self::Realloc(size) => write!(f, "a reallocation to {size} bytes"),
            Self::Free(size) => write!(f, "a free of {size} bytes"),
        }
    }
}

/// Whether [`CheckedAlloc`] is the global allocator. Tests assert this, so a
/// missing `#[global_allocator]` can't quietly turn the checks off.
pub fn checker_installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// Runs `f` with the current thread marked as real-time.
///
/// # Panics
///
/// In debug builds, when the outermost scope ends after `f` allocated or
/// freed memory. Run with `RUST_BACKTRACE=1` to see where the first
/// allocation happened.
pub fn scope<R>(f: impl FnOnce() -> R) -> R {
    if cfg!(not(debug_assertions)) {
        return f();
    }
    struct Exit;
    impl Drop for Exit {
        fn drop(&mut self) {
            DEPTH.set(DEPTH.get() - 1);
        }
    }
    let outermost = DEPTH.get() == 0;
    if outermost {
        COUNT.set(0);
        FIRST.take();
    }
    DEPTH.set(DEPTH.get() + 1);
    let exit = Exit;
    let result = f();
    drop(exit);
    if outermost && COUNT.get() > 0 {
        report();
    }
    result
}

#[cold]
fn report() -> ! {
    let count = COUNT.get();
    let first = match FIRST.take() {
        Some((call, trace)) if trace.status() == BacktraceStatus::Captured => {
            format!("The first was {call}, at:\n{trace}")
        }
        Some((call, _)) => {
            format!("The first was {call}. Run with RUST_BACKTRACE=1 to see where.")
        }
        None => String::new(),
    };
    let thread = std::thread::current();
    panic!(
        "real-time violation: {count} heap allocator call(s) on thread '{}' inside rt::scope. {first}",
        thread.name().unwrap_or("unnamed"),
    );
}

fn check(call: Call) {
    if !INSTALLED.load(Ordering::Relaxed) {
        INSTALLED.store(true, Ordering::Relaxed);
    }
    // `try_with`, because a thread's locals are gone while it shuts down.
    let in_scope = DEPTH.try_with(Cell::get).unwrap_or(0) > 0;
    if !in_scope || RECORDING.try_with(Cell::get).unwrap_or(true) || std::thread::panicking() {
        return;
    }
    RECORDING.set(true);
    COUNT.set(COUNT.get() + 1);
    let _ = FIRST.try_with(|first| {
        if let Ok(mut first) = first.try_borrow_mut()
            && first.is_none()
        {
            *first = Some((call, Backtrace::capture()));
        }
    });
    RECORDING.set(false);
}

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CheckedAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        check(Call::Alloc(layout.size()));
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        check(Call::Alloc(layout.size()));
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        check(Call::Free(layout.size()));
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        check(Call::Realloc(new_size));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checker_is_installed() {
        drop(Box::new(1));
        assert!(checker_installed());
    }

    #[test]
    fn real_time_code_may_use_the_stack() {
        let sum = scope(|| {
            let samples = [0.5f32; 64];
            samples.iter().sum::<f32>()
        });
        assert_eq!(sum, 32.0);
    }

    #[test]
    #[cfg(debug_assertions)]
    fn allocating_in_a_scope_panics() {
        let result =
            std::panic::catch_unwind(|| scope(|| drop(std::hint::black_box(vec![0u8; 24]))));
        let message = *result.unwrap_err().downcast::<String>().unwrap();
        assert!(message.contains("2 heap allocator call(s)"), "{message}");
        assert!(message.contains("an allocation of 24 bytes"), "{message}");
    }

    #[test]
    #[cfg(debug_assertions)]
    fn only_the_outermost_scope_reports() {
        let result = std::panic::catch_unwind(|| {
            scope(|| {
                scope(|| drop(std::hint::black_box(Box::new(1u64))));
                drop(std::hint::black_box(Box::new(2u64)));
            })
        });
        let message = *result.unwrap_err().downcast::<String>().unwrap();
        assert!(message.contains("4 heap allocator call(s)"), "{message}");
        // A clean scope afterwards starts from zero.
        scope(|| ());
    }
}
