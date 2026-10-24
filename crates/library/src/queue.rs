//! The queue for file work in the background: decoding the sample to
//! preview, drawing waveforms, scanning folders, and later analysing sounds.
//!
//! It has threads of its own, `musicable-files-1` and up, never the
//! engine's workers, and they run below normal priority. Jobs come in three
//! [`Priority`]s, and the most urgent waiting goes first. Jobs that can wait
//! for good, such as scans, run on every thread but one while the transport
//! is stopped, which is left for previews and waveforms, on one while it
//! plays, and on one with a rest after each while it records, so the disk
//! and the cores are left to the recorder and the engine; a long one, such
//! as a scan, rests as it goes with a [`Pace`], or goes on in pieces, each a
//! job its [`Adder`] adds. On Windows they also run in background mode,
//! which lowers how soon the system serves their reads.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How long a thread rests after a job that could wait, while recording,
/// and how long a job with a [`Pace`] works before it rests.
const REST: Duration = Duration::from_millis(50);

/// How soon a job is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// The user is waiting for it, as for a preview to start.
    Now,
    /// Something on screen is waiting for it, such as a waveform.
    Soon,
    /// It can wait, however long: scans, and later analysis.
    Later,
}

/// What the engine is doing, which says how much the jobs that can wait
/// may take.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Busy {
    #[default]
    Idle,
    Playing,
    Recording,
}

type Job = Box<dyn FnOnce() + Send>;

/// Runs jobs on threads of its own, below normal priority. Dropping it
/// lets the jobs running finish, drops those waiting, and stops the
/// threads.
pub struct Queue {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a job is added, the engine's business changes, or the
    /// queue stops.
    work: Condvar,
    /// Signalled when a job finishes.
    done: Condvar,
}

#[derive(Default)]
struct State {
    /// The jobs waiting, by priority.
    waiting: [VecDeque<Job>; 3],
    busy: Busy,
    /// Jobs running, and how many of them could have waited.
    running: usize,
    running_later: usize,
    threads: usize,
    stop: bool,
}

impl State {
    /// The most jobs that could wait which may run at once: never every
    /// thread of more than one, so a preview needn't wait for one to
    /// finish.
    fn later_allowed(&self) -> usize {
        match self.busy {
            Busy::Idle => self.threads.saturating_sub(1).max(1),
            Busy::Playing | Busy::Recording => 1,
        }
    }

    /// The most urgent job that may run now, and its priority.
    fn take(&mut self) -> Option<(Priority, Job)> {
        for priority in [Priority::Now, Priority::Soon] {
            if let Some(job) = self.waiting[priority as usize].pop_front() {
                return Some((priority, job));
            }
        }
        if self.running_later < self.later_allowed() {
            let job = self.waiting[Priority::Later as usize].pop_front()?;
            return Some((Priority::Later, job));
        }
        None
    }

    fn idle(&self) -> bool {
        self.running == 0 && self.waiting.iter().all(VecDeque::is_empty)
    }
}

impl Queue {
    /// A queue with `threads` threads, or one if that's zero.
    pub fn new(threads: usize) -> Self {
        let threads = threads.max(1);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                threads,
                ..State::default()
            }),
            work: Condvar::new(),
            done: Condvar::new(),
        });
        let threads = (1..=threads)
            .map(|n| {
                let shared = shared.clone();
                thread::Builder::new()
                    .name(format!("musicable-files-{n}"))
                    .spawn(move || run(&shared))
                    .expect("the system starts a thread")
            })
            .collect();
        Self { shared, threads }
    }

    /// A queue with a thread for every two cores, from one to four, leaving
    /// the rest to the engine.
    pub fn for_this_machine() -> Self {
        Self::new((num_cpus::get_physical() / 2).clamp(1, 4))
    }

    /// How many threads it has.
    pub fn threads(&self) -> usize {
        self.threads.len()
    }

    /// A pace for a long job that can wait, to keep step with between the
    /// pieces of its work.
    pub fn pace(&self) -> Pace {
        Pace {
            shared: self.shared.clone(),
            since: Cell::new(None),
        }
    }

    /// Runs `job` on one of the threads, after the more urgent jobs waiting
    /// and those as urgent added before it.
    pub fn add(&self, priority: Priority, job: impl FnOnce() + Send + 'static) {
        self.shared.add(priority, Box::new(job));
    }

    /// A way for its jobs to add jobs: see [`Adder`].
    pub fn adder(&self) -> Adder {
        Adder {
            shared: self.shared.clone(),
        }
    }

    /// Says what the engine is doing, which says how much the jobs that can
    /// wait may take: see [`Busy`].
    pub fn set_busy(&self, busy: Busy) {
        let mut state = self.shared.state();
        if state.busy != busy {
            state.busy = busy;
            drop(state);
            self.shared.work.notify_all();
        }
    }

    /// How many jobs are waiting or running.
    pub fn pending(&self) -> usize {
        let state = self.shared.state();
        state.running + state.waiting.iter().map(VecDeque::len).sum::<usize>()
    }

    /// Waits until no job is waiting or running.
    pub fn wait(&self) {
        let mut state = self.shared.state();
        while !state.idle() {
            state = self
                .shared
                .done
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        self.shared.state().stop = true;
        self.shared.work.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        // The jobs left waiting go, outside the lock: one that holds an
        // adder or a pace would otherwise keep itself and the rest for good.
        let waiting = std::mem::take(&mut self.shared.state().waiting);
        drop(waiting);
    }
}

/// Adds jobs to a [`Queue`] from its own jobs, as a long one does that goes
/// on in pieces, each a job that adds the next, so the more urgent jobs go
/// between them. Unlike the queue, a job may hold one. Once the queue
/// stops, it adds nothing.
#[derive(Clone)]
pub struct Adder {
    shared: Arc<Shared>,
}

impl Adder {
    /// Runs `job` on one of the queue's threads, as [`Queue::add`] does.
    pub fn add(&self, priority: Priority, job: impl FnOnce() + Send + 'static) {
        self.shared.add(priority, Box::new(job));
    }

    /// A pace for a long job that can wait, as [`Queue::pace`] makes.
    pub fn pace(&self) -> Pace {
        Pace {
            shared: self.shared.clone(),
            since: Cell::new(None),
        }
    }
}

/// How a long job that can wait, such as a scan, slows down while the
/// engine records: it calls [`step`](Self::step) between the pieces of its
/// work, such as files, and once it has worked a while, a step rests as long
/// as it worked, so it takes no more than half of its thread and the disk.
pub struct Pace {
    shared: Arc<Shared>,
    /// When the job last rested, or first stepped.
    since: Cell<Option<Instant>>,
}

impl Pace {
    pub fn step(&self) {
        let now = Instant::now();
        // The first step starts the clock, however long the job waited.
        let Some(since) = self.since.get() else {
            self.since.set(Some(now));
            return;
        };
        let worked = now - since;
        if worked < REST {
            return;
        }
        if self.shared.state().busy == Busy::Recording {
            thread::sleep(worked);
        }
        self.since.set(Some(Instant::now()));
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        // A job that panics does so outside the lock.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add(&self, priority: Priority, job: Job) {
        let mut state = self.state();
        if state.stop {
            return;
        }
        state.waiting[priority as usize].push_back(job);
        drop(state);
        self.work.notify_one();
    }
}

/// A thread of the queue: runs jobs until the queue stops.
fn run(shared: &Shared) {
    priority::lower();
    let mut state = shared.state();
    loop {
        if state.stop {
            return;
        }
        let Some((priority, job)) = state.take() else {
            state = shared
                .work
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let later = priority == Priority::Later;
        state.running += 1;
        if later {
            state.running_later += 1;
        }
        drop(state);
        {
            let _background = later.then(priority::background);
            // A job that panics is gone, and the thread goes on.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        }
        let rest = later && shared.state().busy == Busy::Recording;
        if rest {
            thread::sleep(REST);
        }
        state = shared.state();
        state.running -= 1;
        if later {
            state.running_later -= 1;
            // Another job that could wait may run now.
            shared.work.notify_one();
        }
        shared.done.notify_all();
    }
}

/// Lowering the queue's threads' priority.
mod priority {
    #[cfg(windows)]
    mod imp {
        use std::ffi::c_void;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut c_void;
            fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
        }

        const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
        const THREAD_MODE_BACKGROUND_BEGIN: i32 = 0x0001_0000;
        const THREAD_MODE_BACKGROUND_END: i32 = 0x0002_0000;

        fn set(priority: i32) {
            // SAFETY: the pseudo handle stands for the calling thread, which
            // may always set its own priority. A failure leaves it as it was.
            unsafe { SetThreadPriority(GetCurrentThread(), priority) };
        }

        pub fn lower() {
            set(THREAD_PRIORITY_BELOW_NORMAL);
        }

        /// Background mode, which lowers the priority of the thread's reads
        /// and writes as well as its own, until the guard drops.
        pub struct Background;

        pub fn background() -> Background {
            set(THREAD_MODE_BACKGROUND_BEGIN);
            Background
        }

        impl Drop for Background {
            fn drop(&mut self) {
                set(THREAD_MODE_BACKGROUND_END);
            }
        }
    }

    #[cfg(target_os = "macos")]
    mod imp {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative: i32) -> i32;
        }

        const QOS_CLASS_UTILITY: u32 = 0x11;
        const QOS_CLASS_BACKGROUND: u32 = 0x09;

        fn set(class: u32) {
            // SAFETY: sets the calling thread's own quality of service. A
            // failure leaves it as it was.
            unsafe { pthread_set_qos_class_self_np(class, 0) };
        }

        pub fn lower() {
            set(QOS_CLASS_UTILITY);
        }

        /// The background class, which lowers the priority of the thread's
        /// reads too, until the guard drops.
        pub struct Background;

        pub fn background() -> Background {
            set(QOS_CLASS_BACKGROUND);
            Background
        }

        impl Drop for Background {
            fn drop(&mut self) {
                set(QOS_CLASS_UTILITY);
            }
        }
    }

    #[cfg(target_os = "linux")]
    mod imp {
        unsafe extern "C" {
            fn setpriority(which: i32, who: u32, priority: i32) -> i32;
        }

        const PRIO_PROCESS: i32 = 0;

        /// A nice value of 10. On Linux each thread has its own, and 0
        /// names the calling one. Without the rights to, a thread can't
        /// raise it again, so jobs that can wait get no lower.
        pub fn lower() {
            // SAFETY: sets the calling thread's own nice value. A failure
            // leaves it as it was.
            unsafe { setpriority(PRIO_PROCESS, 0, 10) };
        }

        pub struct Background;

        pub fn background() -> Background {
            Background
        }
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    mod imp {
        pub fn lower() {}

        pub struct Background;

        pub fn background() -> Background {
            Background
        }
    }

    pub(super) use imp::{background, lower};
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    #[test]
    fn the_most_urgent_jobs_go_first() {
        let queue = Queue::new(1);
        let (started, wait) = mpsc::channel::<()>();
        let (release, released) = mpsc::channel::<()>();
        // Holds the one thread while the others are added.
        queue.add(Priority::Later, move || {
            started.send(()).unwrap();
            released.recv().unwrap();
        });
        wait.recv().unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        for (priority, name) in [
            (Priority::Later, "scan"),
            (Priority::Soon, "waveform"),
            (Priority::Now, "preview"),
            (Priority::Soon, "another waveform"),
        ] {
            let order = order.clone();
            queue.add(priority, move || order.lock().unwrap().push(name));
        }
        assert_eq!(queue.pending(), 5);
        release.send(()).unwrap();
        queue.wait();
        assert_eq!(
            *order.lock().unwrap(),
            ["preview", "waveform", "another waveform", "scan"]
        );
        assert_eq!(queue.pending(), 0);
    }

    #[test]
    fn jobs_that_can_wait_leave_a_thread_for_previews_and_take_one_while_playing() {
        let queue = Queue::new(3);
        queue.set_busy(Busy::Playing);
        let (most, running) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        for _ in 0..6 {
            let (most, running) = (most.clone(), running.clone());
            queue.add(Priority::Later, move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                running.fetch_sub(1, Ordering::SeqCst);
            });
        }
        queue.wait();
        assert_eq!(most.load(Ordering::SeqCst), 1);

        // Idle, they take every thread but one, which is left for a preview.
        queue.set_busy(Busy::Idle);
        most.store(0, Ordering::SeqCst);
        /// How many have started, and whether the preview has run.
        type Gate = (Mutex<(usize, bool)>, Condvar);
        fn open(gate: &Gate) {
            gate.0.lock().unwrap().1 = true;
            gate.1.notify_all();
        }
        let gate: Arc<Gate> = Arc::default();
        for _ in 0..3 {
            let (most, running, gate) = (most.clone(), running.clone(), gate.clone());
            queue.add(Priority::Later, move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                // Each holds its thread until the preview has run.
                let mut state = gate.0.lock().unwrap();
                state.0 += 1;
                gate.1.notify_all();
                while !state.1 {
                    state = gate.1.wait(state).unwrap();
                }
                drop(state);
                running.fetch_sub(1, Ordering::SeqCst);
            });
        }
        let mut state = gate.0.lock().unwrap();
        while state.0 < 2 {
            state = gate.1.wait(state).unwrap();
        }
        drop(state);
        let (done, previewed) = mpsc::channel();
        let preview = gate.clone();
        queue.add(Priority::Now, move || {
            open(&preview);
            done.send(()).unwrap();
        });
        let ran = previewed.recv_timeout(Duration::from_secs(10));
        // Lets them go, whatever happened.
        open(&gate);
        assert_eq!(ran, Ok(()));
        queue.wait();
        assert_eq!(most.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn urgent_jobs_run_while_one_that_can_wait_holds_a_thread() {
        let queue = Queue::new(2);
        queue.set_busy(Busy::Recording);
        let (release, released) = mpsc::channel::<()>();
        queue.add(Priority::Later, move || released.recv().unwrap());
        let ran = Arc::new(AtomicUsize::new(0));
        let second = ran.clone();
        queue.add(Priority::Later, move || {
            second.fetch_add(1, Ordering::SeqCst);
        });
        let (done, finished) = mpsc::channel();
        queue.add(Priority::Now, move || done.send("preview").unwrap());
        assert_eq!(
            finished.recv_timeout(Duration::from_secs(10)),
            Ok("preview")
        );
        // Once the preview's job is done, the second job that could wait is
        // still waiting for the first.
        let deadline = Instant::now() + Duration::from_secs(10);
        while queue.pending() > 2 && Instant::now() < deadline {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(20));
        assert_eq!((queue.pending(), ran.load(Ordering::SeqCst)), (2, 0));
        release.send(()).unwrap();
        queue.wait();
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn long_jobs_rest_as_long_as_they_work_while_the_engine_records() {
        let queue = Queue::new(1);
        let work = REST * 2;
        let step = |pace: &Pace| {
            let started = Instant::now();
            pace.step();
            started.elapsed()
        };
        let pace = queue.pace();
        queue.set_busy(Busy::Recording);
        // The first step starts the clock, however long the job waited.
        thread::sleep(work);
        assert!(step(&pace) < work);
        // Then a step rests as long as the job worked.
        thread::sleep(work);
        assert!(step(&pace) >= work);
        // Unless the engine stopped recording.
        queue.set_busy(Busy::Playing);
        thread::sleep(work);
        assert!(step(&pace) < work);
    }

    #[test]
    fn jobs_go_on_in_pieces_with_urgent_jobs_between() {
        let queue = Queue::new(1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let (started, wait) = mpsc::channel::<()>();
        let (release, released) = mpsc::channel::<()>();
        /// A piece of a long job, which adds the next.
        fn piece(n: usize, adder: Adder, order: Arc<Mutex<Vec<String>>>) {
            order.lock().unwrap().push(format!("piece {n}"));
            if n < 3 {
                let next = adder.clone();
                adder.add(Priority::Later, move || piece(n + 1, next, order));
            }
        }
        let (adder, pieces) = (queue.adder(), order.clone());
        queue.add(Priority::Later, move || {
            started.send(()).unwrap();
            released.recv().unwrap();
            piece(1, adder, pieces);
        });
        wait.recv().unwrap();
        let previews = order.clone();
        queue.add(Priority::Now, move || {
            previews.lock().unwrap().push("preview".to_owned())
        });
        release.send(()).unwrap();
        queue.wait();
        assert_eq!(
            *order.lock().unwrap(),
            ["piece 1", "preview", "piece 2", "piece 3"]
        );

        // A queue that stops drops what's waiting, and adds nothing more.
        let (release, released) = mpsc::channel::<()>();
        let (adder, dropped) = (queue.adder(), Arc::new(()));
        let held = dropped.clone();
        queue.add(Priority::Now, move || released.recv().unwrap());
        queue.add(Priority::Later, move || drop(held));
        release.send(()).unwrap();
        drop(queue);
        adder.add(Priority::Now, || unreachable!("the queue stopped"));
        assert_eq!(Arc::strong_count(&dropped), 1);
    }

    #[test]
    fn a_job_that_panics_leaves_the_queue_working() {
        let queue = Queue::new(1);
        queue.add(Priority::Now, || panic!("a broken file"));
        let (done, finished) = mpsc::channel();
        queue.add(Priority::Now, move || done.send(()).unwrap());
        assert_eq!(finished.recv_timeout(Duration::from_secs(10)), Ok(()));
        queue.wait();
    }
}
