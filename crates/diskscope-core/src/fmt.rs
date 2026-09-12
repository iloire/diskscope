//! Human-readable sizes.

/// Formats a byte count the way macOS does: decimal units, three significant
/// digits. 1 GB is 10⁹ bytes, matching what the Finder and `df` report, so a
/// number here can be compared with a number there.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["bytes", "KB", "MB", "GB", "TB", "PB", "EB"];
    if n < 1000 {
        return format!("{n} bytes");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    let decimals = if value < 10.0 {
        2
    } else if value < 100.0 {
        1
    } else {
        0
    };
    format!("{value:.decimals$} {}", UNITS[unit])
}

/// Formats a count with thin separators, for the "1 842 317 files" readout.
pub fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// "1 file" or "12 files". The singular matters: these land in a table where
/// "1 files" is the kind of thing a reader notices.
pub fn files(n: u64) -> String {
    if n == 1 {
        "1 file".into()
    } else {
        format!("{} files", count(n))
    }
}

pub fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "—".into();
    }
    let p = part as f64 * 100.0 / whole as f64;
    if p >= 10.0 {
        format!("{p:.0}%")
    } else if p >= 1.0 {
        format!("{p:.1}%")
    } else if p > 0.0 {
        "<1%".into()
    } else {
        "0%".into()
    }
}

/// Rate in bytes per second, for live scan progress.
pub fn rate(bytes_done: u64, secs: f64) -> String {
    if secs <= 0.0 {
        return "—".into();
    }
    format!("{}/s", bytes((bytes_done as f64 / secs) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_the_finder() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(999), "999 bytes");
        assert_eq!(bytes(1000), "1.00 KB");
        assert_eq!(bytes(1_500_000), "1.50 MB");
        assert_eq!(bytes(412_000_000_000), "412 GB");
        assert_eq!(bytes(u64::MAX), "18.4 EB");
    }

    #[test]
    fn counts_are_grouped() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_842_317), "1\u{202f}842\u{202f}317");
    }

    #[test]
    fn file_counts_are_singular_when_they_should_be() {
        assert_eq!(files(0), "0 files");
        assert_eq!(files(1), "1 file");
        assert_eq!(files(2), "2 files");
    }

    #[test]
    fn percentages_degrade_gracefully() {
        assert_eq!(percent(1, 0), "—");
        assert_eq!(percent(0, 100), "0%");
        assert_eq!(percent(1, 1000), "<1%");
        assert_eq!(percent(55, 1000), "5.5%");
        assert_eq!(percent(550, 1000), "55%");
    }
}
