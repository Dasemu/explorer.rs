// SPDX-License-Identifier: GPL-3.0-or-later
//! Self-update, Windows side: fetch the latest release, download and verify
//! its installer, and start it elevated. The decisions (which release, which
//! file, does it match) are `explorer_core::update`.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use explorer_core::update::{self as up, Release, UpdateState};
use windows::core::{w, HRESULT, PCWSTR};
use windows::Win32::Foundation::ERROR_CANCELLED;
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::util::WideString;

/// Ask GitHub for the latest release of explorer.rs.
pub fn fetch_latest() -> Result<Release> {
    let body = crate::http::get(
        up::LATEST_URL,
        "application/vnd.github+json",
        up::MAX_RELEASE_JSON_BYTES,
        &mut |_, _| true,
    )?;
    let text = String::from_utf8(body).context("release information is not UTF-8")?;
    Ok(up::parse_release(&text)?)
}

/// Check now and record the result in the state file. Returns the new
/// state. Failures are recorded (and logged) too; they are never shown as
/// dialogs by the automatic check.
pub fn check_and_record(state_file: &Path) -> (UpdateState, Result<Release, String>) {
    let now = up::unix_now();
    let result = fetch_latest().map_err(|e| format!("{e:#}"));
    // Re-read: the other process (taskbar / settings) may have written
    // reminder choices meanwhile.
    let mut state = UpdateState::load(state_file);
    match &result {
        Ok(r) => {
            tracing::info!(latest = %r.version, current = %up::Version::current(), "update check done");
            state.record_success(now, r);
        }
        Err(e) => {
            tracing::warn!(error = %e, "update check failed");
            state.record_failure(now, e);
        }
    }
    if let Err(e) = state.save(state_file) {
        tracing::warn!(error = %e, "cannot save the update state");
    }
    (state, result)
}

/// A verified installer on disk, kept open so nothing can replace it
/// between the check and the launch.
pub struct VerifiedSetup {
    pub path: PathBuf,
    _lock: std::fs::File,
}

/// Download the release's checksum file and installer into `dir`, verify
/// the installer and write it. `progress` gets (bytes, total) of the
/// installer and returns false to cancel.
pub fn download_setup(
    release: &Release,
    dir: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<VerifiedSetup> {
    let sums = crate::http::get(
        &release.sums.url,
        "application/octet-stream",
        up::MAX_SUMS_BYTES,
        &mut |_, _| true,
    )
    .context("downloading SHA256SUMS.txt")?;
    let sums = String::from_utf8_lossy(&sums).into_owned();
    if up::parse_sha256sums(&sums, &release.setup.name).is_none() {
        bail!("SHA256SUMS.txt does not list {}", release.setup.name);
    }
    let data = crate::http::get(
        &release.setup.url,
        "application/octet-stream",
        release.setup.size.min(up::MAX_SETUP_BYTES),
        progress,
    )
    .with_context(|| format!("downloading {}", release.setup.name))?;
    up::verify_setup(release, &sums, &data)?;

    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    // Old installers are not needed any more.
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let path = dir.join(&release.setup.name);
    // Writers and deleters are shut out while the handle is open (read
    // sharing only): the file that runs is the file that was verified.
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(&data)?;
    f.flush()?;
    // Read back what is on disk and verify once more.
    f.seek(SeekFrom::Start(0))?;
    let mut back = Vec::with_capacity(data.len());
    f.read_to_end(&mut back)?;
    up::verify_setup(release, &sums, &back)?;
    tracing::info!(path = %path.display(), bytes = data.len(), "installer downloaded and verified");
    Ok(VerifiedSetup { path, _lock: f })
}

#[derive(Debug)]
pub enum RunError {
    /// The user said no to the administrator prompt.
    Cancelled,
    Failed(String),
}

/// Start the installer with "Run as administrator" (UAC prompt). Returns
/// once the installer process exists (or the prompt was refused).
pub fn run_installer(setup: &VerifiedSetup, args: &str) -> Result<(), RunError> {
    let file = WideString::new(&setup.path.to_string_lossy());
    let params = WideString::new(args);
    let dir = setup
        .path
        .parent()
        .map(|d| WideString::new(&d.to_string_lossy()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: return only after the process was created (the caller
        // exits right after). FLAG_NO_UI: errors are reported by the caller.
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: w!("runas"),
        lpFile: file.pcwstr(),
        lpParameters: params.pcwstr(),
        lpDirectory: dir.as_ref().map(|d| d.pcwstr()).unwrap_or(PCWSTR::null()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: all strings outlive the call.
    match unsafe { ShellExecuteExW(&mut info) } {
        Ok(()) => {
            tracing::info!(path = %setup.path.display(), args, "installer started");
            Ok(())
        }
        Err(e) if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => {
            tracing::info!("the administrator prompt was cancelled");
            Err(RunError::Cancelled)
        }
        Err(e) => {
            tracing::error!(error = %e, "cannot start the installer");
            Err(RunError::Failed(e.message()))
        }
    }
}

/// The folder the installer put explorer.rs in, if it is installed.
pub fn install_dir() -> Option<PathBuf> {
    use explorer_core::setup::{MACHINE_KEY, VALUE_INSTALL_DIR};
    crate::registry::read_string(
        crate::registry::Root::LocalMachine,
        MACHINE_KEY,
        VALUE_INSTALL_DIR,
    )
    .map(PathBuf::from)
}

/// Is this program the installed copy? A portable or development copy is
/// not updated by the installer (it would install a second copy, by
/// default as the shell), so it is only sent to the release page.
pub fn running_installed_copy() -> bool {
    let (Some(dir), Ok(exe)) = (install_dir(), std::env::current_exe()) else {
        return false;
    };
    let norm = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    exe.parent().is_some_and(|p| norm(p) == norm(&dir))
}

/// The installer's answer to "Check for updates automatically?"
/// (`HKLM\Software\explorer-rs\CheckUpdates`), if it recorded one.
pub fn installer_check_updates() -> Option<bool> {
    use explorer_core::setup::{MACHINE_KEY, VALUE_CHECK_UPDATES};
    crate::registry::read_dword(
        crate::registry::Root::LocalMachine,
        MACHINE_KEY,
        VALUE_CHECK_UPDATES,
    )
    .map(|v| v != 0)
}

/// Effective "check for updates automatically": the config key if the
/// user set it, else the installer's answer, else the built-in default.
pub fn check_updates_enabled(general: &explorer_core::config::GeneralConfig) -> bool {
    general.check_updates_with(installer_check_updates())
}
