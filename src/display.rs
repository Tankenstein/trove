use std::time::{SystemTime, UNIX_EPOCH};

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

pub fn age(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s if s < 7 * 86_400 => format!("{}d", s / 86_400),
        s if s < 35 * 86_400 => format!("{}w", s / (7 * 86_400)),
        s if s < 365 * 86_400 => format!("{}mo", s / (30 * 86_400)),
        s => format!("{}y", s / (365 * 86_400)),
    }
}

pub fn ago(secs: i64) -> String {
    match age(secs) {
        now if now == "now" => now,
        age => format!("{age} ago"),
    }
}

fn local(ts: i64, tz: &TimeZone) -> Option<Zoned> {
    let t = Timestamp::from_second(ts).ok().filter(|_| ts > 0)?;
    Some(t.to_zoned(tz.clone()))
}

pub fn started(ts: i64, now: i64, tz: &TimeZone) -> String {
    let (Some(t), Some(now)) = (local(ts, tz), local(now, tz)) else {
        return String::new();
    };
    let format = if t.year() == now.year() {
        "%b %-d %H:%M"
    } else {
        "%b %-d %Y"
    };
    t.strftime(format).to_string()
}

pub fn day(ts: i64, now: i64, tz: &TimeZone) -> String {
    let (Some(t), Some(now)) = (local(ts, tz), local(now, tz)) else {
        return String::new();
    };
    let format = if t.year() == now.year() {
        "%b %-d"
    } else {
        "%b %-d %Y"
    };
    t.strftime(format).to_string()
}

/// In UTC, like `2026-03-02T14:10:00Z`.
pub fn iso(ts: i64) -> Option<String> {
    let t = Timestamp::from_second(ts).ok().filter(|_| ts > 0)?;
    Some(t.to_string())
}

pub fn datetime(ts: i64, tz: &TimeZone) -> String {
    local(ts, tz).map_or_else(String::new, |t| {
        t.strftime("%a %b %-d %Y, %H:%M").to_string()
    })
}

pub fn tilde(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
        }
    }
    out
}

pub fn printable(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Like `printable`, but keeps line breaks and tabs.
pub fn printable_text(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() => '?',
            c => c,
        })
        .collect()
}

pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages() {
        assert_eq!(age(-5), "now");
        assert_eq!(age(59), "now");
        assert_eq!(age(60), "1m");
        assert_eq!(age(7200), "2h");
        assert_eq!(age(3 * 86_400), "3d");
        assert_eq!(age(15 * 86_400), "2w");
        assert_eq!(age(90 * 86_400), "3mo");
        assert_eq!(age(800 * 86_400), "2y");
        assert_eq!(ago(10), "now");
        assert_eq!(ago(7200), "2h ago");
    }

    #[test]
    fn tildes() {
        assert_eq!(tilde("/Users/a/x", "/Users/a"), "~/x");
        assert_eq!(tilde("/Users/a", "/Users/a"), "~");
        assert_eq!(tilde("/Users/ab/x", "/Users/a"), "/Users/ab/x");
        assert_eq!(tilde("/tmp", ""), "/tmp");
    }

    #[test]
    fn single_line() {
        assert_eq!(one_line("  a\n\tb   c \r\n"), "a b c");
    }

    #[test]
    fn dates() {
        let utc = TimeZone::UTC;
        let ts = |s: &str| s.parse::<Timestamp>().unwrap().as_second();
        let now = ts("2026-03-04T05:06:07Z");
        let earlier = ts("2026-01-02T03:04:05Z");
        let last_year = ts("2025-11-04T12:00:00Z");
        assert_eq!(started(earlier, now, &utc), "Jan 2 03:04");
        assert_eq!(started(last_year, now, &utc), "Nov 4 2025");
        assert_eq!(started(0, now, &utc), "");
        assert_eq!(datetime(now, &utc), "Wed Mar 4 2026, 05:06");
        assert_eq!(datetime(0, &utc), "");
        assert_eq!(day(earlier, now, &utc), "Jan 2");
        assert_eq!(day(last_year, now, &utc), "Nov 4 2025");
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        assert_eq!(started(earlier, now, &tokyo), "Jan 2 12:04");
    }

    #[test]
    fn printable_strips_escapes() {
        assert_eq!(printable("/a/\x1b[31mred\x07"), "/a/?[31mred?");
        assert_eq!(printable("/a/café b"), "/a/café b");
    }

    #[test]
    fn separators() {
        assert_eq!(thousands(7), "7");
        assert_eq!(thousands(1284), "1,284");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }
}
