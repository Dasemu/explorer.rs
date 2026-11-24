// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal HTTPS GET over WinHTTP.
//!
//! WinHTTP is the operating system's HTTP stack: it uses Windows' TLS,
//! certificate store and proxy settings, so explorer.rs carries no HTTP or
//! TLS library of its own. Only `https://` URLs are accepted; redirects are
//! followed (GitHub's asset downloads redirect to its storage host) under
//! WinHTTP's policy that never goes from HTTPS to HTTP, and the final URL
//! is checked again. Responses are size-limited.

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpQueryOption,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetTimeouts, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE,
    WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP, WINHTTP_OPTION_URL,
    WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

use crate::util::WideString;

/// Generic client name: no user or machine details.
fn user_agent() -> String {
    format!("explorer.rs-updater/{}", env!("CARGO_PKG_VERSION"))
}

/// A WinHTTP handle, closed on drop.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(h: *mut core::ffi::c_void, what: &str) -> Result<Self> {
        if h.is_null() {
            Err(windows::core::Error::from_thread()).context(what.to_string())
        } else {
            Ok(Self(h))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle returned by WinHTTP, closed once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// GET `url` and return the body (status 200 only). `accept` is sent as
/// the Accept header. More than `max_bytes` is an error. `progress` gets
/// (bytes so far, total if known) and returns false to cancel.
pub fn get(
    url: &str,
    accept: &str,
    max_bytes: u64,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<Vec<u8>> {
    if !url.starts_with("https://") {
        bail!("refusing a non-HTTPS URL");
    }
    let ua = WideString::new(&user_agent());
    // SAFETY: valid strings; null proxy names select the system's.
    let session = unsafe {
        let h = WinHttpOpen(
            ua.pcwstr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if h.is_null() {
            // AUTOMATIC_PROXY needs Windows 8.1; keep older systems working.
            WinHttpOpen(
                ua.pcwstr(),
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            )
        } else {
            h
        }
    };
    let session = Handle::new(session, "WinHttpOpen")?;
    // Resolve, connect, send, receive (ms). Generous for slow links, but a
    // dead connection never hangs a check forever.
    // SAFETY: valid session handle.
    unsafe { WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000) }
        .context("WinHttpSetTimeouts")?;

    let url_w: Vec<u16> = url.encode_utf16().collect();
    let mut parts = URL_COMPONENTS {
        dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
        // u32::MAX = "return a pointer into the input" for these parts.
        dwSchemeLength: u32::MAX,
        dwHostNameLength: u32::MAX,
        dwUrlPathLength: u32::MAX,
        dwExtraInfoLength: u32::MAX,
        ..Default::default()
    };
    // SAFETY: `url_w` outlives every pointer stored in `parts`.
    unsafe { WinHttpCrackUrl(&url_w, 0, &mut parts) }.context("WinHttpCrackUrl")?;
    if parts.nScheme != WINHTTP_INTERNET_SCHEME_HTTPS {
        bail!("refusing a non-HTTPS URL");
    }
    // SAFETY: WinHttpCrackUrl pointed these into `url_w` with the lengths.
    let slice = |p: windows::core::PWSTR, n: u32| -> Vec<u16> {
        if p.is_null() || n == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(p.0, n as usize) }.to_vec()
        }
    };
    let mut host = slice(parts.lpszHostName, parts.dwHostNameLength);
    host.push(0);
    let mut path = slice(parts.lpszUrlPath, parts.dwUrlPathLength);
    path.extend(slice(parts.lpszExtraInfo, parts.dwExtraInfoLength));
    if path.is_empty() {
        path.push(u16::from(b'/'));
    }
    path.push(0);

    // SAFETY: valid handle and NUL-terminated host.
    let connect = Handle::new(
        unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), parts.nPort, 0) },
        "WinHttpConnect",
    )?;
    // SAFETY: valid handle and strings; null accept types / referrer.
    let request = Handle::new(
        unsafe {
            WinHttpOpenRequest(
                connect.0,
                w!("GET"),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        },
        "WinHttpOpenRequest",
    )?;
    // This is WinHTTP's default; set explicitly because the downloads rely
    // on it (GitHub redirects asset downloads to another host).
    let policy = WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_ne_bytes();
    // SAFETY: valid handle and a DWORD buffer.
    unsafe {
        WinHttpSetOption(
            Some(request.0.cast_const()),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&policy),
        )
    }
    .context("WinHttpSetOption(REDIRECT_POLICY)")?;
    let headers: Vec<u16> = format!("Accept: {accept}\r\n").encode_utf16().collect();
    // SAFETY: valid handle and header buffer.
    unsafe { WinHttpAddRequestHeaders(request.0, &headers, WINHTTP_ADDREQ_FLAG_ADD) }
        .context("WinHttpAddRequestHeaders")?;
    // SAFETY: valid handle; no body.
    unsafe {
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).context("WinHttpSendRequest")?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .context("WinHttpReceiveResponse")?;
    }

    let final_url = query_url(&request).unwrap_or_default();
    if !final_url.starts_with("https://") {
        bail!("the download was redirected away from HTTPS");
    }
    let status = query_number(&request, WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
    if status != 200 {
        bail!("the server answered HTTP {status}");
    }
    let total = query_number(&request, WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
    if total.is_some_and(|t| t > max_bytes) {
        bail!("the download is larger than expected");
    }

    let mut body: Vec<u8> = Vec::with_capacity(total.unwrap_or(64 * 1024).min(max_bytes) as usize);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let mut avail = 0u32;
        // SAFETY: valid handle and out-pointer.
        unsafe { WinHttpQueryDataAvailable(request.0, &mut avail) }
            .context("WinHttpQueryDataAvailable")?;
        if avail == 0 {
            break;
        }
        let want = (avail as usize).min(buf.len());
        let mut read = 0u32;
        // SAFETY: `buf` has room for `want` bytes.
        unsafe { WinHttpReadData(request.0, buf.as_mut_ptr().cast(), want as u32, &mut read) }
            .context("WinHttpReadData")?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buf[..read as usize]);
        if body.len() as u64 > max_bytes {
            bail!("the download is larger than expected");
        }
        if !progress(body.len() as u64, total) {
            bail!("cancelled");
        }
    }
    if total.is_some_and(|t| t != body.len() as u64) {
        return Err(anyhow!("the download was interrupted"));
    }
    Ok(body)
}

fn query_number(request: &Handle, what: u32) -> Option<u32> {
    let mut v = 0u32;
    let mut len = 4u32;
    // SAFETY: valid handle; DWORD buffer of `len` bytes.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            what | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut v as *mut u32).cast()),
            &mut len,
            std::ptr::null_mut(),
        )
    }
    .ok()
    .map(|_| v)
}

/// The URL actually fetched (after redirects).
fn query_url(request: &Handle) -> Option<String> {
    let mut buf = vec![0u16; 4096];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: valid handle; buffer of `len` bytes.
    unsafe {
        WinHttpQueryOption(
            request.0,
            WINHTTP_OPTION_URL,
            Some(buf.as_mut_ptr().cast()),
            &mut len,
        )
    }
    .ok()?;
    Some(crate::util::from_wide(
        &buf[..(len as usize / 2).min(buf.len())],
    ))
}
