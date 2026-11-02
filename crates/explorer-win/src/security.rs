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
