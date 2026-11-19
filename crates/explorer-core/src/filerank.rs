// SPDX-License-Identifier: GPL-3.0-or-later
//! Which file-index hits the Start menu shows, and how their location reads.
//!
//! The index covers whole volumes (like Everything), so a query such as
//! "blo" also finds `...\resource_packs\vanilla\sounds\block` folders of a
//! game. The Start menu is about the user's documents:
//! - hits under system and application folders (Windows, Program Files,
//!   ProgramData, AppData, WindowsApps, node_modules, .git, ...) are
//!   dropped;
//! - outside the user profile, very deep hits are dropped too (application
//!   data rather than documents);
//! - the rest is ordered: user profile first, then shallower paths, keeping
//!   the index order otherwise.
//!
//! Paths are Windows paths handled as text (`\` or `/`), so the rules are
//! tested on every platform.

/// One hit of the file index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHit {
    pub path: String,
    pub is_dir: bool,
}

/// Folder names (any level, case-insensitive) whose content is never a
/// Start menu result.
const NOISE_DIRS: &[&str] = &[
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "appdata",
    "windowsapps",
    "xboxgames",
    "steamapps",
    "$recycle.bin",
    "system volume information",
    "$winreagent",
    "node_modules",
    ".git",
    ".svn",
    "__pycache__",
    ".cache",
    ".vs",
];

/// Components below the volume root allowed outside the user profile
/// (`D:\Fotos\2024\Viaje\IMG_1.jpg` has 4).
const MAX_DEPTH_OUTSIDE_HOME: usize = 6;

fn components(p: &str) -> Vec<&str> {
    p.split(['\\', '/']).filter(|c| !c.is_empty()).collect()
}

fn under(path: &str, dir: &str) -> bool {
    let (p, d) = (components(path), components(dir));
    !d.is_empty() && p.len() > d.len() && p.iter().zip(&d).all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Whether a hit is worth showing in the Start menu. `home` is the user
/// profile folder (`%USERPROFILE%`).
pub fn relevant(path: &str, home: Option<&str>) -> bool {
    let in_home = home.is_some_and(|h| under(path, h));
    let comps = components(path);
    // Skip the volume ("C:") or UNC server/share when checking folders.
    let below_root = comps.get(1..).unwrap_or(&[]);
    let folders = &below_root[..below_root.len().saturating_sub(1)];
    if folders
        .iter()
        .any(|c| NOISE_DIRS.iter().any(|n| c.eq_ignore_ascii_case(n)))
    {
        return false;
    }
    in_home || below_root.len() <= MAX_DEPTH_OUTSIDE_HOME
}

/// Filter and order hits (see the module docs), keeping at most `limit`.
pub fn rank(hits: Vec<FileHit>, home: Option<&str>, limit: usize) -> Vec<FileHit> {
    let mut v: Vec<(usize, bool, usize, FileHit)> = hits
        .into_iter()
        .enumerate()
        .filter(|(_, h)| relevant(&h.path, home))
        .map(|(i, h)| {
            let in_home = home.is_some_and(|d| under(&h.path, d));
            (i, in_home, components(&h.path).len(), h)
        })
        .collect();
    // Stable: index order breaks ties.
    v.sort_by_key(|(i, in_home, depth, _)| (!*in_home, *depth, *i));
    v.into_iter().take(limit).map(|t| t.3).collect()
}

/// The folder of a result as shown under its name: short paths as they
/// are, long ones as `C:\…\last\three\folders`, and, if still longer than
/// `max_chars`, cut in the middle, so that hits in sibling folders never
/// look identical (a trailing ellipsis hides exactly the part that
/// differs).
pub fn short_location(folder: &str, max_chars: usize) -> String {
    let comps = components(folder);
    let sep = if folder.contains('\\') { '\\' } else { '/' };
    let mut s = if comps.len() > 5 && folder.chars().count() > max_chars {
        let mut out = String::from(comps[0]);
        out.push(sep);
        out.push('…');
        for c in &comps[comps.len() - 3..] {
            out.push(sep);
            out.push_str(c);
        }
        out
    } else {
        folder.to_string()
    };
    let n = s.chars().count();
    if n > max_chars && max_chars >= 5 {
        let keep = max_chars - 1;
        let head: String = s.chars().take(keep / 2).collect();
        let tail: String = s.chars().skip(n - (keep - keep / 2)).collect();
        s = format!("{head}…{tail}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = r"C:\Users\Ana";

    fn hit(p: &str) -> FileHit {
        FileHit {
            path: p.into(),
            is_dir: false,
        }
    }

    #[test]
    fn noise_folders_are_dropped() {
        let h = Some(HOME);
        assert!(!relevant(r"C:\Windows\System32\block.dll", h));
        assert!(!relevant(r"C:\Program Files\App\blocks.txt", h));
        assert!(!relevant(r"C:\Users\Ana\AppData\Local\x\block", h));
        assert!(!relevant(r"D:\src\web\node_modules\block\index.js", h));
        assert!(!relevant(r"E:\XboxGames\Game\Content\block", h));
        // The name itself may be anything.
        assert!(relevant(r"C:\Users\Ana\Documents\Windows", h));
        assert!(relevant(r"C:\Users\Ana\Documents\bloc.docx", h));
    }

    #[test]
    fn deep_paths_outside_home_are_dropped() {
        let h = Some(HOME);
        let game =
            r"J:\Xbox\Minecraft for Windows\Content\data\resource_packs\vanilla\sounds\block";
        assert!(!relevant(game, h));
        assert!(relevant(r"D:\Fotos\2024\Viaje\bloque.jpg", h));
        // Deep inside the profile is fine.
        assert!(relevant(
            r"C:\Users\Ana\Documents\a\b\c\d\e\f\g\bloc.txt",
            h
        ));
        assert!(relevant(r"\\nas\share\docs\bloc.txt", None));
    }

    #[test]
    fn home_first_then_shallow_then_index_order() {
        let hits = vec![
            hit(r"D:\x\y\block.txt"),
            hit(r"D:\block.txt"),
            hit(r"C:\Users\Ana\Desktop\deep\bloc.txt"),
            hit(r"C:\Windows\block.ini"),
            hit(r"C:\Users\Ana\bloc.txt"),
        ];
        let r: Vec<String> = rank(hits, Some(HOME), 3)
            .into_iter()
            .map(|h| h.path)
            .collect();
        assert_eq!(
            r,
            [
                r"C:\Users\Ana\bloc.txt",
                r"C:\Users\Ana\Desktop\deep\bloc.txt",
                r"D:\block.txt"
            ]
        );
        // Case-insensitive profile match; no profile known.
        assert!(under(r"c:\users\ana\x.txt", HOME));
        assert_eq!(rank(vec![hit(r"D:\a.txt")], None, 5).len(), 1);
    }

    #[test]
    fn locations_stay_distinguishable() {
        let a = r"J:\Xbox\Minecraft for Windows\Content\data\resource_packs\vanilla_1.20\sounds";
        let b = r"J:\Xbox\Minecraft for Windows\Content\data\resource_packs\vanilla_1.21\sounds";
        let (sa, sb) = (short_location(a, 48), short_location(b, 48));
        assert_eq!(sa, r"J:\…\resource_packs\vanilla_1.20\sounds");
        assert_ne!(sa, sb);
        assert_eq!(
            short_location(r"C:\Users\Ana\Documents", 48),
            r"C:\Users\Ana\Documents"
        );
        let long = "C:/".to_string() + &"x".repeat(100);
        let s = short_location(&long, 20);
        assert_eq!(s.chars().count(), 20);
        assert!(s.contains('…') && s.starts_with("C:/"));
    }
}
