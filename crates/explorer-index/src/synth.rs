// SPDX-License-Identifier: GPL-3.0-or-later
//! Deterministic synthetic file trees for tests and benchmarks.
//!
//! The shape roughly imitates a Windows system volume: a few thousand
//! folders with fan-out 5..40, file names built from a vocabulary of common
//! words, mixed case, version-like numbers and a realistic extension mix.

use crate::index::{ScannedTree, NONE};

const WORDS: &[&str] = &[
    "report",
    "invoice",
    "Setup",
    "readme",
    "config",
    "main",
    "lib",
    "test",
    "data",
    "image",
    "Photo",
    "backup",
    "notes",
    "Project",
    "draft",
    "final",
    "summary",
    "budget",
    "app",
    "core",
    "util",
    "helper",
    "module",
    "index",
    "server",
    "client",
    "window",
    "System",
    "update",
    "driver",
    "cache",
    "temp",
    "log",
    "music",
    "video",
    "Document",
    "manual",
    "license",
    "changelog",
    "build",
    "release",
    "debug",
    "resource",
    "font",
    "icon",
    "theme",
    "plugin",
    "service",
    "network",
    "storage",
    "user",
    "profile",
    "settings",
    "Microsoft",
    "Windows",
    "Program",
    "explorer",
    "photo",
    "holiday",
    "scan",
    "contract",
    "letter",
    "résumé",
    "café",
];
const EXTS: &[&str] = &[
    "dll", "exe", "txt", "jpg", "png", "rs", "toml", "json", "xml", "pdf", "docx", "xlsx", "mp3",
    "mp4", "log", "ini", "dat", "cab", "mui", "sys", "js", "ts", "py", "c", "h", "md",
];

/// Small deterministic PRNG (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

fn sibling_key(parent: u32, name: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parent.hash(&mut h);
    name.to_lowercase().hash(&mut h);
    h.finish()
}

/// Generate a tree with exactly `n` entries (about 8% folders). Sibling
/// names are unique case-insensitively, as on a real file system.
pub fn tree(n: usize, seed: u64) -> ScannedTree {
    let mut rng = Rng::new(seed);
    let mut taken = std::collections::HashSet::with_capacity(n);
    let mut t = ScannedTree::default();
    let mut dirs: Vec<u32> = vec![NONE];
    let mut name = String::new();
    while t.len() < n {
        // Prefer recent folders so depth grows to ~6-10 levels.
        let pick = if dirs.len() > 1 && rng.below(4) != 0 {
            dirs.len() - 1 - rng.below(dirs.len().min(64) as u64) as usize
        } else {
            rng.below(dirs.len() as u64) as usize
        };
        let parent = dirs[pick];
        let is_dir = rng.below(100) < 8;
        name.clear();
        let words = 1 + rng.below(3);
        for i in 0..words {
            if i > 0 {
                name.push(match rng.below(4) {
                    0 => ' ',
                    1 => '-',
                    2 => '_',
                    _ => '.',
                });
            }
            name.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
        }
        if rng.below(3) == 0 {
            name.push_str(&format!("{}", rng.below(10_000)));
        }
        if !is_dir {
            name.push('.');
            name.push_str(EXTS[rng.below(EXTS.len() as u64) as usize]);
        }
        let mut k = 2;
        while !taken.insert(sibling_key(parent, &name)) {
            name.insert_str(0, &format!("({k}) "));
            k += 1;
        }
        let size = (!is_dir).then(|| rng.below(1 << 24));
        let i = t.push(parent, &name, is_dir, size, NONE);
        if is_dir {
            dirs.push(i);
        }
    }
    t
}

#[cfg(test)]
mod tests {
    #[test]
    fn deterministic() {
        let a = super::tree(1000, 7);
        let b = super::tree(1000, 7);
        assert_eq!(a.len(), 1000);
        assert_eq!(
            (0..1000).map(|i| a.name(i)).collect::<Vec<_>>(),
            (0..1000).map(|i| b.name(i)).collect::<Vec<_>>()
        );
        assert!(a.is_dir.iter().filter(|&&d| d).count() > 30);
    }
}
