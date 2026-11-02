// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading Windows personalisation settings from the registry (documented
//! user-visible settings; the value names are stable but not formally
//! documented as an API). Re-read on `WM_SETTINGCHANGE`.

use explorer_core::theme::SystemTheme;

use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

use crate::util::WideString;

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const DWM: &str = r"Software\Microsoft\Windows\DWM";
const ADVANCED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";

/// Read a REG_DWORD from HKCU.
pub fn read_dword(subkey: &str, value: &str) -> Option<u32> {
    let k = WideString::new(subkey);
    let v = WideString::new(value);
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: out-pointers to a u32 and its size.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            k.pcwstr(),
            v.pcwstr(),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    r.is_ok().then_some(data)
}

/// Current light/dark mode and accent colour.
pub fn system_theme() -> SystemTheme {
    let light = read_dword(PERSONALIZE, "SystemUsesLightTheme").unwrap_or(0) != 0;
    let accent_on_taskbar = read_dword(PERSONALIZE, "ColorPrevalence").unwrap_or(0) != 0;
    // AccentColor is 0xAABBGGRR.
    let accent = read_dword(DWM, "AccentColor")
        .map(|v| ((v & 0xff) << 16) | (v & 0xff00) | ((v >> 16) & 0xff))
        .or_else(|| {
            let mut argb = 0u32;
            let mut opaque = windows::core::BOOL(0);
            // SAFETY: valid out-pointers.
            unsafe { DwmGetColorizationColor(&mut argb, &mut opaque) }
                .ok()
                .map(|_| argb & 0xFF_FFFF)
        })
        .unwrap_or(0x0078D4);
    SystemTheme {
        light,
        accent,
        accent_on_taskbar,
    }
}

/// Windows 11 "Taskbar alignment": true = center (the default when unset).
pub fn taskbar_centered() -> bool {
    read_dword(ADVANCED, "TaskbarAl")
        .map(|v| v != 0)
        .unwrap_or(true)
}

/// Display name of the signed-in user for the Start menu footer.
pub fn user_display_name() -> String {
    use windows::Win32::System::WindowsProgramming::GetUserNameW;
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    // SAFETY: valid buffer and length.
    if unsafe { GetUserNameW(Some(windows::core::PWSTR(buf.as_mut_ptr())), &mut len) }.is_ok() {
        let n = (len as usize).saturating_sub(1);
        return String::from_utf16_lossy(&buf[..n]);
    }
    std::env::var("USERNAME").unwrap_or_default()
}
