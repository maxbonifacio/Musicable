//! For the tests of the crates that host the test plugins: a copy of the
//! plugins' library for each test, so the counts it keeps are the test's own,
//! and the hooks the tests drive the plugins with.
//!
//! The hooks reach the copy the host loads, not this crate as it's linked
//! into the test: each copy of the library has statics of its own.

use std::ffi::{CStr, CString, c_char};
use std::fs;
use std::path::{Path, PathBuf};

use clap_sys::plugin::clap_plugin;

/// Copies the test plugins' library, as Cargo built it for the tests
/// running, to `name.clap` in `dir`, without loading it. A name that starts
/// with `crash`, `hang` or `refuse` makes the copy misbehave as it loads:
/// see the crate's documentation.
pub fn copy(dir: &Path, name: &str) -> PathBuf {
    let file = format!(
        "{}musicable_test_plugins{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let exe = std::env::current_exe().expect("the tests know where they are");
    let deps = exe.parent().expect("tests run from target/<profile>/deps");
    let built = [deps.join(&file), deps.parent().unwrap_or(deps).join(&file)]
        .into_iter()
        .find(|path| path.exists())
        .expect("Cargo builds the test plugins before the tests that list them");
    fs::create_dir_all(dir).expect("the test's folder is writable");
    let copy = dir.join(format!("{name}.clap"));
    fs::copy(built, &copy).expect("the test's folder is writable");
    copy
}

/// A copy of the test plugins' library, and the hooks into it.
pub struct Library {
    path: PathBuf,
    hooks: libloading::Library,
}

impl Library {
    /// Copies the library to `name.clap` in `dir`, as [`copy`] does, and
    /// loads the copy for its hooks. Loading runs none of the plugins' code:
    /// that starts with CLAP's entry, which only the host calls.
    pub fn copy(dir: &Path, name: &str) -> Self {
        let path = copy(dir, name);
        // The path the host loads it by, so it's the same library.
        let canonical = canonical(&path);
        // SAFETY: as above, loading the library runs none of its code.
        let hooks = unsafe { libloading::Library::new(&canonical) }
            .expect("the test plugins' library loads");
        Self { path, hooks }
    }

    /// Where the copy is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn hook<T: Copy>(&self, name: &str) -> T {
        let name = CString::new(name).expect("hook names have no nul");
        // SAFETY: each hook is asked for with its type.
        *unsafe { self.hooks.get::<T>(name.as_bytes_with_nul()) }
            .expect("the library has its hooks")
    }

    /// How many times the host broke CLAP's rules with this copy's plugins.
    pub fn violations(&self) -> u32 {
        self.hook::<extern "C" fn() -> u32>("musicable_test_violations")()
    }

    /// The first rule the host broke, or nothing.
    pub fn first_violation(&self) -> String {
        let first = self.hook::<unsafe extern "C" fn(*mut c_char, u32) -> bool>(
            "musicable_test_first_violation",
        );
        let mut buffer = [0 as c_char; 512];
        // SAFETY: the buffer holds as much as it says.
        if unsafe { first(buffer.as_mut_ptr(), buffer.len() as u32) } {
            // SAFETY: the hook nul-terminates it.
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        }
    }

    /// How many of this copy's plugins are alive.
    pub fn alive(&self) -> u32 {
        self.hook::<extern "C" fn() -> u32>("musicable_test_alive")()
    }

    /// Checks that the host kept CLAP's rules, and destroyed every plugin
    /// it made.
    pub fn assert_alright(&self) {
        assert_eq!(self.violations(), 0, "{}", self.first_violation());
        assert_eq!(self.alive(), 0, "plugins left alive");
    }

    /// Acts in the plugin's window as someone would: see
    /// [`musicable_test_gui`](crate::musicable_test_gui).
    ///
    /// # Safety
    ///
    /// `plugin` must be a plugin of this copy that isn't destroyed, and this
    /// must be called on its main thread.
    pub unsafe fn gui(&self, plugin: *const clap_plugin, action: u32, param: u32, value: f64) {
        let gui = self
            .hook::<unsafe extern "C" fn(*const clap_plugin, u32, u32, f64)>("musicable_test_gui");
        // SAFETY: the caller's promise.
        unsafe { gui(plugin, action, param, value) };
    }

    /// Changes the plugin's label, state that isn't a parameter.
    ///
    /// # Safety
    ///
    /// As for [`gui`](Self::gui).
    pub unsafe fn set_label(&self, plugin: *const clap_plugin, label: &str) {
        let set = self.hook::<unsafe extern "C" fn(*const clap_plugin, *const c_char)>(
            "musicable_test_set_label",
        );
        let label = CString::new(label).expect("labels have no nul");
        // SAFETY: the caller's promise.
        unsafe { set(plugin, label.as_ptr()) };
    }

    /// The plugin's label.
    ///
    /// # Safety
    ///
    /// As for [`gui`](Self::gui).
    pub unsafe fn label(&self, plugin: *const clap_plugin) -> String {
        let get = self.hook::<unsafe extern "C" fn(*const clap_plugin, *mut c_char, u32)>(
            "musicable_test_label",
        );
        let mut buffer = [0 as c_char; 256];
        // SAFETY: the caller's promise, and the buffer holds as much as it
        // says.
        unsafe {
            get(plugin, buffer.as_mut_ptr(), buffer.len() as u32);
            CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned()
        }
    }

    /// Asks the host for a callback on the main thread, from whatever thread
    /// calls it.
    ///
    /// # Safety
    ///
    /// `plugin` must be a plugin of this copy that isn't destroyed.
    pub unsafe fn request_callback(&self, plugin: *const clap_plugin) {
        let request = self
            .hook::<unsafe extern "C" fn(*const clap_plugin)>("musicable_test_request_callback");
        // SAFETY: the caller's promise.
        unsafe { request(plugin) };
    }

    /// How many times the host called the plugin's `on_main_thread`.
    ///
    /// # Safety
    ///
    /// As for [`request_callback`](Self::request_callback).
    pub unsafe fn main_thread_calls(&self, plugin: *const clap_plugin) -> u32 {
        let calls = self.hook::<unsafe extern "C" fn(*const clap_plugin) -> u32>(
            "musicable_test_main_thread_calls",
        );
        // SAFETY: the caller's promise.
        unsafe { calls(plugin) }
    }

    /// The pretend window: -1 for none, 0 hidden, 1 showing, and the window
    /// the host put it in, or 0.
    ///
    /// # Safety
    ///
    /// As for [`gui`](Self::gui).
    pub unsafe fn window(&self, plugin: *const clap_plugin) -> (i32, usize) {
        let window = self.hook::<unsafe extern "C" fn(*const clap_plugin, *mut usize) -> i32>(
            "musicable_test_window",
        );
        let mut parent = 0;
        // SAFETY: the caller's promise.
        let state = unsafe { window(plugin, &mut parent) };
        (state, parent)
    }

    /// Asks the host to resize the plugin's window, and says what it
    /// answered.
    ///
    /// # Safety
    ///
    /// As for [`gui`](Self::gui).
    pub unsafe fn request_resize(
        &self,
        plugin: *const clap_plugin,
        width: u32,
        height: u32,
    ) -> bool {
        let resize = self.hook::<unsafe extern "C" fn(*const clap_plugin, u32, u32) -> bool>(
            "musicable_test_request_resize",
        );
        // SAFETY: the caller's promise.
        unsafe { resize(plugin, width, height) }
    }

    /// How many times the synth had the host run its tasks, and on how many
    /// threads they ran.
    ///
    /// # Safety
    ///
    /// As for [`request_callback`](Self::request_callback).
    pub unsafe fn pool_use(&self, plugin: *const clap_plugin) -> (u32, u32) {
        let pool = self.hook::<unsafe extern "C" fn(*const clap_plugin, *mut u32, *mut u32)>(
            "musicable_test_pool_use",
        );
        let (mut requests, mut threads) = (0, 0);
        // SAFETY: the caller's promise.
        unsafe { pool(plugin, &mut requests, &mut threads) };
        (requests, threads)
    }

    /// The transport's flags, position in beats and tempo, as the plugin
    /// last saw them.
    ///
    /// # Safety
    ///
    /// As for [`request_callback`](Self::request_callback).
    pub unsafe fn transport(&self, plugin: *const clap_plugin) -> Option<(u32, i64, f64)> {
        type Transport =
            unsafe extern "C" fn(*const clap_plugin, *mut u32, *mut i64, *mut f64) -> bool;
        let transport = self.hook::<Transport>("musicable_test_transport");
        let (mut flags, mut beats, mut tempo) = (0, 0, 0.0);
        // SAFETY: the caller's promise.
        unsafe { transport(plugin, &mut flags, &mut beats, &mut tempo) }
            .then_some((flags, beats, tempo))
    }
}

/// `path` as the host loads libraries by: canonical, and on Windows without
/// the `\\?\` that canonical paths start with there.
fn canonical(path: &Path) -> PathBuf {
    let path = path.canonicalize().expect("the copy is there");
    if cfg!(windows)
        && let Some(rest) = path.to_str().and_then(|p| p.strip_prefix(r"\\?\"))
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return PathBuf::from(rest);
    }
    path
}
