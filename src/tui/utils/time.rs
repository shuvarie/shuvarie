//! Relative time labels: how long until something happens, in at most two
//! units (`4m`, `3h 5m`, `1d 4h`).

use std::time::Duration;

/// A wall-clock span as a relative time: the largest whole unit, plus the
/// next one when it rounds to something (`3h 5m`, `1d 4h`). Anything under a
/// minute reads `<1m` so a live countdown never sits on `0m`.
pub fn fmt_relative(remaining: Duration) -> String {
    let minutes = remaining.as_secs() / 60;
    if minutes == 0 {
        return "<1m".to_string();
    }
    let hours = minutes / 60;
    if hours == 0 {
        return format!("{minutes}m");
    }
    let days = hours / 24;
    if days == 0 {
        return match minutes % 60 {
            0 => format!("{hours}h"),
            minutes => format!("{hours}h {minutes}m"),
        };
    }
    match hours % 24 {
        0 => format!("{days}d"),
        hours => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::fmt_relative;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn under_a_minute_reads_less_than_one() {
        assert_eq!(fmt_relative(Duration::ZERO), "<1m");
        assert_eq!(fmt_relative(secs(1)), "<1m");
        assert_eq!(fmt_relative(secs(59)), "<1m");
    }

    #[test]
    fn minutes() {
        assert_eq!(fmt_relative(secs(60)), "1m");
        assert_eq!(fmt_relative(secs(4 * 60)), "4m");
        assert_eq!(fmt_relative(secs(59 * 60 + 59)), "59m");
    }

    #[test]
    fn hours_keep_the_minutes() {
        assert_eq!(fmt_relative(secs(60 * 60)), "1h");
        assert_eq!(fmt_relative(secs(3 * 60 * 60 + 5 * 60)), "3h 5m");
        assert_eq!(fmt_relative(secs(23 * 60 * 60 + 59 * 60)), "23h 59m");
    }

    #[test]
    fn days_keep_the_hours() {
        assert_eq!(fmt_relative(secs(24 * 60 * 60)), "1d");
        assert_eq!(fmt_relative(secs(28 * 60 * 60)), "1d 4h");
        assert_eq!(fmt_relative(secs(95 * 60 * 60)), "3d 23h");
    }
}
