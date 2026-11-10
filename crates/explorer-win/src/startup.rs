// SPDX-License-Identifier: GPL-3.0-or-later
//! Startup apps in shell mode: Run/RunOnce registry keys and the Startup
//! folders, honouring the "Startup apps" switches (rules in
//! `explorer_core::startup`). Call [`run_all`] on a background thread
//! (it initialises COM for `ShellExecuteExW` and sleeps between launches).

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use explorer_core::startup::{
    approved_enabled, expand_env, is_startup_folder_entry, run_once_name, split_command,
    CommandLine, Source, APPROVED_SUBKEY, RUN_ONCE_SUBKEY, RUN_SUBKEY, RUN_WOW64_SUBKEY,
    STAGGER_MS,
};
use windows::core::{GUID, PCWSTR};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{REG_EXPAND_SZ, REG_SZ};
use windows::Win32::UI::Shell::{
    FOLDERID_CommonStartup, FOLDERID_Startup, SHGetKnownFolderPath, ShellExecuteExW,
    KF_FLAG_DEFAULT, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::registry::{self, Root};
use crate::util::{ComApartment, WideString};

/// `ERROR_ELEVATION_REQUIRED`.
const ERROR_ELEVATION_REQUIRED: i32 = 740;

/// One startup entry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub source: Source,
    /// Registry value name or file name (also the `StartupApproved` key).
    pub name: String,
    pub what: What,
}

#[derive(Debug, Clone)]
pub enum What {
    /// A command line from a Run key (already expanded).
    Command(String),
    /// A file in a Startup folder (shortcut, program, document).
    File(PathBuf),
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    // SAFETY: the returned buffer is freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s.map(PathBuf::from)
    }
}

fn env_lookup(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn is_enabled(source: Source, name: &str) -> bool {
    let Some(sub) = source.approval_subkey() else {
        return true;
    };
    let key = format!(r"{APPROVED_SUBKEY}\{sub}");
    [Root::CurrentUser, Root::LocalMachine]
        .into_iter()
        .all(|root| approved_enabled(registry::read_binary(root, &key, name).as_deref()))
}

fn registry_entries(source: Source, root: Root, subkey: &str) -> Vec<Entry> {
    registry::values(root, subkey)
        .into_iter()
        .filter(|v| v.kind == REG_SZ || v.kind == REG_EXPAND_SZ)
        .map(|v| Entry {
            source,
            what: What::Command(expand_env(&v.as_string(), env_lookup)),
            name: v.name,
        })
        .collect()
}

fn folder_entries(source: Source, dir: Option<PathBuf>) -> Vec<Entry> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(is_startup_folder_entry)
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| Entry {
            source,
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            what: What::File(p),
        })
        .collect()
}

/// Every startup entry in launch order, including disabled ones.
pub fn collect() -> Vec<(Entry, bool)> {
    let mut out = Vec::new();
    for source in Source::ORDER {
        let entries = match source {
            Source::UserRunOnce => registry_entries(source, Root::CurrentUser, RUN_ONCE_SUBKEY),
            Source::MachineRun => registry_entries(source, Root::LocalMachine, RUN_SUBKEY),
            Source::MachineRun32 => registry_entries(source, Root::LocalMachine, RUN_WOW64_SUBKEY),
            Source::UserRun => registry_entries(source, Root::CurrentUser, RUN_SUBKEY),
            Source::CommonStartupFolder => {
                folder_entries(source, known_folder(&FOLDERID_CommonStartup))
            }
            Source::UserStartupFolder => folder_entries(source, known_folder(&FOLDERID_Startup)),
        };
        for e in entries {
            let enabled = is_enabled(e.source, &e.name);
            out.push((e, enabled));
        }
    }
    out
}

fn shell_execute(file: &Path, params: &str, dir: Option<&Path>) -> windows::core::Result<()> {
    let f = WideString::new(&file.to_string_lossy());
    let p = WideString::new(params);
    let d = dir.map(|d| WideString::new(&d.to_string_lossy()));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: f.pcwstr(),
        lpParameters: if params.is_empty() {
            PCWSTR::null()
        } else {
            p.pcwstr()
        },
        lpDirectory: d.as_ref().map(|d| d.pcwstr()).unwrap_or(PCWSTR::null()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: all strings outlive the call.
    unsafe { ShellExecuteExW(&mut info) }
}

fn run_command(cmd: &str) -> Result<(), String> {
    let Some(cl) = split_command(cmd, |p| Path::new(p).is_file()) else {
        return Err("empty command".into());
    };
    launch(&cl)
}

fn launch(cl: &CommandLine) -> Result<(), String> {
    let program = Path::new(&cl.program);
    let dir = program
        .parent()
        .filter(|d| d.is_absolute() && d.is_dir())
        .map(Path::to_path_buf);
    let mut c = std::process::Command::new(program);
    if !cl.args.is_empty() {
        c.raw_arg(&cl.args);
    }
    if let Some(d) = &dir {
        c.current_dir(d);
    }
    match c.spawn() {
        Ok(child) => {
            tracing::info!(pid = child.id(), program = %cl.program, "startup app started");
            Ok(())
        }
        Err(e) if e.raw_os_error() == Some(ERROR_ELEVATION_REQUIRED) => {
            // explorer.exe does not elevate startup apps either.
            Err("requires elevation; skipped (like explorer.exe)".into())
        }
        Err(e) => {
            // Not a PE file (script, document...) or not found by
            // CreateProcess: let the shell try (file associations, App Paths).
            tracing::debug!(error = %e, program = %cl.program, "CreateProcess failed; trying ShellExecuteEx");
            shell_execute(program, &cl.args, dir.as_deref()).map_err(|e2| format!("{e}; {e2}"))
        }
    }
}

/// Run every enabled startup entry, staggered, logging each one.
pub fn run_all() {
    let _com = ComApartment::sta();
    let entries = collect();
    tracing::info!(count = entries.len(), "running startup apps");
    for (e, enabled) in entries {
        if !enabled {
            tracing::info!(source = e.source.as_str(), name = %e.name, "startup app disabled in Settings; skipped");
            continue;
        }
        let mut delete_after = false;
        if e.source == Source::UserRunOnce {
            delete_after = run_once_name(&e.name).delete_after;
            if !delete_after && !registry::delete_value(Root::CurrentUser, RUN_ONCE_SUBKEY, &e.name)
            {
                tracing::warn!(name = %e.name, "could not delete RunOnce value; not running it");
                continue;
            }
        }
        let result = match &e.what {
            What::Command(cmd) => run_command(cmd),
            What::File(path) => {
                shell_execute(path, "", path.parent()).map_err(|err| err.to_string())
            }
        };
        match &result {
            Ok(()) => {
                tracing::info!(source = e.source.as_str(), name = %e.name, "startup entry launched")
            }
            Err(err) => {
                tracing::warn!(source = e.source.as_str(), name = %e.name, error = %err, "startup entry failed")
            }
        }
        if delete_after && result.is_ok() {
            registry::delete_value(Root::CurrentUser, RUN_ONCE_SUBKEY, &e.name);
        }
        std::thread::sleep(Duration::from_millis(STAGGER_MS));
    }
    tracing::info!("startup apps done");
}
