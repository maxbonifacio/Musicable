//! A plugin's shared library: loading it, and what's in it.

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, Weak};

use clap_sys::entry::clap_plugin_entry;
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::version::clap_version_is_compatible;
use musicable_model::PluginFormat;

use crate::{Error, PluginInfo};

/// A `.clap` file, loaded and initialized. It's deinitialized and unloaded
/// once nothing holds it.
pub(crate) struct Library {
    path: PathBuf,
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
    /// Unloaded last, after `deinit`.
    library: Option<libloading::Library>,
}

// SAFETY: the factory's functions are thread-safe, and `init` and `deinit`
// only run inside `LOADED`'s lock.
unsafe impl Send for Library {}
// SAFETY: as above.
unsafe impl Sync for Library {}

/// The libraries loaded, by path, so the process loads each file once:
/// CLAP forbids initializing a library twice. Its lock is held while one
/// initializes or deinitializes, which CLAP forbids doing concurrently.
static LOADED: Mutex<Vec<(PathBuf, Weak<Library>)>> = Mutex::new(Vec::new());

impl Library {
    /// Loads and initializes the library at `path`, or hands back the one
    /// already loaded.
    pub(crate) fn load(path: &Path) -> Result<Arc<Library>, Error> {
        let path = canonical(path)
            .map_err(|e| Error::new(format!("{} can't be read: {e}", path.display())))?;
        let mut loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        loaded.retain(|(_, library)| library.strong_count() > 0);
        if let Some(library) = loaded
            .iter()
            .find(|(p, _)| *p == path)
            .and_then(|(_, library)| library.upgrade())
        {
            return Ok(library);
        }
        let library = Arc::new(Self::open(&path)?);
        loaded.push((path, Arc::downgrade(&library)));
        Ok(library)
    }

    fn open(path: &Path) -> Result<Library, Error> {
        let binary = binary(path)?;
        let fail = |what: &str| Error::new(format!("{} {what}", path.display()));
        // SAFETY: loading a library runs its initializers, which is what
        // hosting it means. It's the scanner's job to find the ones that
        // crash.
        let library = unsafe { libloading::Library::new(&binary) }
            .map_err(|e| fail(&format!("doesn't load: {e}")))?;
        let entry: *const clap_plugin_entry = {
            // SAFETY: `clap_entry` is a `clap_plugin_entry` in CLAP
            // libraries.
            let symbol = unsafe { library.get::<*const clap_plugin_entry>(c"clap_entry") }
                .map_err(|_| fail("isn't a CLAP plugin: it has no clap_entry"))?;
            *symbol
        };
        // SAFETY: the symbol points at the entry, which lives as long as
        // the library.
        let Some(found) = (unsafe { entry.as_ref() }) else {
            return Err(fail("has a null clap_entry"));
        };
        if !clap_version_is_compatible(found.clap_version) {
            return Err(fail("is for an incompatible version of CLAP"));
        }
        let (Some(init), Some(get_factory)) = (found.init, found.get_factory) else {
            return Err(fail("has an incomplete clap_entry"));
        };
        let bundle = path_cstring(path)?;
        // SAFETY: called once, inside `LOADED`'s lock, with the path CLAP
        // asks for: the file, or the bundle on macOS.
        if !unsafe { init(bundle.as_ptr()) } {
            return Err(fail("failed to initialize"));
        }
        // SAFETY: the library is initialized.
        let factory = unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) };
        if factory.is_null() {
            if let Some(deinit) = found.deinit {
                // SAFETY: initialized above, and nothing of it is in use.
                unsafe { deinit() };
            }
            return Err(fail("has no plugin factory"));
        }
        Ok(Library {
            path: path.to_owned(),
            entry,
            factory: factory.cast(),
            library: Some(library),
        })
    }

    fn factory(&self) -> &clap_plugin_factory {
        // SAFETY: checked non-null in `open`, and valid until `deinit`.
        unsafe { &*self.factory }
    }

    /// The plugins in the library, in the factory's order.
    pub(crate) fn plugins(&self) -> Vec<PluginInfo> {
        let factory = self.factory();
        let (Some(count), Some(get)) = (factory.get_plugin_count, factory.get_plugin_descriptor)
        else {
            return Vec::new();
        };
        // SAFETY: the factory's functions are thread-safe.
        let count = unsafe { count(factory) };
        (0..count)
            .filter_map(|index| {
                // SAFETY: as above, with an index below the count.
                let descriptor = unsafe { get(factory, index).as_ref() }?;
                // SAFETY: the descriptor's strings live as long as the
                // library.
                unsafe { self.describe(descriptor) }
            })
            .collect()
    }

    /// # Safety
    ///
    /// `descriptor` must be one of the library's.
    unsafe fn describe(&self, descriptor: &clap_plugin_descriptor) -> Option<PluginInfo> {
        // SAFETY: the caller's promise.
        let text = |s: *const c_char| unsafe { string(s) };
        let id = text(descriptor.id);
        if id.is_empty() || !clap_version_is_compatible(descriptor.clap_version) {
            return None;
        }
        let mut features = Vec::new();
        let mut feature = descriptor.features;
        // SAFETY: a null-terminated list of strings, or null.
        while let Some(&name) = (!feature.is_null()).then(|| unsafe { &*feature }) {
            if name.is_null() {
                break;
            }
            features.push(text(name));
            // SAFETY: the list goes on until its null.
            feature = unsafe { feature.add(1) };
        }
        Some(PluginInfo {
            format: PluginFormat::Clap,
            id,
            name: text(descriptor.name),
            vendor: text(descriptor.vendor),
            version: text(descriptor.version),
            description: text(descriptor.description),
            features,
            path: self.path.clone(),
        })
    }

    /// Creates the plugin `id`, not yet initialized, for `host`.
    ///
    /// # Safety
    ///
    /// `host` must stay valid until the plugin is destroyed.
    pub(crate) unsafe fn create(&self, host: *const clap_host, id: &CStr) -> *const clap_plugin {
        let factory = self.factory();
        match factory.create_plugin {
            // SAFETY: the factory's functions are thread-safe, and the
            // caller keeps `host` alive.
            Some(create) => unsafe { create(factory, host, id.as_ptr()) },
            None => ptr::null(),
        }
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        // As in `load`: CLAP forbids deinitializing as another library
        // initializes.
        let _loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: initialized in `open`, and nothing of the library's is in
        // use: every instance holds the library.
        if let Some(deinit) = unsafe { (*self.entry).deinit } {
            // SAFETY: as above.
            unsafe { deinit() };
        }
        drop(self.library.take());
    }
}

/// The path a library is known by: absolute, with links resolved. On
/// Windows, without the `\\?\` prefix that resolving adds, which plugins
/// that work out paths from theirs don't expect.
pub(crate) fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let path = path.canonicalize()?;
    if cfg!(windows)
        && let Some(rest) = path.to_str().and_then(|p| p.strip_prefix(r"\\?\"))
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(rest));
    }
    Ok(path)
}

/// A C string from the library, or empty if it's null.
///
/// # Safety
///
/// `s` must be null or nul-terminated.
pub(crate) unsafe fn string(s: *const c_char) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: the caller's promise.
    unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
}

fn path_cstring(path: &Path) -> Result<CString, Error> {
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| Error::new(format!("{} has a nul in its path", path.display())))
}

/// The file to load for the plugin at `path`: on macOS, a `.clap` is a
/// bundle, with its binary inside; elsewhere, the file itself.
fn binary(path: &Path) -> Result<PathBuf, Error> {
    if !path.is_dir() {
        return Ok(path.to_owned());
    }
    let missing = || Error::new(format!("{} has no binary in it", path.display()));
    let macos = path.join("Contents").join("MacOS");
    let plist =
        std::fs::read_to_string(path.join("Contents").join("Info.plist")).unwrap_or_default();
    if let Some(name) = bundle_executable(&plist) {
        return Ok(macos.join(name));
    }
    let mut files = std::fs::read_dir(&macos)
        .map_err(|_| missing())?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.is_file());
    match (files.next(), files.next()) {
        (Some(only), None) => Ok(only),
        _ => Err(missing()),
    }
}

/// `CFBundleExecutable` in an `Info.plist`.
fn bundle_executable(plist: &str) -> Option<&str> {
    let after = plist.split_once("<key>CFBundleExecutable</key>")?.1;
    let value = after.trim_start().strip_prefix("<string>")?;
    let name = value.split_once("</string>")?.0.trim();
    (!name.is_empty() && !name.contains(['/', '\\'])).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_name_their_binary() {
        let plist = "<dict>\n\t<key>CFBundleExecutable</key>\n\t<string>Surge XT</string>\n</dict>";
        assert_eq!(bundle_executable(plist), Some("Surge XT"));
        assert_eq!(bundle_executable("<dict></dict>"), None);
        let escape = "<key>CFBundleExecutable</key><string>../evil</string>";
        assert_eq!(bundle_executable(escape), None);
    }
}
