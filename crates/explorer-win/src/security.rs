// SPDX-License-Identifier: GPL-3.0-or-later
//! Token and process identity helpers (user SID, elevation, image path).

use std::path::PathBuf;

use anyhow::{Context, Result};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TokenUser, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::util::from_wide;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: handle owned by us.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn open_process(pid: u32) -> Result<Handle> {
    // SAFETY: plain FFI call; handle closed by Drop.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .with_context(|| format!("OpenProcess({pid})"))?;
    Ok(Handle(h))
}

fn process_token(process: HANDLE) -> Result<Handle> {
    let mut tok = HANDLE::default();
    // SAFETY: valid process handle; out pointer valid.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut tok) }.context("OpenProcessToken")?;
    Ok(Handle(tok))
}

fn token_user_sid(token: &Handle) -> Result<String> {
    let mut len = 0u32;
    // SAFETY: size query; the call fails with ERROR_INSUFFICIENT_BUFFER by design.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut len) };
    let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: buffer is at least `len` bytes and 8-byte aligned.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 8) as u32,
            &mut len,
        )
    }
    .context("GetTokenInformation(TokenUser)")?;
    // SAFETY: the buffer now holds a TOKEN_USER.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut s = PWSTR::null();
    // SAFETY: SID pointer is valid while `buf` lives; `s` is LocalAlloc'ed.
    unsafe {
        ConvertSidToStringSidW(user.User.Sid, &mut s).context("ConvertSidToStringSidW")?;
        let out = s.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        Ok(out)
    }
}

/// String SID (`S-1-5-21-...`) of the user running this process.
pub fn current_user_sid() -> Result<String> {
    // SAFETY: pseudo handle, no need to close.
    let tok = process_token(unsafe { GetCurrentProcess() })?;
    token_user_sid(&tok)
}

/// String SID of the user running process `pid`.
pub fn process_user_sid(pid: u32) -> Result<String> {
    let p = open_process(pid)?;
    let tok = process_token(p.0)?;
    token_user_sid(&tok)
}

/// Full image path of process `pid`.
pub fn process_image_path(pid: u32) -> Result<PathBuf> {
    let p = open_process(pid)?;
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    // SAFETY: buffer and length are valid.
    unsafe {
        QueryFullProcessImageNameW(p.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    }
    .context("QueryFullProcessImageNameW")?;
    Ok(PathBuf::from(from_wide(&buf[..len as usize])))
}

/// True if this process runs with an elevated (administrator) token.
pub fn is_elevated() -> bool {
    // SAFETY: pseudo handle.
    let Ok(tok) = process_token(unsafe { GetCurrentProcess() }) else {
        return false;
    };
    let mut e = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: out buffer is a TOKEN_ELEVATION.
    unsafe {
        GetTokenInformation(
            tok.0,
            TokenElevation,
            Some((&mut e as *mut TOKEN_ELEVATION).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    }
    .is_ok()
        && e.TokenIsElevated != 0
}

/// Enable a privilege (e.g. `SE_TAKE_OWNERSHIP_NAME`) in this process's
/// token if the token holds it. Returns false (and logs) on failure.
pub fn enable_privilege(name: windows::core::PCWSTR) -> bool {
    use windows::Win32::Foundation::LUID;
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
    };
    // SAFETY: token handle closed below; structures are valid.
    unsafe {
        let mut tok = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut tok,
        )
        .is_err()
        {
            return false;
        }
        let tok = Handle(tok);
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(windows::core::PCWSTR::null(), name, &mut luid).is_err() {
            return false;
        }
        let mut tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            ..Default::default()
        };
        tp.Privileges[0].Luid = luid;
        tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
        match AdjustTokenPrivileges(tok.0, false, Some(&tp), 0, None, None) {
            // ERROR_NOT_ALL_ASSIGNED is reported through GetLastError.
            Ok(()) => windows::Win32::Foundation::GetLastError().is_ok(),
            Err(e) => {
                tracing::warn!(error = %e, "AdjustTokenPrivileges failed");
                false
            }
        }
    }
}

/// Whether the user `sid`, signed in on the console session, can run
/// programs elevated: a UAC split token (limited or full) or, without UAC,
/// membership of Administrators. Looks at one of that user's processes;
/// `None` when no such process can be inspected.
pub fn user_can_elevate(sid: &str) -> Option<bool> {
    use windows::Win32::Security::{
        CheckTokenMembership, CreateWellKnownSid, DuplicateToken, SecurityIdentification,
        TokenElevationType, TokenElevationTypeDefault, WinBuiltinAdministratorsSid, PSID,
        TOKEN_DUPLICATE, TOKEN_ELEVATION_TYPE,
    };
    use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
    // SAFETY: plain query.
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    let pid = crate::process::all_processes()
        .into_iter()
        .filter(|p| crate::process::session_of(p.pid) == Some(console))
        .find(|p| process_user_sid(p.pid).is_ok_and(|s| s.eq_ignore_ascii_case(sid)))?
        .pid;
    let process = open_process(pid).ok()?;
    let mut raw = HANDLE::default();
    // SAFETY: valid process handle and out pointer; closed by Handle.
    unsafe { OpenProcessToken(process.0, TOKEN_QUERY | TOKEN_DUPLICATE, &mut raw) }.ok()?;
    let tok = Handle(raw);
    let mut ty = TOKEN_ELEVATION_TYPE::default();
    let mut len = 0u32;
    // SAFETY: out buffer is a TOKEN_ELEVATION_TYPE.
    unsafe {
        GetTokenInformation(
            tok.0,
            TokenElevationType,
            Some((&mut ty as *mut TOKEN_ELEVATION_TYPE).cast()),
            std::mem::size_of::<TOKEN_ELEVATION_TYPE>() as u32,
            &mut len,
        )
    }
    .ok()?;
    if ty != TokenElevationTypeDefault {
        // Limited (filtered admin token) or full: an administrator.
        return Some(true);
    }
    // No split token: a standard user, or an administrator with UAC off.
    let mut dup = HANDLE::default();
    // SAFETY: valid token; out pointer valid; closed by Handle.
    unsafe { DuplicateToken(tok.0, SecurityIdentification, &mut dup) }.ok()?;
    let dup = Handle(dup);
    let mut sid_buf = [0u8; 68]; // SECURITY_MAX_SID_SIZE
    let mut size = sid_buf.len() as u32;
    let admins = PSID(sid_buf.as_mut_ptr().cast());
    // SAFETY: buffer of SECURITY_MAX_SID_SIZE bytes.
    unsafe { CreateWellKnownSid(WinBuiltinAdministratorsSid, None, Some(admins), &mut size) }
        .ok()?;
    let mut member = windows::core::BOOL(0);
    // SAFETY: impersonation token and SID valid for the call.
    unsafe { CheckTokenMembership(Some(dup.0), admins, &mut member) }.ok()?;
    Some(member.as_bool())
}
