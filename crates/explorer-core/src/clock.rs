// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal strftime-like formatting for the taskbar clock.
//!
//! Supported: `%H %M %S %I %p %d %m %Y %a %b %%`. Unknown specifiers are
//! printed verbatim so a typo is visible rather than fatal.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    /// 1..=12
    pub month: u8,
    pub day: u8,
    /// 0 = Sunday
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub fn format(fmt: &str, t: &DateTime) -> String {
    let mut out = String::with_capacity(fmt.len() + 8);
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('H') => out.push_str(&format!("{:02}", t.hour)),
            Some('M') => out.push_str(&format!("{:02}", t.minute)),
            Some('S') => out.push_str(&format!("{:02}", t.second)),
            Some('I') => {
                let h = match t.hour % 12 {
                    0 => 12,
                    h => h,
                };
                out.push_str(&format!("{h:02}"));
            }
            Some('p') => out.push_str(if t.hour < 12 { "AM" } else { "PM" }),
            Some('d') => out.push_str(&format!("{:02}", t.day)),
            Some('m') => out.push_str(&format!("{:02}", t.month)),
            Some('Y') => out.push_str(&format!("{:04}", t.year)),
            Some('a') => out.push_str(WEEKDAYS.get(t.weekday as usize).unwrap_or(&"?")),
            Some('b') => out.push_str(
                MONTHS
                    .get((t.month as usize).wrapping_sub(1))
                    .unwrap_or(&"?"),
            ),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: DateTime = DateTime {
        year: 2026,
        month: 11,
        day: 2,
        weekday: 1,
        hour: 0,
        minute: 5,
        second: 9,
    };

    #[test]
    fn formats() {
        assert_eq!(format("%H:%M", &T), "00:05");
        assert_eq!(format("%I:%M %p", &T), "12:05 AM");
        assert_eq!(format("%a %d %b %Y", &T), "Mon 02 Nov 2026");
        assert_eq!(format("%d/%m/%Y %S", &T), "02/11/2026 09");
        assert_eq!(format("100%% %q %", &T), "100% %q %");
    }

    #[test]
    fn bad_values_do_not_panic() {
        let t = DateTime {
            month: 0,
            weekday: 9,
            ..T
        };
        assert_eq!(format("%a %b", &t), "? ?");
    }
}
