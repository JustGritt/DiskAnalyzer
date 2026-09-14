//! Small helpers shared across the crate: formatting, size parsing and a
//! minimal JSON writer for the HTML payload.

use std::fmt::Write as _;

// --------------------------------------------------------------------------
// Formatting
// --------------------------------------------------------------------------

const SIZE_UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// Insert thousands separators into the integer part of an already
/// formatted number (`"1234.5"` -> `"1,234.5"`).
fn group_thousands(number: &str) -> String {
    let (sign, rest) = match number.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", number),
    };
    let (int_part, frac_part) = match rest.find('.') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };
    let digits: Vec<char> = int_part.chars().collect();
    let mut out = String::with_capacity(number.len() + digits.len() / 3 + 1);
    out.push_str(sign);
    for (i, ch) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*ch);
    }
    out.push_str(frac_part);
    out
}

/// Format bytes the way Explorer does (binary multiples, decimal labels).
pub fn human_size(num_bytes: u64) -> String {
    let mut value = num_bytes as f64;
    for (i, unit) in SIZE_UNITS.iter().enumerate() {
        if value.abs() < 1024.0 || i == SIZE_UNITS.len() - 1 {
            if i == 0 {
                return format!("{} B", value as u64);
            }
            return format!("{} {}", group_thousands(&format!("{value:.1}")), unit);
        }
        value /= 1024.0;
    }
    unreachable!()
}

pub fn human_count(num: u64) -> String {
    group_thousands(&num.to_string())
}

pub fn human_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let (minutes, secs) = (seconds / 60, seconds % 60);
    if minutes < 60 {
        return format!("{minutes}m {secs:02}s");
    }
    let (hours, minutes) = (minutes / 60, minutes % 60);
    format!("{hours}h {minutes:02}m {secs:02}s")
}

/// Render a modification time as an approximate age.
pub fn human_age(timestamp: f64, now: f64) -> String {
    if timestamp <= 0.0 {
        return "unknown".to_string();
    }
    let days = ((now - timestamp) / 86400.0).max(0.0) as u64;
    if days < 1 {
        "today".to_string()
    } else if days < 60 {
        format!("{days}d")
    } else if days < 730 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

/// A day count in the unit a person would say: whole years as years
/// (`365` -> "1 year"), anything else as days (`90` -> "90 days").
pub fn human_days(days: u64) -> String {
    if days > 0 && days.is_multiple_of(365) {
        match days / 365 {
            1 => "1 year".to_string(),
            years => format!("{years} years"),
        }
    } else if days == 1 {
        "1 day".to_string()
    } else {
        format!("{days} days")
    }
}

/// Shorten a path but keep both ends, which is where the meaning lives.
pub fn truncate_middle(text: &str, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if width <= 3 || chars.len() <= width {
        return text.to_string();
    }
    let keep = width - 3;
    let head = keep / 2;
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push_str("...");
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// Approximate on-disk size: every file occupies whole clusters.
pub fn round_to_cluster(size: u64, cluster: u64) -> u64 {
    if cluster <= 1 || size == 0 {
        return size;
    }
    size.div_ceil(cluster) * cluster
}

/// Seconds since the Unix epoch, as a float.
pub fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Accept `500`, `500MB`, `1.5g` and friends. Binary units.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let amount_len = trimmed
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(trimmed.len());
    let (amount, unit) = trimmed.split_at(amount_len);
    let unit = unit.trim().to_ascii_lowercase();
    let factor: f64 = match unit.as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1024.0,
        "m" | "mb" => 1024.0 * 1024.0,
        "g" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(format!("not a size: {text:?} (try 500MB or 2GB)")),
    };
    let value: f64 = amount
        .parse()
        .map_err(|_| format!("not a size: {text:?} (try 500MB or 2GB)"))?;
    Ok((value * factor) as u64)
}

// --------------------------------------------------------------------------
// JSON
// --------------------------------------------------------------------------

/// A minimal JSON value tree. The HTML report embeds one document and the
/// payload is capped in size, so building it in memory is fine and keeps
/// serde out of the dependency list.
#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Int(u64),
    Float(f64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn obj() -> Json {
        Json::Object(Vec::new())
    }

    /// Append a key to an object; a no-op on any other variant.
    pub fn set(mut self, key: &str, value: impl Into<Json>) -> Json {
        if let Json::Object(ref mut fields) = self {
            fields.push((key.to_string(), value.into()));
        }
        self
    }

    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        self.write_into(&mut out);
        out
    }

    fn write_into(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Json::Float(f) => {
                if f.is_finite() {
                    let _ = write!(out, "{f}");
                } else {
                    out.push_str("null");
                }
            }
            Json::Str(s) => write_json_string(s, out),
            Json::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_into(out);
                }
                out.push(']');
            }
            Json::Object(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(key, out);
                    out.push(':');
                    value.write_into(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl From<bool> for Json {
    fn from(v: bool) -> Json {
        Json::Bool(v)
    }
}
impl From<u64> for Json {
    fn from(v: u64) -> Json {
        Json::Int(v)
    }
}
impl From<u32> for Json {
    fn from(v: u32) -> Json {
        Json::Int(v as u64)
    }
}
impl From<usize> for Json {
    fn from(v: usize) -> Json {
        Json::Int(v as u64)
    }
}
impl From<f64> for Json {
    fn from(v: f64) -> Json {
        Json::Float(v)
    }
}
impl From<&str> for Json {
    fn from(v: &str) -> Json {
        Json::Str(v.to_string())
    }
}
impl From<String> for Json {
    fn from(v: String) -> Json {
        Json::Str(v)
    }
}
impl From<Vec<Json>> for Json {
    fn from(v: Vec<Json>) -> Json {
        Json::Array(v)
    }
}
impl<T: Into<Json>> From<Option<T>> for Json {
    fn from(v: Option<T>) -> Json {
        match v {
            Some(inner) => inner.into(),
            None => Json::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_python_formatting() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(human_size(1_099_511_627_776), "1.0 TB");
        assert_eq!(human_size(1_125_899_906_842_624), "1.0 PB");
        assert_eq!(human_size(2_000 * 1_125_899_906_842_624), "2,000.0 PB");
    }

    #[test]
    fn counts_are_grouped() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1000), "1,000");
        assert_eq!(human_count(2_534_112), "2,534,112");
    }

    #[test]
    fn durations() {
        assert_eq!(human_duration(0.4), "0s");
        assert_eq!(human_duration(59.9), "59s");
        assert_eq!(human_duration(61.0), "1m 01s");
        assert_eq!(human_duration(3661.0), "1h 01m 01s");
    }

    #[test]
    fn ages() {
        let now = 1_700_000_000.0;
        assert_eq!(human_age(0.0, now), "unknown");
        assert_eq!(human_age(now - 100.0, now), "today");
        assert_eq!(human_age(now + 100.0, now), "today");
        assert_eq!(human_age(now - 5.0 * 86400.0, now), "5d");
        assert_eq!(human_age(now - 100.0 * 86400.0, now), "3mo");
        assert_eq!(human_age(now - 800.0 * 86400.0, now), "2y");
    }

    #[test]
    fn day_counts() {
        assert_eq!(human_days(0), "0 days");
        assert_eq!(human_days(1), "1 day");
        assert_eq!(human_days(90), "90 days");
        assert_eq!(human_days(364), "364 days");
        assert_eq!(human_days(365), "1 year");
        assert_eq!(human_days(400), "400 days");
        assert_eq!(human_days(730), "2 years");
    }

    #[test]
    fn truncation_keeps_both_ends() {
        assert_eq!(truncate_middle("short", 10), "short");
        assert_eq!(truncate_middle("abcdefghij", 3), "abcdefghij");
        assert_eq!(truncate_middle("abcdefghijklmnop", 10), "abc...mnop");
        assert_eq!(truncate_middle("ééééééééééé", 8), "éé...ééé");
    }

    #[test]
    fn cluster_rounding() {
        assert_eq!(round_to_cluster(0, 4096), 0);
        assert_eq!(round_to_cluster(1, 4096), 4096);
        assert_eq!(round_to_cluster(4096, 4096), 4096);
        assert_eq!(round_to_cluster(4097, 4096), 8192);
        assert_eq!(round_to_cluster(123, 1), 123);
        assert_eq!(round_to_cluster(123, 0), 123);
    }

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("500").unwrap(), 500);
        assert_eq!(parse_size("500MB").unwrap(), 500 * 1024 * 1024);
        assert_eq!(parse_size(" 2 gb ").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1.5g").unwrap(), 1_610_612_736);
        assert_eq!(parse_size("10k").unwrap(), 10_240);
        assert_eq!(parse_size("0").unwrap(), 0);
        assert!(parse_size("abc").is_err());
        assert!(parse_size("5 lightyears").is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn json_escaping() {
        let doc = Json::obj()
            .set("s", "a\"b\\c\n</script>")
            .set("n", 42u64)
            .set("f", 0.25f64)
            .set("b", true)
            .set("list", vec![Json::Null, Json::from("x")]);
        assert_eq!(
            doc.to_json_string(),
            r#"{"s":"a\"b\\c\n</script>","n":42,"f":0.25,"b":true,"list":[null,"x"]}"#
        );
        assert_eq!(Json::from("\u{1}").to_json_string(), "\"\\u0001\"");
    }
}
