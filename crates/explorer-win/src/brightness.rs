// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in display brightness through WMI (`root\WMI`):
//! `WmiMonitorBrightness.CurrentBrightness` to read and
//! `WmiMonitorBrightnessMethods.WmiSetBrightness(Timeout, Brightness)` to
//! write. Both classes are documented and only exist for panels whose
//! brightness the OS controls (laptops, tablets). External monitors (DDC/CI)
//! are not supported; on desktops the query returns no instance and the
//! Quick settings flyout hides the slider.
//!
//! Requires COM (MTA recommended) on the calling thread; calls may take tens
//! of milliseconds, so the taskbar only uses this from a worker thread.

use anyhow::{anyhow, Result};
use windows::core::{w, BSTR, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoSetProxyBlanket, CLSCTX_INPROC_SERVER, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemContext, IWbemLocator, IWbemServices, WbemLocator,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE,
};

/// RPC_C_AUTHN_WINNT / RPC_C_AUTHZ_NONE (rpcdce.h).
const RPC_C_AUTHN_WINNT: u32 = 10;
const RPC_C_AUTHZ_NONE: u32 = 0;
/// Per-object timeout for enumerations, in milliseconds.
const NEXT_TIMEOUT_MS: i32 = 2000;

pub struct Brightness {
    services: IWbemServices,
}

impl Brightness {
    /// Connect to `root\WMI`. Succeeds even when no panel supports
    /// brightness; [`Brightness::get`] then returns `None`.
    pub fn open() -> Result<Self> {
        // SAFETY: documented WMI client set-up.
        unsafe {
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
            let services = locator.ConnectServer(
                &BSTR::from("ROOT\\WMI"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None::<&IWbemContext>,
            )?;
            CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )?;
            Ok(Self { services })
        }
    }

    fn first(&self, query: &str) -> Result<Option<IWbemClassObject>> {
        // SAFETY: documented WMI calls; bounded wait per object.
        unsafe {
            let en: IEnumWbemClassObject = self.services.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(query),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None::<&IWbemContext>,
            )?;
            let mut objs: [Option<IWbemClassObject>; 1] = [None];
            let mut n = 0u32;
            let hr = en.Next(NEXT_TIMEOUT_MS, &mut objs, &mut n);
            if hr.is_err() || n == 0 {
                return Ok(None);
            }
            Ok(objs[0].take())
        }
    }

    /// Current brightness in percent, or `None` if not supported.
    pub fn get(&self) -> Result<Option<u8>> {
        let Some(obj) = self.first("SELECT CurrentBrightness FROM WmiMonitorBrightness")? else {
            return Ok(None);
        };
        let mut v = VARIANT::default();
        // SAFETY: valid out-pointer.
        unsafe { obj.Get(w!("CurrentBrightness"), 0, &mut v, None, None)? };
        let level = u32::try_from(&v)?;
        Ok(Some(level.min(100) as u8))
    }

    /// Set the brightness in percent.
    pub fn set(&self, percent: u8) -> Result<()> {
        let Some(inst) = self.first("SELECT * FROM WmiMonitorBrightnessMethods")? else {
            return Err(anyhow!("WmiMonitorBrightnessMethods not available"));
        };
        // SAFETY: documented WMI method invocation; all out-pointers valid.
        unsafe {
            let mut path = VARIANT::default();
            inst.Get(w!("__PATH"), 0, &mut path, None, None)?;
            let path = BSTR::try_from(&path)?;
            let mut class: Option<IWbemClassObject> = None;
            self.services.GetObject(
                &BSTR::from("WmiMonitorBrightnessMethods"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None::<&IWbemContext>,
                Some(&mut class),
                None,
            )?;
            let class = class.ok_or_else(|| anyhow!("no WmiMonitorBrightnessMethods class"))?;
            let mut in_sig: Option<IWbemClassObject> = None;
            class.GetMethod(w!("WmiSetBrightness"), 0, &mut in_sig, std::ptr::null_mut())?;
            let in_sig = in_sig.ok_or_else(|| anyhow!("no WmiSetBrightness signature"))?;
            let params = in_sig.SpawnInstance(0)?;
            // uint32 Timeout, uint8 Brightness; WMI converts from VT_I4.
            params.Put(w!("Timeout"), 0, &VARIANT::from(0i32), 0)?;
            params.Put(
                w!("Brightness"),
                0,
                &VARIANT::from(percent.min(100) as i32),
                0,
            )?;
            self.services.ExecMethod(
                &path,
                &BSTR::from("WmiSetBrightness"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None::<&IWbemContext>,
                &params,
                None,
                None,
            )?;
        }
        Ok(())
    }
}
