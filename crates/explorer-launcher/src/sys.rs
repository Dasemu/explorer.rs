// SPDX-License-Identifier: GPL-3.0-or-later
//! Win32 helpers used only by the Start menu: clipboard, command
//! resolution, running commands, recent files, power capabilities, the
//! account picture. Thin wrappers; every failure is logged and degrades to
//! "not available". Unverified on Windows.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use explorer_core::image::{Image, SharedImage};
use explorer_core::recent::{self, RecentFile};
use explorer_core::run_command::program_candidates;
use explorer_win::util::{from_wide, WideString};
use explorer_win::windows::core::{Interface, PCWSTR};
use explorer_win::windows::Win32::Foundation::{
    CloseHandle, GlobalFree, HANDLE, HWND, LPARAM, LUID, SIZE, WPARAM,
};
use explorer_win::windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use explorer_win::windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use explorer_win::windows::Win32::Storage::FileSystem::SearchPathW;
use explorer_win::windows::Win32::System::Com::{
    CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
};
use explorer_win::windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use explorer_win::windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use explorer_win::windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
};
use explorer_win::windows::Win32::System::Power::{
    GetPwrCapabilities, SetSuspendState, SYSTEM_POWER_CAPABILITIES,
};
use explorer_win::windows::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};
use explorer_win::windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
use explorer_win::windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use explorer_win::windows::Win32::UI::Shell::{
    FOLDERID_Recent, IShellItemImageFactory, IShellLinkW, SHCreateItemFromParsingName,
    SHGetKnownFolderPath, ShellExecuteW, ShellLink, KF_FLAG_DEFAULT, SIIGBF_BIGGERSIZEOK,
};
use explorer_win::windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, HWND_BROADCAST, SC_MONITORPOWER, SW_SHOWNORMAL, WM_SYSCOMMAND,
};

const CF_UNICODETEXT: u32 = 13;
const EXPLORER_ADVANCED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
const APP_PATHS: &str = r"Software\Microsoft\Windows\CurrentVersion\App Paths";

/// Put `text` on the clipboard as CF_UNICODETEXT.
pub fn copy_to_clipboard(owner: HWND, text: &str) -> bool {
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = w.len() * 2;
    // SAFETY: standard clipboard protocol; the global block is handed to
    // the clipboard on success and freed by us on failure.
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            tracing::warn!("OpenClipboard failed");
            return false;
        }
        let _ = EmptyClipboard();
        let ok = match GlobalAlloc(GMEM_MOVEABLE, bytes) {
            Ok(h) => {
                let p = GlobalLock(h) as *mut u16;
                if p.is_null() {
                    let _ = GlobalFree(Some(h));
                    false
                } else {
                    std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                    let _ = GlobalUnlock(h);
                    if SetClipboardData(CF_UNICODETEXT, Some(HANDLE(h.0))).is_ok() {
                        true
                    } else {
                        let _ = GlobalFree(Some(h));
                        false
                    }
                }
            }
            Err(_) => false,
        };
        let _ = CloseClipboard();
        ok
    }
}

/// Read a string value (REG_SZ / REG_EXPAND_SZ, expanded).
fn reg_string(root: HKEY, subkey: &str, value: Option<&str>) -> Option<String> {
    let k = WideString::new(subkey);
    let v = value.map(WideString::new);
    let vp = v.as_ref().map(|v| v.pcwstr()).unwrap_or(PCWSTR::null());
    let mut buf = vec![0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buffer and its byte size passed together.
    let r = unsafe {
        RegGetValueW(
            root,
            k.pcwstr(),
            vp,
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if r.is_err() {
        return None;
    }
    let s = from_wide(&buf);
    (!s.is_empty()).then_some(s)
}

/// Expand `%VAR%` references.
pub fn expand_env(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let src = WideString::new(s);
    let mut buf = vec![0u16; 2048];
    // SAFETY: valid source string and output buffer.
    let n = unsafe { ExpandEnvironmentStringsW(src.pcwstr(), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return s.to_string();
    }
    from_wide(&buf)
}

/// Resolve a bare program name like the Run dialog: App Paths (user, then
/// machine), then the search path. Touches the file system: call it from a
/// worker thread.
pub fn resolve_program(name: &str) -> Option<PathBuf> {
    for cand in program_candidates(name) {
        let lower = cand.to_lowercase();
        if lower.ends_with(".exe") {
            for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
                if let Some(p) = reg_string(root, &format!(r"{APP_PATHS}\{cand}"), None) {
                    let p = PathBuf::from(p.trim_matches('"'));
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
        let file = WideString::new(&cand);
        let mut buf = vec![0u16; 1024];
        // SAFETY: valid strings and buffer.
        let n = unsafe {
            SearchPathW(
                PCWSTR::null(),
                file.pcwstr(),
                PCWSTR::null(),
                Some(&mut buf),
                None,
            )
        } as usize;
        if n > 0 && n < buf.len() {
            return Some(PathBuf::from(from_wide(&buf)));
        }
    }
    None
}

/// `ShellExecuteW` with parameters; `elevated` uses the "runas" verb.
pub fn shell_run(file: &str, args: &str, elevated: bool) -> Result<()> {
    let verb = WideString::new(if elevated { "runas" } else { "open" });
    let f = WideString::new(file);
    let a = WideString::new(args);
    let ap = if args.is_empty() {
        PCWSTR::null()
    } else {
        a.pcwstr()
    };
    // SAFETY: valid strings kept alive for the call.
    let r = unsafe {
        ShellExecuteW(
            None,
            verb.pcwstr(),
            f.pcwstr(),
            ap,
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if r.0 as isize <= 32 {
        return Err(anyhow!(
            "ShellExecuteW({file}) failed with code {}",
            r.0 as isize
        ));
    }
    Ok(())
}

/// Run `f` on a short-lived STA thread (ShellExecute may block on slow
/// handlers or network paths; the UI thread must not).
pub fn spawn_com(name: &'static str, f: impl FnOnce() -> Result<()> + Send + 'static) {
    let r = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _com = explorer_win::util::ComApartment::sta();
            if let Err(e) = f() {
                tracing::error!(error = %e, task = name, "background launch failed");
            }
        });
    if let Err(e) = r {
        tracing::error!(error = %e, "cannot spawn launch thread");
    }
}

/// Show `path` selected in a File Explorer window.
pub fn open_location(path: &Path) {
    use std::os::windows::process::CommandExt;
    let arg = format!("/select,\"{}\"", path.display());
    if let Err(e) = std::process::Command::new("explorer.exe")
        .raw_arg(arg)
        .spawn()
    {
        tracing::error!(error = %e, "cannot open file location");
    }
}

/// Which sleep states the power menu may offer: (sleep, hibernate).
pub fn power_capabilities() -> (bool, bool) {
    let mut caps = SYSTEM_POWER_CAPABILITIES::default();
    // SAFETY: valid out-pointer.
    if !unsafe { GetPwrCapabilities(&mut caps) } {
        return (true, false);
    }
    let sleep = caps.SystemS1 || caps.SystemS2 || caps.SystemS3 || caps.AoAc;
    let hibernate = caps.SystemS4 && caps.HiberFilePresent;
    (sleep, hibernate)
}

/// Whether the machine uses Modern Standby (S0 low-power idle).
fn modern_standby() -> bool {
    let mut caps = SYSTEM_POWER_CAPABILITIES::default();
    // SAFETY: valid out-pointer.
    let ok = unsafe { GetPwrCapabilities(&mut caps) };
    ok && caps.AoAc
}

/// Enable SeShutdownPrivilege for this process (present but disabled for
/// standard users); needed by SetSuspendState.
fn enable_shutdown_privilege() {
    // SAFETY: token handle opened and closed here; structures sized.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            return;
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid).is_ok() {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        }
        let _ = CloseHandle(token);
    }
}

/// Sleep or hibernate. On Modern Standby machines "sleep" turns the
/// display off, which is how they enter standby.
pub fn suspend(hibernate: bool) {
    if !hibernate && modern_standby() {
        // SAFETY: broadcast of a standard system command.
        let _ = unsafe {
            PostMessageW(
                Some(HWND_BROADCAST),
                WM_SYSCOMMAND,
                WPARAM(SC_MONITORPOWER as usize),
                LPARAM(2),
            )
        };
        return;
    }
    enable_shutdown_privilege();
    // SAFETY: plain FFI call.
    if !unsafe { SetSuspendState(hibernate, false, false) } {
        tracing::error!(hibernate, "SetSuspendState failed");
    }
}

/// Windows' "Show recently opened items in Start" setting.
pub fn recent_files_enabled() -> bool {
    recent::recent_files_enabled(explorer_win::theme::read_dword(
        EXPLORER_ADVANCED,
        "Start_TrackDocs",
    ))
}

fn recent_folder() -> Option<PathBuf> {
    // SAFETY: valid GUID; the returned string is freed below.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Recent, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

/// Target path of a `.lnk` file (no resolution / searching). Requires COM.
fn lnk_target(lnk: &Path) -> Option<PathBuf> {
    let name = WideString::new(&lnk.to_string_lossy());
    // SAFETY: documented ShellLink COM calls with valid buffers.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(name.pcwstr(), STGM_READ).ok()?;
        let mut buf = vec![0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let s = from_wide(&buf);
        (!s.is_empty()).then(|| PathBuf::from(s))
    }
}

/// Recently opened files, newest first. Requires COM; runs on a worker.
pub fn recent_files(max: usize) -> Vec<RecentFile> {
    let Some(dir) = recent_folder() else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let items: Vec<(PathBuf, i64)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            if !m.is_file() {
                return None;
            }
            let t = m
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs() as i64;
            Some((e.path(), t))
        })
        .collect();
    let mut out = Vec::new();
    for (lnk, opened) in recent::newest_shortcuts(items) {
        let Some(target) = lnk_target(&lnk) else {
            continue;
        };
        if recent::may_check_exists(&target) {
            match std::fs::metadata(&target) {
                Ok(m) if m.is_file() => {}
                _ => continue, // missing, or a folder
            }
        }
        out.push(RecentFile {
            name: recent::display_name(&target),
            path: target,
            shortcut: lnk,
            opened,
        });
        if out.len() >= max * 2 {
            break;
        }
    }
    recent::dedupe(out, max)
}

/// Local time minus UTC, in seconds (rounded to 15 minutes).
pub fn utc_offset_secs() -> i64 {
    // SAFETY: no arguments.
    let (l, u) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let secs = |t: &explorer_win::windows::Win32::Foundation::SYSTEMTIME| {
        recent::days_from_civil(t.wYear as i64, t.wMonth as u32, t.wDay as u32) * 86_400
            + t.wHour as i64 * 3600
            + t.wMinute as i64 * 60
    };
    let d = secs(&l) - secs(&u);
    (d as f64 / 900.0).round() as i64 * 900
}

/// Path of the user's account picture (best effort): the per-user tile
/// Windows keeps under `C:\Users\Public\AccountPictures`.
pub fn account_picture_path() -> Option<PathBuf> {
    let sid = explorer_win::security::current_user_sid().ok()?;
    let key = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\AccountPicture\Users\{sid}");
    for v in ["Image96", "Image192", "Image64", "Image240", "Image448"] {
        if let Some(p) = reg_string(HKEY_LOCAL_MACHINE, &key, Some(v)) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// Thumbnail (not icon) of an image file. Requires COM.
pub fn thumbnail(path: &Path, size: u32) -> Option<SharedImage> {
    let p = WideString::new(&path.to_string_lossy());
    // SAFETY: documented Shell calls; the HBITMAP is owned and freed.
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(p.pcwstr(), None).ok()?;
        let hbm = factory
            .GetImage(
                SIZE {
                    cx: size as i32,
                    cy: size as i32,
                },
                SIIGBF_BIGGERSIZEOK,
            )
            .ok()?;
        let img = hbitmap_to_image(hbm);
        let _ = DeleteObject(HGDIOBJ(hbm.0));
        img
    }
}

fn hbitmap_to_image(hbm: HBITMAP) -> Option<SharedImage> {
    // SAFETY: GDI calls with properly sized structures; DC released.
    unsafe {
        let mut bm = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut BITMAP as *mut core::ffi::c_void),
        ) == 0
        {
            return None;
        }
        let (w, h) = (bm.bmWidth.max(0) as u32, bm.bmHeight.unsigned_abs());
        if w == 0 || h == 0 || w > 1024 || h > 1024 {
            return None;
        }
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut px = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            hbm,
            0,
            h,
            Some(px.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        if lines == 0 {
            return None;
        }
        // Thumbnails of photos often come without alpha: make them opaque.
        if px.chunks_exact(4).all(|p| p[3] == 0) {
            for p in px.chunks_exact_mut(4) {
                p[3] = 255;
            }
        }
        Image::from_bgra(w, h, px)
    }
}
