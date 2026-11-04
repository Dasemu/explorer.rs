// SPDX-License-Identifier: GPL-3.0-or-later
//! Exclusion rules (`[index] exclude_names` / `exclude_paths`).
//!
//! Name rules match a single path component case-insensitively and may use
//! `*` wildcards. Path rules are case-insensitive prefixes of the full path;
//! `/` and `\` are treated as the same separator.

use explorer_core::config::IndexConfig;

use crate::index::fold;

#[derive(Debug, Clone, Default)]
pub struct Rules {
    names: Vec<String>,
    paths: Vec<String>,
}

fn normalize_path(p: &str) -> String {
    let mut s: String = fold(p)
        .chars()
        .map(|c| if c == '\\' { '/' } else { c })
        .collect();
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// Case-sensitive `*` wildcard match (inputs are already folded).
fn wildcard(pat: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

impl Rules {
    pub fn new(names: &[String], paths: &[String]) -> Self {
        Self {
            names: names
                .iter()
                .map(|n| fold(n))
                .filter(|n| !n.is_empty())
                .collect(),
            paths: paths
                .iter()
                .map(|p| normalize_path(p))
                .filter(|p| !p.is_empty())
                .collect(),
        }
    }

    pub fn from_config(c: &IndexConfig) -> Self {
        Self::new(&c.exclude_names, &c.exclude_paths)
    }

    pub fn has_path_rules(&self) -> bool {
        !self.paths.is_empty()
    }

    pub fn excludes_name(&self, name: &str) -> bool {
        if self.names.is_empty() {
            return false;
        }
        let f = fold(name);
        self.names.iter().any(|p| {
            if p.contains('*') {
                wildcard(p, &f)
            } else {
                *p == f
            }
        })
    }

    /// True if `path` equals or lies below an excluded path.
    pub fn excludes_path(&self, path: &str) -> bool {
        if self.paths.is_empty() {
            return false;
        }
        let n = normalize_path(path);
        self.paths.iter().any(|p| {
            n.starts_with(p.as_str())
                && (n.len() == p.len() || n.as_bytes()[p.len()] == b'/' || p.ends_with('/'))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_wildcards() {
        let r = Rules::new(
            &["$Recycle.Bin".into(), "*.tmp".into(), "node_*".into()],
            &[],
        );
        assert!(r.excludes_name("$RECYCLE.BIN"));
        assert!(r.excludes_name("a.TMP"));
        assert!(r.excludes_name("node_modules"));
        assert!(!r.excludes_name("tmp"));
        assert!(!r.excludes_name("recycle"));
        assert!(wildcard("a*b*c", "axxbyyc"));
        assert!(!wildcard("a*b*c", "axxbyy"));
        assert!(wildcard("*", ""));
    }

    #[test]
    fn path_prefixes() {
        let r = Rules::new(&[], &["C:\\Windows\\WinSxS".into(), "/proc/".into()]);
        assert!(r.excludes_path("c:/windows/winsxs"));
        assert!(r.excludes_path("C:\\Windows\\WinSxS\\x86\\a.dll"));
        assert!(!r.excludes_path("C:\\Windows\\WinSxSBackup"));
        assert!(r.excludes_path("/proc/1"));
        assert!(!Rules::default().excludes_path("/anything"));
    }
}
