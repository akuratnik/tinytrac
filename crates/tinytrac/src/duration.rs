//! Showing and typing durations.

use jiff::SignedDuration;

/// Hours and minutes, e.g. `2h 05m` or `45m`. Seconds are dropped.
pub fn short(duration: SignedDuration) -> String {
    let sign = if duration.is_negative() { "-" } else { "" };
    let minutes = duration.as_mins().abs();
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{sign}{m}m"),
        (h, m) => format!("{sign}{h}h {m:02}m"),
    }
}

/// A ticking clock face, e.g. `1:05:09`.
pub fn clock(duration: SignedDuration) -> String {
    let seconds = duration.as_secs().max(0);
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Reads what someone typed: `1:30`, `90` (minutes), `1h30m`, `1h 30m`, `2h`, `45m`.
pub fn parse(input: &str) -> Option<SignedDuration> {
    let input = input.trim().to_lowercase();
    let number = |text: &str| text.trim().parse::<i64>().ok().filter(|n| *n >= 0);
    let minutes = if let Some((hours, minutes)) = input.split_once(':') {
        let minutes = number(minutes).filter(|m| *m < 60)?;
        number(hours)?.checked_mul(60)?.checked_add(minutes)?
    } else if input.contains(['h', 'm']) {
        let (hours, rest) = input.split_once('h').unwrap_or(("0", &input));
        let minutes = rest.trim().trim_end_matches('m');
        let minutes = if minutes.trim().is_empty() {
            0
        } else {
            number(minutes)?
        };
        number(hours)?.checked_mul(60)?.checked_add(minutes)?
    } else {
        number(&input)?
    };
    SignedDuration::try_from_mins(minutes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: SignedDuration = SignedDuration::from_mins(1);

    #[test]
    fn formats_hours_and_minutes() {
        assert_eq!(short(MIN * 45), "45m");
        assert_eq!(short(MIN * 125), "2h 05m");
        assert_eq!(short(-MIN * 5), "-5m");
        assert_eq!(clock(MIN * 65 + SignedDuration::from_secs(9)), "1:05:09");
    }

    #[test]
    fn parses_common_forms() {
        for (input, minutes) in [
            ("1:30", 90),
            ("90", 90),
            ("1h30m", 90),
            ("1h 30m", 90),
            ("2h", 120),
            ("45m", 45),
            (" 0:05 ", 5),
        ] {
            assert_eq!(parse(input), Some(MIN * minutes), "{input}");
        }
    }

    #[test]
    fn rejects_nonsense() {
        for input in [
            "",
            "abc",
            "1:75",
            "-5",
            "h",
            "1:",
            "1h-30m",
            "999999999999999999",
            "99999999999999999h",
        ] {
            assert_eq!(parse(input), None, "{input}");
        }
    }
}
