// SPDX-License-Identifier: GPL-3.0-or-later
//! Month-grid math and localized names for the taskbar's calendar flyout.
//!
//! Weekdays are numbered 0 = Sunday .. 6 = Saturday (like
//! [`crate::clock::DateTime::weekday`]). Windows' `LOCALE_IFIRSTDAYOFWEEK`
//! uses 0 = Monday .. 6 = Sunday; see [`week_start_from_locale`].

use crate::clock::DateTime;
use crate::i18n::Lang;

pub fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days in `month` (1..=12); 0 for an invalid month.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Day of week (0 = Sunday) of a Gregorian date (Sakamoto's method).
pub fn weekday(year: i32, month: u32, day: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let m = month.clamp(1, 12);
    let y = if m < 3 { year - 1 } else { year };
    let w = y + y.div_euclid(4) - y.div_euclid(100)
        + y.div_euclid(400)
        + T[(m - 1) as usize]
        + day as i32;
    w.rem_euclid(7) as u32
}

/// Convert `LOCALE_IFIRSTDAYOFWEEK` (0 = Monday .. 6 = Sunday) to our
/// numbering (0 = Sunday). Unknown values fall back to Monday (ISO 8601).
pub fn week_start_from_locale(ifirstdayofweek: u32) -> u32 {
    if ifirstdayofweek <= 6 {
        (ifirstdayofweek + 1) % 7
    } else {
        1
    }
}

/// `(year, month)` shifted by `delta` months.
pub fn add_months(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let idx = year * 12 + (month.clamp(1, 12) as i32 - 1) + delta;
    (idx.div_euclid(12), (idx.rem_euclid(12) + 1) as u32)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayCell {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    /// False for the leading/trailing days of the neighbouring months.
    pub in_month: bool,
}

/// Six weeks (42 cells) covering `month`, starting on `week_start`
/// (0 = Sunday). The first row always contains day 1.
pub fn month_grid(year: i32, month: u32, week_start: u32) -> Vec<DayCell> {
    let month = month.clamp(1, 12);
    let first = weekday(year, month, 1);
    let lead = (first + 7 - week_start % 7) % 7;
    let (py, pm) = add_months(year, month, -1);
    let (ny, nm) = add_months(year, month, 1);
    let prev_days = days_in_month(py, pm);
    let cur_days = days_in_month(year, month);
    (0..42u32)
        .map(|i| {
            if i < lead {
                DayCell {
                    year: py,
                    month: pm,
                    day: prev_days - lead + 1 + i,
                    in_month: false,
                }
            } else if i - lead < cur_days {
                DayCell {
                    year,
                    month,
                    day: i - lead + 1,
                    in_month: true,
                }
            } else {
                DayCell {
                    year: ny,
                    month: nm,
                    day: i - lead - cur_days + 1,
                    in_month: false,
                }
            }
        })
        .collect()
}

/// Weekday order of the grid's columns.
pub fn weekday_order(week_start: u32) -> [u32; 7] {
    let mut out = [0; 7];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (week_start + i as u32) % 7;
    }
    out
}

const MONTHS_EN: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const MONTHS_ES: [&str; 12] = [
    "enero",
    "febrero",
    "marzo",
    "abril",
    "mayo",
    "junio",
    "julio",
    "agosto",
    "septiembre",
    "octubre",
    "noviembre",
    "diciembre",
];
const DAYS_EN: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const DAYS_ES: [&str; 7] = [
    "domingo",
    "lunes",
    "martes",
    "miércoles",
    "jueves",
    "viernes",
    "sábado",
];
const DAYS_SHORT_EN: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
const DAYS_SHORT_ES: [&str; 7] = ["do", "lu", "ma", "mi", "ju", "vi", "sá"];

pub fn month_name(lang: Lang, month: u32) -> &'static str {
    let i = (month.clamp(1, 12) - 1) as usize;
    match lang {
        Lang::En => MONTHS_EN[i],
        Lang::Es => MONTHS_ES[i],
    }
}

pub fn weekday_name(lang: Lang, wd: u32) -> &'static str {
    let i = (wd % 7) as usize;
    match lang {
        Lang::En => DAYS_EN[i],
        Lang::Es => DAYS_ES[i],
    }
}

/// Two-letter column header, as in the Windows calendar.
pub fn weekday_short(lang: Lang, wd: u32) -> &'static str {
    let i = (wd % 7) as usize;
    match lang {
        Lang::En => DAYS_SHORT_EN[i],
        Lang::Es => DAYS_SHORT_ES[i],
    }
}

/// "November 2026" / "noviembre de 2026".
pub fn month_title(lang: Lang, year: i32, month: u32) -> String {
    match lang {
        Lang::En => format!("{} {year}", month_name(lang, month)),
        Lang::Es => format!("{} de {year}", month_name(lang, month)),
    }
}

/// "Tuesday, November 3, 2026" / "martes, 3 de noviembre de 2026".
pub fn long_date(lang: Lang, t: &DateTime) -> String {
    let wd = weekday_name(lang, t.weekday as u32);
    let m = month_name(lang, t.month as u32);
    match lang {
        Lang::En => format!("{wd}, {m} {}, {}", t.day, t.year),
        Lang::Es => format!("{wd}, {} de {m} de {}", t.day, t.year),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leap_and_lengths() {
        assert!(is_leap(2024) && is_leap(2000) && !is_leap(1900) && !is_leap(2026));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 11), 30);
        assert_eq!(days_in_month(2026, 13), 0);
    }

    #[test]
    fn weekdays() {
        assert_eq!(weekday(2026, 11, 3), 2); // Tuesday
        assert_eq!(weekday(2000, 1, 1), 6); // Saturday
        assert_eq!(weekday(2024, 2, 29), 4); // Thursday
        assert_eq!(weekday(1970, 1, 1), 4); // Thursday
    }

    #[test]
    fn locale_week_start() {
        assert_eq!(week_start_from_locale(0), 1); // Monday (es-ES)
        assert_eq!(week_start_from_locale(6), 0); // Sunday (en-US)
        assert_eq!(week_start_from_locale(5), 6); // Saturday
        assert_eq!(week_start_from_locale(42), 1);
    }

    #[test]
    fn month_arithmetic() {
        assert_eq!(add_months(2026, 1, -1), (2025, 12));
        assert_eq!(add_months(2026, 12, 1), (2027, 1));
        assert_eq!(add_months(2026, 11, 14), (2028, 1));
        assert_eq!(add_months(2026, 3, -27), (2023, 12));
    }

    #[test]
    fn grid_monday_first() {
        // November 2026 starts on a Sunday: Monday-first grid has 6 lead days.
        let g = month_grid(2026, 11, 1);
        assert_eq!(g.len(), 42);
        assert_eq!(
            g[0],
            DayCell {
                year: 2026,
                month: 10,
                day: 26,
                in_month: false
            }
        );
        assert_eq!(g[6].day, 1);
        assert!(g[6].in_month);
        assert_eq!(g[35].day, 30);
        assert!(g[35].in_month);
        assert_eq!(
            g[36],
            DayCell {
                year: 2026,
                month: 12,
                day: 1,
                in_month: false
            }
        );
    }

    #[test]
    fn grid_sunday_first() {
        let g = month_grid(2026, 11, 0);
        assert_eq!(g[0].day, 1);
        assert!(g[0].in_month);
        assert_eq!(g.iter().filter(|c| c.in_month).count(), 30);
        // Every column has the same weekday.
        for (i, c) in g.iter().enumerate() {
            assert_eq!(weekday(c.year, c.month, c.day), (i % 7) as u32);
        }
        // January: previous month is December of the previous year.
        let j = month_grid(2027, 1, 1);
        assert!(j.iter().any(|c| c.year == 2026 && c.month == 12));
    }

    #[test]
    fn names() {
        assert_eq!(weekday_order(1), [1, 2, 3, 4, 5, 6, 0]);
        assert_eq!(weekday_short(Lang::Es, 1), "lu");
        assert_eq!(month_title(Lang::Es, 2026, 11), "noviembre de 2026");
        assert_eq!(month_title(Lang::En, 2026, 11), "November 2026");
        let t = DateTime {
            year: 2026,
            month: 11,
            day: 3,
            weekday: 2,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(long_date(Lang::Es, &t), "martes, 3 de noviembre de 2026");
        assert_eq!(long_date(Lang::En, &t), "Tuesday, November 3, 2026");
    }
}
