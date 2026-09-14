//! Small helpers shared across the crate: formatting, size parsing and a
//! minimal JSON writer and reader for the report payload.

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

    /// Parse a JSON document. Just enough for reading back `report.json`:
    /// strict syntax, numbers as `Int` when they are non-negative integers
    /// that fit a `u64` and `Float` otherwise.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut parser = Parser {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        };
        parser.skip_whitespace();
        let value = parser.value(0)?;
        parser.skip_whitespace();
        if parser.pos != parser.bytes.len() {
            return Err(parser.error("unexpected text after the document"));
        }
        Ok(value)
    }

    /// The value of the first matching key of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Json::Int(n) => Some(n),
            _ => None,
        }
    }

    /// Any number. A float written without a fractional part reads back as
    /// `Int`, so both variants count.
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Json::Int(n) => Some(n as f64),
            Json::Float(f) => Some(f),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Json::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(items) => Some(items),
            _ => None,
        }
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

/// Nesting beyond this is refused rather than risking the stack; reports
/// nest a few levels plus the treemap's folder depth.
const JSON_MAX_DEPTH: usize = 512;

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        format!("invalid JSON at byte {}: {what}", self.pos)
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.pos) {
            self.pos += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.pos) == Some(&byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            Ok(value)
        } else {
            Err(self.error("unknown literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > JSON_MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        match self.bytes.get(self.pos) {
            None => Err(self.error("unexpected end of input")),
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("expected a value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.pos += 1;
        let mut fields = Vec::new();
        self.skip_whitespace();
        if self.eat(b'}') {
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_whitespace();
            if self.bytes.get(self.pos) != Some(&b'"') {
                return Err(self.error("expected a key"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if !self.eat(b':') {
                return Err(self.error("expected ':'"));
            }
            self.skip_whitespace();
            fields.push((key, self.value(depth + 1)?));
            self.skip_whitespace();
            if self.eat(b'}') {
                return Ok(Json::Object(fields));
            }
            if !self.eat(b',') {
                return Err(self.error("expected ',' or '}'"));
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.eat(b']') {
            return Ok(Json::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            if self.eat(b']') {
                return Ok(Json::Array(items));
            }
            if !self.eat(b',') {
                return Err(self.error("expected ',' or ']'"));
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .text
            .get(self.pos..self.pos + 4)
            .filter(|digits| digits.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.error("bad \\u escape"))?;
        let code = u32::from_str_radix(digits, 16).map_err(|_| self.error("bad \\u escape"))?;
        self.pos += 4;
        Ok(code)
    }

    fn string(&mut self) -> Result<String, String> {
        self.pos += 1;
        let mut out = String::new();
        // Plain runs are copied as slices. They start and end at ASCII bytes
        // (quote, backslash), so they are always valid UTF-8 boundaries.
        let mut run_start = self.pos;
        loop {
            match self.bytes.get(self.pos) {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    let escape = *self
                        .bytes
                        .get(self.pos)
                        .ok_or_else(|| self.error("unterminated escape"))?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let first = self.hex4()?;
                            let code = match first {
                                0xD800..=0xDBFF => {
                                    if !(self.eat(b'\\') && self.eat(b'u')) {
                                        return Err(self.error("unpaired surrogate"));
                                    }
                                    let second = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&second) {
                                        return Err(self.error("unpaired surrogate"));
                                    }
                                    0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                                }
                                0xDC00..=0xDFFF => return Err(self.error("unpaired surrogate")),
                                other => other,
                            };
                            out.push(
                                char::from_u32(code).ok_or_else(|| self.error("bad code point"))?,
                            );
                        }
                        _ => return Err(self.error("unknown escape")),
                    }
                    run_start = self.pos;
                }
                Some(&b) if b < 0x20 => return Err(self.error("control character in string")),
                Some(_) => self.pos += 1,
            }
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        let negative = self.eat(b'-');
        let digits_start = self.pos;
        while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
            self.pos += 1;
        }
        let int_digits = self.pos - digits_start;
        if int_digits == 0 || (int_digits > 1 && self.bytes[digits_start] == b'0') {
            return Err(self.error("malformed number"));
        }
        let mut integral = !negative;
        if self.eat(b'.') {
            integral = false;
            let frac_start = self.pos;
            while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return Err(self.error("malformed number"));
            }
        }
        if let Some(b'e' | b'E') = self.bytes.get(self.pos) {
            integral = false;
            self.pos += 1;
            if let Some(b'+' | b'-') = self.bytes.get(self.pos) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while self.bytes.get(self.pos).is_some_and(u8::is_ascii_digit) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return Err(self.error("malformed number"));
            }
        }
        let text = &self.text[start..self.pos];
        if integral {
            if let Ok(n) = text.parse::<u64>() {
                return Ok(Json::Int(n));
            }
        }
        text.parse::<f64>()
            .map(Json::Float)
            .map_err(|_| self.error("malformed number"))
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

    #[test]
    fn json_round_trips_through_the_parser() {
        let doc = Json::obj()
            .set("s", "a\"b\\c\n\t\u{1}</script> é 😀")
            .set("n", u64::MAX)
            .set("f", 0.25f64)
            .set("whole", 1789000000.0f64)
            .set("neg", -3.5f64)
            .set("b", false)
            .set("empty", Json::obj())
            .set(
                "list",
                vec![Json::Null, Json::from("x"), Json::Array(vec![])],
            );
        let text = doc.to_json_string();
        let parsed = Json::parse(&text).unwrap();
        assert_eq!(parsed.to_json_string(), text);
        assert_eq!(parsed.get("n").and_then(Json::as_u64), Some(u64::MAX));
        assert_eq!(parsed.get("f").and_then(Json::as_f64), Some(0.25));
        // A float without a fraction is written as an integer; as_f64 still reads it.
        assert_eq!(
            parsed.get("whole").and_then(Json::as_f64),
            Some(1789000000.0)
        );
        assert_eq!(parsed.get("neg").and_then(Json::as_u64), None);
        assert_eq!(parsed.get("b").and_then(Json::as_bool), Some(false));
        assert_eq!(
            parsed.get("list").and_then(Json::as_array).map(<[_]>::len),
            Some(3)
        );
        assert!(parsed.get("missing").is_none());
    }

    #[test]
    fn json_parser_handles_escapes_whitespace_and_numbers() {
        let doc = Json::parse(
            " { \"a\" : [ 1 , -0 , 2.5e3 , 1E-2, 18446744073709551616 ] ,\r\n \"u\":\"\\u00e9\\ud83d\\ude00\\/\" } ",
        )
        .unwrap();
        let items = doc.get("a").and_then(Json::as_array).unwrap();
        assert_eq!(items[0].as_u64(), Some(1));
        assert_eq!(items[1].as_f64(), Some(0.0));
        assert!(items[1].as_u64().is_none());
        assert_eq!(items[2].as_f64(), Some(2500.0));
        assert_eq!(items[3].as_f64(), Some(0.01));
        assert!(
            items[4].as_u64().is_none(),
            "past u64::MAX reads as a float"
        );
        assert_eq!(doc.get("u").and_then(Json::as_str), Some("é😀/"));
    }

    #[test]
    fn json_parser_rejects_malformed_input() {
        for bad in [
            "",
            "{",
            "[1,]",
            "{\"a\":1,}",
            "{a:1}",
            "01",
            "1.",
            "-",
            "1e",
            "\"unterminated",
            "\"bad \\x escape\"",
            "\"\\ud83d alone\"",
            "\"\\ude00\"",
            "\"\\u12G4\"",
            "\"raw\ncontrol\"",
            "tru",
            "nul",
            "[1] 2",
        ] {
            assert!(Json::parse(bad).is_err(), "accepted {bad:?}");
        }
        let deep = "[".repeat(10_000);
        assert!(Json::parse(&deep).is_err());
    }
}
