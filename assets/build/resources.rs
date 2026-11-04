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
//   2. writes OUT_DIR/<bin>.rc (icon, manifest, VERSIONINFO from Cargo),
//   3. compiles it with `llvm-rc` (or `$RC`, or `rc.exe`) to <bin>.res,
//   4. links the .res into that binary only (`cargo:rustc-link-arg-bin`).
// If no resource compiler is found the build continues without resources
// and prints a cargo warning. See docs/PACKAGING.md.

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
    use std::path::{Path, PathBuf};

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let assets = manifest_dir.join("../../assets");
    let icon = assets.join("explorer-rs.ico");
    let template = assets.join("manifests/app.manifest.in");
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", template.display());
    println!("cargo:rerun-if-changed=../../assets/build/resources.rs");
    println!("cargo:rerun-if-env-changed=RC");

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

    // rc string literal: backslashes doubled, quotes doubled.
    fn rc_str(s: &str) -> String {
        s.replace('\\', "\\\\").replace('"', "\"\"")
    }
    fn rc_path(p: &Path) -> String {
        rc_str(&p.to_string_lossy().replace('\\', "/"))
    }

    let rc_tool = find_rc();
    for bin in bins {
        let level = match bin.level {
            Level::AsInvoker => "asInvoker",
            Level::RequireAdministrator => "requireAdministrator",
        };
        let manifest = manifest_base
            .replace("@NAME@", bin.name)
            .replace("@VERSION4@", &format!("{major}.{minor}.{patch}.0"))
            .replace("@LEVEL@", level);
        let manifest_path = out.join(format!("{}.manifest", bin.name));
        std::fs::write(&manifest_path, manifest.trim_start()).expect("write manifest");

        let rc = format!(
            "#pragma code_page(65001)\n\
             1 ICON \"{icon}\"\n\
             1 24 \"{manifest}\"\n\
             1 VERSIONINFO\n\
             FILEVERSION {major},{minor},{patch},0\n\
             PRODUCTVERSION {major},{minor},{patch},0\n\
             FILEFLAGSMASK 0x3F\n\
             FILEFLAGS 0x0\n\
             FILEOS 0x40004\n\
             FILETYPE 0x1\n\
             FILESUBTYPE 0x0\n\
             BEGIN\n\
             \x20 BLOCK \"StringFileInfo\"\n\
             \x20 BEGIN\n\
             \x20   BLOCK \"040904B0\"\n\
             \x20   BEGIN\n\
             \x20     VALUE \"CompanyName\", \"explorer.rs contributors\"\n\
             \x20     VALUE \"FileDescription\", \"{desc}\"\n\
             \x20     VALUE \"FileVersion\", \"{version}\"\n\
             \x20     VALUE \"InternalName\", \"{name}\"\n\
             \x20     VALUE \"LegalCopyright\", \"GPL-3.0-or-later. Not affiliated with Microsoft.\"\n\
             \x20     VALUE \"OriginalFilename\", \"{name}.exe\"\n\
             \x20     VALUE \"ProductName\", \"explorer.rs\"\n\
             \x20     VALUE \"ProductVersion\", \"{version}\"\n\
             \x20   END\n\
             \x20 END\n\
             \x20 BLOCK \"VarFileInfo\"\n\
             \x20 BEGIN\n\
             \x20   VALUE \"Translation\", 0x409, 1200\n\
             \x20 END\n\
             END\n",
            icon = rc_path(&icon),
            manifest = rc_path(&manifest_path),
            desc = rc_str(bin.description),
            version = rc_str(&version),
            name = rc_str(bin.name),
        );
        let rc_path_file = out.join(format!("{}.rc", bin.name));
        let res_path = out.join(format!("{}.res", bin.name));
        std::fs::write(&rc_path_file, rc).expect("write .rc");

        let Some((tool, is_llvm)) = rc_tool.as_ref() else {
            println!(
                "cargo:warning=no resource compiler (llvm-rc / rc.exe / $RC) found; {} is built without icon, manifest and version info",
                bin.name
            );
            continue;
        };
        let mut cmd = std::process::Command::new(tool);
        if *is_llvm {
            cmd.arg("-no-preprocess");
        }
        cmd.arg("/nologo")
            .arg("/fo")
            .arg(&res_path)
            .arg(&rc_path_file);
        match cmd.output() {
            Ok(o) if o.status.success() => {
                println!(
                    "cargo:rustc-link-arg-bin={}={}",
                    bin.name,
                    res_path.display()
                );
            }
            Ok(o) => panic!(
                "resource compiler failed for {}: {}{}",
                bin.name,
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(e) => panic!("could not run {}: {e}", tool.display()),
        }
    }

    fn find_rc() -> Option<(PathBuf, bool)> {
        let on_path = |name: &str| -> Option<PathBuf> {
            let path = std::env::var_os("PATH")?;
            std::env::split_paths(&path)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        };
        if let Some(rc) = std::env::var_os("RC").filter(|v| !v.is_empty()) {
            let p = PathBuf::from(rc);
            let llvm = p.to_string_lossy().contains("llvm-rc");
            return Some((p, llvm));
        }
        for name in ["llvm-rc", "llvm-rc.exe"] {
            if let Some(p) = on_path(name) {
                return Some((p, true));
            }
        }
        on_path("rc.exe").map(|p| (p, false))
    }
}
