// SPDX-License-Identifier: GPL-3.0-or-later
//
// Windows resources (icon, application manifest, VERSIONINFO) for the
// explorer.rs binaries. Shared by every binary crate's build.rs through
// `include!` so no extra build-dependency crate is needed:
//
//     // build.rs
//     include!("../../assets/build/resources.rs");
//     fn main() {
//         embed_resources(&[Bin::new("explorer-rs-taskbar", "explorer.rs taskbar")]);
//     }
//
// What it does (only when the TARGET is Windows MSVC; a no-op otherwise, so
// Linux builds and tests are unaffected):
//   1. writes OUT_DIR/<bin>.manifest from assets/manifests/app.manifest.in,
//   2. writes OUT_DIR/<bin>.res (icon, manifest, VERSIONINFO from Cargo)
//      directly in the compiled resource format, so no resource compiler
//      is needed (a missing one used to ship binaries without manifest:
//      unthemed comctl32 v5 tooltips, no elevation for the installer),
//   3. links the .res into that binary only (`cargo:rustc-link-arg-bin`).
// See docs/PACKAGING.md.

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    AsInvoker,
    RequireAdministrator,
}

#[allow(dead_code)]
struct Bin {
    /// Cargo binary name (also the file name without `.exe`).
    name: &'static str,
    /// VERSIONINFO FileDescription.
    description: &'static str,
    level: Level,
}

#[allow(dead_code)]
impl Bin {
    fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            description,
            level: Level::AsInvoker,
        }
    }

    fn require_admin(mut self) -> Self {
        self.level = Level::RequireAdministrator;
        self
    }
}

#[allow(dead_code)]
fn embed_resources(bins: &[Bin]) {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let assets = manifest_dir.join("../../assets");
    let icon = assets.join("explorer-rs.ico");
    let template = assets.join("manifests/app.manifest.in");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", template.display());
    println!("cargo:rerun-if-changed=../../assets/build/resources.rs");

    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" {
        return;
    }
    if env != "msvc" {
        println!("cargo:warning=Windows resources are only embedded for MSVC targets");
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ver = |k: &str| -> u32 {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let (major, minor, patch) = (
        ver("CARGO_PKG_VERSION_MAJOR"),
        ver("CARGO_PKG_VERSION_MINOR"),
        ver("CARGO_PKG_VERSION_PATCH"),
    );
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let template_text = std::fs::read_to_string(&template).expect("read manifest template");
    // Drop XML comments (licence header, notes) from the embedded manifest.
    let mut manifest_base = String::new();
    let mut rest = template_text.as_str();
    while let Some(start) = rest.find("<!--") {
        manifest_base.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    manifest_base.push_str(rest);

    for bin in bins {
        let level = match bin.level {
            Level::AsInvoker => "asInvoker",
            Level::RequireAdministrator => "requireAdministrator",
        };
        let manifest = manifest_base
            .replace("@NAME@", bin.name)
            .replace("@VERSION4@", &format!("{major}.{minor}.{patch}.0"))
            .replace("@LEVEL@", level);
        let manifest = manifest.trim_start();
        std::fs::write(out.join(format!("{}.manifest", bin.name)), manifest)
            .expect("write manifest");

        let ico = std::fs::read(&icon).expect("read icon");
        let mut res = res::ResFile::new();
        res::add_icon(&mut res, &ico);
        res.add(24, 1, 0x0409, 0x0030, manifest.as_bytes().to_vec());
        let strings: [(&str, String); 8] = [
            ("CompanyName", "explorer.rs contributors".into()),
            ("FileDescription", bin.description.into()),
            ("FileVersion", version.clone()),
            ("InternalName", bin.name.into()),
            (
                "LegalCopyright",
                "GPL-3.0-or-later. Not affiliated with Microsoft.".into(),
            ),
            ("OriginalFilename", format!("{}.exe", bin.name)),
            ("ProductName", "explorer.rs".into()),
            ("ProductVersion", version.clone()),
        ];
        res.add(
            16,
            1,
            0x0409,
            0x0030,
            res::version_info([major, minor, patch, 0], &strings),
        );
        let res_path = out.join(format!("{}.res", bin.name));
        std::fs::write(&res_path, res.finish()).expect("write .res");
        // link.exe and lld-link accept compiled .res files as inputs.
        println!(
            "cargo:rustc-link-arg-bin={}={}",
            bin.name,
            res_path.display()
        );
    }
}

/// Minimal writer of compiled resource (`.res`) files: the format link.exe
/// and lld-link take directly, so no resource compiler is needed.
#[allow(dead_code)]
mod res {
    fn u16s(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn u32s(v: &mut Vec<u8>, x: u32) {
        v.extend_from_slice(&x.to_le_bytes());
    }
    fn pad4(v: &mut Vec<u8>) {
        while v.len() % 4 != 0 {
            v.push(0);
        }
    }
    fn wstr(v: &mut Vec<u8>, s: &str) {
        for c in s.encode_utf16() {
            u16s(v, c);
        }
        u16s(v, 0);
    }

    pub struct ResFile(Vec<u8>);

    impl ResFile {
        pub fn new() -> Self {
            // The file starts with an empty resource entry.
            let mut r = ResFile(Vec::new());
            r.add(0, 0, 0, 0, Vec::new());
            r
        }

        /// Resource with numeric type and name.
        pub fn add(&mut self, ty: u16, name: u16, lang: u16, flags: u16, data: Vec<u8>) {
            let v = &mut self.0;
            u32s(v, data.len() as u32);
            u32s(v, 32); // header size
            u16s(v, 0xFFFF);
            u16s(v, ty);
            u16s(v, 0xFFFF);
            u16s(v, name);
            u32s(v, 0); // data version
            u16s(v, flags);
            u16s(v, lang);
            u32s(v, 0); // version
            u32s(v, 0); // characteristics
            v.extend_from_slice(&data);
            pad4(v);
        }

        pub fn finish(self) -> Vec<u8> {
            self.0
        }
    }

    /// RT_ICON images (ids 1..) and RT_GROUP_ICON 1 from an .ico file.
    pub fn add_icon(res: &mut ResFile, ico: &[u8]) {
        let rd16 = |o: usize| u16::from_le_bytes([ico[o], ico[o + 1]]);
        let rd32 = |o: usize| u32::from_le_bytes([ico[o], ico[o + 1], ico[o + 2], ico[o + 3]]);
        let count = rd16(4) as usize;
        let mut group = Vec::new();
        u16s(&mut group, 0);
        u16s(&mut group, 1);
        u16s(&mut group, count as u16);
        for i in 0..count {
            let e = 6 + 16 * i;
            let size = rd32(e + 8) as usize;
            let offset = rd32(e + 12) as usize;
            let id = (i + 1) as u16;
            res.add(3, id, 0x0409, 0x1010, ico[offset..offset + size].to_vec());
            group.extend_from_slice(&ico[e..e + 8]); // w, h, colors, reserved, planes, bpp
            u32s(&mut group, size as u32);
            u16s(&mut group, id);
        }
        res.add(14, 1, 0x0409, 0x1030, group);
    }

    /// One node of the VS_VERSIONINFO tree.
    fn node(key: &str, ty: u16, value: &[u8], value_len: u16, children: &[Vec<u8>]) -> Vec<u8> {
        let mut v = Vec::new();
        u16s(&mut v, 0); // length, patched below
        u16s(&mut v, value_len);
        u16s(&mut v, ty);
        wstr(&mut v, key);
        pad4(&mut v);
        v.extend_from_slice(value);
        for c in children {
            pad4(&mut v);
            v.extend_from_slice(c);
        }
        let len = v.len() as u16;
        v[0..2].copy_from_slice(&len.to_le_bytes());
        v
    }

    pub fn version_info(ver: [u32; 4], strings: &[(&str, String)]) -> Vec<u8> {
        let mut fixed = Vec::new();
        u32s(&mut fixed, 0xFEEF_04BD);
        u32s(&mut fixed, 0x0001_0000);
        let ms = (ver[0] << 16) | (ver[1] & 0xFFFF);
        let ls = (ver[2] << 16) | (ver[3] & 0xFFFF);
        for x in [ms, ls, ms, ls, 0x3F, 0, 0x0004_0004, 1, 0, 0, 0] {
            u32s(&mut fixed, x);
        }
        let entries: Vec<Vec<u8>> = strings
            .iter()
            .map(|(k, val)| {
                let mut w = Vec::new();
                wstr(&mut w, val);
                node(k, 1, &w, (w.len() / 2) as u16, &[])
            })
            .collect();
        let table = node("040904B0", 1, &[], 0, &entries);
        let sfi = node("StringFileInfo", 1, &[], 0, &[table]);
        let mut tr = Vec::new();
        u16s(&mut tr, 0x0409);
        u16s(&mut tr, 1200);
        let var = node("Translation", 0, &tr, 4, &[]);
        let vfi = node("VarFileInfo", 1, &[], 0, &[var]);
        node("VS_VERSION_INFO", 0, &fixed, fixed.len() as u16, &[sfi, vfi])
    }
}
