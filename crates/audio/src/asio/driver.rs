//! A loaded driver: its methods, with our errors.

use std::ffi::{CStr, c_char, c_void};
use std::iter::successors;
use std::ptr::{self, NonNull};

use super::sys::{self, BufferInfo, Callbacks, ChannelInfo, Code, Object, Vtable, Wide};
use crate::Error;

/// The largest buffer size worth listing, in frames.
const MAX_BUFFER: u32 = 1 << 16;

/// Above this many sizes, only some are worth offering.
const MANY_SIZES: usize = 12;

/// An initialized driver. It stays on the thread that loaded it: drivers take
/// every call but the real-time ones from that thread.
pub(super) struct Driver {
    object: NonNull<Object>,
}

impl Driver {
    /// Initializes the driver object `object`, and takes over its reference.
    ///
    /// # Safety
    ///
    /// `object` points to an ASIO driver object, and the reference to it is
    /// the caller's to hand over.
    pub unsafe fn init(object: NonNull<Object>, system: *mut c_void) -> Result<Driver, Error> {
        let driver = Driver { object };
        // SAFETY: a driver object, initialized once.
        if unsafe { (driver.vtable().init)(driver.this(), system) } == 0 {
            let said = driver.error_message();
            return Err(Error::Os {
                call: "initializing the ASIO driver",
                message: if said.is_empty() {
                    "the driver didn't say why".to_owned()
                } else {
                    said
                },
            });
        }
        Ok(driver)
    }

    fn this(&self) -> *mut Object {
        self.object.as_ptr()
    }

    fn vtable(&self) -> &Vtable {
        // SAFETY: a driver object's vtable lives as long as the object.
        unsafe { &*(*self.this()).vtable }
    }

    /// What the driver says went wrong last, if anything.
    pub fn error_message(&self) -> String {
        // The specification allows 124 bytes: leave plenty more.
        let mut text = [0 as c_char; 512];
        // SAFETY: the driver writes a NUL-terminated string that fits.
        unsafe { (self.vtable().error_message)(self.this(), text.as_mut_ptr()) };
        text[text.len() - 1] = 0;
        // SAFETY: NUL-terminated, as just made sure.
        let text = unsafe { CStr::from_ptr(text.as_ptr()) };
        text.to_string_lossy().trim().to_owned()
    }

    /// The number of input and output channels.
    pub fn channels(&self) -> Result<(u32, u32), Error> {
        let (mut inputs, mut outputs) = (0, 0);
        // SAFETY: an initialized driver, writing two longs.
        let code = unsafe { (self.vtable().channels)(self.this(), &mut inputs, &mut outputs) };
        self.check(code, "ASIOGetChannels")?;
        Ok((inputs.max(0) as u32, outputs.max(0) as u32))
    }

    pub fn buffer_sizes(&self) -> Result<BufferSizes, Error> {
        let (mut min, mut max, mut preferred, mut granularity) = (0, 0, 0, 0);
        // SAFETY: an initialized driver, writing four longs.
        let code = unsafe {
            (self.vtable().buffer_size)(
                self.this(),
                &mut min,
                &mut max,
                &mut preferred,
                &mut granularity,
            )
        };
        self.check(code, "ASIOGetBufferSize")?;
        Ok(BufferSizes::new(min, max, preferred, granularity))
    }

    pub fn can_sample_rate(&self, rate: u32) -> bool {
        // SAFETY: an initialized driver.
        unsafe { (self.vtable().can_sample_rate)(self.this(), f64::from(rate)) == sys::OK }
    }

    /// The rate the driver runs at now.
    pub fn sample_rate(&self) -> Result<u32, Error> {
        let mut rate = 0.0;
        // SAFETY: an initialized driver, writing a double.
        let code = unsafe { (self.vtable().sample_rate)(self.this(), &mut rate) };
        self.check(code, "ASIOGetSampleRate")?;
        if rate.is_finite() && rate >= 1.0 {
            Ok(rate.round() as u32)
        } else {
            Err(Error::Os {
                call: "ASIOGetSampleRate",
                message: format!("the driver reported a rate of {rate} Hz"),
            })
        }
    }

    pub fn set_sample_rate(&self, rate: u32) -> Result<(), Error> {
        // SAFETY: an initialized driver.
        let code = unsafe { (self.vtable().set_sample_rate)(self.this(), f64::from(rate)) };
        self.check(code, "ASIOSetSampleRate")
    }

    /// The sample type of output channel `channel`.
    pub fn output_type(&self, channel: u32) -> Result<sys::Long, Error> {
        self.channel_type(channel, false)
    }

    /// The sample type of input channel `channel`.
    pub fn input_type(&self, channel: u32) -> Result<sys::Long, Error> {
        self.channel_type(channel, true)
    }

    fn channel_type(&self, channel: u32, input: bool) -> Result<sys::Long, Error> {
        let mut info = ChannelInfo {
            channel: channel as sys::Long,
            is_input: sys::Bool::from(input),
            is_active: 0,
            group: 0,
            sample_type: -1,
            name: [0; 32],
        };
        // SAFETY: an initialized driver, filling in the structure.
        let code = unsafe { (self.vtable().channel_info)(self.this(), &mut info) };
        self.check(code, "ASIOGetChannelInfo")?;
        Ok(info.sample_type)
    }

    /// Asks the driver for the buffers in `infos`, each `size` frames long,
    /// and hands it `callbacks` to call from then on.
    pub fn create_buffers(
        &self,
        infos: &mut [BufferInfo],
        size: u32,
        callbacks: &'static Callbacks,
    ) -> Result<(), Error> {
        let count = sys::Long::try_from(infos.len()).unwrap_or(sys::Long::MAX);
        // SAFETY: an initialized driver; it fills in the buffer pointers, and
        // keeps the callbacks, which live forever.
        let code = unsafe {
            (self.vtable().create_buffers)(
                self.this(),
                infos.as_mut_ptr(),
                count,
                size as sys::Long,
                callbacks,
            )
        };
        self.check(code, "ASIOCreateBuffers")
    }

    pub fn dispose_buffers(&self) -> Result<(), Error> {
        // SAFETY: an initialized driver.
        let code = unsafe { (self.vtable().dispose_buffers)(self.this()) };
        self.check(code, "ASIODisposeBuffers")
    }

    pub fn start(&self) -> Result<(), Error> {
        // SAFETY: an initialized driver.
        let code = unsafe { (self.vtable().start)(self.this()) };
        self.check(code, "ASIOStart")
    }

    /// Stops the stream. Once it returns, the driver makes no more buffer
    /// switch calls.
    pub fn stop(&self) -> Result<(), Error> {
        // SAFETY: an initialized driver.
        let code = unsafe { (self.vtable().stop)(self.this()) };
        self.check(code, "ASIOStop")
    }

    /// The input and output latencies in frames: how old the first frame of
    /// an input half is at the buffer switch that hands it over, and how
    /// long after a buffer switch the half the host fills then plays.
    pub fn latencies(&self) -> Result<(u32, u32), Error> {
        let (mut input, mut output) = (0, 0);
        // SAFETY: an initialized driver, writing two longs.
        let code = unsafe { (self.vtable().latencies)(self.this(), &mut input, &mut output) };
        self.check(code, "ASIOGetLatencies")?;
        Ok((input.max(0) as u32, output.max(0) as u32))
    }

    /// Whether the driver reports every dropout itself.
    pub fn reports_overloads(&self) -> bool {
        // SAFETY: an initialized driver; the request takes no parameters.
        let code = unsafe {
            (self.vtable().future)(
                self.this(),
                sys::future::CAN_REPORT_OVERLOAD,
                ptr::null_mut(),
            )
        };
        code == sys::SUCCESS
    }

    /// Whether the driver wants to hear when each half is written, which
    /// some use to play it sooner. Asking is how hosts find out.
    pub fn takes_output_ready(&self) -> bool {
        // SAFETY: an initialized driver.
        unsafe { (self.vtable().output_ready)(self.this()) == sys::OK }
    }

    /// Shows the driver's own settings, until the user closes them if the
    /// driver waits for that.
    pub fn control_panel(&self) -> Result<(), Error> {
        // SAFETY: an initialized driver.
        let code = unsafe { (self.vtable().control_panel)(self.this()) };
        self.check(code, "ASIOControlPanel")
    }

    /// For the callbacks to call the driver.
    pub fn handle(&self) -> Handle {
        Handle(self.object)
    }

    fn check(&self, code: Code, call: &'static str) -> Result<(), Error> {
        match code {
            sys::OK => Ok(()),
            sys::NOT_PRESENT => Err(Error::DeviceNotFound),
            _ => Err(Error::Os {
                call,
                message: describe(code, &self.error_message()),
            }),
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        // SAFETY: the reference handed over in `init`, released once.
        unsafe { (self.vtable().release)(self.this()) };
    }
}

/// What an error code means, and what the driver said about it.
fn describe(code: Code, said: &str) -> String {
    let meaning = match code {
        sys::HW_MALFUNCTION => "the hardware is malfunctioning",
        sys::INVALID_PARAMETER => "the driver refused a parameter",
        sys::INVALID_MODE => "the driver can't do that in its current mode",
        sys::SP_NOT_ADVANCING => "the hardware isn't running",
        sys::NO_CLOCK => "the driver has no sample clock",
        sys::NO_MEMORY => "the driver ran out of memory",
        _ if said.is_empty() => return format!("error {code}"),
        _ => return format!("{said} (error {code})"),
    };
    if said.is_empty() {
        meaning.to_owned()
    } else {
        format!("{meaning}: {said}")
    }
}

/// The driver's real-time methods, for the callbacks.
#[derive(Debug, Clone, Copy)]
pub(super) struct Handle(NonNull<Object>);

// SAFETY: drivers take these calls on their callback thread, whichever it is.
unsafe impl Send for Handle {}

impl Handle {
    /// The sample position at the start of the buffer half the driver asks
    /// for now, if it knows it. Real-time safe.
    ///
    /// # Safety
    ///
    /// The driver hasn't been released.
    pub unsafe fn sample_position(self) -> Option<u64> {
        let (mut position, mut time) = (Wide::default(), Wide::default());
        // SAFETY: a loaded driver, as the caller promised, writing two pairs.
        let code = unsafe {
            let this = self.0.as_ptr();
            ((*(*this).vtable).sample_position)(this, &mut position, &mut time)
        };
        (code == sys::OK).then(|| position.get())
    }

    /// Tells the driver the half is written. Real-time safe.
    ///
    /// # Safety
    ///
    /// The driver hasn't been released.
    pub unsafe fn output_ready(self) {
        // SAFETY: a loaded driver, as the caller promised.
        unsafe {
            let this = self.0.as_ptr();
            ((*(*this).vtable).output_ready)(this);
        }
    }
}

/// The buffer sizes a driver takes, from `ASIOGetBufferSize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BufferSizes {
    pub min: u32,
    pub max: u32,
    pub preferred: u32,
    /// The step from one size to the next: -1 to double from `min`, 0 when
    /// the driver only takes `preferred`.
    pub granularity: i32,
}

impl BufferSizes {
    /// Makes sense of what a driver reported. Some report sizes that
    /// contradict each other.
    pub fn new(
        min: sys::Long,
        max: sys::Long,
        preferred: sys::Long,
        granularity: sys::Long,
    ) -> Self {
        let min = min.max(1) as u32;
        let max = (max.max(0) as u32).clamp(min, MAX_BUFFER.max(min));
        let preferred = if preferred > 0 { preferred as u32 } else { min };
        Self {
            min,
            max,
            preferred,
            granularity,
        }
    }

    /// Every size the driver takes, smallest first.
    fn all(&self) -> Vec<u32> {
        let mut sizes: Vec<u32> = match self.granularity {
            -1 => successors(Some(self.min), |n| n.checked_mul(2))
                .take_while(|&n| n <= self.max)
                .collect(),
            step if step > 0 => (self.min..=self.max).step_by(step as usize).collect(),
            _ => Vec::new(),
        };
        sizes.push(self.preferred);
        sizes.sort_unstable();
        sizes.dedup();
        sizes
    }

    /// The sizes worth offering, smallest first: each one the driver takes,
    /// or for a driver that takes many, the powers of two among them, with
    /// its smallest, largest and preferred.
    pub fn offered(&self) -> Vec<u32> {
        let all = self.all();
        if all.len() <= MANY_SIZES {
            return all;
        }
        all.into_iter()
            .filter(|&n| {
                n.is_power_of_two() || n == self.min || n == self.max || n == self.preferred
            })
            .collect()
    }

    /// The size for a request: the preferred one by default, otherwise the
    /// smallest the driver takes from the one requested up, or its largest.
    pub fn pick(&self, requested: Option<u32>) -> u32 {
        let Some(frames) = requested else {
            return self.preferred;
        };
        let all = self.all();
        all.iter()
            .copied()
            .find(|&n| n >= frames)
            .or(all.last().copied())
            .unwrap_or(self.preferred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driver_with_one_size_takes_only_that() {
        // As RME's drivers report: the size set in their own settings.
        let sizes = BufferSizes::new(256, 256, 256, 0);
        assert_eq!(sizes.offered(), [256]);
        assert_eq!(sizes.pick(None), 256);
        assert_eq!(sizes.pick(Some(64)), 256);
        assert_eq!(sizes.pick(Some(1024)), 256);
    }

    #[test]
    fn doubling_sizes_start_from_the_smallest() {
        let sizes = BufferSizes::new(48, 1536, 192, -1);
        assert_eq!(sizes.offered(), [48, 96, 192, 384, 768, 1536]);
        assert_eq!(sizes.pick(None), 192);
        assert_eq!(sizes.pick(Some(100)), 192, "rounded up");
        assert_eq!(sizes.pick(Some(16)), 48, "below the smallest");
        assert_eq!(sizes.pick(Some(4096)), 1536, "above the largest");
    }

    #[test]
    fn sizes_in_steps_offer_the_powers_of_two_when_there_are_many() {
        let sizes = BufferSizes::new(64, 2048, 512, 8);
        assert_eq!(sizes.offered(), [64, 128, 256, 512, 1024, 2048]);
        assert_eq!(sizes.pick(Some(100)), 104, "the next step up");
        assert_eq!(sizes.pick(Some(512)), 512);

        let few = BufferSizes::new(128, 512, 256, 128);
        assert_eq!(few.offered(), [128, 256, 384, 512]);
    }

    #[test]
    fn contradictory_sizes_still_make_sense() {
        // No preferred size, and the largest below the smallest.
        let sizes = BufferSizes::new(128, 64, 0, 0);
        assert_eq!(sizes.offered(), [128]);
        assert_eq!(sizes.pick(Some(32)), 128);
        // A preferred size off the steps is offered too.
        let sizes = BufferSizes::new(64, 256, 100, -1);
        assert_eq!(sizes.offered(), [64, 100, 128, 256]);
        // A step of one across a huge range.
        let sizes = BufferSizes::new(1, sys::Long::MAX, 512, 1);
        assert_eq!(*sizes.offered().last().unwrap(), MAX_BUFFER);
        assert_eq!(sizes.pick(Some(300)), 300);
    }

    #[test]
    fn error_codes_say_what_they_mean() {
        assert_eq!(
            describe(sys::NO_CLOCK, ""),
            "the driver has no sample clock"
        );
        assert_eq!(
            describe(sys::INVALID_MODE, "buffer size not supported"),
            "the driver can't do that in its current mode: buffer size not supported"
        );
        assert_eq!(describe(-5, ""), "error -5");
        assert_eq!(describe(-5, "odd"), "odd (error -5)");
    }
}
