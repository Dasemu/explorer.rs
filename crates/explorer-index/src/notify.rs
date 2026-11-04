// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure parsing of `ReadDirectoryChangesW` output (`FILE_NOTIFY_INFORMATION`
//! records) and translation into [`Change`]s.
//!
//! The Windows watcher (`win::watch`) only performs the system call; all
//! decisions live here so they are unit-tested on Linux.

use crate::index::VolId;
use crate::source::Change;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Added,
    Removed,
    Modified,
    RenamedOld,
    RenamedNew,
    Other(u32),
}

impl Action {
    fn from_raw(v: u32) -> Self {
        match v {
            1 => Self::Added,
            2 => Self::Removed,
            3 => Self::Modified,
            4 => Self::RenamedOld,
            5 => Self::RenamedNew,
            o => Self::Other(o),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notify {
    pub action: Action,
    /// Path relative to the watched directory, `\`-separated.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotifyError {
    #[error("notification buffer truncated")]
    Truncated,
    #[error("bad next-entry offset")]
    BadOffset,
}

/// Parse a buffer of `FILE_NOTIFY_INFORMATION` records.
pub fn parse_notify(buf: &[u8]) -> Result<Vec<Notify>, NotifyError> {
    let mut out = Vec::new();
    let mut off = 0usize;
    if buf.is_empty() {
        return Ok(out);
    }
    loop {
        let rec = buf.get(off..).ok_or(NotifyError::Truncated)?;
        if rec.len() < 12 {
            return Err(NotifyError::Truncated);
        }
        let next = u32::from_le_bytes(rec[0..4].try_into().unwrap_or_default()) as usize;
        let action = u32::from_le_bytes(rec[4..8].try_into().unwrap_or_default());
        let len = u32::from_le_bytes(rec[8..12].try_into().unwrap_or_default()) as usize;
        if len % 2 != 0 || 12 + len > rec.len() {
            return Err(NotifyError::Truncated);
        }
        let units: Vec<u16> = rec[12..12 + len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        out.push(Notify {
            action: Action::from_raw(action),
            path: String::from_utf16_lossy(&units),
        });
        if next == 0 {
            return Ok(out);
        }
        if next < 12 + len {
            return Err(NotifyError::BadOffset);
        }
        off += next;
    }
}

/// Encode records (tests and fuzz seeds).
pub fn encode_notify(items: &[(u32, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, (action, path)) in items.iter().enumerate() {
        let units: Vec<u16> = path.encode_utf16().collect();
        let len = 12 + units.len() * 2;
        let padded = len.div_ceil(4) * 4;
        let next = if i + 1 == items.len() { 0 } else { padded };
        out.extend((next as u32).to_le_bytes());
        out.extend(action.to_le_bytes());
        out.extend(((units.len() * 2) as u32).to_le_bytes());
        for u in units {
            out.extend(u.to_le_bytes());
        }
        out.resize(out.len() + (padded - len), 0);
    }
    out
}

/// Split a relative notification path into components.
pub fn components(rel: &str) -> Vec<String> {
    rel.split(['\\', '/'])
        .filter(|c| !c.is_empty() && *c != ".")
        .map(String::from)
        .collect()
}

/// Result of translating notifications.
#[derive(Debug, Default)]
pub struct Translated {
    pub changes: Vec<Change>,
    /// Newly appeared directories whose contents must be walked (a folder
    /// moved in from elsewhere produces a single notification).
    pub walk: Vec<Vec<String>>,
}

/// Translate notifications into changes. `stat` reports whether a path
/// (components below the root) currently exists, is a directory, and its
/// size; `None` means it is gone (or inaccessible).
pub fn translate(
    vol: VolId,
    events: &[Notify],
    mut stat: impl FnMut(&[String]) -> Option<(bool, Option<u64>)>,
) -> Translated {
    let mut t = Translated::default();
    let mut pending_old: Option<Vec<String>> = None;
    for ev in events {
        let path = components(&ev.path);
        if path.is_empty() {
            continue;
        }
        match ev.action {
            Action::Added | Action::Modified => {
                if let Some((is_dir, size)) = stat(&path) {
                    if is_dir && ev.action == Action::Added {
                        t.walk.push(path.clone());
                    }
                    t.changes.push(Change::Upsert {
                        vol,
                        path,
                        is_dir,
                        size,
                    });
                }
            }
            Action::Removed => t.changes.push(Change::Remove { vol, path }),
            Action::RenamedOld => {
                if let Some(old) = pending_old.replace(path) {
                    t.changes.push(Change::Remove { vol, path: old });
                }
            }
            Action::RenamedNew => {
                let (is_dir, size) = stat(&path).unwrap_or((false, None));
                match pending_old.take() {
                    Some(from) => t.changes.push(Change::Rename {
                        vol,
                        from,
                        to: path,
                        is_dir,
                        size,
                    }),
                    None => {
                        if is_dir {
                            t.walk.push(path.clone());
                        }
                        t.changes.push(Change::Upsert {
                            vol,
                            path,
                            is_dir,
                            size,
                        })
                    }
                }
            }
            Action::Other(_) => {}
        }
    }
    if let Some(old) = pending_old {
        t.changes.push(Change::Remove { vol, path: old });
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip_and_garbage() {
        let b = encode_notify(&[(1, "a\\b.txt"), (4, "old"), (5, "névé")]);
        let n = parse_notify(&b).unwrap();
        assert_eq!(n.len(), 3);
        assert_eq!(n[0].action, Action::Added);
        assert_eq!(n[2].path, "névé");
        for cut in 1..b.len() {
            let _ = parse_notify(&b[..cut]);
        }
        assert!(parse_notify(&[0; 5]).is_err());
        assert_eq!(parse_notify(&[]).unwrap(), vec![]);
        // next offset pointing backwards into the header is rejected
        let mut bad = encode_notify(&[(1, "x"), (1, "y")]);
        bad[0] = 4;
        assert_eq!(parse_notify(&bad), Err(NotifyError::BadOffset));
    }

    #[test]
    fn translate_pairs() {
        let evs = parse_notify(&encode_notify(&[
            (1, "dir"),
            (1, "dir\\f.txt"),
            (4, "dir\\f.txt"),
            (5, "dir\\g.txt"),
            (3, "gone.txt"),
            (2, "old.txt"),
            (4, "orphan-old"),
        ]))
        .unwrap();
        let t = translate(0, &evs, |p| match p.last().map(String::as_str) {
            Some("dir") => Some((true, None)),
            Some("gone.txt") => None,
            _ => Some((false, Some(3))),
        });
        assert_eq!(t.walk, vec![vec!["dir".to_string()]]);
        assert_eq!(t.changes.len(), 5);
        assert!(
            matches!(&t.changes[2], Change::Rename { from, to, .. } if from[1] == "f.txt" && to[1] == "g.txt")
        );
        assert!(matches!(&t.changes[4], Change::Remove { path, .. } if path[0] == "orphan-old"));
    }
}
