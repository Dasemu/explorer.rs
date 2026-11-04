// SPDX-License-Identifier: GPL-3.0-or-later
//! Host tool: build an installer payload.
//!
//! ```text
//! explorer-setup-pack <out.bin> <version> <src-dir>
//! ```
//!
//! Packs every regular file below `<src-dir>` (relative paths become the
//! install-folder layout), sorted for reproducibility, then re-reads and
//! verifies the result. Used by scripts/build-windows.sh.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use explorer_setup::payload;

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for e in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let e = e?;
        let p = e.path();
        let ft = e.file_type()?;
        if ft.is_dir() {
            collect(root, &p, out)?;
        } else if ft.is_file() {
            let rel = p
                .strip_prefix(root)?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            if !payload::validate_path(&rel) {
                bail!("invalid file name for the payload: {rel:?}");
            }
            out.push((rel, p));
        } else {
            bail!("unsupported file type: {}", p.display());
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [out, version, src] = args.as_slice() else {
        bail!("usage: explorer-setup-pack <out.bin> <version> <src-dir>");
    };
    let root = PathBuf::from(src);
    let mut list = Vec::new();
    collect(&root, &root, &mut list)?;
    list.sort_by(|a, b| a.0.cmp(&b.0));
    if list.is_empty() {
        bail!("{} contains no files", root.display());
    }
    let mut files = Vec::with_capacity(list.len());
    for (rel, p) in &list {
        files.push((
            rel.clone(),
            std::fs::read(p).with_context(|| format!("read {}", p.display()))?,
        ));
    }
    let bytes = payload::pack(version, &files);
    let check = payload::unpack(&bytes).context("verifying the payload just built")?;
    if check.entries.len() != files.len() {
        bail!("payload verification failed");
    }
    std::fs::write(out, &bytes).with_context(|| format!("write {out}"))?;
    let raw: u64 = files.iter().map(|f| f.1.len() as u64).sum();
    println!(
        "payload {out}: {} files, {} bytes -> {} bytes",
        files.len(),
        raw,
        bytes.len()
    );
    for (rel, _) in &list {
        println!("  {rel}");
    }
    Ok(())
}
