// SPDX-License-Identifier: GPL-3.0-or-later
//! Task Scheduler through `schtasks.exe` (documented command line). The
//! task XML itself is produced by pure, tested code (`explorer-setup`'s
//! `task_xml`). `schtasks.exe` is run by absolute path from System32 so an
//! elevated caller never picks up a different program from `PATH`, and its
//! console window is suppressed.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn schtasks() -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("windir"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join("schtasks.exe")
}

fn run(args: &[&str]) -> Result<Output> {
    let out = Command::new(schtasks())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .context("run schtasks.exe")?;
    tracing::debug!(?args, status = ?out.status, "schtasks");
    Ok(out)
}

fn fail(what: &str, out: &Output) -> anyhow::Error {
    anyhow::anyhow!(
        "schtasks {what} failed ({}): {} {}",
        out.status,
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
}

/// True if a task with this full name (e.g. `\explorer-rs\IndexHelper`)
/// is registered.
pub fn exists(name: &str) -> bool {
    run(&["/Query", "/TN", name])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Register (or replace) a task from its XML definition. `xml_file_bytes`
/// is the complete file content (UTF-16 LE with BOM). The XML file is
/// written to `protected_dir` (administrators-only, see `secdir`; never
/// `%TEMP%`, which unelevated code of the same user could tamper with
/// between the write and schtasks reading it) under a fresh name with an
/// exclusive create, and deleted afterwards.
pub fn create_from_xml(name: &str, xml_file_bytes: &[u8], protected_dir: &Path) -> Result<()> {
    use std::io::Write;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = protected_dir.join(format!(
        "task-{}-{stamp:x}-{}.xml",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    {
        // create_new: fail rather than reuse anything already there.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        if let Err(e) = f.write_all(xml_file_bytes).and_then(|_| f.sync_all()) {
            drop(f);
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("write {}", path.display()));
        }
    }
    let p = path.to_string_lossy().into_owned();
    let out = run(&["/Create", "/TN", name, "/XML", &p, "/F"]);
    let _ = std::fs::remove_file(&path);
    let out = out?;
    if !out.status.success() {
        return Err(fail("/Create", &out));
    }
    Ok(())
}

/// Delete a task; a missing task is not an error. Returns true if a task
/// was deleted.
pub fn delete(name: &str) -> Result<bool> {
    if !exists(name) {
        return Ok(false);
    }
    let out = run(&["/Delete", "/TN", name, "/F"])?;
    if !out.status.success() {
        return Err(fail("/Delete", &out));
    }
    Ok(true)
}

/// Start a registered task now.
pub fn run_now(name: &str) -> Result<()> {
    let out = run(&["/Run", "/TN", name])?;
    if !out.status.success() {
        bail!(fail("/Run", &out));
    }
    Ok(())
}
