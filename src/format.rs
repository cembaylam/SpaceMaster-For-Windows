//! Explorer-style byte formatting: binary units (1 KB = 1024 B) with three significant digits
//! ("2.51 GB", "25.1 GB", "251 GB"), exactly like Windows Explorer shows sizes.

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

pub fn bytes(n: i64) -> String {
    let n = n.max(0);
    if n < 1000 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 || (u == 1 && v >= 10.0) {
        format!("{} {}", v.round() as i64, UNITS[u])
    } else if v >= 10.0 {
        format!("{:.1} {}", (v * 10.0).round() / 10.0, UNITS[u])
    } else {
        format!("{:.2} {}", (v * 100.0).round() / 100.0, UNITS[u])
    }
}

/// Thousands separated count ("1,234,567").
pub fn count(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Percentage label used in the list ("12%", "3.4%", "<0.1%").
pub fn percent(share: f64) -> String {
    let pct = share * 100.0;
    if pct >= 10.0 {
        format!("{}%", pct.round() as i64)
    } else if pct >= 0.1 {
        format!("{pct:.1}%")
    } else {
        "<0.1%".to_string()
    }
}

/// FILETIME (100 ns ticks since 1601) to "yyyy-mm-dd hh:mm" in UTC-agnostic local-less form.
/// The UI converts with the system local time zone; this pure helper formats a civil time.
pub fn civil(filetime: i64) -> (i32, u32, u32, u32, u32) {
    let secs = filetime / 10_000_000 - 11_644_473_600;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32)
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((y + i64::from(m <= 2)) as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(-5), "0 B");
        assert_eq!(bytes(1000), "0.98 KB");
        assert_eq!(bytes(1024), "1.00 KB");
        assert_eq!(bytes(1536), "1.50 KB");
        assert_eq!(bytes(10 * 1024), "10 KB");
        assert_eq!(bytes(1024 * 1024), "1.00 MB");
        assert_eq!(bytes((2.51 * 1024.0 * 1024.0 * 1024.0) as i64), "2.51 GB");
        assert_eq!(bytes((25.1 * 1024.0 * 1024.0 * 1024.0) as i64), "25.1 GB");
        assert_eq!(bytes(251 * 1024 * 1024 * 1024), "251 GB");
        assert_eq!(bytes(1i64 << 40), "1.00 TB");
    }

    #[test]
    fn counts() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn percents() {
        assert_eq!(percent(0.5), "50%");
        assert_eq!(percent(0.034), "3.4%");
        assert_eq!(percent(0.0001), "<0.1%");
    }

    #[test]
    fn civil_time() {
        // 2024-02-29 12:34:00 UTC
        let ft = (1_709_210_040i64 + 11_644_473_600) * 10_000_000;
        assert_eq!(civil(ft), (2024, 2, 29, 12, 34));
        assert_eq!(civil(11_644_473_600 * 10_000_000), (1970, 1, 1, 0, 0));
    }
}
