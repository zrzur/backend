//! Time zones from the system's IANA database (`/usr/share/zoneinfo`, or
//! `ZONEINFO`), enough to turn a local wall-clock time into UTC and back.
//! Reads TZif files (version 2+, 64-bit section) and their POSIX footer
//! for dates after the last listed change. No library: the workspace has
//! no time-zone crate.
//!
//! Used by repeating server events, so "every Thursday at 19:00 in
//! Vienna" stays at 19:00 across the daylight saving changes.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A daylight saving rule from a POSIX TZ string
#[derive(Clone, Debug, PartialEq, Eq)]
struct PosixRule {
    /// Standard offset east of UTC, in seconds
    std: i64,
    /// Daylight offset east of UTC, in seconds (when there is daylight time)
    dst: Option<(i64, DayRule, i64, DayRule, i64)>,
}

/// `Mm.w.d`: day `d` (0 = Sunday) of week `w` (5 = last) of month `m`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DayRule {
    month: u32,
    week: u32,
    weekday: u32,
}

/// One zone: its changes and the rule after them
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    /// UTC second of each change, with the offset from then on
    changes: Vec<(i64, i64)>,
    /// Offset before the first change
    initial: i64,
    footer: Option<PosixRule>,
}

/// Whether a name looks like an IANA zone (`Europe/Vienna`, `UTC`) and
/// nothing that walks out of the zone folder
pub fn valid_zone_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('/')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '+'))
}

fn cache() -> &'static Mutex<HashMap<String, Option<Zone>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Zone>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Zone {
    /// UTC itself
    pub fn utc() -> Zone {
        Zone {
            changes: vec![],
            initial: 0,
            footer: None,
        }
    }

    /// Load a zone by its IANA name; None when unknown or unreadable
    pub fn load(name: &str) -> Option<Zone> {
        if !valid_zone_name(name) {
            return None;
        }
        if let Some(known) = cache().lock().ok()?.get(name) {
            return known.clone();
        }
        let dir = std::env::var("ZONEINFO").unwrap_or_else(|_| "/usr/share/zoneinfo".into());
        let zone = std::fs::read(format!("{dir}/{name}"))
            .ok()
            .and_then(|bytes| Zone::parse(&bytes));
        if let Ok(mut map) = cache().lock() {
            map.insert(name.to_string(), zone.clone());
        }
        zone
    }

    /// Parse a TZif file
    pub fn parse(data: &[u8]) -> Option<Zone> {
        let header = |at: usize| -> Option<[usize; 6]> {
            if data.get(at..at + 4)? != b"TZif" {
                return None;
            }
            let mut counts = [0usize; 6];
            for (i, count) in counts.iter_mut().enumerate() {
                let start = at + 20 + i * 4;
                *count = u32::from_be_bytes(data.get(start..start + 4)?.try_into().ok()?) as usize;
            }
            Some(counts)
        };
        // counts: isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt
        let first = header(0)?;
        let version = *data.get(4)?;
        let v1_len = first[3] * 4 + first[3] + first[4] * 6 + first[5] + first[2] * 8 + first[1] + first[0];
        let (at, counts, wide) = if version >= b'2' {
            let at = 44 + v1_len;
            (at, header(at)?, true)
        } else {
            (0, first, false)
        };
        let [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt] = counts;
        let time_size = if wide { 8 } else { 4 };
        let mut pos = at + 44;
        let mut times = Vec::with_capacity(timecnt);
        for _ in 0..timecnt {
            let bytes = data.get(pos..pos + time_size)?;
            times.push(if wide {
                i64::from_be_bytes(bytes.try_into().ok()?)
            } else {
                i32::from_be_bytes(bytes.try_into().ok()?) as i64
            });
            pos += time_size;
        }
        let indexes = data.get(pos..pos + timecnt)?.to_vec();
        pos += timecnt;
        let mut offsets = Vec::with_capacity(typecnt);
        for _ in 0..typecnt {
            let bytes = data.get(pos..pos + 6)?;
            offsets.push(i32::from_be_bytes(bytes[0..4].try_into().ok()?) as i64);
            pos += 6;
        }
        pos += charcnt + leapcnt * (time_size + 4) + isstdcnt + isutcnt;
        let changes = times
            .iter()
            .zip(indexes.iter())
            .map(|(time, index)| Some((*time, *offsets.get(*index as usize)?)))
            .collect::<Option<Vec<_>>>()?;
        let footer = if wide {
            data.get(pos..)
                .and_then(|rest| std::str::from_utf8(rest).ok())
                .and_then(|text| text.trim_matches('\n').lines().next().map(str::to_string))
                .and_then(|line| parse_posix(&line))
        } else {
            None
        };
        Some(Zone {
            changes,
            initial: *offsets.first()?,
            footer,
        })
    }

    /// Offset east of UTC, in seconds, at a UTC second
    pub fn offset_at(&self, utc: i64) -> i64 {
        match self.changes.last() {
            Some((last, offset)) if utc >= *last => match &self.footer {
                Some(rule) => rule.offset_at(utc),
                None => *offset,
            },
            None => self.footer.as_ref().map_or(self.initial, |rule| rule.offset_at(utc)),
            Some(_) => {
                let index = self.changes.partition_point(|(time, _)| *time <= utc);
                if index == 0 {
                    self.initial
                } else {
                    self.changes[index - 1].1
                }
            }
        }
    }

    /// The UTC second of a local wall-clock second. A time that happens
    /// twice (clocks go back) is its first; a time that does not exist
    /// (clocks go forward) moves forward by the jump, as clocks do.
    pub fn to_utc(&self, local: i64) -> i64 {
        let before = self.offset_at(local - 86_400);
        let after = self.offset_at(local + 86_400);
        let mut valid: Vec<i64> = [before, after]
            .into_iter()
            .map(|offset| local - offset)
            .filter(|utc| local - self.offset_at(*utc) == *utc)
            .collect();
        valid.sort();
        valid.first().copied().unwrap_or(local - before)
    }

    /// The local wall-clock second of a UTC second
    pub fn to_local(&self, utc: i64) -> i64 {
        utc + self.offset_at(utc)
    }
}

impl PosixRule {
    fn offset_at(&self, utc: i64) -> i64 {
        let Some((dst, start, start_time, end, end_time)) = &self.dst else {
            return self.std;
        };
        let year = civil_from_days((utc + self.std).div_euclid(86_400)).0;
        // the changes of this year, as UTC seconds
        let start_utc = day_of_rule(year, start) * 86_400 + start_time - self.std;
        let end_utc = day_of_rule(year, end) * 86_400 + end_time - dst;
        let in_dst = if start_utc < end_utc {
            utc >= start_utc && utc < end_utc
        } else {
            // southern hemisphere: daylight time spans the new year
            utc >= start_utc || utc < end_utc
        };
        if in_dst { *dst } else { self.std }
    }
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian)
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let month = month as i64;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day since 1970-01-01
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + if month <= 2 { 1 } else { 0 }, month, day)
}

/// Days in a month
pub fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// Day (since 1970) a `Mm.w.d` rule falls on in a year
fn day_of_rule(year: i64, rule: &DayRule) -> i64 {
    let first = days_from_civil(year, rule.month, 1);
    // 1970-01-01 was a Thursday (4)
    let first_weekday = (first + 4).rem_euclid(7) as u32;
    let mut day = 1 + (rule.weekday + 7 - first_weekday) % 7 + (rule.week.saturating_sub(1)) * 7;
    let length = days_in_month(year, rule.month);
    while day > length {
        day -= 7;
    }
    first + day as i64 - 1
}

/// `[+-]hh[:mm[:ss]]` as seconds
fn parse_hms(text: &str) -> Option<i64> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'-' => (-1, &text[1..]),
        b'+' => (1, &text[1..]),
        _ => (1, text),
    };
    let mut parts = rest.split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next().map_or(Some(0), |m| m.parse().ok())?;
    let seconds: i64 = parts.next().map_or(Some(0), |s| s.parse().ok())?;
    Some(sign * (hours * 3600 + minutes * 60 + seconds))
}

/// A zone abbreviation (`CET`, `<+03>`) at the start; the rest
fn take_name(text: &str) -> Option<&str> {
    if let Some(rest) = text.strip_prefix('<') {
        return rest.split_once('>').map(|(_, rest)| rest);
    }
    let end = text.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(text.len());
    (end >= 3).then(|| &text[end..])
}

/// An offset (until the next letter, `<` or `,`) at the start; the rest
fn take_offset(text: &str) -> (Option<i64>, &str) {
    let end = text
        .find(|c: char| c.is_ascii_alphabetic() || c == '<' || c == ',')
        .unwrap_or(text.len());
    if end == 0 {
        return (None, text);
    }
    (parse_hms(&text[..end]), &text[end..])
}

fn parse_day_rule(text: &str) -> Option<(DayRule, i64)> {
    let (date, time) = match text.split_once('/') {
        Some((date, time)) => (date, parse_hms(time)?),
        None => (text, 7200),
    };
    let mut parts = date.strip_prefix('M')?.split('.');
    let rule = DayRule {
        month: parts.next()?.parse().ok()?,
        week: parts.next()?.parse().ok()?,
        weekday: parts.next()?.parse().ok()?,
    };
    (1..=12).contains(&rule.month).then_some((rule, time))
}

/// A POSIX TZ string such as `CET-1CEST,M3.5.0,M10.5.0/3`; only `M` rules
/// (every zone in use today). POSIX offsets count west, so they flip here.
fn parse_posix(text: &str) -> Option<PosixRule> {
    let rest = take_name(text)?;
    let (std, rest) = take_offset(rest);
    let std = -std?;
    if rest.is_empty() {
        return Some(PosixRule { std, dst: None });
    }
    let rest = take_name(rest)?;
    let (dst, rest) = take_offset(rest);
    let dst = dst.map_or(std + 3600, |offset| -offset);
    let mut rules = rest.strip_prefix(',')?.split(',');
    let (start, start_time) = parse_day_rule(rules.next()?)?;
    let (end, end_time) = parse_day_rule(rules.next()?)?;
    Some(PosixRule {
        std,
        dst: Some((dst, start, start_time, end, end_time)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
        days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60
    }

    #[test]
    fn civil_dates_round_trip() {
        for days in [-1_000_000, -1, 0, 1, 10_957, 20_000, 2_000_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
    }

    #[test]
    fn posix_rules_read_like_the_database() {
        let vienna = parse_posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        assert_eq!(vienna.std, 3600);
        // 2026: summer time 29 March 01:00 UTC to 25 October 01:00 UTC
        assert_eq!(vienna.offset_at(utc(2026, 3, 29, 0, 59)), 3600);
        assert_eq!(vienna.offset_at(utc(2026, 3, 29, 1, 0)), 7200);
        assert_eq!(vienna.offset_at(utc(2026, 10, 25, 0, 59)), 7200);
        assert_eq!(vienna.offset_at(utc(2026, 10, 25, 1, 0)), 3600);
        let new_york = parse_posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(new_york.offset_at(utc(2026, 7, 1, 12, 0)), -4 * 3600);
        assert_eq!(new_york.offset_at(utc(2026, 12, 1, 12, 0)), -5 * 3600);
        // a zone without daylight time, written with <>
        let damascus = parse_posix("<+03>-3").unwrap();
        assert_eq!(damascus.offset_at(utc(2026, 7, 1, 0, 0)), 3 * 3600);
        // southern hemisphere
        let sydney = parse_posix("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(sydney.offset_at(utc(2026, 1, 15, 0, 0)), 11 * 3600);
        assert_eq!(sydney.offset_at(utc(2026, 7, 15, 0, 0)), 10 * 3600);
    }

    #[test]
    fn local_times_in_gaps_and_overlaps() {
        let zone = Zone {
            changes: vec![],
            initial: 3600,
            footer: parse_posix("CET-1CEST,M3.5.0,M10.5.0/3"),
        };
        // 18:00 local before and after the October change
        assert_eq!(zone.to_utc(utc(2026, 10, 22, 18, 0)), utc(2026, 10, 22, 16, 0));
        assert_eq!(zone.to_utc(utc(2026, 10, 29, 18, 0)), utc(2026, 10, 29, 17, 0));
        // 02:30 on 29 March 2026 does not exist: it becomes 03:30
        assert_eq!(zone.to_utc(utc(2026, 3, 29, 2, 30)), utc(2026, 3, 29, 1, 30));
        // 02:30 on 25 October 2026 happens twice: the first one
        assert_eq!(zone.to_utc(utc(2026, 10, 25, 2, 30)), utc(2026, 10, 25, 0, 30));
        assert_eq!(zone.to_local(utc(2026, 10, 29, 17, 0)), utc(2026, 10, 29, 18, 0));
    }

    #[test]
    fn the_system_database_is_read_when_present() {
        assert!(!valid_zone_name("../etc/passwd"));
        assert!(!valid_zone_name("/etc/passwd"));
        assert!(valid_zone_name("America/Argentina/Buenos_Aires"));
        let Some(vienna) = Zone::load("Europe/Vienna") else {
            // no zone database on this machine: the rule tests above cover it
            return;
        };
        assert_eq!(vienna.offset_at(utc(2026, 7, 1, 12, 0)), 7200);
        assert_eq!(vienna.offset_at(utc(2027, 1, 1, 12, 0)), 3600);
        assert_eq!(vienna.offset_at(utc(2040, 7, 1, 12, 0)), 7200);
        assert!(Zone::load("Not/AZone").is_none());
    }
}
