//! Plugin files: the paths the host knows them by, and the binaries inside
//! macOS bundles. What hosting CLAP and VST3 plugins share.

use std::io;
use std::path::{Path, PathBuf};

use crate::Error;

/// The path a library is known by: absolute, with links resolved. On
/// Windows, without the `\\?\` prefix that resolving adds, which plugins
/// that work out paths from theirs don't expect.
pub(crate) fn canonical(path: &Path) -> io::Result<PathBuf> {
    let path = path.canonicalize()?;
    if cfg!(windows)
        && let Some(rest) = path.to_str().and_then(|p| p.strip_prefix(r"\\?\"))
        && rest.as_bytes().get(1) == Some(&b':')
    {
        return Ok(PathBuf::from(rest));
    }
    Ok(path)
}

/// The binary inside a macOS bundle: the one its `Info.plist` names, or the
/// only file in `Contents/MacOS`.
pub(crate) fn macos_binary(bundle: &Path) -> Result<PathBuf, Error> {
    let missing = || Error::new(format!("{} has no binary in it", bundle.display()));
    let macos = bundle.join("Contents").join("MacOS");
    let plist =
        std::fs::read_to_string(bundle.join("Contents").join("Info.plist")).unwrap_or_default();
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
