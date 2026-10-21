//! Real-time safety checks, and the floating-point mode real-time code runs
//! in.
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
//!
//! Nodes run with denormal floats flushed to zero ([`flush_denormals`]), as
//! in every DAW: arithmetic on numbers that small is many times slower on
//! most CPUs, and a decaying filter or reverb tail produces them for as long
//! as it rings.

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

/// Runs `f` with denormal floats flushed to zero on the calling thread, and
/// puts the thread's floating-point mode back afterwards, even if `f`
/// panics.
///
/// Denormals (subnormals) are the floats closer to zero than
/// `f32::MIN_POSITIVE`. In this mode, results that small become zero, and
/// so do inputs that small. On x86-64 this sets MXCSR's flush-to-zero and
/// denormals-are-zero bits, and on AArch64 FPCR's flush-to-zero bit. On other
/// targets, and under Miri, it just runs `f`.
///
/// The renderers run every node this way, on the audio thread and on the
/// parallel renderer's workers alike, so they keep matching sample for
/// sample. Rust assumes the default floating-point mode, so a constant the
/// compiler works out ahead of time keeps a denormal that the same sum at run
/// time would flush. That's the same for every thread, so it can't make the
/// renderers differ, but node code shouldn't rely on denormals either way.
pub fn flush_denormals<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(fp::Mode);
    impl Drop for Restore {
        fn drop(&mut self) {
            fp::set(self.0);
        }
    }
    let mode = fp::get();
    fp::set(mode | fp::FLUSH);
    let _restore = Restore(mode);
    f()
}

#[cfg(all(target_arch = "x86_64", not(miri)))]
mod fp {
    use std::arch::asm;

    pub(super) type Mode = u32;

    /// MXCSR's flush-to-zero (FTZ, bit 15) and denormals-are-zero (DAZ, bit
    /// 6) bits.
    pub(super) const FLUSH: Mode = 1 << 15 | 1 << 6;

    pub(super) fn get() -> Mode {
        let mut csr: Mode = 0;
        // SAFETY: `stmxcsr` stores the 4-byte MXCSR at the address given,
        // which is `csr`'s.
        unsafe {
            asm!(
                "stmxcsr dword ptr [{}]",
                in(reg) &raw mut csr,
                options(nostack, preserves_flags),
            );
        }
        csr
    }

    pub(super) fn set(csr: Mode) {
        // SAFETY: `ldmxcsr` loads MXCSR from the 4 bytes at the address
        // given, which are `csr`'s. Callers only change the FTZ and DAZ bits,
        // which change how denormals round and nothing else.
        unsafe {
            asm!(
                "ldmxcsr dword ptr [{}]",
                in(reg) &raw const csr,
                options(nostack, preserves_flags, readonly),
            );
        }
    }
}

#[cfg(all(target_arch = "aarch64", not(miri)))]
mod fp {
    use std::arch::asm;

    pub(super) type Mode = u64;

    /// FPCR's flush-to-zero bit (FZ, bit 24), which flushes denormal inputs
    /// and results alike.
    pub(super) const FLUSH: Mode = 1 << 24;

    pub(super) fn get() -> Mode {
        let fpcr: Mode;
        // SAFETY: reading FPCR has no side effects.
        unsafe {
            asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
        }
        fpcr
    }

    pub(super) fn set(fpcr: Mode) {
        // SAFETY: callers only change the FZ bit, which changes how denormals
        // round and nothing else.
        unsafe {
            asm!("msr fpcr, {}", in(reg) fpcr, options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(not(any(
    all(target_arch = "x86_64", not(miri)),
    all(target_arch = "aarch64", not(miri))
)))]
mod fp {
    pub(super) type Mode = u32;

    pub(super) const FLUSH: Mode = 0;

    pub(super) fn get() -> Mode {
        0
    }

    pub(super) fn set(_: Mode) {}
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

    /// Whether this target flushes denormals in [`flush_denormals`].
    const FLUSHES: bool = cfg!(all(
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ));

    fn halve(x: f32) -> f32 {
        std::hint::black_box(x) * std::hint::black_box(0.5)
    }

    #[test]
    fn denormals_flush_to_zero_inside_and_not_outside() {
        let tiny = f32::MIN_POSITIVE;
        assert!(halve(tiny) > 0.0, "the default mode keeps denormals");
        let inside = flush_denormals(|| halve(tiny));
        if FLUSHES {
            assert_eq!(inside, 0.0);
            // Denormal inputs count as zero too.
            let denormal = std::hint::black_box(tiny / 4.0);
            assert_eq!(
                flush_denormals(|| denormal * std::hint::black_box(2.0)),
                0.0
            );
        } else {
            assert_eq!(inside, tiny / 2.0);
        }
        assert!(halve(tiny) > 0.0, "the mode comes back afterwards");
        // Nested, the outer mode stays until the outer call ends.
        flush_denormals(|| {
            flush_denormals(|| ());
            assert_eq!(halve(tiny) == 0.0, FLUSHES);
        });
    }

    #[test]
    fn a_panic_puts_the_mode_back() {
        let result = std::panic::catch_unwind(|| flush_denormals(|| panic!("in a node")));
        assert!(result.is_err());
        assert!(halve(f32::MIN_POSITIVE) > 0.0);
    }
}
