// SPDX-License-Identifier: GPL-3.0-or-later
//! Filtering results by the caller's access rights.
//!
//! An MFT index contains every file name on the volume, including names in
//! folders the user cannot open. The daemon runs as the (non-elevated)
//! user, and the pipe only admits that user, so "can the caller see this
//! name" is approximated by "can this process list the parent folder".
//! Results are checked lazily, page by page, with one check per distinct
//! parent folder.

use std::collections::HashMap;
use std::path::Path;

/// Decides whether a parent folder is listable. Cached per query.
pub trait AccessCheck {
    fn can_list(&mut self, dir: &Path) -> bool;
}

/// Allows everything (`check_access = false`).
pub struct AllowAll;

impl AccessCheck for AllowAll {
    fn can_list(&mut self, _dir: &Path) -> bool {
        true
    }
}

/// Checks with `std::fs::read_dir` (`FindFirstFileW` on Windows), which
/// fails with "access denied" exactly when the folder cannot be listed.
#[derive(Default)]
pub struct FsAccess {
    cache: HashMap<std::path::PathBuf, bool>,
}

impl AccessCheck for FsAccess {
    fn can_list(&mut self, dir: &Path) -> bool {
        if let Some(&v) = self.cache.get(dir) {
            return v;
        }
        let ok = std::fs::read_dir(dir).is_ok();
        self.cache.insert(dir.to_path_buf(), ok);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_access_caches() {
        let d = tempfile::tempdir().unwrap();
        let mut a = FsAccess::default();
        assert!(a.can_list(d.path()));
        assert!(!a.can_list(&d.path().join("missing")));
        assert_eq!(a.cache.len(), 2);
        assert!(AllowAll.can_list(Path::new("/nonexistent")));
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_dir_is_hidden() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok(); // true when running as root
        let mut a = FsAccess::default();
        assert_eq!(a.can_list(&locked), readable);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
