// SPDX-License-Identifier: GPL-3.0-or-later
//! Local time for the clock.

use explorer_core::clock::DateTime;
use windows::Win32::System::SystemInformation::GetLocalTime;

pub fn now_local() -> DateTime {
    // SAFETY: no arguments.
    let t = unsafe { GetLocalTime() };
    DateTime {
        year: t.wYear,
        month: t.wMonth as u8,
        day: t.wDay as u8,
        weekday: t.wDayOfWeek as u8,
        hour: t.wHour as u8,
        minute: t.wMinute as u8,
        second: t.wSecond as u8,
    }
}
