// SPDX-License-Identifier: GPL-3.0-or-later
//! Virtual desktops (feature `virtual-desktops`).
//!
//! WARNING: uses the `winvd` crate, which talks to the **undocumented**
//! `IVirtualDesktopManagerInternal` COM interfaces. Their IIDs and vtables
//! change between Windows builds; an unsupported build makes these calls
//! fail (errors are returned, never panics). Unverified on Windows.
//!
//! TODO(vdesk): show a desktop switcher in the taskbar; listen for desktop
//! change notifications (`winvd::listen_desktop_events`) to refresh the
//! window list; filter buttons to the current desktop (today cloaked windows
//! on other desktops are already hidden by the DWM-cloak rule).

use anyhow::{anyhow, Result};

pub fn desktop_count() -> Result<u32> {
    winvd::get_desktop_count().map_err(|e| anyhow!("winvd: {e:?}"))
}

pub fn current_desktop_index() -> Result<u32> {
    winvd::get_current_desktop()
        .and_then(|d| d.get_index())
        .map_err(|e| anyhow!("winvd: {e:?}"))
}

pub fn switch_to(index: u32) -> Result<()> {
    winvd::switch_desktop(index).map_err(|e| anyhow!("winvd: {e:?}"))
}
