//! Timestamp parsing without a date-time dependency.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parse an RFC 3339 timestamp (`2026-01-02T03:04:05.678Z`,
/// `…+02:00`). `None` for anything else.
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let b = s.trim().as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = std::str::from_utf8(b.get(from..to)?).ok()?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (hour, min, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut nanos: u64 = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let frac = std::str::from_utf8(&b[start..i]).ok()?;
        if frac.is_empty() {
            return None;
        }
        let digits: String = frac.chars().chain(std::iter::repeat('0')).take(9).collect();
        nanos = digits.parse().ok()?;
    }
    let offset_secs: i64 = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(sign @ (b'+' | b'-')) if i + 6 == b.len() && b[i + 3] == b':' => {
            let off = num(i + 1, i + 3)? * 3600 + num(i + 4, i + 6)? * 60;
            if *sign == b'+' { off } else { -off }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec - offset_secs;
    let secs = u64::try_from(secs).ok()?;
    Some(UNIX_EPOCH + Duration::new(secs, u32::try_from(nanos).ok()?))
}

/// Milliseconds since the Unix epoch; `None` for negative values.
pub fn from_millis(ms: i64) -> Option<SystemTime> {
    u64::try_from(ms)
        .ok()
        .map(|ms| UNIX_EPOCH + Duration::from_millis(ms))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(t: SystemTime) -> u64 {
        t.duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn parses_utc_and_offsets() {
        assert_eq!(secs(parse_rfc3339("1970-01-01T00:00:00Z").unwrap()), 0);
        assert_eq!(
            secs(parse_rfc3339("2026-01-02T03:04:05.678Z").unwrap()),
            1_767_323_045
        );
        assert_eq!(
            parse_rfc3339("2026-01-02T05:04:05.678+02:00"),
            parse_rfc3339("2026-01-02T03:04:05.678Z")
        );
        let t = parse_rfc3339("2026-01-02T03:04:05.5Z").unwrap();
        assert_eq!(t.duration_since(UNIX_EPOCH).unwrap().subsec_millis(), 500);
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-01-01T00:00:00",
            "2026-01-01T00:00:00.Z",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
    }
}
