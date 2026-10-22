//! Plugins' own windows. On Windows, a plugin's window goes inside a window
//! of Musicable's, which the main window owns, so it stays in front of it
//! and minimizes with it. Elsewhere, and for a plugin that can't go inside
//! another window, the plugin opens a window of its own.

use std::collections::BTreeMap;

use musicable_model::DeviceId;
use musicable_plugins::{PluginEvent, Waker};

use crate::Session;

/// The plugin windows that are open, by device.
#[derive(Default)]
pub(crate) struct PluginWindows {
    open: BTreeMap<DeviceId, Window>,
}

/// An open plugin window.
enum Window {
    /// Inside a window of Musicable's.
    #[cfg(windows)]
    Embedded(native::Frame),
    /// The plugin's own.
    Floating,
}

impl PluginWindows {
    /// Shows the window of the plugin device `device`'s plugin, inside a
    /// window that `owner`, the app's main window, owns where it can. It
    /// comes to the front if it's open already. `scale` is the display's
    /// pixels per point, and `wake` wakes the app when the user closes the
    /// window.
    pub fn show(
        &mut self,
        session: &mut Session,
        device: DeviceId,
        owner: Option<isize>,
        scale: f64,
        wake: Waker,
    ) -> Result<(), String> {
        let title = title(session, device);
        let Some(instance) = session.plugin_mut(device) else {
            return Err(match session.plugin_problem(device) {
                Some(problem) => problem.to_owned(),
                None => "the plugin isn't loaded".to_owned(),
            });
        };
        if !instance.has_window() {
            return Err(format!("{} has no window", instance.info().name));
        }
        if let Some(window) = self.open.get(&device)
            && instance.is_window_open()
        {
            window.raise();
            return Ok(());
        }
        let window = open(instance, owner, scale, &title, wake)?;
        self.open.insert(device, window);
        Ok(())
    }

    /// Closes the window of the device `device`'s plugin, if it's open.
    pub fn hide(&mut self, session: &mut Session, device: DeviceId) {
        // Kept until the plugin has closed its window, which is inside it.
        let Some(_window) = self.open.remove(&device) else {
            return;
        };
        if let Some(instance) = session.plugin_mut(device) {
            instance.close_window();
        }
    }

    /// Whether the device `device`'s plugin window is open.
    pub fn is_open(&self, device: DeviceId) -> bool {
        self.open.contains_key(&device)
    }

    /// Follows what the plugins' windows asked for, and what the user did
    /// to them: closes the windows the user closed and the ones whose
    /// plugin went, and resizes the ones that asked. Call it every frame.
    pub fn tick(&mut self, session: &mut Session) {
        for (device, event) in session.take_window_events() {
            match event {
                PluginEvent::WindowClosed => {
                    self.open.remove(&device);
                }
                PluginEvent::WindowResize { width, height } => {
                    let Some(window) = self.open.get(&device) else {
                        continue;
                    };
                    let size = session
                        .plugin_mut(device)
                        .and_then(|instance| instance.resize_window(width, height));
                    if let Some((width, height)) = size {
                        window.set_size(width, height);
                    }
                }
                _ => {}
            }
        }
        self.open.retain(|&device, window| {
            let Some(instance) = session.plugin_mut(device) else {
                // The device went, and its plugin closed its window.
                return false;
            };
            if window.closed() || !instance.is_window_open() {
                instance.close_window();
                return false;
            }
            true
        });
    }

    /// Closes every plugin window, as when the app closes.
    pub fn close_all(&mut self, session: &mut Session) {
        for (device, _window) in std::mem::take(&mut self.open) {
            if let Some(instance) = session.plugin_mut(device) {
                instance.close_window();
            }
        }
    }
}

impl Window {
    fn raise(&self) {
        match self {
            #[cfg(windows)]
            Window::Embedded(frame) => frame.raise(),
            Window::Floating => {}
        }
    }

    fn set_size(&self, width: u32, height: u32) {
        match self {
            #[cfg(windows)]
            Window::Embedded(frame) => frame.set_client_size(width, height),
            Window::Floating => {
                let _ = (width, height);
            }
        }
    }

    /// Whether the user closed it.
    fn closed(&self) -> bool {
        match self {
            #[cfg(windows)]
            Window::Embedded(frame) => frame.closed(),
            Window::Floating => false,
        }
    }
}

/// What a plugin's window is called: the plugin's name, and the track it's
/// on.
fn title(session: &Session, device: DeviceId) -> String {
    let project = session.project();
    let Some((chain, _, found)) = project.find_device(device) else {
        return "Plugin".to_owned();
    };
    let plugin = found.kind.plugin().map_or("Plugin", |p| p.name.as_str());
    let on = match chain {
        musicable_model::Chain::Track(track) => {
            project.track(track).map_or("", |t| t.name.as_str())
        }
        musicable_model::Chain::Master => "Master",
    };
    format!("{plugin} - {on}")
}

#[cfg(windows)]
fn open(
    instance: &mut musicable_plugins::Instance,
    owner: Option<isize>,
    scale: f64,
    title: &str,
    wake: Waker,
) -> Result<Window, String> {
    use musicable_plugins::ParentWindow;

    let frame = native::Frame::new(owner, title, wake)?;
    match instance.open_window(ParentWindow::Win32(frame.hwnd()), scale) {
        Ok((width, height)) => {
            frame.set_client_size(width, height);
            frame.show();
            Ok(Window::Embedded(frame))
        }
        // A plugin that can't go inside another window has one of its own.
        Err(_) => {
            drop(frame);
            instance
                .open_floating_window(title)
                .map_err(|e| e.to_string())?;
            Ok(Window::Floating)
        }
    }
}

#[cfg(not(windows))]
fn open(
    instance: &mut musicable_plugins::Instance,
    _owner: Option<isize>,
    _scale: f64,
    title: &str,
    _wake: Waker,
) -> Result<Window, String> {
    instance
        .open_floating_window(title)
        .map_err(|e| e.to_string())?;
    Ok(Window::Floating)
}

/// Musicable's window for a plugin's window to go inside, in Win32.
#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    use std::sync::Once;
    use std::sync::atomic::{AtomicBool, Ordering};

    use musicable_plugins::Waker;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
        GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, GetWindowRect, IDC_ARROW, LoadCursorW,
        RegisterClassW, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetForegroundWindow,
        SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
        WNDCLASSW, WS_CAPTION, WS_CLIPCHILDREN, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
    };
    use windows::core::{HSTRING, PCWSTR, w};

    const CLASS: PCWSTR = w!("MusicablePluginWindow");
    /// A title bar with a close and a minimize button, and a border that
    /// doesn't resize.
    const STYLE: WINDOW_STYLE = WINDOW_STYLE(
        WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0,
    );

    /// What the window procedure shares with the app: whether the user
    /// closed the window, and how to wake the app to notice.
    struct Shared {
        closed: AtomicBool,
        wake: Waker,
    }

    /// A top-level window, owned by the app's main window, that a plugin's
    /// window goes inside. Closing it only marks it closed: the app closes
    /// the plugin's window first, then drops this.
    pub(crate) struct Frame {
        hwnd: HWND,
        shared: *mut Shared,
    }

    impl Frame {
        pub fn new(owner: Option<isize>, title: &str, wake: Waker) -> Result<Self, String> {
            static REGISTER: Once = Once::new();
            // SAFETY: plain Win32 calls, with a class whose procedure is
            // below, and pointers that live for each call.
            unsafe {
                let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
                REGISTER.call_once(|| {
                    let class = WNDCLASSW {
                        style: CS_HREDRAW | CS_VREDRAW,
                        lpfnWndProc: Some(procedure),
                        hInstance: instance.into(),
                        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                        lpszClassName: CLASS,
                        ..WNDCLASSW::default()
                    };
                    RegisterClassW(&class);
                });
                let owner = owner.map(|hwnd| HWND(hwnd as *mut c_void));
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    CLASS,
                    &HSTRING::from(title),
                    STYLE,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    400,
                    300,
                    owner,
                    None,
                    Some(instance.into()),
                    None,
                )
                .map_err(|e| format!("Musicable couldn't make the plugin's window: {e}"))?;
                let shared = Box::into_raw(Box::new(Shared {
                    closed: AtomicBool::new(false),
                    wake,
                }));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, shared as isize);
                Ok(Self { hwnd, shared })
            }
        }

        pub fn hwnd(&self) -> *mut c_void {
            self.hwnd.0
        }

        /// Sizes the window so the inside is `width` by `height` pixels.
        pub fn set_client_size(&self, width: u32, height: u32) {
            let mut window = RECT::default();
            let mut client = RECT::default();
            // SAFETY: a window this owns, and rects that live for the calls.
            unsafe {
                if GetWindowRect(self.hwnd, &mut window).is_err()
                    || GetClientRect(self.hwnd, &mut client).is_err()
                {
                    return;
                }
                // The title bar and the border, as big as they are on the
                // display the window is on.
                let border_width = (window.right - window.left) - client.right;
                let border_height = (window.bottom - window.top) - client.bottom;
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    border_width.saturating_add(width.try_into().unwrap_or(i32::MAX)),
                    border_height.saturating_add(height.try_into().unwrap_or(i32::MAX)),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }

        pub fn show(&self) {
            // SAFETY: a window this owns.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOW);
            }
        }

        pub fn raise(&self) {
            // SAFETY: a window this owns.
            unsafe {
                let _ = SetForegroundWindow(self.hwnd);
            }
        }

        pub fn closed(&self) -> bool {
            // SAFETY: the shared state lives as long as the frame.
            unsafe { (*self.shared).closed.load(Ordering::Acquire) }
        }
    }

    impl Drop for Frame {
        fn drop(&mut self) {
            // SAFETY: a window this owns, whose procedure stops reading the
            // shared state once it's gone; and the state this boxed.
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(self.hwnd);
                drop(Box::from_raw(self.shared));
            }
        }
    }

    unsafe extern "system" fn procedure(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_CLOSE {
            // SAFETY: the frame's state, or null once the frame is going.
            let shared = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Shared;
            // SAFETY: as above.
            if let Some(shared) = unsafe { shared.as_ref() } {
                shared.closed.store(true, Ordering::Release);
                (shared.wake)();
            }
            return LRESULT(0);
        }
        // SAFETY: the default for every other message.
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}
