// SPDX-License-Identifier: GPL-3.0-or-later
//! Small file helpers shared by the state files several processes write
//! (pins, crash log).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Makes temporary file names unique within this process.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary file name next to `path`, unique per process and call, so
/// two writers (two processes, or two threads) never share a temp file.
pub fn unique_tmp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!("{name}.{}.{n}.tmp", std::process::id()))
}

/// Write `data` to `path` atomically: unique temporary file, then rename.
/// The temporary file is removed on failure.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = unique_tmp_path(path);
    if let Err(e) = std::fs::write(&tmp, data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_are_unique() {
        let p = Path::new("/x/pins.toml");
        let a = unique_tmp_path(p);
        let b = unique_tmp_path(p);
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("pins.toml."));
        assert!(name.contains(&std::process::id().to_string()));
        assert_eq!(a.parent(), p.parent());
    }

    #[test]
    fn atomic_write_replaces_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f.toml");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
        // Missing folder: error, no leftovers.
        assert!(write_atomic(&d.path().join("no/f.toml"), b"x").is_err());
    }
}
