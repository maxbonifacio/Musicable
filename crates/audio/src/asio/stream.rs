//! The host thread: loads the driver, sets the stream up, and serves the
//! driver's requests while it plays.
//!
//! Drivers take every call but the real-time ones from the thread that
//! loaded them, in a single-threaded COM apartment, so each stream has a
//! thread of its own for them, named `musicable-asio`. The driver calls back
//! on a thread of its own to have each buffer half filled, and may make the
//! first calls from inside `ASIOStart`, on the host thread.

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use musicable_engine::Engine;

use super::context::{self, CALLBACKS, Context, Published, Shared, TryLock};
use super::convert::Encoding;
use super::driver::{BufferSizes, Driver};
use super::platform::{self, Apartment};
use super::render::{Output, Render};
use super::sys::{BufferInfo, Long};
use crate::{
    Api, AsioCapabilities, Capabilities, Device, Error, StartError, Stopped, StreamInfo,
    StreamRequest, StreamStats,
};

/// Loads and initializes a driver, on the thread that is to use it.
pub(super) type Loader = Box<dyn FnOnce() -> Result<Driver, Error> + Send>;

/// How long the host thread waits between checks on the stream.
const WAIT: Duration = Duration::from_millis(250);
/// How long a driver can go without a callback before the stream gives up
/// on it.
const GIVE_UP: Duration = Duration::from_secs(2);

/// The rates a driver is offered to switch to: those WASAPI's exclusive mode
/// offers.
const COMMON_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// How many outputs a stream plays through: the driver's first two, which
/// drivers make their most useful pair, analog if they have any.
const OUTPUTS: u32 = 2;

/// Whether an ASIO stream is open in this process: there can only be one.
static OPEN: AtomicBool = AtomicBool::new(false);

/// Held while a driver loads and sets up, so a question about a driver
/// waits for a stream that's opening it, instead of loading it again.
static LOADING: Mutex<()> = Mutex::new(());

/// The open stream's driver, and what it supports, so asking about it
/// doesn't load it a second time.
static PLAYING: Mutex<Option<(String, Capabilities)>> = Mutex::new(None);

/// The process's one ASIO stream, while the value lives.
struct Claim;

impl Claim {
    fn take() -> Result<Claim, Error> {
        if OPEN.swap(true, Ordering::AcqRel) {
            return Err(Error::Os {
                call: "opening an ASIO driver",
                message: "another ASIO stream is open, and there can only be one".into(),
            });
        }
        Ok(Claim)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        *PLAYING.lock().unwrap_or_else(PoisonError::into_inner) = None;
        OPEN.store(false, Ordering::Release);
    }
}

/// What the driver `id` names supports. The open stream's driver answers
/// from what it said as the stream opened. Any other is loaded to ask, on a
/// thread of its own.
pub(super) fn capabilities(id: &str, load: Loader) -> Result<Capabilities, Error> {
    let _loading = LOADING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((playing, capabilities)) = &*PLAYING.lock().unwrap_or_else(PoisonError::into_inner)
        && playing == id
    {
        return Ok(capabilities.clone());
    }
    thread::scope(|scope| {
        let thread = thread::Builder::new()
            .name("musicable-asio-query".into())
            .spawn_scoped(scope, || {
                let _apartment = Apartment::enter()?;
                describe(&load()?)
            })
            .map_err(|e| Error::Os {
                call: "spawning a thread",
                message: e.to_string(),
            })?;
        thread
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

fn describe(driver: &Driver) -> Result<Capabilities, Error> {
    let outputs = driver.outputs()?;
    let rate = driver.sample_rate()?;
    let sizes = driver.buffer_sizes()?;
    let sample_rates = COMMON_RATES
        .into_iter()
        .filter(|&common| common == rate || driver.can_sample_rate(common))
        .collect();
    Ok(Capabilities {
        sample_rate: rate,
        channels: outputs.min(OUTPUTS) as u16,
        min_period: Some(sizes.min),
        default_period: Some(sizes.preferred),
        exclusive: None,
        asio: Some(AsioCapabilities {
            sample_rates,
            buffer_sizes: sizes.offered(),
            reports_dropouts: driver.reports_overloads(),
        }),
    })
}

/// Opens the driver `load` loads, as `device`.
pub(super) fn open(
    request: &StreamRequest,
    device: Device,
    load: Loader,
) -> Result<(StreamInfo, OpenStream), Error> {
    open_with(request, device, load, GIVE_UP)
}

/// [`open`], giving up on a driver that stops calling back after `give_up`.
pub(super) fn open_with(
    request: &StreamRequest,
    device: Device,
    load: Loader,
    give_up: Duration,
) -> Result<(StreamInfo, OpenStream), Error> {
    let claim = Claim::take()?;
    let shared = Arc::new(Shared::new()?);
    let (opened_tx, opened) = mpsc::sync_channel(1);
    let (start, start_rx) = mpsc::sync_channel(1);
    let (started_tx, started) = mpsc::sync_channel(1);
    let host = HostThread {
        request: request.clone(),
        device,
        load,
        shared: shared.clone(),
        give_up,
        opened: opened_tx,
        start: start_rx,
        started: started_tx,
    };
    let thread = thread::Builder::new()
        .name("musicable-asio".into())
        .spawn(move || {
            let _claim = claim;
            host.run()
        })
        .map_err(|e| Error::Os {
            call: "spawning the ASIO thread",
            message: e.to_string(),
        })?;
    match opened.recv() {
        Ok(Ok(info)) => Ok((
            info,
            OpenStream {
                thread: Some(thread),
                start: Some(start),
                started,
                shared,
            },
        )),
        Ok(Err(error)) => {
            join(thread);
            Err(error)
        }
        Err(_) => {
            // Only a panic ends the thread without a word, and joining
            // passes the panic on.
            join(thread);
            unreachable!("the ASIO thread reports whether the driver opened")
        }
    }
}

/// What the host thread hands back when it ends.
#[derive(Default)]
struct Ended {
    engine: Option<Engine>,
    error: Option<Error>,
}

struct HostThread {
    request: StreamRequest,
    device: Device,
    load: Loader,
    shared: Arc<Shared>,
    give_up: Duration,
    opened: SyncSender<Result<StreamInfo, Error>>,
    start: Receiver<Engine>,
    started: SyncSender<Result<(), Error>>,
}

impl HostThread {
    fn run(self) -> Ended {
        let _apartment = match Apartment::enter() {
            Ok(apartment) => apartment,
            Err(error) => {
                let _ = self.opened.send(Err(error));
                return Ended::default();
            }
        };
        let opened = {
            let _loading = LOADING.lock().unwrap_or_else(PoisonError::into_inner);
            Session::open(&self.request, self.device, self.load, &self.shared)
        };
        let mut session = match opened {
            Ok(session) => session,
            Err(error) => {
                let _ = self.opened.send(Err(error));
                return Ended::default();
            }
        };
        if self.opened.send(Ok(session.info.clone())).is_err() {
            return Ended::default();
        }
        // Wait for the engine. Closing the stream without starting it drops
        // the sender instead.
        let Ok(engine) = self.start.recv() else {
            return Ended::default();
        };
        if let Err(error) = session.start(engine) {
            let engine = session.take_engine();
            drop(session);
            let _ = self.started.send(Err(error));
            return Ended {
                engine,
                error: None,
            };
        }
        let _ = self.started.send(Ok(()));
        let error = session.serve(self.give_up).err();
        let engine = session.stop();
        // The driver is closed before the stream says it ended.
        drop(session);
        self.shared.counters.end();
        Ended { engine, error }
    }
}

/// A driver with its buffers created, and the stream's context published to
/// the callbacks. Dropping it stops the driver if it's running, disposes of
/// the buffers, takes the context back from the callbacks and releases the
/// driver, in that order.
struct Session {
    info: StreamInfo,
    published: Option<Published>,
    driver: Driver,
    running: bool,
}

impl Session {
    fn open(
        request: &StreamRequest,
        device: Device,
        load: Loader,
        shared: &Arc<Shared>,
    ) -> Result<Session, Error> {
        let driver = load()?;
        let outputs = driver.outputs()?.min(OUTPUTS);
        if outputs == 0 {
            return Err(Error::Os {
                call: "ASIOGetChannels",
                message: "the driver has no outputs".into(),
            });
        }
        let rate = set_rate(&driver, request.sample_rate)?;
        let sizes = driver.buffer_sizes()?;
        let encodings = (0..outputs)
            .map(|channel| {
                Encoding::from_type(driver.output_type(channel)?).ok_or(Error::UnsupportedFormat)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let capabilities = describe(&driver)?;
        let output_ready = driver.takes_output_ready();
        shared.set_rate(rate);
        let published = Published::new(Context {
            shared: shared.clone(),
            render: TryLock::new(None),
        })?;
        let (period, buffers) = create_buffers(&driver, outputs, &sizes, request.period)?;
        let mut session = Session {
            info: StreamInfo {
                device,
                api: Api::Asio,
                sample_rate: rate,
                channels: outputs as u16,
                format: encodings[0].format(),
                period,
                buffer: period,
                latency: period,
                // The driver's own thread, which ASIO has drivers run at
                // real-time priority.
                realtime: true,
            },
            published: Some(published),
            driver,
            running: false,
        };
        let driver_reports = session.driver.reports_overloads();
        if let Ok(latency) = session.driver.output_latency()
            && latency > 0
        {
            session.info.latency = latency;
        }
        shared.set_latency(session.info.latency);

        let outputs: Vec<Output> = buffers
            .iter()
            .zip(encodings)
            .map(|(info, encoding)| Output {
                halves: info.buffers.map(|half| half.cast::<u8>()),
                encoding,
            })
            .collect();
        // The device plays one half before the first callback asks for
        // anything: silence, in both.
        for output in &outputs {
            for half in output.halves {
                // SAFETY: a half the driver just handed over, `period`
                // samples in the channel's encoding.
                unsafe { ptr::write_bytes(half, 0, period as usize * output.encoding.bytes()) };
            }
        }
        let render = Render::new(
            session.driver.handle(),
            outputs,
            period,
            rate,
            output_ready,
            driver_reports,
        );
        *session.context().render.lock() = Some(render);
        *PLAYING.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((session.info.device.id.clone(), capabilities));
        Ok(session)
    }

    fn context(&self) -> &Context {
        self.published
            .as_ref()
            .expect("an open session is published")
    }

    /// Starts the stream, playing what `engine` renders. If it fails, the
    /// engine is still there to take back.
    fn start(&mut self, engine: Engine) -> Result<(), Error> {
        if let Some(render) = self.context().render.lock().as_mut() {
            render.engine = Some(engine);
        }
        // What came up while setting up, such as a reset after a change of
        // rate, is dealt with.
        let shared = &self.context().shared;
        shared.take_requests();
        shared.take_reported();
        self.driver.start()?;
        self.running = true;
        Ok(())
    }

    fn take_engine(&self) -> Option<Engine> {
        let mut render = self.context().render.lock();
        render.as_mut().and_then(|render| render.engine.take())
    }

    /// Serves requests until the stream is stopped, the driver asks to be
    /// reset, or it stops calling back.
    fn serve(&self, give_up: Duration) -> Result<(), Error> {
        let shared = &self.context().shared;
        let mut callbacks = shared.counters.snapshot().wakeups;
        let mut heard = Instant::now();
        loop {
            shared.wait(WAIT);
            let requests = shared.take_requests();
            if requests & context::STOP != 0 {
                return Ok(());
            }
            if requests & context::RESET != 0 {
                return Err(Error::DriverReset);
            }
            if requests & context::LATENCIES != 0
                && let Ok(latency) = self.driver.output_latency()
            {
                shared.set_latency(latency);
            }
            if requests & context::SETTINGS != 0 {
                platform::allow_foreground();
                // Some drivers wait until the user closes their settings.
                let _ = self.driver.control_panel();
                heard = Instant::now();
                continue;
            }
            let now = shared.counters.snapshot().wakeups;
            if now != callbacks {
                callbacks = now;
                heard = Instant::now();
            } else if heard.elapsed() >= give_up {
                return Err(Error::Timeout);
            }
        }
    }

    /// Stops the stream, and takes the engine back.
    fn stop(&mut self) -> Option<Engine> {
        if std::mem::take(&mut self.running) {
            // After this, the driver makes no more buffer switch calls.
            let _ = self.driver.stop();
        }
        self.take_engine()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.running {
            let _ = self.driver.stop();
        }
        if let Some(published) = &self.published {
            // The callbacks' state goes before the driver it calls, and the
            // engine with it if it's still there: here, not on the driver's
            // thread.
            published.render.lock().take();
        }
        let _ = self.driver.dispose_buffers();
        // No callback can reach the context once this returns. The driver is
        // released after it, as its field drops.
        self.published.take();
    }
}

/// Sets the driver to the requested rate if it doesn't run at it, and
/// returns the rate it runs at.
fn set_rate(driver: &Driver, requested: Option<u32>) -> Result<u32, Error> {
    let rate = driver.sample_rate()?;
    let Some(requested) = requested.filter(|&requested| requested != rate) else {
        return Ok(rate);
    };
    if !driver.can_sample_rate(requested) {
        return Err(Error::SampleRate {
            requested,
            device: rate,
        });
    }
    driver.set_sample_rate(requested)?;
    match driver.sample_rate()? {
        rate if rate == requested => Ok(rate),
        rate => Err(Error::SampleRate {
            requested,
            device: rate,
        }),
    }
}

/// Asks the driver for the first `outputs` outputs' buffers, at the size the
/// request picks, or at the driver's preferred size if it won't take that
/// one. Returns the size, and the buffers.
fn create_buffers(
    driver: &Driver,
    outputs: u32,
    sizes: &BufferSizes,
    requested: Option<u32>,
) -> Result<(u32, Vec<BufferInfo>), Error> {
    let infos = || -> Vec<BufferInfo> {
        (0..outputs)
            .map(|channel| BufferInfo {
                is_input: 0,
                channel: channel as Long,
                buffers: [ptr::null_mut(); 2],
            })
            .collect()
    };
    let mut period = sizes.pick(requested);
    let mut buffers = infos();
    if let Err(error) = driver.create_buffers(&mut buffers, period, &CALLBACKS) {
        if period == sizes.preferred {
            return Err(error);
        }
        // A driver that reports sizes it doesn't take still takes the one
        // it prefers.
        period = sizes.preferred;
        buffers = infos();
        driver.create_buffers(&mut buffers, period, &CALLBACKS)?;
    }
    if buffers
        .iter()
        .any(|info| info.buffers.iter().any(|half| half.is_null()))
    {
        let _ = driver.dispose_buffers();
        return Err(Error::Os {
            call: "ASIOCreateBuffers",
            message: "the driver handed back no buffers".into(),
        });
    }
    Ok((period, buffers))
}

fn join(thread: JoinHandle<Ended>) -> Ended {
    thread
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

pub(crate) struct OpenStream {
    thread: Option<JoinHandle<Ended>>,
    start: Option<SyncSender<Engine>>,
    started: Receiver<Result<(), Error>>,
    shared: Arc<Shared>,
}

impl OpenStream {
    pub fn start(mut self, engine: Engine) -> Result<Stream, StartError> {
        let thread = self.thread.take().expect("an open stream has its thread");
        let start = self.start.take().expect("an open stream hasn't started");
        if let Err(mpsc::SendError(engine)) = start.send(engine) {
            // The thread is gone, which only a panic does: joining passes it
            // on.
            join(thread);
            return Err(StartError {
                engine: Box::new(engine),
                error: Error::Os {
                    call: "starting the stream",
                    message: "the ASIO thread ended".into(),
                },
            });
        }
        match self.started.recv() {
            Ok(Ok(())) => Ok(Stream {
                thread: Some(thread),
                shared: self.shared.clone(),
            }),
            Ok(Err(error)) => {
                let engine = join(thread)
                    .engine
                    .expect("the ASIO thread returns the engine");
                Err(StartError {
                    engine: Box::new(engine),
                    error,
                })
            }
            Err(_) => {
                join(thread);
                unreachable!("the ASIO thread reports whether the stream started")
            }
        }
    }
}

impl Drop for OpenStream {
    fn drop(&mut self) {
        // Dropping the sender tells the host thread to close the driver.
        drop(self.start.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) struct Stream {
    thread: Option<JoinHandle<Ended>>,
    shared: Arc<Shared>,
}

impl Stream {
    pub fn stats(&self) -> StreamStats {
        self.shared.counters.snapshot()
    }

    pub fn has_ended(&self) -> bool {
        self.shared.counters.ended()
    }

    /// Opens the driver's own settings.
    pub fn show_driver_settings(&self) {
        self.shared.request(context::SETTINGS);
    }

    pub fn stop(mut self) -> Stopped {
        self.shared.request(context::STOP);
        let ended = join(
            self.thread
                .take()
                .expect("a stream has its thread until stopped"),
        );
        Stopped {
            engine: ended.engine.expect("the ASIO thread returns the engine"),
            error: ended.error,
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.shared.request(context::STOP);
            // The engine comes back with the thread and is dropped here.
            let _ = thread.join();
        }
    }
}
