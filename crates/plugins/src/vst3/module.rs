//! A VST3 module: its binary, loaded and entered, and the factory that
//! makes its classes.

use std::ffi::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use musicable_model::PluginFormat;
use musicable_model::plugin::PluginKind;
use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::{BusDirection, IComponent, IComponentTrait, MediaType};
use vst3::Steinberg::{
    FUnknown, IPluginBaseTrait, IPluginFactory, IPluginFactory2, IPluginFactory2Trait,
    IPluginFactory3, IPluginFactory3Trait, IPluginFactoryTrait, PClassInfo, PClassInfo2,
    PClassInfoW, PFactoryInfo, TUID, kResultOk,
};
use vst3::{ComPtr, ComWrapper, Interface};

use super::com::HostApplication;
use crate::bundle::{self, canonical};
use crate::loaded::{Loaded, Open, Registry};
use crate::{Buses, Error, PluginInfo};

/// The category of the classes that are plugins.
const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// How long the scan spends making a module's plugins to count their buses
/// ([`Module::probe`]), so a module with hundreds of plugins in it doesn't
/// take it too long.
const PROBE_TIME: Duration = Duration::from_secs(10);

/// The folder in a bundle's `Contents` with this platform's binary, except
/// on macOS, where it's `MacOS`.
const ARCH_DIR: &str = if cfg!(windows) {
    if cfg!(target_arch = "aarch64") {
        "arm64-win"
    } else {
        "x86_64-win"
    }
} else if cfg!(target_arch = "aarch64") {
    "aarch64-linux"
} else {
    "x86_64-linux"
};

type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
type Exit = unsafe extern "C" fn() -> bool;

/// A `.vst3` module, loaded and entered. It exits and is unloaded once
/// nothing holds it, on the thread that loaded it ([`Loaded`]), as VST3
/// has modules exit on the UI thread.
pub(crate) struct Module {
    path: PathBuf,
    /// Released before the module exits.
    factory: Option<ComPtr<IPluginFactory>>,
    /// What the factory knows the host by, if it asked.
    _host: ComWrapper<HostApplication>,
    exit: Option<Exit>,
    #[cfg(target_os = "macos")]
    bundle: mac::Bundle,
    /// Unloaded last, after the module exits.
    library: Option<libloading::Library>,
}

// SAFETY: VST3's factories are thread-safe, and entering and exiting only
// happen inside `LOADED`'s lock.
unsafe impl Send for Module {}
// SAFETY: as above.
unsafe impl Sync for Module {}

/// The modules loaded, so the process loads each one once. Its lock is held
/// while one enters or exits.
static LOADED: Registry<Module> = Registry::new();

/// A class in a module, as its factory describes it.
struct Class {
    cid: TUID,
    category: String,
    name: String,
    vendor: String,
    version: String,
    sub_categories: String,
}

impl Module {
    /// Loads and enters the module at `path`, or hands back the one already
    /// loaded.
    pub(crate) fn load(path: &Path) -> Result<Arc<Loaded<Module>>, Error> {
        let path = canonical(path)
            .map_err(|e| Error::new(format!("{} can't be read: {e}", path.display())))?;
        LOADED.load(&path, Self::open)
    }

    /// Exits and unloads the modules this thread loaded that were let go of
    /// on other threads.
    pub(crate) fn close_unused() {
        LOADED.close_unused();
    }

    fn open(path: &Path) -> Result<Module, Error> {
        let binary = binary(path)?;
        let fail = |what: &str| Error::new(format!("{} {what}", path.display()));
        // SAFETY: loading a library runs its initializers, which is what
        // hosting it means. It's the scanner's job to find the ones that
        // crash.
        let (library, handle) =
            unsafe { open_library(&binary) }.map_err(|e| fail(&format!("doesn't load: {e}")))?;
        // SAFETY: `GetPluginFactory` has this type in VST3 modules.
        let get_factory = unsafe { library.get::<GetFactory>(c"GetPluginFactory") }
            .map(|symbol| *symbol)
            .map_err(|_| fail("isn't a VST3 plugin: it has no GetPluginFactory"))?;
        #[cfg(target_os = "macos")]
        let bundle = mac::Bundle::new(path);
        #[cfg(target_os = "macos")]
        let context = bundle.as_ptr();
        #[cfg(not(target_os = "macos"))]
        let context = handle;
        #[cfg(target_os = "macos")]
        let _ = handle;
        // SAFETY: called once, inside `LOADED`'s lock, with what the entry
        // takes on this platform.
        let Some(exit) = (unsafe { enter(&library, context) }) else {
            return Err(fail("failed to initialize"));
        };
        // SAFETY: the module is entered.
        let factory = unsafe { ComPtr::from_raw(get_factory()) };
        let Some(factory) = factory else {
            if let Some(exit) = exit {
                // SAFETY: entered above, and nothing of it is in use.
                unsafe { exit() };
            }
            return Err(fail("has no plugin factory"));
        };
        let host = ComWrapper::new(HostApplication);
        if let (Some(factory), Some(context)) = (
            factory.cast::<IPluginFactory3>(),
            host.as_com_ref::<FUnknown>(),
        ) {
            // SAFETY: the factory is the module's, and the host's context
            // lives as long as the module.
            unsafe { factory.setHostContext(context.as_ptr()) };
        }
        Ok(Module {
            path: path.to_owned(),
            factory: Some(factory),
            _host: host,
            exit,
            #[cfg(target_os = "macos")]
            bundle,
            library: Some(library),
        })
    }

    fn factory(&self) -> &ComPtr<IPluginFactory> {
        self.factory
            .as_ref()
            .expect("the factory is there until the module exits")
    }

    /// Every class in the module, in the factory's order.
    fn classes(&self) -> Vec<Class> {
        let factory = self.factory();
        let factory2 = factory.cast::<IPluginFactory2>();
        let factory3 = factory.cast::<IPluginFactory3>();
        // SAFETY: zeroed is a valid info, and the factory fills it in.
        let mut info: PFactoryInfo = unsafe { std::mem::zeroed() };
        // SAFETY: the factory's functions are thread-safe, with somewhere
        // to write.
        let vendor = if unsafe { factory.getFactoryInfo(&mut info) } == kResultOk {
            ascii(&info.vendor)
        } else {
            String::new()
        };
        // SAFETY: as above.
        let count = unsafe { factory.countClasses() };
        (0..count)
            .filter_map(|index| {
                // SAFETY: as above, for each index below the count.
                let mut class = unsafe {
                    unicode_class(factory3.as_ref(), index)
                        .or_else(|| class2(factory2.as_ref(), index))
                        .or_else(|| class1(factory, index))
                }?;
                if class.vendor.is_empty() {
                    class.vendor.clone_from(&vendor);
                }
                Some(class)
            })
            .collect()
    }

    /// The plugins in the module: its audio module classes, in the
    /// factory's order.
    pub(crate) fn plugins(&self) -> Vec<PluginInfo> {
        self.classes()
            .into_iter()
            .filter(|class| class.category == AUDIO_MODULE_CLASS)
            .map(|class| PluginInfo {
                format: PluginFormat::Vst3,
                id: class_id_string(&class.cid),
                name: class.name,
                vendor: class.vendor,
                version: class.version,
                description: String::new(),
                features: class
                    .sub_categories
                    .split('|')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
                buses: None,
                path: self.path.clone(),
            })
            .collect()
    }

    /// Makes each of `plugins`, the module's, that isn't an instrument, to
    /// count its buses, and lets it go again: VST3 has no subcategory for
    /// MIDI effects, so the scan tells them by their buses
    /// ([`PluginInfo::kind`]). It stops after [`PROBE_TIME`], and the
    /// plugins it didn't make go by their subcategories alone. Called on
    /// the thread that loaded the module, as VST3 has plugins made on the
    /// main thread.
    pub(crate) fn probe(&self, plugins: &mut [PluginInfo]) {
        let started = Instant::now();
        let host = ComWrapper::new(HostApplication);
        let context = host
            .as_com_ref::<FUnknown>()
            .map_or(ptr::null_mut(), |c| c.as_ptr());
        for plugin in plugins.iter_mut() {
            if started.elapsed() > PROBE_TIME {
                break;
            }
            if plugin.kind() == PluginKind::Instrument {
                continue;
            }
            let Some(component) = parse_class_id(&plugin.id).and_then(|cid| self.create(&cid))
            else {
                continue;
            };
            // SAFETY: on the thread that loaded the module, just made, with
            // a context that lives as long as the calls. One that fails is
            // let go of, not terminated.
            if unsafe { component.initialize(context) } != kResultOk {
                continue;
            }
            plugin.buses = Some(buses(&component));
            // SAFETY: initialized above, and terminated once.
            unsafe { component.terminate() };
        }
    }

    /// Makes an object of class `cid`, as interface `I`.
    pub(crate) fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        let mut object = ptr::null_mut();
        // SAFETY: the factory's functions are thread-safe, with the ids it
        // takes, and somewhere to write.
        let result = unsafe {
            self.factory().createInstance(
                cid.as_ptr(),
                I::IID.as_ptr().cast::<c_char>(),
                &mut object,
            )
        };
        if result != kResultOk {
            return None;
        }
        // SAFETY: the factory made an object with interface `I`, and hands
        // over a reference to it.
        unsafe { ComPtr::from_raw(object.cast::<I>()) }
    }
}

impl Open for Module {
    /// Called inside `LOADED`'s lock.
    fn close(mut self) {
        drop(self.factory.take());
        if let Some(exit) = self.exit {
            // SAFETY: entered in `open`, and nothing of the module's is in
            // use: every instance holds the module, and lets go of its
            // objects first.
            unsafe { exit() };
        }
        #[cfg(target_os = "macos")]
        self.bundle.release();
        drop(self.library.take());
    }
}

/// How many buses of each kind `component` has, which is initialized.
pub(crate) fn buses(component: &ComPtr<IComponent>) -> Buses {
    let count = |media: MediaType, direction: BusDirection| {
        // SAFETY: initialized, on the thread that made it.
        let count = unsafe { component.getBusCount(media, direction) };
        count.max(0) as u32
    };
    let (audio, events) = (kAudio as MediaType, kEvent as MediaType);
    let (input, output) = (kInput as BusDirection, kOutput as BusDirection);
    Buses {
        audio_in: count(audio, input),
        audio_out: count(audio, output),
        notes_in: count(events, input),
        notes_out: count(events, output),
    }
}

/// The file to load for the module at `path`: the binary for this platform
/// in a bundle, or the file itself, as Windows' single-file modules are.
fn binary(path: &Path) -> Result<PathBuf, Error> {
    if !path.is_dir() {
        return Ok(path.to_owned());
    }
    if cfg!(target_os = "macos") {
        return bundle::macos_binary(path);
    }
    let stem = path.file_stem().unwrap_or_default();
    let mut name = stem.to_owned();
    name.push(if cfg!(windows) { ".vst3" } else { ".so" });
    let binary = path.join("Contents").join(ARCH_DIR).join(name);
    if binary.is_file() {
        Ok(binary)
    } else {
        Err(Error::new(format!(
            "{} has no binary for {ARCH_DIR}",
            path.display()
        )))
    }
}

/// Loads a library, and says the handle the module's entry takes on Linux.
///
/// # Safety
///
/// Loading a library runs its initializers.
unsafe fn open_library(
    binary: &Path,
) -> Result<(libloading::Library, *mut c_void), libloading::Error> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // SAFETY: the caller's promise.
        let library = unsafe { libloading::os::unix::Library::new(binary) }?;
        let handle = library.into_raw();
        // SAFETY: the handle was just let go of, and isn't closed.
        let library = unsafe { libloading::os::unix::Library::from_raw(handle) };
        Ok((library.into(), handle))
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    {
        // SAFETY: the caller's promise.
        let library = unsafe { libloading::Library::new(binary) }?;
        Ok((library, ptr::null_mut()))
    }
}

/// Calls the module's entry, which some modules have: `InitDll` on
/// Windows, `bundleEntry` with the bundle on macOS, `ModuleEntry` with the
/// library's handle on Linux. Says whether it worked, and with the exit to
/// call as the module unloads, or nothing if it failed.
///
/// # Safety
///
/// Called once per load, with what the entry takes.
unsafe fn enter(library: &libloading::Library, context: *mut c_void) -> Option<Option<Exit>> {
    // SAFETY: the symbols have these types in VST3 modules.
    let symbol = |name: &std::ffi::CStr| unsafe { library.get::<Exit>(name) }.ok().map(|s| *s);
    let (entry, exit) = if cfg!(windows) {
        (c"InitDll", c"ExitDll")
    } else if cfg!(target_os = "macos") {
        if symbol(c"bundleEntry").is_some() {
            (c"bundleEntry", c"bundleExit")
        } else {
            (c"BundleEntry", c"BundleExit")
        }
    } else {
        (c"ModuleEntry", c"ModuleExit")
    };
    let entered = if cfg!(windows) {
        // SAFETY: as above.
        let entry = unsafe { library.get::<unsafe extern "C" fn() -> bool>(entry) };
        // SAFETY: the caller's promise.
        entry.map_or(true, |entry| unsafe { entry() })
    } else {
        // SAFETY: as above.
        let entry = unsafe { library.get::<unsafe extern "C" fn(*mut c_void) -> bool>(entry) };
        // SAFETY: the caller's promise.
        entry.map_or(true, |entry| unsafe { entry(context) })
    };
    entered.then(|| symbol(exit))
}

/// # Safety
///
/// `index` must be below the factory's count.
unsafe fn unicode_class(factory: Option<&ComPtr<IPluginFactory3>>, index: i32) -> Option<Class> {
    // SAFETY: zeroed is a valid info.
    let mut info: PClassInfoW = unsafe { std::mem::zeroed() };
    // SAFETY: the caller's promise, with somewhere to write.
    (unsafe { factory?.getClassInfoUnicode(index, &mut info) } == kResultOk).then(|| Class {
        cid: info.cid,
        category: ascii(&info.category),
        name: utf16(&info.name),
        vendor: utf16(&info.vendor),
        version: utf16(&info.version),
        sub_categories: ascii(&info.subCategories),
    })
}

/// # Safety
///
/// As for [`unicode_class`].
unsafe fn class2(factory: Option<&ComPtr<IPluginFactory2>>, index: i32) -> Option<Class> {
    // SAFETY: zeroed is a valid info.
    let mut info: PClassInfo2 = unsafe { std::mem::zeroed() };
    // SAFETY: the caller's promise, with somewhere to write.
    (unsafe { factory?.getClassInfo2(index, &mut info) } == kResultOk).then(|| Class {
        cid: info.cid,
        category: ascii(&info.category),
        name: ascii(&info.name),
        vendor: ascii(&info.vendor),
        version: ascii(&info.version),
        sub_categories: ascii(&info.subCategories),
    })
}

/// # Safety
///
/// As for [`unicode_class`].
unsafe fn class1(factory: &ComPtr<IPluginFactory>, index: i32) -> Option<Class> {
    // SAFETY: zeroed is a valid info.
    let mut info: PClassInfo = unsafe { std::mem::zeroed() };
    // SAFETY: the caller's promise, with somewhere to write.
    (unsafe { factory.getClassInfo(index, &mut info) } == kResultOk).then(|| Class {
        cid: info.cid,
        category: ascii(&info.category),
        name: ascii(&info.name),
        vendor: String::new(),
        version: String::new(),
        sub_categories: String::new(),
    })
}

/// A string in a fixed-size array of 8-bit characters, as VST3's infos hold
/// them: UTF-8, or ASCII.
pub(crate) fn ascii(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A string in a fixed-size array of UTF-16 characters.
pub(crate) fn utf16(chars: &[u16]) -> String {
    let end = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..end])
}

/// How Windows lays out a class id's bytes, as COM does: its first three
/// numbers little-endian. The order is its own inverse.
const COM_ORDER: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];

/// A class id as VST3 writes it, as in `.vstpreset` files: 32 hex digits,
/// whatever the platform's byte order.
pub(crate) fn class_id_string(cid: &TUID) -> String {
    let byte = |i: usize| cid[if cfg!(windows) { COM_ORDER[i] } else { i }] as u8;
    (0..16).map(|i| format!("{:02X}", byte(i))).collect()
}

/// A class id written as [`class_id_string`] writes it.
pub(crate) fn parse_class_id(text: &str) -> Option<TUID> {
    if text.len() != 32 || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    let mut cid: TUID = [0; 16];
    for (i, c) in cid.iter_mut().enumerate() {
        *c = bytes[if cfg!(windows) { COM_ORDER[i] } else { i }] as c_char;
    }
    Some(cid)
}

#[cfg(target_os = "macos")]
mod mac {
    //! The `CFBundle` that a macOS module's entry takes.

    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: u8,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
        fn CFRelease(object: *const c_void);
    }

    /// A bundle, or none if it couldn't be made.
    pub(super) struct Bundle(*mut c_void);

    impl Bundle {
        pub(super) fn new(path: &Path) -> Self {
            let bytes = path.as_os_str().as_bytes();
            // SAFETY: the path's bytes, with their length, and the default
            // allocator.
            let url = unsafe {
                CFURLCreateFromFileSystemRepresentation(
                    ptr::null(),
                    bytes.as_ptr(),
                    bytes.len() as isize,
                    1,
                )
            };
            if url.is_null() {
                return Self(ptr::null_mut());
            }
            // SAFETY: a URL just made, released once the bundle has it.
            let bundle = unsafe { CFBundleCreate(ptr::null(), url) };
            // SAFETY: made above, and not used after.
            unsafe { CFRelease(url) };
            Self(bundle)
        }

        pub(super) fn as_ptr(&self) -> *mut c_void {
            self.0
        }

        pub(super) fn release(&mut self) {
            if !self.0.is_null() {
                // SAFETY: made in `new`, and released once.
                unsafe { CFRelease(self.0) };
                self.0 = ptr::null_mut();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_read_as_they_are_written() {
        let cid = vst3::uid(0x12345678, 0x9ABCDEF0, 0x0F1E2D3C, 0x4B5A6978);
        let text = class_id_string(&cid);
        assert_eq!(text, "123456789ABCDEF00F1E2D3C4B5A6978");
        assert_eq!(parse_class_id(&text), Some(cid));
        assert_eq!(parse_class_id("123"), None);
        assert_eq!(parse_class_id(&"G".repeat(32)), None);
    }
}
