//! The ASIO driver interface: a driver object's methods in vtable order, the
//! structures they take, and the callbacks a driver calls. Written from
//! Steinberg's ASIO 2.3 specification.
//!
//! A driver is a COM object: `IUnknown`'s three methods, then ASIO's own.
//! On 64-bit Windows, COM methods, C++ virtual methods and C functions share
//! one calling convention, so `extern "C"` matches a driver's vtable. 32-bit
//! builds would need `thiscall` for the driver's own methods, and aren't
//! supported.

use std::ffi::{c_char, c_void};

#[cfg(target_pointer_width = "32")]
compile_error!("ASIO support needs a 64-bit build");

/// C's `long`, which is 32 bits on Windows.
pub(super) type Long = i32;
/// `ASIOBool`.
pub(super) type Bool = Long;
/// `ASIOError`: what most methods return.
pub(super) type Code = Long;

pub(super) const OK: Code = 0;
/// What `future` returns when it did what was asked.
pub(super) const SUCCESS: Code = 0x3f48_47a0;
pub(super) const NOT_PRESENT: Code = -1000;
pub(super) const HW_MALFUNCTION: Code = -999;
pub(super) const INVALID_PARAMETER: Code = -998;
pub(super) const INVALID_MODE: Code = -997;
pub(super) const SP_NOT_ADVANCING: Code = -996;
pub(super) const NO_CLOCK: Code = -995;
pub(super) const NO_MEMORY: Code = -994;

/// Sample types (`ASIOSampleType`). MSB types are big-endian, LSB types
/// little-endian. The `INT32_*16` to `INT32_*24` types hold a 16- to 24-bit
/// sample in the low bits of a 32-bit word.
pub(super) mod sample_type {
    use super::Long;

    pub const INT16_MSB: Long = 0;
    pub const INT24_MSB: Long = 1;
    pub const INT32_MSB: Long = 2;
    pub const FLOAT32_MSB: Long = 3;
    pub const FLOAT64_MSB: Long = 4;
    pub const INT32_MSB16: Long = 8;
    pub const INT32_MSB18: Long = 9;
    pub const INT32_MSB20: Long = 10;
    pub const INT32_MSB24: Long = 11;
    pub const INT16_LSB: Long = 16;
    pub const INT24_LSB: Long = 17;
    pub const INT32_LSB: Long = 18;
    pub const FLOAT32_LSB: Long = 19;
    pub const FLOAT64_LSB: Long = 20;
    pub const INT32_LSB16: Long = 24;
    pub const INT32_LSB18: Long = 25;
    pub const INT32_LSB20: Long = 26;
    pub const INT32_LSB24: Long = 27;
}

/// What a driver tells or asks the host through the message callback.
pub(super) mod message {
    use super::Long;

    /// Whether the host handles the selector in `value`.
    pub const SELECTOR_SUPPORTED: Long = 1;
    /// The ASIO version the host implements.
    pub const ENGINE_VERSION: Long = 2;
    /// The driver needs to be reset: closed and opened again.
    pub const RESET_REQUEST: Long = 3;
    /// The buffer size changed. Drivers are told to ask for a reset instead.
    pub const BUFFER_SIZE_CHANGE: Long = 4;
    /// The driver lost audio and its sample position went out of step.
    pub const RESYNC_REQUEST: Long = 5;
    pub const LATENCIES_CHANGED: Long = 6;
    /// Whether the host takes `buffer_switch_time_info`.
    pub const SUPPORTS_TIME_INFO: Long = 7;
    /// The driver saw audio drop out.
    pub const OVERLOAD: Long = 15;
}

/// Requests through the driver's `future` method.
pub(super) mod future {
    use super::Long;

    /// Whether the driver reports every dropout itself, with
    /// [`message::OVERLOAD`](super::message::OVERLOAD).
    pub const CAN_REPORT_OVERLOAD: Long = 0x2404_2012;
}

/// [`TimeInfo::flags`]: the sample position is valid.
pub(super) const SAMPLE_POSITION_VALID: u32 = 1 << 1;

/// A 64-bit count in two 32-bit halves, the high one first: sample positions,
/// and system times in nanoseconds.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Wide {
    pub hi: u32,
    pub lo: u32,
}

impl Wide {
    pub fn get(self) -> u64 {
        (u64::from(self.hi) << 32) | u64::from(self.lo)
    }

    #[cfg(test)]
    pub fn new(value: u64) -> Wide {
        Wide {
            hi: (value >> 32) as u32,
            lo: value as u32,
        }
    }
}

// The structures with doubles in them are packed to 4 bytes, as drivers are
// built with them.

/// `AsioTimeInfo`: the time of the buffer half a callback asks for.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub(super) struct TimeInfo {
    pub speed: f64,
    pub system_time: Wide,
    /// Frames since the stream started, at the start of the half.
    pub sample_position: Wide,
    pub sample_rate: f64,
    pub flags: u32,
    pub reserved: [u8; 12],
}

/// `ASIOTimeCode`, which we don't use.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub(super) struct TimeCode {
    pub speed: f64,
    pub samples: Wide,
    pub flags: u32,
    pub future: [u8; 64],
}

/// `ASIOTime`, which `buffer_switch_time_info` takes.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub(super) struct Time {
    pub reserved: [Long; 4],
    pub info: TimeInfo,
    pub code: TimeCode,
}

/// `ASIOChannelInfo`: the host sets `channel` and `is_input`, the driver the
/// rest.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ChannelInfo {
    pub channel: Long,
    pub is_input: Bool,
    pub is_active: Bool,
    pub group: Long,
    pub sample_type: Long,
    pub name: [c_char; 32],
}

/// `ASIOBufferInfo`: the host sets `is_input` and `channel`, and the driver
/// hands back the channel's two buffer halves.
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct BufferInfo {
    pub is_input: Bool,
    pub channel: Long,
    pub buffers: [*mut c_void; 2],
}

/// `ASIOCallbacks`. The driver keeps a pointer to it until the buffers are
/// disposed of.
#[repr(C)]
pub(super) struct Callbacks {
    /// Fill the output buffer half `index` now.
    pub buffer_switch: unsafe extern "C" fn(index: Long, direct: Bool),
    pub sample_rate_changed: unsafe extern "C" fn(rate: f64),
    pub message: unsafe extern "C" fn(
        selector: Long,
        value: Long,
        message: *mut c_void,
        opt: *mut f64,
    ) -> Long,
    /// `buffer_switch` with the half's time, for hosts that take it.
    pub buffer_switch_time_info:
        unsafe extern "C" fn(time: *mut Time, index: Long, direct: Bool) -> *mut Time,
}

/// A driver object, as COM hands it over: a pointer to its vtable.
#[repr(C)]
pub(super) struct Object {
    pub vtable: *const Vtable,
}

type This = *mut Object;

/// `IUnknown`'s methods, then `IASIO`'s, in their order.
#[repr(C)]
pub(super) struct Vtable {
    pub query_interface:
        unsafe extern "system" fn(this: This, iid: *const c_void, out: *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(this: This) -> u32,
    pub release: unsafe extern "system" fn(this: This) -> u32,
    /// Returns [`TRUE`] when the driver and its hardware are ready.
    pub init: unsafe extern "C" fn(this: This, system: *mut c_void) -> Bool,
    pub driver_name: unsafe extern "C" fn(this: This, name: *mut c_char),
    pub driver_version: unsafe extern "C" fn(this: This) -> Long,
    pub error_message: unsafe extern "C" fn(this: This, text: *mut c_char),
    pub start: unsafe extern "C" fn(this: This) -> Code,
    pub stop: unsafe extern "C" fn(this: This) -> Code,
    pub channels: unsafe extern "C" fn(this: This, inputs: *mut Long, outputs: *mut Long) -> Code,
    pub latencies: unsafe extern "C" fn(this: This, input: *mut Long, output: *mut Long) -> Code,
    pub buffer_size: unsafe extern "C" fn(
        this: This,
        min: *mut Long,
        max: *mut Long,
        preferred: *mut Long,
        granularity: *mut Long,
    ) -> Code,
    pub can_sample_rate: unsafe extern "C" fn(this: This, rate: f64) -> Code,
    pub sample_rate: unsafe extern "C" fn(this: This, rate: *mut f64) -> Code,
    pub set_sample_rate: unsafe extern "C" fn(this: This, rate: f64) -> Code,
    pub clock_sources:
        unsafe extern "C" fn(this: This, clocks: *mut c_void, count: *mut Long) -> Code,
    pub set_clock_source: unsafe extern "C" fn(this: This, index: Long) -> Code,
    pub sample_position:
        unsafe extern "C" fn(this: This, position: *mut Wide, time: *mut Wide) -> Code,
    pub channel_info: unsafe extern "C" fn(this: This, info: *mut ChannelInfo) -> Code,
    pub create_buffers: unsafe extern "C" fn(
        this: This,
        infos: *mut BufferInfo,
        count: Long,
        size: Long,
        callbacks: *const Callbacks,
    ) -> Code,
    pub dispose_buffers: unsafe extern "C" fn(this: This) -> Code,
    pub control_panel: unsafe extern "C" fn(this: This) -> Code,
    pub future: unsafe extern "C" fn(this: This, selector: Long, params: *mut c_void) -> Code,
    pub output_ready: unsafe extern "C" fn(this: This) -> Code,
}

#[cfg(test)]
mod tests {
    use std::mem::{offset_of, size_of};

    use super::*;

    #[test]
    fn structures_have_the_drivers_layout() {
        assert_eq!(size_of::<Wide>(), 8);
        assert_eq!(size_of::<TimeInfo>(), 48);
        assert_eq!(size_of::<TimeCode>(), 84);
        assert_eq!(size_of::<Time>(), 148);
        assert_eq!(offset_of!(Time, info), 16);
        assert_eq!(offset_of!(Time, code), 64);
        assert_eq!(offset_of!(TimeInfo, sample_position), 16);
        assert_eq!(offset_of!(TimeInfo, flags), 32);
        assert_eq!(size_of::<ChannelInfo>(), 52);
        assert_eq!(offset_of!(ChannelInfo, sample_type), 16);
        assert_eq!(size_of::<BufferInfo>(), 24);
        assert_eq!(offset_of!(BufferInfo, buffers), 8);
        assert_eq!(size_of::<Callbacks>(), 32);
    }

    #[test]
    fn the_vtable_has_every_method_in_order() {
        let slot = size_of::<usize>();
        assert_eq!(offset_of!(Vtable, init), 3 * slot);
        assert_eq!(offset_of!(Vtable, start), 7 * slot);
        assert_eq!(offset_of!(Vtable, sample_position), 17 * slot);
        assert_eq!(offset_of!(Vtable, create_buffers), 19 * slot);
        assert_eq!(offset_of!(Vtable, output_ready), 23 * slot);
        assert_eq!(size_of::<Vtable>(), 24 * slot);
    }

    #[test]
    fn wide_counts_put_the_high_half_first() {
        let wide = Wide { hi: 1, lo: 2 };
        assert_eq!(wide.get(), (1 << 32) + 2);
        assert_eq!(Wide::new(wide.get()), wide);
    }
}
