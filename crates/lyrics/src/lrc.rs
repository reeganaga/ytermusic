//! Parser for the LRC format: `[mm:ss.xx]text` lines with optional metadata
//! tags (`[ar:...]`, `[offset:+250]`).

/// A timed line: start time in milliseconds and its text.
pub type TimedLine = (u64, String);

/// Parses LRC text into lines sorted by time. A line with several timestamps
/// (`[00:12.00][01:02.00]chorus`) is repeated at each time. The `[offset:ms]`
/// tag is applied (positive offset = lyrics shown earlier).
pub fn parse(text: &str) -> Vec<TimedLine> {
    let mut offset: i64 = 0;
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut times = Vec::new();
        while let Some(tag_end) = rest.strip_prefix('[').and_then(|r| r.find(']')) {
            let tag = &rest[1..tag_end + 1];
            if let Some(time) = parse_timestamp(tag) {
                times.push(time);
            } else if let Some(value) = tag.strip_prefix("offset:") {
                offset = value.trim().parse().unwrap_or(0);
            } else if times.is_empty() {
                // Other metadata tag ([ar:], [ti:], ...): the whole line is metadata
                rest = "";
                break;
            } else {
                break;
            }
            rest = &rest[tag_end + 2..];
        }
        for time in times {
            let time = (time as i64 - offset).max(0) as u64;
            lines.push((time, rest.trim().to_owned()));
        }
    }
    lines.sort_by_key(|(time, _)| *time);
    lines
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss.xxx` (also `mm:ss:xx`) in milliseconds.
fn parse_timestamp(tag: &str) -> Option<u64> {
    let (minutes, rest) = tag.split_once(':')?;
    let minutes: u64 = minutes.trim().parse().ok()?;
    let (seconds, fraction) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let seconds: u64 = seconds.trim().parse().ok()?;
    if seconds >= 60 {
        return None;
    }
    let fraction_ms = if fraction.is_empty() {
        0
    } else {
        if !fraction.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        // "5" -> 500ms, "45" -> 450ms, "456" -> 456ms
        let digits: String = fraction.chars().take(3).collect();
        let value: u64 = digits.parse().ok()?;
        value * 10u64.pow(3 - digits.len() as u32)
    };
    Some(minutes * 60_000 + seconds * 1000 + fraction_ms)
}

/// Index of the line being sung at `position_ms`: the last line starting at
/// or before it. `None` before the first line.
pub fn active_line(times: &[u64], position_ms: u64) -> Option<usize> {
    times
        .partition_point(|time| *time <= position_ms)
        .checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lines_and_fractions() {
        let lines = parse("[00:01.5]a\n[00:02.45]b\n[01:02.123] c \n");
        assert_eq!(
            lines,
            vec![
                (1500, "a".to_owned()),
                (2450, "b".to_owned()),
                (62123, "c".to_owned())
            ]
        );
    }

    #[test]
    fn repeats_multi_timestamp_lines_and_sorts() {
        let lines = parse("[00:10.00][00:30.00]chorus\n[00:20.00]verse");
        assert_eq!(
            lines,
            vec![
                (10_000, "chorus".to_owned()),
                (20_000, "verse".to_owned()),
                (30_000, "chorus".to_owned())
            ]
        );
    }

    #[test]
    fn skips_metadata_and_applies_offset() {
        let lines = parse("[ar:Someone]\n[ti:Song]\n[offset:+500]\n[00:02.00]x\n[00:00.20]y");
        assert_eq!(lines, vec![(0, "y".to_owned()), (1500, "x".to_owned())]);
    }

    #[test]
    fn keeps_empty_lines_and_ignores_plain_text() {
        let lines = parse("not a lyric\n[00:05.00]\n[00:06.00]z");
        assert_eq!(lines, vec![(5000, String::new()), (6000, "z".to_owned())]);
    }

    #[test]
    fn finds_active_line() {
        let times = [1000, 2000, 3000];
        assert_eq!(active_line(&times, 500), None);
        assert_eq!(active_line(&times, 1000), Some(0));
        assert_eq!(active_line(&times, 2999), Some(1));
        assert_eq!(active_line(&times, 99_000), Some(2));
        assert_eq!(active_line(&[], 10), None);
    }
}
