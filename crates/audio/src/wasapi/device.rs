//! Finding output endpoints.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{E_INVALIDARG, ERROR_NOT_FOUND};
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator, eConsole,
    eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::core::{HSTRING, Interface};

use super::client;
use super::com::{TaskMem, in_mta, os};
use crate::{Capabilities, Device, DriverType, Error};

pub(crate) fn output_devices() -> Result<Vec<Device>, Error> {
    in_mta(|| {
        let enumerator = enumerator()?;
        let default = default_id(&enumerator);
        // SAFETY: COM calls on an initialized MTA thread, as for every
        // WASAPI call in this backend.
        let collection = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
            .map_err(|e| os("IMMDeviceEnumerator::EnumAudioEndpoints", e))?;
        let count =
            unsafe { collection.GetCount() }.map_err(|e| os("IMMDeviceCollection::GetCount", e))?;
        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let device = unsafe { collection.Item(index) }
                .map_err(|e| os("IMMDeviceCollection::Item", e))?;
            devices.push(describe(&device, default.as_deref())?);
        }
        Ok(devices)
    })
}

pub(crate) fn capabilities(id: Option<&str>) -> Result<Capabilities, Error> {
    in_mta(|| client::capabilities(&find(&enumerator()?, id)?))
}

pub(super) fn enumerator() -> Result<IMMDeviceEnumerator, Error> {
    // SAFETY: on an MTA thread.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|e| os("CoCreateInstance(MMDeviceEnumerator)", e))
}

/// The output endpoint with this id, or the default output.
pub(super) fn find(enumerator: &IMMDeviceEnumerator, id: Option<&str>) -> Result<IMMDevice, Error> {
    let Some(id) = id else {
        // SAFETY: on an MTA thread.
        return unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.map_err(|e| {
            // There is no default when no output is plugged in.
            if e.code() == ERROR_NOT_FOUND.to_hresult() {
                Error::DeviceNotFound
            } else {
                os("IMMDeviceEnumerator::GetDefaultAudioEndpoint", e)
            }
        });
    };
    // SAFETY: on an MTA thread.
    let device = unsafe { enumerator.GetDevice(&HSTRING::from(id)) }.map_err(|e| {
        // An unknown id is E_NOTFOUND, which `os` maps, or E_INVALIDARG
        // under Wine.
        if e.code() == E_INVALIDARG {
            Error::DeviceNotFound
        } else {
            os("IMMDeviceEnumerator::GetDevice", e)
        }
    })?;
    // `GetDevice` also finds unplugged and disabled endpoints, and inputs.
    let active = unsafe { device.GetState() }.is_ok_and(|state| state == DEVICE_STATE_ACTIVE);
    let output = device
        .cast::<IMMEndpoint>()
        .and_then(|endpoint| unsafe { endpoint.GetDataFlow() })
        .is_ok_and(|flow| flow == eRender);
    if active && output {
        Ok(device)
    } else {
        Err(Error::DeviceNotFound)
    }
}

pub(super) fn default_id(enumerator: &IMMDeviceEnumerator) -> Option<String> {
    // SAFETY: on an MTA thread.
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.ok()?;
    id_of(&device).ok()
}

pub(super) fn describe(device: &IMMDevice, default_id: Option<&str>) -> Result<Device, Error> {
    let id = id_of(device)?;
    let name = friendly_name(device).unwrap_or_else(|| id.clone());
    Ok(Device {
        is_default: default_id == Some(id.as_str()),
        id,
        name,
        driver_type: DriverType::System,
    })
}

fn id_of(device: &IMMDevice) -> Result<String, Error> {
    // SAFETY: on an MTA thread. The string is COM's to free, once.
    let id = unsafe { device.GetId() }.map_err(|e| os("IMMDevice::GetId", e))?;
    let _free = TaskMem(id.0);
    unsafe { id.to_string() }.map_err(|e| Error::Os {
        call: "IMMDevice::GetId",
        message: e.to_string(),
    })
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: on an MTA thread.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;
    // The name is a VT_LPWSTR. Read it directly: Wine lacks the propsys
    // function that formatting a PROPVARIANT calls.
    if value.vt() != VT_LPWSTR {
        return None;
    }
    // SAFETY: a VT_LPWSTR value holds a null-terminated string, alive as
    // long as `value`.
    let name = unsafe { value.Anonymous.Anonymous.Anonymous.pwszVal.to_string() }.ok()?;
    Some(name).filter(|name| !name.is_empty())
}
