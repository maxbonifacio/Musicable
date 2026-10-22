//! Finding output and input endpoints.

use windows::Win32::Devices::FunctionDiscovery::{
    PKEY_Device_ContainerId, PKEY_Device_FriendlyName,
};
use windows::Win32::Foundation::{E_INVALIDARG, ERROR_NOT_FOUND};
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, EDataFlow, IDeviceTopology, IMMDevice, IMMDeviceEnumerator, IMMEndpoint,
    MMDeviceEnumerator, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
use windows::Win32::System::Variant::{VT_CLSID, VT_LPWSTR};
use windows::core::{GUID, HSTRING, Interface};

use super::client;
use super::com::{TaskMem, in_mta, os};
use crate::input::{self, InputDevice, InputKind};
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

/// The input endpoints that are plugged in and enabled, then a loopback
/// input for each output endpoint.
pub(crate) fn input_devices() -> Result<Vec<InputDevice>, Error> {
    in_mta(|| {
        let enumerator = enumerator()?;
        let mut devices = Vec::new();
        let default = default_input_id(&enumerator);
        for device in endpoints(&enumerator, eCapture)? {
            let id = id_of(&device)?;
            devices.push(InputDevice {
                is_default: default.as_deref() == Some(id.as_str()),
                name: friendly_name(&device).unwrap_or_else(|| id.clone()),
                id,
                kind: InputKind::Device,
                channels: client::channels(&device).unwrap_or(0),
            });
        }
        for device in endpoints(&enumerator, eRender)? {
            let id = id_of(&device)?;
            devices.push(InputDevice {
                is_default: false,
                name: friendly_name(&device).unwrap_or_else(|| id.clone()),
                id: input::loopback_id(&id),
                kind: InputKind::Loopback,
                channels: client::channels(&device).unwrap_or(0),
            });
        }
        Ok(devices)
    })
}

/// The active endpoints that `flow` goes through.
fn endpoints(enumerator: &IMMDeviceEnumerator, flow: EDataFlow) -> Result<Vec<IMMDevice>, Error> {
    // SAFETY: COM calls on an initialized MTA thread.
    let collection = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }
        .map_err(|e| os("IMMDeviceEnumerator::EnumAudioEndpoints", e))?;
    let count =
        unsafe { collection.GetCount() }.map_err(|e| os("IMMDeviceCollection::GetCount", e))?;
    (0..count)
        .map(|index| {
            unsafe { collection.Item(index) }.map_err(|e| os("IMMDeviceCollection::Item", e))
        })
        .collect()
}

fn default_input_id(enumerator: &IMMDeviceEnumerator) -> Option<String> {
    // SAFETY: on an MTA thread.
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eCapture, eConsole) }.ok()?;
    id_of(&device).ok()
}

/// The input endpoint with this id, or for a loopback input's id, the output
/// endpoint it records, and whether it's a loopback.
pub(super) fn find_input(
    enumerator: &IMMDeviceEnumerator,
    id: &str,
) -> Result<(IMMDevice, bool), Error> {
    match input::loopback_of(id) {
        Some(output) => Ok((find(enumerator, Some(output))?, true)),
        None => Ok((find_flow(enumerator, id, eCapture)?, false)),
    }
}

/// The physical device an endpoint belongs to: an interface's inputs and
/// outputs share it, and with it their clock.
pub(super) fn container_id(device: &IMMDevice) -> Option<GUID> {
    // SAFETY: on an MTA thread.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let value = unsafe { store.GetValue(&PKEY_Device_ContainerId) }.ok()?;
    if value.vt() != VT_CLSID {
        return None;
    }
    // SAFETY: a VT_CLSID value points to a GUID, alive as long as `value`.
    let guid = unsafe { value.Anonymous.Anonymous.Anonymous.puuid.as_ref() }?;
    Some(*guid)
}

/// The adapter an endpoint belongs to, such as a sound card: the device
/// its topology connects to, less the filter's interface and reference
/// string, in lower case. A card's inputs and outputs share it, and with it
/// their clock, where their container id can't say so: Windows gives every
/// device built into the computer the same one.
pub(super) fn adapter_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: on an MTA thread. An endpoint's topology has one connector,
    // to its adapter. The string is COM's to free, once.
    let topology: IDeviceTopology = client::activate(device).ok()?;
    let connector = unsafe { topology.GetConnector(0) }.ok()?;
    let id = unsafe { connector.GetDeviceIdConnectedTo() }.ok()?;
    let _free = TaskMem(id.0);
    let id = unsafe { id.to_string() }.ok()?;
    Some(adapter_of(&id))
}

/// The adapter in a device id such as
/// `{2}.\\?\pci#ven_10ee&dev_3fc6#4&2c&0&0008#{6994ad04-...}\analog12`:
/// what comes before its interface class, which each of the adapter's
/// filters shares.
fn adapter_of(id: &str) -> String {
    let id = id.to_lowercase();
    match id.find("#{") {
        Some(end) => id[..end].to_owned(),
        None => id,
    }
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
    find_flow(enumerator, id, eRender)
}

/// The active endpoint with this id that `flow` goes through.
fn find_flow(
    enumerator: &IMMDeviceEnumerator,
    id: &str,
    flow: EDataFlow,
) -> Result<IMMDevice, Error> {
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
    // `GetDevice` also finds unplugged and disabled endpoints, and the
    // other direction's.
    let active = unsafe { device.GetState() }.is_ok_and(|state| state == DEVICE_STATE_ACTIVE);
    let matches = device
        .cast::<IMMEndpoint>()
        .and_then(|endpoint| unsafe { endpoint.GetDataFlow() })
        .is_ok_and(|found| found == flow);
    if active && matches {
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

pub(super) fn id_of(device: &IMMDevice) -> Result<String, Error> {
    // SAFETY: on an MTA thread. The string is COM's to free, once.
    let id = unsafe { device.GetId() }.map_err(|e| os("IMMDevice::GetId", e))?;
    let _free = TaskMem(id.0);
    unsafe { id.to_string() }.map_err(|e| Error::Os {
        call: "IMMDevice::GetId",
        message: e.to_string(),
    })
}

pub(super) fn friendly_name(device: &IMMDevice) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cards_filters_share_its_adapter() {
        let output = r"{2}.\\?\pci#ven_10ee&dev_3fc6&subsys_3fc610ee#4&2c1a&0&0008#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\analog12";
        let input = r"{2}.\\?\PCI#VEN_10EE&DEV_3FC6&SUBSYS_3FC610EE#4&2c1a&0&0008#{65e8773d-8f56-11d0-a3b9-00a0c9223196}\Record12";
        assert_eq!(adapter_of(output), adapter_of(input));
        let onboard = r"{2}.\\?\hdaudio#func_01&ven_10ec&dev_0887#4&5a1&0&0001#{6994ad04-93ef-11d0-a3cc-00a0c9223196}\elineouttopo";
        assert_ne!(adapter_of(output), adapter_of(onboard));
    }
}
