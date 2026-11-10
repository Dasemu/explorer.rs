// SPDX-License-Identifier: GPL-3.0-or-later
//! Shell shortcuts (`.lnk`) through the documented `IShellLinkW` +
//! `IPersistFile` COM objects. The calling thread must have initialised
//! COM (see `util::ComApartment`).

use std::path::Path;

use anyhow::{Context, Result};
use windows::core::{Interface, HSTRING};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

/// What a shortcut points to.
#[derive(Debug, Clone, Default)]
pub struct ShortcutSpec<'a> {
    pub target: &'a str,
    pub arguments: &'a str,
    pub work_dir: &'a str,
    pub description: &'a str,
    /// Icon file and index; `None` uses the target's first icon.
    pub icon: Option<(&'a str, i32)>,
}

/// Create (or overwrite) the shortcut file `lnk`.
pub fn create(lnk: &Path, spec: &ShortcutSpec<'_>) -> Result<()> {
    // SAFETY: COM calls on interfaces we own; strings outlive each call.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .context("CoCreateInstance(ShellLink)")?;
        link.SetPath(&HSTRING::from(spec.target))
            .context("IShellLinkW::SetPath")?;
        if !spec.arguments.is_empty() {
            link.SetArguments(&HSTRING::from(spec.arguments))
                .context("IShellLinkW::SetArguments")?;
        }
        if !spec.work_dir.is_empty() {
            link.SetWorkingDirectory(&HSTRING::from(spec.work_dir))
                .context("IShellLinkW::SetWorkingDirectory")?;
        }
        if !spec.description.is_empty() {
            // INFOTIPSIZE is 1024 characters.
            let d: String = spec.description.chars().take(1000).collect();
            link.SetDescription(&HSTRING::from(d))
                .context("IShellLinkW::SetDescription")?;
        }
        if let Some((file, index)) = spec.icon {
            link.SetIconLocation(&HSTRING::from(file), index)
                .context("IShellLinkW::SetIconLocation")?;
        }
        let file: IPersistFile = link.cast().context("IPersistFile")?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
            .with_context(|| format!("save {}", lnk.display()))?;
    }
    Ok(())
}
