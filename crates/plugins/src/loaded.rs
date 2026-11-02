//! Plugin libraries, loaded once per process and unloaded on the thread
//! that loaded them: what hosting CLAP and VST3 plugins share.
//!
//! Every instance and every node holds its library, so the last to let go
//! of it can be anywhere: the engine's compiler drops the nodes of a job it
//! skips on its own thread. A library let go of on another thread than the
//! one that loaded it waits for that thread to close it
//! ([`unload_unused`](crate::unload_unused)), as VST3 has modules exit on
//! the UI thread, or for someone to load it again, who gets it as it is.
//!
//! A library on its way out is still listed until it has closed or gone to
//! wait, so loading it again waits for that, rather than initializing it
//! again before it's deinitialized.

use std::mem;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::Error;
use crate::threads::thread_mark;

/// A library, open: loaded and initialized, or entered.
pub(crate) trait Open: Send + Sync + Sized + 'static {
    /// Deinitializes or exits the library and unloads it.
    fn close(self);
}

/// The libraries of one format loaded in the process. Its lock is held
/// while one opens or closes, which neither format allows at once.
pub(crate) struct Registry<T: Open> {
    state: Mutex<State<T>>,
}

struct State<T: Open> {
    /// Each library in use, by path, until the last of it is let go of.
    live: Vec<(PathBuf, Weak<Loaded<T>>)>,
    /// Libraries let go of on a thread other than the one that loaded them.
    unused: Vec<Unused<T>>,
}

struct Unused<T> {
    path: PathBuf,
    open: T,
    /// The thread that loaded it, as [`thread_mark`] gives it.
    loader: usize,
}

impl<T: Open> Registry<T> {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(State {
                live: Vec::new(),
                unused: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The library at `path`, a canonical path: the one in use, or the
    /// one let go of that waits to close, or else one `open` opens.
    pub(crate) fn load(
        &'static self,
        path: &Path,
        open: impl FnOnce(&Path) -> Result<T, Error>,
    ) -> Result<Arc<Loaded<T>>, Error> {
        let mut state = loop {
            let mut state = self.lock();
            state.close_unused(Some(path));
            match state.live.iter().find(|(p, _)| p == path) {
                None => break state,
                Some((_, live)) => match live.upgrade() {
                    Some(loaded) => return Ok(loaded),
                    // Let go of on another thread, which waits for the lock
                    // to close it or put it aside.
                    None => {
                        drop(state);
                        std::thread::yield_now();
                    }
                },
            }
        };
        let open = match state.unused.iter().position(|u| u.path == path) {
            Some(index) => state.unused.swap_remove(index).open,
            None => open(path)?,
        };
        let loaded = Arc::new(Loaded {
            path: path.to_owned(),
            loader: thread_mark(),
            open: Some(open),
            registry: self,
        });
        state.live.push((path.to_owned(), Arc::downgrade(&loaded)));
        Ok(loaded)
    }

    /// Closes the libraries this thread loaded that were let go of on
    /// others.
    pub(crate) fn close_unused(&self) {
        self.lock().close_unused(None);
    }

    /// How many libraries wait for the thread that loaded them to close
    /// them.
    #[cfg(test)]
    pub(crate) fn unused(&self) -> usize {
        self.lock().unused.len()
    }
}

impl<T: Open> State<T> {
    /// Closes the libraries this thread loaded that were let go of on
    /// others, but the one at `but`, which is about to be taken back.
    fn close_unused(&mut self, but: Option<&Path>) {
        let here = thread_mark();
        let mut index = 0;
        while index < self.unused.len() {
            let unused = &self.unused[index];
            if unused.loader == here && Some(unused.path.as_path()) != but {
                self.unused.swap_remove(index).open.close();
            } else {
                index += 1;
            }
        }
    }
}

/// A library in use. It closes as the last of it is let go of, on the
/// thread that loaded it, or waits for that thread.
pub(crate) struct Loaded<T: Open> {
    path: PathBuf,
    loader: usize,
    /// Taken as it closes.
    open: Option<T>,
    registry: &'static Registry<T>,
}

impl<T: Open> Deref for Loaded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.open
            .as_ref()
            .expect("a library is open until it's let go of")
    }
}

impl<T: Open> Drop for Loaded<T> {
    fn drop(&mut self) {
        let mut state = self.registry.lock();
        let me: *const Self = self;
        state.live.retain(|(_, live)| !ptr::eq(live.as_ptr(), me));
        let Some(open) = self.open.take() else {
            return;
        };
        if thread_mark() == self.loader {
            open.close();
        } else {
            state.unused.push(Unused {
                path: mem::take(&mut self.path),
                open,
                loader: self.loader,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// How many times each kind of library opened and closed.
    static OPENED: AtomicU32 = AtomicU32::new(0);
    static CLOSED: AtomicU32 = AtomicU32::new(0);

    struct Fake;

    impl Open for Fake {
        fn close(self) {
            CLOSED.fetch_add(1, Ordering::SeqCst);
        }
    }

    static FAKES: Registry<Fake> = Registry::new();

    fn open(_: &Path) -> Result<Fake, Error> {
        OPENED.fetch_add(1, Ordering::SeqCst);
        Ok(Fake)
    }

    #[test]
    fn libraries_close_on_the_thread_that_loaded_them() {
        let path = Path::new("/fake/library");
        let counts = || (OPENED.load(Ordering::SeqCst), CLOSED.load(Ordering::SeqCst));
        // Let go of here, it closes at once.
        let a = FAKES.load(path, open).unwrap();
        let b = FAKES.load(path, open).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        drop((a, b));
        assert_eq!(counts(), (1, 1));

        // Let go of elsewhere, it waits for this thread.
        let a = FAKES.load(path, open).unwrap();
        std::thread::spawn(move || drop(a)).join().unwrap();
        assert_eq!((counts(), FAKES.unused()), ((2, 1), 1));
        // Loading it again takes it back as it is.
        let a = FAKES.load(path, open).unwrap();
        assert_eq!((counts(), FAKES.unused()), ((2, 1), 0));
        std::thread::spawn(move || drop(a)).join().unwrap();
        // Another thread can't close it.
        std::thread::spawn(|| FAKES.close_unused()).join().unwrap();
        assert_eq!(counts(), (2, 1));
        FAKES.close_unused();
        assert_eq!((counts(), FAKES.unused()), ((2, 2), 0));
    }
}
