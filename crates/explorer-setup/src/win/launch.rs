// SPDX-License-Identifier: GPL-3.0-or-later
//! Start programs as the interactive user WITHOUT elevation.
//!
//! The installer runs elevated, but the shell and its children must never
//! inherit that token. Two documented techniques, in order:
//!
//! 1. Ask the running Windows Explorer to start the program
//!    (`IShellWindows::FindWindowSW(SWC_DESKTOP)` -> `IShellBrowser` ->
//!    desktop `IShellView` -> `IShellFolderViewDual::Application` ->
//!    `IShellDispatch2::ShellExecute`), described by Raymond Chen in "How
//!    can I launch an unelevated process from my elevated process and vice
//!    versa?". The program then runs with Explorer's (unelevated) token.
//! 2. If Explorer does not run (explorer.rs is the shell) or step 1 fails:
//!    register the on-demand task `\explorer-rs\Launch` for the interactive
//!    user's SID with `LeastPrivilege` + `InteractiveToken` and run it.

use anyhow::{Context, Result};
use explorer_win::windows::core::{Interface, BSTR};
use explorer_win::windows::Win32::System::Com::{
    CoCreateInstance, IDispatch, IServiceProvider, CLSCTX_LOCAL_SERVER,
};
use explorer_win::windows::Win32::System::Variant::VARIANT;
use explorer_win::windows::Win32::UI::Shell::{
    IShellBrowser, IShellDispatch2, IShellFolderViewDual, IShellView, IShellWindows,
    SID_STopLevelBrowser, ShellWindows, SVGIO_BACKGROUND, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::task_xml;

/// How a program was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Explorer,
    ScheduledTask,
}

/// Start `exe args` unelevated as the interactive user `sid`. COM must be
/// initialised (STA) on this thread.
pub fn start_unelevated(exe: &str, args: &str, work_dir: &str, sid: &str) -> Result<Method> {
    if explorer_win::process::explorer_shell_running() {
        match via_explorer(exe, args, work_dir) {
            Ok(()) => return Ok(Method::Explorer),
            Err(e) => {
                tracing::warn!(error = %e, "launch through Explorer failed; using a scheduled task")
            }
        }
    }
    via_task(exe, args, work_dir, sid)?;
    Ok(Method::ScheduledTask)
}

fn via_explorer(exe: &str, args: &str, work_dir: &str) -> Result<()> {
    // SAFETY: COM calls on interfaces obtained here; VARIANTs are owned.
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)
            .context("CoCreateInstance(ShellWindows)")?;
        // CSIDL_DESKTOP (0) as the location.
        let loc = VARIANT::from(0i32);
        let empty = VARIANT::default();
        let mut hwnd = 0i32;
        let disp: IDispatch = windows
            .FindWindowSW(&loc, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)
            .context("FindWindowSW(SWC_DESKTOP)")?;
        let sp: IServiceProvider = disp.cast().context("IServiceProvider")?;
        let browser: IShellBrowser = sp
            .QueryService(&SID_STopLevelBrowser)
            .context("QueryService(SID_STopLevelBrowser)")?;
        let view: IShellView = browser
            .QueryActiveShellView()
            .context("QueryActiveShellView")?;
        let bg: IDispatch = view
            .GetItemObject(SVGIO_BACKGROUND)
            .context("GetItemObject(SVGIO_BACKGROUND)")?;
        let folder_view: IShellFolderViewDual = bg.cast().context("IShellFolderViewDual")?;
        let app: IDispatch = folder_view.Application().context("Application")?;
        let shell: IShellDispatch2 = app.cast().context("IShellDispatch2")?;
        shell
            .ShellExecute(
                &BSTR::from(exe),
                &VARIANT::from(args),
                &VARIANT::from(work_dir),
                &VARIANT::from("open"),
                &VARIANT::from(SW_SHOWNORMAL.0),
            )
            .context("IShellDispatch2::ShellExecute")?;
    }
    tracing::info!(exe, args, "started through Windows Explorer");
    Ok(())
}

fn via_task(exe: &str, args: &str, work_dir: &str, sid: &str) -> Result<()> {
    let xml = task_xml::launch_task(sid, exe, args, work_dir);
    explorer_win::sched::create_from_xml(
        task_xml::LAUNCH_TASK,
        &task_xml::to_utf16_file(&xml),
        &crate::win::log::secure_data_dir()?,
    )?;
    explorer_win::sched::run_now(task_xml::LAUNCH_TASK)?;
    tracing::info!(exe, args, "started through the launch task");
    Ok(())
}

/// `%SystemRoot%\explorer.exe`.
pub fn explorer_exe() -> String {
    let root = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("windir"))
        .unwrap_or_else(|_| r"C:\Windows".into());
    format!(r"{root}\explorer.exe")
}
