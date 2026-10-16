//! Portable atomic file writes for the core.
//!
//! The desktop crate writes state through `fastframe-file`, which also panics
//! when a write runs on the UI thread; core cannot depend on it. This module
//! provides the same write-a-sibling-then-rename pattern in plain `std`, so a
//! crash never leaves half a file. Writes that wait for the disk still belong
//! on the runtime, not the UI thread, whether they go through this or through
//! fastframe-file.

use std::io;
use std::path::{Path, PathBuf};

/// Writes `bytes` to `path` atomically: a temporary file beside the target is
/// written, then renamed over it.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with_mode(path, bytes, None)
}

/// Like [`write`], with the file readable only by its owner where the platform
/// supports it.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_with_mode(path, bytes, Some(0o600))
}

fn write_with_mode(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(path);
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, path)
}

/// A temporary name beside `path`, so the rename below stays on the same
/// filesystem and cannot cross devices.
fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(name)
}
