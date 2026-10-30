//! For the tests of the crates that host the test plugins: a copy of the
//! plugins' library for each test, so the counts it keeps are the test's own,
//! and the hooks the tests drive the plugins with.
//!
//! The hooks reach the copy the host loads, not this crate as it's linked
//! into the test: each copy of the library has statics of its own.

use std::ffi::{CStr, CString, c_char, c_void};
use std::fs;
use std::path::{Path, PathBuf};

use clap_sys::plugin::clap_plugin;
use vst3::Steinberg::Vst::PrefetchableSupport;
use vst3::Steinberg::Vst::RestartFlags_::kPrefetchableSupportChanged;
use vst3::Steinberg::Vst::ePrefetchableSupport_::{kIsNotYetPrefetchable, kIsYetPrefetchable};

use crate::vst3::Vst3Transport;

/// The test plugins' library, as Cargo built it for the tests running.
fn built() -> PathBuf {
    let file = format!(
        "{}musicable_test_plugins{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let exe = std::env::current_exe().expect("the tests know where they are");
    let deps = exe.parent().expect("tests run from target/<profile>/deps");
    [deps.join(&file), deps.parent().unwrap_or(deps).join(&file)]
        .into_iter()
        .find(|path| path.exists())
        .or_else(|| built_apart(&exe, &file))
        .expect("Cargo builds the test plugins before the tests that list them")
}

/// Where the library is in the layout nightly Cargo builds in, which keeps
/// each package's output apart: the tests run from
/// `target/<profile>/build/<package>/<hash>/out`, and the library is in
/// `musicable-test-plugins/<hash>/out` beside theirs. The newest, as the
/// one in `deps` is.
fn built_apart(exe: &Path, file: &str) -> Option<PathBuf> {
    let packages = exe.ancestors().nth(4)?;
    fs::read_dir(packages.join("musicable-test-plugins"))
        .ok()?
        .flatten()
        .map(|build| build.path().join("out").join(file))
        .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
        .max()
        .map(|(_, path)| path)
}

/// Copies the test plugins' library to `name.clap` in `dir`, without
/// loading it. A name that starts with `crash`, `hang` or `refuse` makes
/// the copy misbehave as it loads: see the crate's documentation.
pub fn copy(dir: &Path, name: &str) -> PathBuf {
    fs::create_dir_all(dir).expect("the test's folder is writable");
    let copy = dir.join(format!("{name}.clap"));
    fs::copy(built(), &copy).expect("the test's folder is writable");
    copy
}

/// Makes a VST3 bundle, `name.vst3` in `dir`, with a copy of the test
/// plugins' library as its binary for this platform, without loading it.
/// Names misbehave as they do for [`copy`]. Returns the bundle and its
/// binary.
pub fn copy_vst3(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    let bundle = dir.join(format!("{name}.vst3"));
    let contents = bundle.join("Contents");
    let binary = if cfg!(target_os = "macos") {
        contents.join("MacOS").join(name)
    } else {
        let arch = match (cfg!(windows), cfg!(target_arch = "aarch64")) {
            (true, true) => "arm64-win",
            (true, false) => "x86_64-win",
            (false, true) => "aarch64-linux",
            (false, false) => "x86_64-linux",
        };
        let extension = if cfg!(windows) { "vst3" } else { "so" };
        contents.join(arch).join(format!("{name}.{extension}"))
    };
    let folder = binary.parent().expect("the binary is in the bundle");
    fs::create_dir_all(folder).expect("the test's folder is writable");
    fs::copy(built(), &binary).expect("the test's folder is writable");
    if cfg!(target_os = "macos") {
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <plist version=\"1.0\">\n<dict>\n\
             \t<key>CFBundleExecutable</key>\n\t<string>{name}</string>\n\
             \t<key>CFBundleIdentifier</key>\n\t<string>org.musicable.test.{name}</string>\n\
             \t<key>CFBundlePackageType</key>\n\t<string>BNDL</string>\n\
             </dict>\n</plist>\n"
        );
        fs::write(contents.join("Info.plist"), plist).expect("the test's folder is writable");
    }
    (bundle, binary)
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
        Self::load(path.clone(), &path)
    }

    /// Makes a VST3 bundle, as [`copy_vst3`] does, and loads its binary for
    /// its hooks. Loading runs none of the plugins' code: that starts with
    /// the module's entry, which only the host calls.
    pub fn copy_vst3(dir: &Path, name: &str) -> Self {
        let (bundle, binary) = copy_vst3(dir, name);
        Self::load(bundle, &binary)
    }

    fn load(path: PathBuf, binary: &Path) -> Self {
        // The path the host loads it by, so it's the same library.
        let canonical = canonical(binary);
        // SAFETY: as above, loading the library runs none of its code.
        let hooks = unsafe { libloading::Library::new(&canonical) }
            .expect("the test plugins' library loads");
        Self { path, hooks }
    }

    /// Where the copy is: the library, or the VST3 bundle.
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

    /// How many times the host started the plugin processing, stopped it
    /// and reset it, and how many blocks of frames it processed.
    ///
    /// # Safety
    ///
    /// As for [`request_callback`](Self::request_callback).
    pub unsafe fn lifecycle(&self, plugin: *const clap_plugin) -> Lifecycle {
        let get = self.hook::<unsafe extern "C" fn(*const clap_plugin, *mut [u32; 4])>(
            "musicable_test_lifecycle",
        );
        let mut counts = [0; 4];
        // SAFETY: the caller's promise.
        unsafe { get(plugin, &mut counts) };
        let [starts, stops, resets, blocks] = counts;
        Lifecycle {
            starts,
            stops,
            resets,
            blocks,
        }
    }

    /// Whether the gain's sidechain is on, as the host last switched it.
    ///
    /// # Safety
    ///
    /// As for [`request_callback`](Self::request_callback).
    pub unsafe fn sidechain(&self, plugin: *const clap_plugin) -> bool {
        let get = self
            .hook::<unsafe extern "C" fn(*const clap_plugin) -> bool>("musicable_test_sidechain");
        // SAFETY: the caller's promise.
        unsafe { get(plugin) }
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

    /// Takes the MIDI messages the synth got since the last time, oldest
    /// first: see [`musicable_test_midi`](crate::musicable_test_midi).
    /// Panics if it got more than it keeps.
    ///
    /// # Safety
    ///
    /// As for [`gui`](Self::gui), and it must be called between blocks.
    pub unsafe fn midi(&self, plugin: *const clap_plugin) -> Vec<[u8; 3]> {
        let take = self.hook::<unsafe extern "C" fn(*const clap_plugin, *mut u32, u32) -> u32>(
            "musicable_test_midi",
        );
        let mut buffer = vec![0u32; 4096];
        // SAFETY: the caller's promise, and the buffer holds as much as it
        // says.
        let logged = unsafe { take(plugin, buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        assert!(
            logged <= buffer.len(),
            "the synth got more MIDI than it logs"
        );
        buffer[..logged]
            .iter()
            .map(|message| {
                let [status, first, second, _] = message.to_le_bytes();
                [status, first, second]
            })
            .collect()
    }

    // The VST3 plugins' hooks. Each takes one of a plugin's objects, as the
    // host has it: Test Gain's component or its controller, or Test Synth,
    // which is both.

    /// Edits a parameter in a VST3 plugin's window: see
    /// [`musicable_test_vst3_gui`](crate::vst3::musicable_test_vst3_gui).
    /// Says whether the host took it.
    ///
    /// # Safety
    ///
    /// `object` must be a controller of this copy's plugins, as the host has
    /// it, that's alive, and this must be called on the plugin's UI thread.
    pub unsafe fn vst3_gui(
        &self,
        object: *mut c_void,
        action: u32,
        param: u32,
        value: f64,
    ) -> bool {
        let gui = self.hook::<unsafe extern "C" fn(*mut c_void, u32, u32, f64) -> bool>(
            "musicable_test_vst3_gui",
        );
        // SAFETY: the caller's promise.
        unsafe { gui(object, action, param, value) }
    }

    /// Sets a VST3 plugin's label, as someone would in its window.
    ///
    /// # Safety
    ///
    /// As for [`vst3_gui`](Self::vst3_gui).
    pub unsafe fn vst3_set_label(&self, object: *mut c_void, label: &str) -> bool {
        let set = self.hook::<unsafe extern "C" fn(*mut c_void, *const c_char) -> bool>(
            "musicable_test_vst3_set_label",
        );
        let label = CString::new(label).expect("labels have no nul");
        // SAFETY: the caller's promise.
        unsafe { set(object, label.as_ptr()) }
    }

    /// The label a VST3 plugin's component has, which its state holds.
    ///
    /// # Safety
    ///
    /// `object` must be a component of this copy's plugins, as the host has
    /// it, that's alive.
    pub unsafe fn vst3_label(&self, object: *mut c_void) -> Option<String> {
        let get = self.hook::<unsafe extern "C" fn(*mut c_void, *mut c_char, u32) -> bool>(
            "musicable_test_vst3_label",
        );
        let mut buffer = [0 as c_char; 256];
        // SAFETY: the caller's promise, and the buffer holds as much as it
        // says, nul-terminated by the hook.
        unsafe {
            get(object, buffer.as_mut_ptr(), buffer.len() as u32).then(|| {
                CStr::from_ptr(buffer.as_ptr())
                    .to_string_lossy()
                    .into_owned()
            })
        }
    }

    /// Sets the zoom of Test Gain's window, its controller's own state.
    ///
    /// # Safety
    ///
    /// As for [`vst3_gui`](Self::vst3_gui).
    pub unsafe fn vst3_set_zoom(&self, object: *mut c_void, zoom: f64) -> bool {
        let set = self
            .hook::<unsafe extern "C" fn(*mut c_void, f64) -> bool>("musicable_test_vst3_set_zoom");
        // SAFETY: the caller's promise.
        unsafe { set(object, zoom) }
    }

    /// The zoom of Test Gain's window.
    ///
    /// # Safety
    ///
    /// As for [`vst3_gui`](Self::vst3_gui).
    pub unsafe fn vst3_zoom(&self, object: *mut c_void) -> Option<f64> {
        let get = self.hook::<unsafe extern "C" fn(*mut c_void) -> f64>("musicable_test_vst3_zoom");
        // SAFETY: the caller's promise.
        Some(unsafe { get(object) }).filter(|zoom| !zoom.is_nan())
    }

    /// A parameter's value, normalized, as a VST3 plugin's processor has
    /// it.
    ///
    /// # Safety
    ///
    /// As for [`vst3_label`](Self::vst3_label).
    pub unsafe fn vst3_processor_value(&self, object: *mut c_void, param: u32) -> Option<f64> {
        let get = self.hook::<unsafe extern "C" fn(*mut c_void, u32) -> f64>(
            "musicable_test_vst3_processor_value",
        );
        // SAFETY: the caller's promise.
        Some(unsafe { get(object, param) }).filter(|value| !value.is_nan())
    }

    /// A VST3 plugin's pretend window: -1 for none, 0 made but not in the
    /// host's, 1 in it; the window it's in; its platform (1 Win32, 2 Cocoa,
    /// 3 X11); and the scale the host gave it.
    ///
    /// # Safety
    ///
    /// As for [`vst3_gui`](Self::vst3_gui).
    pub unsafe fn vst3_window(&self, object: *mut c_void) -> (i32, usize, i32, f64) {
        type Window = unsafe extern "C" fn(*mut c_void, *mut usize, *mut i32, *mut f64) -> i32;
        let window = self.hook::<Window>("musicable_test_vst3_window");
        let (mut parent, mut platform, mut scale) = (0, 0, 0.0);
        // SAFETY: the caller's promise.
        let state = unsafe { window(object, &mut parent, &mut platform, &mut scale) };
        (state, parent, platform, scale)
    }

    /// Asks the host to resize a VST3 plugin's window, and says what it
    /// answered.
    ///
    /// # Safety
    ///
    /// As for [`vst3_gui`](Self::vst3_gui).
    pub unsafe fn vst3_request_resize(&self, object: *mut c_void, width: u32, height: u32) -> bool {
        let resize = self.hook::<unsafe extern "C" fn(*mut c_void, u32, u32) -> bool>(
            "musicable_test_vst3_request_resize",
        );
        // SAFETY: the caller's promise.
        unsafe { resize(object, width, height) }
    }

    /// The transport as a VST3 plugin last saw it.
    ///
    /// # Safety
    ///
    /// As for [`vst3_label`](Self::vst3_label).
    pub unsafe fn vst3_transport(&self, object: *mut c_void) -> Option<Vst3Transport> {
        let get = self.hook::<unsafe extern "C" fn(*mut c_void, *mut Vst3Transport) -> bool>(
            "musicable_test_vst3_transport",
        );
        let mut transport = Vst3Transport::default();
        // SAFETY: the caller's promise.
        unsafe { get(object, &mut transport) }.then_some(transport)
    }

    /// How many times the host turned a VST3 plugin's processing on and
    /// off, as [`Lifecycle::starts`] and [`Lifecycle::stops`], and how many
    /// blocks of frames it processed.
    ///
    /// # Safety
    ///
    /// As for [`vst3_label`](Self::vst3_label).
    pub unsafe fn vst3_lifecycle(&self, object: *mut c_void) -> Option<Lifecycle> {
        let get = self.hook::<unsafe extern "C" fn(*mut c_void, *mut [u32; 3]) -> bool>(
            "musicable_test_vst3_lifecycle",
        );
        let mut counts = [0; 3];
        // SAFETY: the caller's promise.
        unsafe { get(object, &mut counts) }.then(|| {
            let [starts, stops, blocks] = counts;
            Lifecycle {
                starts,
                stops,
                resets: 0,
                blocks,
            }
        })
    }

    /// Whether Test Gain's sidechain bus is on, as the host last turned it,
    /// if `object` is Test Gain's component.
    ///
    /// # Safety
    ///
    /// As for [`vst3_label`](Self::vst3_label).
    pub unsafe fn vst3_sidechain(&self, object: *mut c_void) -> Option<bool> {
        let get = self.hook::<unsafe extern "C" fn(*mut c_void, *mut bool) -> bool>(
            "musicable_test_vst3_sidechain",
        );
        let mut on = false;
        // SAFETY: the caller's promise.
        unsafe { get(object, &mut on) }.then_some(on)
    }

    /// Has Test Gain say it can render in VST3's prefetch mode, or can't
    /// for now, from now on, and ask the host to look again with a
    /// restart, as a plugin whose mode changed does. `component` and
    /// `controller` are its. Says whether they were.
    ///
    /// # Safety
    ///
    /// As for [`vst3_gui`](Self::vst3_gui), for both.
    pub unsafe fn vst3_set_prefetchable(
        &self,
        component: *mut c_void,
        controller: *mut c_void,
        prefetchable: bool,
    ) -> bool {
        let set = self.hook::<unsafe extern "C" fn(*mut c_void, u32) -> bool>(
            "musicable_test_vst3_set_prefetchable",
        );
        let support = match prefetchable {
            true => kIsYetPrefetchable,
            false => kIsNotYetPrefetchable,
        } as PrefetchableSupport;
        // SAFETY: the caller's promise.
        unsafe {
            set(component, support) && self.vst3_restart(controller, kPrefetchableSupportChanged)
        }
    }

    /// Has a VST3 plugin ask the host to restart it with VST3's `flags`.
    ///
    /// # Safety
    ///
    /// As for [`vst3_gui`](Self::vst3_gui).
    pub unsafe fn vst3_restart(&self, object: *mut c_void, flags: i32) -> bool {
        let restart = self
            .hook::<unsafe extern "C" fn(*mut c_void, i32) -> bool>("musicable_test_vst3_restart");
        // SAFETY: the caller's promise.
        unsafe { restart(object, flags) }
    }
}

/// How many times a host started a plugin processing, stopped it and reset
/// it, and how many blocks of frames the plugin processed: see
/// [`Library::lifecycle`] and [`Library::vst3_lifecycle`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Lifecycle {
    pub starts: u32,
    pub stops: u32,
    /// CLAP's resets. VST3 has none: it turns processing off and on.
    pub resets: u32,
    pub blocks: u32,
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
