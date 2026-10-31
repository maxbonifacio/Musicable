//! Pictures of windows that egui doesn't draw a picture of itself: the
//! windows of their own that panels move to, which eframe paints without
//! its screenshot step, and plugins' windows, which the plugins draw. On
//! Windows, Windows.Graphics.Capture takes them from the window's handle,
//! as the system composes it, so a covered window, or one on another
//! monitor, comes out whole. Elsewhere there's no way yet.

use eframe::egui::ColorImage;

/// A picture of the window `hwnd`, Win32's `HWND`, as it's shown now, of
/// what's inside its frame, as egui's pictures of the main window are, or
/// why there isn't one. It blocks until the system hands over a frame, so
/// it runs off the window's thread.
#[cfg(windows)]
pub(crate) fn capture(hwnd: isize) -> Result<ColorImage, String> {
    native::capture(hwnd).map_err(|error| format!("Windows couldn't capture the window: {error}"))
}

#[cfg(not(windows))]
pub(crate) fn capture(_hwnd: isize) -> Result<ColorImage, String> {
    Err("Musicable takes pictures of these windows only on Windows so far.".to_owned())
}

/// The part `[left, top, right, bottom]`, in pixels, of a picture whose
/// rows are blue, green, red and alpha bytes, each `pitch` bytes from the
/// last, `size` pixels across and down, as RGBA pixels. The part stops at
/// the picture's edges.
#[cfg_attr(not(windows), allow(dead_code))]
fn from_bgra(bytes: &[u8], size: [usize; 2], pitch: usize, part: [i32; 4]) -> ColorImage {
    let clamp = |value: i32, most: usize| (value.max(0) as usize).min(most);
    let (left, right) = (clamp(part[0], size[0]), clamp(part[2], size[0]));
    let (top, bottom) = (clamp(part[1], size[1]), clamp(part[3], size[1]));
    let (left, top) = (left.min(right), top.min(bottom));
    let pixels = (top..bottom)
        .flat_map(|row| {
            bytes[row * pitch + left * 4..row * pitch + right * 4]
                .as_chunks::<4>()
                .0
        })
        .map(|bgra| eframe::egui::Color32::from_rgb(bgra[2], bgra[1], bgra[0]))
        .collect();
    ColorImage::new([right - left, bottom - top], pixels)
}

#[cfg(windows)]
mod native {
    use std::sync::mpsc;
    use std::time::Duration;

    use eframe::egui::ColorImage;
    use windows::Foundation::TypedEventHandler;
    use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Foundation::{HMODULE, HWND, POINT, RECT};
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ,
        D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
        D3D11CreateDevice, ID3D11Device, ID3D11Texture2D,
    };
    use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows::Win32::Graphics::Dxgi::IDXGIDevice;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::System::WinRT::Direct3D11::{
        CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
    };
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    use windows::core::{Interface, Result, factory};

    /// How long to wait for the system's first frame of the window.
    const FIRST_FRAME: Duration = Duration::from_secs(2);

    pub fn capture(hwnd: isize) -> Result<ColorImage> {
        // The thread may be in an apartment already, which is as good.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item: GraphicsCaptureItem =
            unsafe { interop.CreateForWindow(HWND(hwnd as *mut std::ffi::c_void))? };
        let device = device()?;
        let direct3d: IDirect3DDevice = unsafe {
            CreateDirect3D11DeviceFromDXGIDevice(&device.cast::<IDXGIDevice>()?)?.cast()?
        };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &direct3d,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            1,
            item.Size()?,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        // Without the pointer, and without the yellow border where the
        // system lets an app go without it.
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        let (frames, arrived) = mpsc::sync_channel(1);
        pool.FrameArrived(&TypedEventHandler::new(
            move |pool: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
                if let Some(pool) = pool.as_ref() {
                    let _ = frames.try_send(pool.TryGetNextFrame());
                }
                Ok(())
            },
        ))?;
        session.StartCapture()?;
        let frame = arrived.recv_timeout(FIRST_FRAME);
        let image = match frame {
            Ok(frame) => picture(&device, &frame?.Surface()?.cast()?, client_area(hwnd)?),
            Err(_) => Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "no frame came: is the window shown?",
            )),
        };
        let _ = session.Close();
        let _ = pool.Close();
        image
    }

    /// A Direct3D 11 device on the graphics card, for the frames.
    fn device() -> Result<ID3D11Device> {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )?;
        }
        device.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
    }

    /// Where the inside of the window `hwnd` is in the pictures the system
    /// takes of it, which start at its frame's visible edge: its left, top,
    /// right and bottom edges, in pixels.
    fn client_area(hwnd: isize) -> Result<[i32; 4]> {
        let hwnd = HWND(hwnd as *mut std::ffi::c_void);
        let mut frame = RECT::default();
        unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                (&raw mut frame).cast(),
                size_of::<RECT>() as u32,
            )?;
        }
        let mut client = RECT::default();
        unsafe { GetClientRect(hwnd, &mut client)? };
        let mut origin = POINT::default();
        unsafe { ClientToScreen(hwnd, &mut origin).ok()? };
        let (left, top) = (origin.x - frame.left, origin.y - frame.top);
        Ok([left, top, left + client.right, top + client.bottom])
    }

    /// The part `area` of the frame `surface` holds, copied to where the
    /// processor can read it.
    fn picture(
        device: &ID3D11Device,
        surface: &IDirect3DDxgiInterfaceAccess,
        area: [i32; 4],
    ) -> Result<ColorImage> {
        let texture: ID3D11Texture2D = unsafe { surface.GetInterface()? };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut staging = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging))? };
        let staging = staging
            .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let context = unsafe { device.GetImmediateContext()? };
        unsafe { context.CopyResource(&staging, &texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))? };
        let (width, height) = (desc.Width as usize, desc.Height as usize);
        let pitch = mapped.RowPitch as usize;
        let bytes =
            unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, pitch * height) };
        let image = super::from_bgra(bytes, [width, height], pitch, area);
        unsafe { context.Unmap(&staging, 0) };
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::Color32;

    use super::*;

    #[test]
    fn rows_of_bgra_bytes_are_rgba_pixels() {
        // Two pixels a row, and two bytes of padding at each row's end.
        let bytes = [
            1, 2, 3, 255, 4, 5, 6, 255, 0, 0, //
            7, 8, 9, 255, 10, 11, 12, 255, 0, 0,
        ];
        let image = from_bgra(&bytes, [2, 2], 10, [0, 0, 2, 2]);
        assert_eq!(image.size, [2, 2]);
        assert_eq!(image.pixels[0], Color32::from_rgb(3, 2, 1));
        assert_eq!(image.pixels[3], Color32::from_rgb(12, 11, 10));
        // A part of it, which stops at its edges.
        let part = from_bgra(&bytes, [2, 2], 10, [1, -5, 9, 1]);
        assert_eq!(part.size, [1, 1]);
        assert_eq!(part.pixels[0], Color32::from_rgb(6, 5, 4));
        assert_eq!(from_bgra(&bytes, [2, 2], 10, [3, 3, 1, 1]).size, [0, 0]);
    }
}
