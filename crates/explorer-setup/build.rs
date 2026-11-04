// SPDX-License-Identifier: GPL-3.0-or-later
//! 1. Selects the payload embedded in `explorer-rs-setup.exe`:
//!    `$EXPLORER_RS_PAYLOAD` if set (scripts/build-windows.sh), otherwise an
//!    empty file so `cargo check`, clippy and tests work without building
//!    the payload first. The setup refuses to install an empty payload.
//! 2. Embeds the icon, manifest and version info (assets/build/resources.rs).

include!("../../assets/build/resources.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=EXPLORER_RS_PAYLOAD");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let payload = match std::env::var_os("EXPLORER_RS_PAYLOAD").filter(|v| !v.is_empty()) {
        Some(p) => {
            let p = std::path::PathBuf::from(p);
            let p = std::fs::canonicalize(&p)
                .unwrap_or_else(|e| panic!("EXPLORER_RS_PAYLOAD={}: {e}", p.display()));
            println!("cargo:rerun-if-changed={}", p.display());
            p
        }
        None => {
            let empty = out.join("empty-payload.bin");
            if !empty.exists() {
                std::fs::write(&empty, b"").expect("write empty payload");
            }
            empty
        }
    };
    println!(
        "cargo:rustc-env=EXPLORER_RS_PAYLOAD_FILE={}",
        payload.display()
    );

    embed_resources(&[
        Bin::new("explorer-rs-setup", "explorer.rs setup").require_admin(),
        Bin::new("explorer-rs-settings", "explorer.rs settings"),
        Bin::new("explorer-setup-pack", "explorer.rs payload packer"),
    ]);
}
