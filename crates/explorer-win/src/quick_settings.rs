// SPDX-License-Identifier: GPL-3.0-or-later
//! Quick settings back-ends (feature `quick-settings`).
//!
//! Implemented (unverified on Windows): battery via `GetSystemPowerStatus`,
//! master volume via WASAPI `IAudioEndpointVolume`, network connectivity via
//! `INetworkListManager`. All APIs used here are documented.
//!
//! TODO(quick-settings):
//! - UI flyout in the taskbar (click on the clock area) — not started.
//! - Brightness: WMI `WmiMonitorBrightness` (laptops) or DDC/CI through
//!   `dxva2` `SetMonitorBrightness` (external monitors). Not started.
//! - Change notifications (IAudioEndpointVolumeCallback,
//!   INetworkListManagerEvents, WM_POWERBROADCAST) instead of polling.

use anyhow::Result;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::Networking::NetworkListManager::{
    INetworkListManager, NetworkListManager, NLM_CONNECTIVITY_IPV4_INTERNET,
    NLM_CONNECTIVITY_IPV6_INTERNET,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// None when there is no battery or the level is unknown.
    pub percent: Option<u8>,
    pub on_ac: bool,
}

pub fn battery() -> Option<Battery> {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: valid out-pointer.
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    // BatteryFlag 128 = no system battery.
    if s.BatteryFlag & 128 != 0 {
        return None;
    }
    Some(Battery {
        percent: (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent),
        on_ac: s.ACLineStatus == 1,
    })
}

fn endpoint_volume() -> Result<IAudioEndpointVolume> {
    // SAFETY: documented COM calls; requires COM initialised on this thread.
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
        Ok(dev.Activate(CLSCTX_ALL, None)?)
    }
}

/// Master volume in [0, 1] and mute state of the default render device.
pub fn volume() -> Result<(f32, bool)> {
    let v = endpoint_volume()?;
    // SAFETY: COM calls on a live interface.
    unsafe { Ok((v.GetMasterVolumeLevelScalar()?, v.GetMute()?.as_bool())) }
}

pub fn set_volume(level: f32) -> Result<()> {
    let v = endpoint_volume()?;
    // SAFETY: COM call on a live interface.
    unsafe { Ok(v.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null())?) }
}

pub fn set_mute(mute: bool) -> Result<()> {
    let v = endpoint_volume()?;
    // SAFETY: COM call on a live interface.
    unsafe { Ok(v.SetMute(mute, std::ptr::null())?) }
}

/// True if Windows believes there is internet connectivity.
pub fn internet_connected() -> Result<bool> {
    // SAFETY: documented COM calls.
    unsafe {
        let nlm: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL)?;
        let c = nlm.GetConnectivity()?;
        Ok(c.0 & (NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0) != 0)
    }
}
