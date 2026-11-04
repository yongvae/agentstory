//! Time parsing and display in the local timezone.

use anyhow::{Result, bail};
use chrono::{Local, NaiveDate, TimeZone};

pub fn fmt_local(ms: Option<i64>) -> String {
    match ms.and_then(|m| Local.timestamp_millis_opt(m).single()) {
        Some(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        None => "?".into(),
    }
}

pub fn day_local(ms: Option<i64>) -> String {
    match ms.and_then(|m| Local.timestamp_millis_opt(m).single()) {
        Some(t) => t.format("%Y-%m-%d").to_string(),
        None => "unknown".into(),
    }
}

/// `2026-11-01`, `2026-11-01T10:00`, `7d`, `24h`, `30m`, `today`, `yesterday` → unix ms.
pub fn parse_when(s: &str) -> Result<i64> {
    let now = Local::now();
    let s = s.trim();
    let midnight = |d: NaiveDate| {
        Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .map(|t| t.timestamp_millis())
    };
    match s {
        "today" => return midnight(now.date_naive()).ok_or_else(|| anyhow::anyhow!("bad date")),
        "yesterday" => {
            return midnight(now.date_naive().pred_opt().unwrap())
                .ok_or_else(|| anyhow::anyhow!("bad date"));
        }
        _ => {}
    }
    if let Some((n, unit)) = s.char_indices().last().map(|(i, c)| (&s[..i], c))
        && let Ok(n) = n.parse::<i64>()
    {
        let ms = match unit {
            'd' => n * 86_400_000,
            'h' => n * 3_600_000,
            'm' => n * 60_000,
            'w' => n * 7 * 86_400_000,
            _ => bail!("unknown unit in `{s}` (use d/h/m/w)"),
        };
        return Ok(now.timestamp_millis() - ms);
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return midnight(d).ok_or_else(|| anyhow::anyhow!("bad date"));
    }
    for f in ["%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s, f)
            && let Some(t) = Local.from_local_datetime(&t).single()
        {
            return Ok(t.timestamp_millis());
        }
    }
    bail!("can't read time `{s}` (try 2026-11-01, 7d, 24h, today)")
}

#[cfg(test)]
mod tests {
    #[test]
    fn relative_and_absolute() {
        let now = chrono::Local::now().timestamp_millis();
        let d = super::parse_when("7d").unwrap();
        assert!((now - d - 7 * 86_400_000).abs() < 5_000);
        assert!(super::parse_when("2026-11-01").is_ok());
        assert!(super::parse_when("nonsense").is_err());
    }
}
