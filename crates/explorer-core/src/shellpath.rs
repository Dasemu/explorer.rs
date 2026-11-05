// SPDX-License-Identifier: GPL-3.0-or-later
//! Path helpers shared by the taskbar's app matching and the tray's
//! promotion lookup.
//!
//! Windows stores many program paths relative to a *known folder*:
//! `{6D809377-6AF0-444B-8957-A3773F02200E}\Vendor\app.exe` means
//! `C:\Program Files\Vendor\app.exe`. `shell:AppsFolder` parsing names of
//! classic apps and `HKCU\Control Panel\NotifyIconSettings\*\ExecutablePath`
//! both use this form. The GUID -> folder map is resolved by the Win32 layer
//! (`SHGetKnownFolderPath`) and passed in, so everything here is pure.

/// Known-folder GUIDs commonly found in stored program paths. The Win32
/// layer resolves each with `SHGetKnownFolderPath`.
pub const COMMON_KNOWN_FOLDERS: &[&str] = &[
    "{905E63B6-C1BF-494E-B29C-65B732D3D21A}", // ProgramFiles
    "{6D809377-6AF0-444B-8957-A3773F02200E}", // ProgramFilesX64
    "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}", // ProgramFilesX86
    "{F7F1ED05-9F6D-47A2-AAAE-29D317C6F066}", // ProgramFilesCommon
    "{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}", // System
    "{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}", // SystemX86
    "{F38BF404-1D43-42F2-9305-67DE0B28FC23}", // Windows
    "{F1B32785-6FBA-4FCF-9D55-7B8E7F157091}", // LocalAppData
    "{3EB685DB-65F9-4CF6-A03A-E3EF65729F3D}", // RoamingAppData
    "{5CD7AEE2-2219-4A67-B85D-6C9CE15660CB}", // UserProgramFiles
    "{A77F5D77-2E2B-44C3-A6A2-ABA601054A51}", // Programs (Start menu)
    "{0139D44E-6AFE-49F2-8690-3DAFCAE6FFB8}", // CommonPrograms
    "{5E6C858F-0E22-4760-9AFE-EA3317B67173}", // Profile
];

/// Map of known-folder GUID (any case, with braces) to its file-system path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownFolders {
    entries: Vec<(String, String)>,
}

impl KnownFolders {
    pub fn new(entries: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(g, p)| (g.to_ascii_uppercase(), p))
                .collect(),
        }
    }

    pub fn get(&self, guid: &str) -> Option<&str> {
        let g = guid.to_ascii_uppercase();
        self.entries
            .iter()
            .find(|(k, _)| *k == g)
            .map(|(_, p)| p.as_str())
    }

    /// Replace a leading `{GUID}` with the folder path when known. Paths
    /// without a GUID prefix are returned unchanged.
    pub fn expand(&self, path: &str) -> String {
        let p = path.trim();
        if p.starts_with('{') {
            if let Some(end) = p.find('}') {
                let guid = &p[..=end];
                if let Some(folder) = self.get(guid) {
                    let rest = p[end + 1..].trim_start_matches(['\\', '/']);
                    if rest.is_empty() {
                        return folder.to_string();
                    }
                    return format!("{}\\{}", folder.trim_end_matches(['\\', '/']), rest);
                }
            }
        }
        p.to_string()
    }
}

/// Canonical form for comparisons: lower case, backslashes, no `\\?\`
/// prefix, no trailing separator, no surrounding quotes.
pub fn normalize(path: &str) -> String {
    let p = path.trim().trim_matches('"');
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    let mut s: String = p
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect::<String>()
        .to_lowercase();
    while s.len() > 3 && s.ends_with('\\') {
        s.pop();
    }
    s
}

/// Case-insensitive path equality (Windows semantics). Empty paths never
/// match.
pub fn same_path(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    !a.is_empty() && a == b
}

/// File name without directory and extension (`C:\x\Foo.exe` -> `Foo`).
pub fn file_stem(path: &str) -> &str {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// True if an id is a file-system path (possibly known-folder relative)
/// rather than an AppUserModelID.
pub fn looks_like_path(id: &str) -> bool {
    let l = id.to_ascii_lowercase();
    id.contains('\\')
        || id.contains('/')
        || (id.len() > 2 && id.as_bytes()[1] == b':')
        || l.ends_with(".exe")
        || l.ends_with(".lnk")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kf() -> KnownFolders {
        KnownFolders::new([
            (
                "{6d809377-6af0-444b-8957-a3773f02200e}".to_string(),
                r"C:\Program Files".to_string(),
            ),
            (
                "{F38BF404-1D43-42F2-9305-67DE0B28FC23}".to_string(),
                r"C:\Windows\".to_string(),
            ),
        ])
    }

    #[test]
    fn expands_known_folders() {
        let k = kf();
        assert_eq!(
            k.expand(r"{6D809377-6AF0-444B-8957-A3773F02200E}\Mozilla Firefox\firefox.exe"),
            r"C:\Program Files\Mozilla Firefox\firefox.exe"
        );
        assert_eq!(
            k.expand(r"{F38BF404-1D43-42F2-9305-67DE0B28FC23}\explorer.exe"),
            r"C:\Windows\explorer.exe"
        );
        assert_eq!(
            k.expand(r"{00000000-0000-0000-0000-000000000000}\x"),
            r"{00000000-0000-0000-0000-000000000000}\x"
        );
        assert_eq!(k.expand(r"D:\Tools\a.exe"), r"D:\Tools\a.exe");
        assert_eq!(k.expand("{broken"), "{broken");
    }

    #[test]
    fn path_comparison() {
        assert!(same_path(
            r"C:\Program Files\A\a.EXE",
            r"c:/program files/a/a.exe"
        ));
        assert!(same_path(r"\\?\C:\x\y.exe", r#""C:\X\Y.exe""#));
        assert!(same_path(r"C:\x\", r"C:\x"));
        assert!(!same_path("", ""));
        assert!(!same_path(r"C:\a.exe", r"C:\b.exe"));
        assert_eq!(normalize(r"C:\"), r"c:\");
    }

    #[test]
    fn stems_and_path_detection() {
        assert_eq!(file_stem(r"C:\x\Foo.Bar.exe"), "Foo.Bar");
        assert_eq!(file_stem("notepad"), "notepad");
        assert_eq!(file_stem(".hidden"), ".hidden");
        assert!(looks_like_path(r"{GUID}\a\b.exe"));
        assert!(looks_like_path(r"C:\a"));
        assert!(looks_like_path("app.exe"));
        assert!(!looks_like_path(
            "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"
        ));
        assert!(!looks_like_path("MSEdge"));
        assert!(!looks_like_path("Chrome"));
    }
}
