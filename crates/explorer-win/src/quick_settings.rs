// SPDX-License-Identifier: GPL-3.0-or-later
//! Quick settings back-ends used by the taskbar's system glyphs and its
//! Quick settings flyout. All APIs are documented:
//!
//! - battery: `GetSystemPowerStatus`;
//! - volume / mute / output device name: WASAPI `IAudioEndpointVolume` on
//!   the default render endpoint, `IMMDevice::OpenPropertyStore`
//!   (`PKEY_Device_FriendlyName`);
//! - network: `INetworkListManager` (connectivity, connected network names)
//!   and `GetAdaptersAddresses` (is the active adapter Wi-Fi?);
//! - brightness: see [`crate::brightness`] (WMI).
//!
//! The taskbar polls these from a worker thread (COM MTA); nothing here is
//! called on the UI thread. Change callbacks (`IAudioEndpointVolumeCallback`,
//! `INetworkListManagerEvents`) would need COM object implementations; the
//! polling interval is short enough for a status glyph.

use anyhow::Result;
use windows::core::GUID;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST,
    GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST, IF_TYPE_IEEE80211, IP_ADAPTER_ADDRESSES_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::NetworkListManager::{
    INetwork, INetworkListManager, NetworkListManager, NLM_CONNECTIVITY_IPV4_INTERNET,
    NLM_CONNECTIVITY_IPV4_LOCALNETWORK, NLM_CONNECTIVITY_IPV4_SUBNET,
    NLM_CONNECTIVITY_IPV6_INTERNET, NLM_CONNECTIVITY_IPV6_LOCALNETWORK,
    NLM_CONNECTIVITY_IPV6_SUBNET, NLM_ENUM_NETWORK_CONNECTED,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

/// PKEY_Device_FriendlyName {a45c254e-df1c-4efd-8020-67d146a850e0}, 14.
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 14,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    /// None when the level is unknown.
    pub percent: Option<u8>,
    pub on_ac: bool,
    pub charging: bool,
}

/// Battery state, or `None` on machines without a system battery.
pub fn battery() -> Option<Battery> {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: valid out-pointer.
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    // BatteryFlag 128 = no system battery, 255 = unknown status.
    if s.BatteryFlag & 128 != 0 || s.BatteryFlag == 255 {
        return None;
    }
    Some(Battery {
        percent: (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent),
        on_ac: s.ACLineStatus == 1,
        // BatteryFlag 8 = charging.
        charging: s.BatteryFlag & 8 != 0,
    })
}

/// Default output device: level, mute and name.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub level: f32,
    pub muted: bool,
    pub device: String,
}

/// Holds the default render endpoint between polls. Recreate it (call
/// [`AudioEndpoint::open`] again) after an error: the default device may
/// have changed or disappeared.
pub struct AudioEndpoint {
    volume: IAudioEndpointVolume,
    device: String,
}

impl AudioEndpoint {
    /// Requires COM on the calling thread.
    pub fn open() -> Result<Self> {
        // SAFETY: documented COM calls.
        unsafe {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let device = dev
                .OpenPropertyStore(STGM_READ)
                .ok()
                .and_then(|ps| ps.GetValue(&PKEY_DEVICE_FRIENDLY_NAME).ok())
                .and_then(|v| PropVariantToStringAlloc(&v).ok())
                .map(|p| {
                    let s = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
                    s
                })
                .unwrap_or_default();
            let volume: IAudioEndpointVolume = dev.Activate(CLSCTX_ALL, None)?;
            Ok(Self { volume, device })
        }
    }

    pub fn read(&self) -> Result<Audio> {
        // SAFETY: COM calls on a live interface.
        unsafe {
            Ok(Audio {
                level: self.volume.GetMasterVolumeLevelScalar()?,
                muted: self.volume.GetMute()?.as_bool(),
                device: self.device.clone(),
            })
        }
    }

    pub fn set_level(&self, level: f32) -> Result<()> {
        // SAFETY: COM call on a live interface.
        unsafe {
            Ok(self
                .volume
                .SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null())?)
        }
    }

    pub fn set_mute(&self, mute: bool) -> Result<()> {
        // SAFETY: COM call on a live interface.
        unsafe { Ok(self.volume.SetMute(mute, std::ptr::null())?) }
    }
}

/// Master volume in [0, 1] and mute state of the default render device.
pub fn volume() -> Result<(f32, bool)> {
    let a = AudioEndpoint::open()?.read()?;
    Ok((a.level, a.muted))
}

pub fn set_volume(level: f32) -> Result<()> {
    AudioEndpoint::open()?.set_level(level)
}

pub fn set_mute(mute: bool) -> Result<()> {
    AudioEndpoint::open()?.set_mute(mute)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    None,
    /// Connected to a network without internet access.
    Local,
    Internet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub connectivity: Connectivity,
    /// Names of the connected networks (SSID or "Network 2" for wired).
    pub names: Vec<String>,
    /// The adapter carrying the default route is Wi-Fi.
    pub wifi: bool,
}

/// Network status. Requires COM on the calling thread.
pub fn network() -> Result<Network> {
    // SAFETY: documented COM calls; the enumerator is bounded.
    let (connectivity, names) = unsafe {
        let nlm: INetworkListManager = CoCreateInstance(&NetworkListManager, None, CLSCTX_ALL)?;
        let c = nlm.GetConnectivity()?.0;
        let internet = NLM_CONNECTIVITY_IPV4_INTERNET.0 | NLM_CONNECTIVITY_IPV6_INTERNET.0;
        let local = NLM_CONNECTIVITY_IPV4_LOCALNETWORK.0
            | NLM_CONNECTIVITY_IPV6_LOCALNETWORK.0
            | NLM_CONNECTIVITY_IPV4_SUBNET.0
            | NLM_CONNECTIVITY_IPV6_SUBNET.0;
        let connectivity = if c & internet != 0 {
            Connectivity::Internet
        } else if c & local != 0 {
            Connectivity::Local
        } else {
            Connectivity::None
        };
        let mut names = Vec::new();
        if let Ok(en) = nlm.GetNetworks(NLM_ENUM_NETWORK_CONNECTED) {
            for _ in 0..16 {
                let mut batch: [Option<INetwork>; 1] = [None];
                let mut fetched = 0u32;
                if en.Next(&mut batch, Some(&mut fetched)).is_err() || fetched == 0 {
                    break;
                }
                if let Some(n) = batch[0].take() {
                    if let Ok(name) = n.GetName() {
                        let s = name.to_string();
                        if !s.is_empty() && !names.contains(&s) {
                            names.push(s);
                        }
                    }
                }
            }
        }
        (connectivity, names)
    };
    Ok(Network {
        connectivity,
        names,
        wifi: connectivity != Connectivity::None && active_adapter_is_wifi(),
    })
}

/// True if the first operational adapter with a default gateway is Wi-Fi.
fn active_adapter_is_wifi() -> bool {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS
        | GAA_FLAG_SKIP_ANYCAST
        | GAA_FLAG_SKIP_MULTICAST
        | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    for _ in 0..3 {
        // u64 elements keep the buffer suitably aligned for the struct.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let p = buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        // SAFETY: buffer of `size` bytes; the API reports the needed size.
        let r = unsafe { GetAdaptersAddresses(0, flags, None, Some(p), &mut size) };
        match r {
            0 => {
                // SAFETY: on success the buffer holds a linked list of
                // adapters that lives as long as `buf`.
                unsafe {
                    let mut cur = p;
                    let mut guard = 0;
                    while !cur.is_null() && guard < 256 {
                        let a = &*cur;
                        if a.OperStatus == IfOperStatusUp && !a.FirstGatewayAddress.is_null() {
                            return a.IfType == IF_TYPE_IEEE80211;
                        }
                        cur = a.Next;
                        guard += 1;
                    }
                }
                return false;
            }
            // ERROR_BUFFER_OVERFLOW: retry with the reported size.
            111 => continue,
            _ => return false,
        }
    }
    false
}

/// True if Windows believes there is internet connectivity.
pub fn internet_connected() -> Result<bool> {
    Ok(network()?.connectivity == Connectivity::Internet)
}
