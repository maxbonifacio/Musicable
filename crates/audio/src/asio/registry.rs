//! Finding the installed drivers, and loading one: Windows lists ASIO
//! drivers in the registry, and each is a COM class.

use std::mem::ManuallyDrop;
use std::ptr::NonNull;

use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSIDFromString, CoCreateInstanceEx, MULTI_QI,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE, KEY_READ, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
use windows::core::{GUID, HRESULT, HSTRING, Interface, PCWSTR, PWSTR, w};

use super::driver::Driver;
use super::stream::{self, OpenStream};
use super::sys::Object;
use crate::{ASIO_ID_PREFIX, Capabilities, Device, DriverType, Error, StreamInfo, StreamRequest};

/// A driver as the registry lists it.
struct Installed {
    id: String,
    name: String,
    class: GUID,
}

impl Installed {
    fn device(&self) -> Device {
        Device {
            id: self.id.clone(),
            name: self.name.clone(),
            is_default: false,
            driver_type: DriverType::Asio,
        }
    }

    fn loader(&self) -> stream::Loader {
        let class = self.class;
        Box::new(move || load(&class))
    }
}

/// The installed drivers, as devices.
pub(crate) fn output_devices() -> Vec<Device> {
    installed().iter().map(Installed::device).collect()
}

pub(crate) fn capabilities(id: &str) -> Result<Capabilities, Error> {
    let driver = find(id)?;
    stream::capabilities(&driver.id, driver.loader())
}

pub(crate) fn open(id: &str, request: &StreamRequest) -> Result<(StreamInfo, OpenStream), Error> {
    let driver = find(id)?;
    stream::open(request, driver.device(), driver.loader())
}

fn find(id: &str) -> Result<Installed, Error> {
    installed()
        .into_iter()
        .find(|driver| driver.id.eq_ignore_ascii_case(id))
        .ok_or(Error::DeviceNotFound)
}

/// The drivers under `HKEY_LOCAL_MACHINE\SOFTWARE\ASIO` that are installed
/// for 64-bit apps. Drivers installed for 32-bit apps only list themselves
/// there too, through the 32-bit view, but have no class in ours.
fn installed() -> Vec<Installed> {
    let mut key = HKEY::default();
    // SAFETY: opens a key, closed below.
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\ASIO"),
            None,
            KEY_READ,
            &mut key,
        )
    };
    if opened != ERROR_SUCCESS {
        // No drivers installed.
        return Vec::new();
    }
    let mut drivers: Vec<Installed> = Vec::new();
    for index in 0..1024 {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        // SAFETY: an open key, and a buffer of `length` characters.
        let found = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if found == ERROR_NO_MORE_ITEMS {
            break;
        }
        if found != ERROR_SUCCESS {
            continue;
        }
        let subkey = &name[..length as usize];
        if let Some(driver) = read(key, subkey)
            && !drivers.iter().any(|known| known.id == driver.id)
        {
            drivers.push(driver);
        }
    }
    // SAFETY: opened above, and closed once.
    let _ = unsafe { RegCloseKey(key) };
    drivers
}

/// The driver listed under `subkey`, if it's installed for us.
fn read(key: HKEY, subkey: &[u16]) -> Option<Installed> {
    let subkey_name = HSTRING::from_wide(subkey);
    let class_text = read_string(key, &subkey_name, w!("CLSID"))?;
    let class_text = HSTRING::from(class_text.trim());
    // SAFETY: a NUL-terminated string.
    let class = unsafe { CLSIDFromString(&class_text) }.ok()?;
    let id = format!("{ASIO_ID_PREFIX}{}", braced(&class));
    let server = HSTRING::from(format!("CLSID\\{}\\InprocServer32", braced(&class)));
    if read_string(HKEY_CLASSES_ROOT, &server, PCWSTR::null())?.is_empty() {
        return None;
    }
    let name = read_string(key, &subkey_name, w!("Description"))
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| String::from_utf16_lossy(subkey));
    Some(Installed {
        id,
        name: name.trim().to_owned(),
        class,
    })
}

/// A string value from the registry, or `None` if there isn't one.
fn read_string(key: HKEY, subkey: &HSTRING, value: PCWSTR) -> Option<String> {
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut bytes = 0u32;
    // SAFETY: asks for the value's size.
    let sized = unsafe { RegGetValueW(key, subkey, value, flags, None, None, Some(&mut bytes)) };
    if sized != ERROR_SUCCESS {
        return None;
    }
    let mut text = vec![0u16; (bytes as usize).div_ceil(2) + 1];
    let mut bytes = (text.len() * 2) as u32;
    // SAFETY: a buffer of `bytes` bytes.
    let read = unsafe {
        RegGetValueW(
            key,
            subkey,
            value,
            flags,
            None,
            Some(text.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    };
    if read != ERROR_SUCCESS {
        return None;
    }
    let length = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    Some(String::from_utf16_lossy(&text[..length]))
}

/// A class id as the registry writes it: `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`.
fn braced(class: &GUID) -> String {
    let d = class.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        class.data1, class.data2, class.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

/// Loads the driver with class `class`, on this thread, and initializes it.
fn load(class: &GUID) -> Result<Driver, Error> {
    // A driver's interface has its class's id.
    let mut results = [MULTI_QI {
        pIID: class,
        pItf: ManuallyDrop::new(None),
        hr: HRESULT(0),
    }];
    // SAFETY: creates the class, asking for one interface.
    unsafe { CoCreateInstanceEx(class, None, CLSCTX_INPROC_SERVER, None, &mut results) }
        .map_err(|e| os("CoCreateInstanceEx", &e))?;
    // SAFETY: filled in by the call that just succeeded, and taken once.
    let interface = unsafe { ManuallyDrop::take(&mut results[0].pItf) };
    results[0]
        .hr
        .ok()
        .map_err(|e| os("CoCreateInstanceEx", &e))?;
    let object = interface
        .map(Interface::into_raw)
        .and_then(|raw| NonNull::new(raw.cast::<Object>()))
        .ok_or_else(|| Error::Os {
            call: "CoCreateInstanceEx",
            message: "the driver handed back nothing".into(),
        })?;
    // Drivers use the window to show their own settings over.
    // SAFETY: always succeeds.
    let window = unsafe { GetDesktopWindow() };
    // SAFETY: an ASIO driver object, and its reference is ours.
    unsafe { Driver::init(object, window.0) }
}

/// Our error for a failed system call.
pub(super) fn os(call: &'static str, error: &windows::core::Error) -> Error {
    Error::Os {
        call,
        message: format!("{} ({:#010X})", error.message(), error.code().0 as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_read_as_the_registry_writes_them() {
        let class = GUID::from_u128(0x1234_5678_9abc_def0_1122_3344_5566_7788);
        assert_eq!(braced(&class), "{12345678-9ABC-DEF0-1122-334455667788}");
    }

    #[test]
    fn listing_drivers_works_with_none_installed() {
        // Whatever is installed, none has this class.
        let error = find("asio:{00000000-0000-0000-0000-000000000000}").err();
        assert!(matches!(error, Some(Error::DeviceNotFound)), "{error:?}");
        for device in output_devices() {
            assert!(device.id.starts_with(ASIO_ID_PREFIX), "{device:?}");
            assert_eq!(device.driver_type, DriverType::Asio);
        }
    }
}
