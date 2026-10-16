//! Small helpers that reproduce the exact Python semantics dashboard.py relies
//! on: str.isprintable / isspace / strip / split, codepoint-based truncation,
//! round(), float repr, datetime.fromisoformat / isoformat, parse_qs, and the
//! truthiness and int() coercions of JSON values.

use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use serde_json::{Map, Value};
use unicode_general_category::{get_general_category, GeneralCategory as GC};

pub type Obj = Map<String, Value>;

/// Python str.isprintable() for one character.
pub fn is_printable(c: char) -> bool {
    if c == ' ' {
        return true;
    }
    !matches!(
        get_general_category(c),
        GC::Control
            | GC::Format
            | GC::Surrogate
            | GC::PrivateUse
            | GC::Unassigned
            | GC::SpaceSeparator
            | GC::LineSeparator
            | GC::ParagraphSeparator
    )
}

/// Python str.isspace() for one character (Rust's White_Space plus the
/// \x1c-\x1f separators Python also treats as whitespace).
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python str.strip() with no argument.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub fn lstrip(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// Python str.split() with no argument.
pub fn split_ws(s: &str) -> Vec<&str> {
    s.split(is_space).filter(|p| !p.is_empty()).collect()
}

/// " ".join(s.split())
pub fn collapse_ws(s: &str) -> String {
    split_ws(s).join(" ")
}

/// The first `n` characters (codepoints), like s[:n].
pub fn take_chars(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// The last `n` characters, like s[-n:].
pub fn last_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    let skip = count - n;
    match s.char_indices().nth(skip) {
        Some((i, _)) => &s[i..],
        None => "",
    }
}

pub fn char_len(s: &str) -> usize {
    s.chars().count()
}

/// s[a:b] by codepoints (clamped).
pub fn char_slice(s: &str, a: usize, b: usize) -> String {
    s.chars().skip(a).take(b.saturating_sub(a)).collect()
}

/// dashboard.truncate: s if len(s) <= n else s[:n-1] + "…"
pub fn truncate(s: &str, n: usize) -> String {
    if char_len(s) <= n {
        s.to_string()
    } else {
        let mut out = take_chars(s, n.saturating_sub(1));
        out.push('…');
        out
    }
}

/// "".join(c for c in s if c.isprintable())
pub fn printable_only(s: &str) -> String {
    s.chars().filter(|c| is_printable(*c)).collect()
}

/// " ".join("".join(c for c in s if c.isprintable()).split())[:cap]
pub fn clean_note(s: &str, cap: usize) -> String {
    take_chars(&collapse_ws(&printable_only(s)), cap)
}

/// Python str.splitlines() (no keepends).
pub fn splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if brk {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// Lines of a text file read in Python's universal-newline mode.
pub fn universal_lines(s: &str) -> impl Iterator<Item = &str> {
    // \r\n and lone \r both end a line, like open(..., "r").
    let mut rest = s;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        match rest.find(['\n', '\r']) {
            Some(i) => {
                let line = &rest[..i];
                let skip = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
                rest = &rest[i + skip..];
                Some(line)
            }
            None => {
                let line = rest;
                rest = "";
                Some(line)
            }
        }
    })
}

/// Python round(x, n) for floats (correctly rounded, ties to even on the exact
/// binary value -- the same algorithm Rust's fixed-precision formatting uses).
pub fn round(x: f64, n: usize) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", n, x).parse().unwrap_or(x)
}

/// Python round(x) to an int (ties to even).
pub fn round_int(x: f64) -> i64 {
    format!("{:.0}", x).parse::<f64>().unwrap_or(0.0) as i64
}

/// Python repr(float) / str(float).
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let e = format!("{:e}", x); // shortest round-trip digits: "-1.2345e-5"
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    if (-4..16).contains(&exp) {
        let ndig = digits.len() as i32;
        let s = if exp >= 0 {
            let int_len = exp + 1;
            if ndig <= int_len {
                format!("{}{}.0", digits, "0".repeat((int_len - ndig) as usize))
            } else {
                format!("{}.{}", &digits[..int_len as usize], &digits[int_len as usize..])
            }
        } else {
            format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
        };
        format!("{}{}", sign, s)
    } else {
        let m = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits.clone()
        };
        format!("{}{}e{}{:02}", sign, m, if exp < 0 { "-" } else { "+" }, exp.abs())
    }
}

/// f"{n:,}"
pub fn thousands(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{}", out)
    } else {
        out
    }
}

// --------------------------------------------------------------------------
// datetime
// --------------------------------------------------------------------------

/// An aware UTC instant with microsecond precision (what parse_ts returns).
pub type Dt = DateTime<Utc>;

fn parse_frac(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut digits: String = s.chars().take(6).collect();
    while digits.len() < 6 {
        digits.push('0');
    }
    digits.parse().ok()
}

fn parse_hms(t: &str) -> Option<(u32, u32, u32, u32)> {
    // HH[:MM[:SS[.ffffff]]] or the basic HHMM[SS[.f]] form
    let (main, frac) = match t.find(['.', ',']) {
        Some(i) => (&t[..i], Some(&t[i + 1..])),
        None => (t, None),
    };
    let parts: Vec<&str> = if main.contains(':') {
        main.split(':').collect()
    } else {
        let mut v = Vec::new();
        let mut i = 0;
        while i < main.len() {
            v.push(main.get(i..i + 2)?);
            i += 2;
        }
        v
    };
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.len() != 2 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let h: u32 = parts[0].parse().ok()?;
    let m: u32 = if parts.len() > 1 { parts[1].parse().ok()? } else { 0 };
    let s: u32 = if parts.len() > 2 { parts[2].parse().ok()? } else { 0 };
    let us = match frac {
        Some(f) => {
            if parts.len() < 3 {
                return None;
            }
            parse_frac(f)?
        }
        None => 0,
    };
    if h > 24 || m > 59 || s > 59 {
        return None;
    }
    Some((h, m, s, us))
}

fn parse_offset(z: &str) -> Option<i32> {
    // "+HH:MM", "+HHMM", "+HH", "+HH:MM:SS"
    let sign = match z.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &z[1..];
    let digits: String = body.chars().filter(|c| *c != ':').collect();
    let (main, frac) = match digits.find('.') {
        Some(i) => (digits[..i].to_string(), Some(digits[i + 1..].to_string())),
        None => (digits.clone(), None),
    };
    if !main.bytes().all(|b| b.is_ascii_digit()) || !(main.len() == 2 || main.len() == 4 || main.len() == 6) {
        return None;
    }
    let h: i32 = main[0..2].parse().ok()?;
    let m: i32 = if main.len() >= 4 { main[2..4].parse().ok()? } else { 0 };
    let s: i32 = if main.len() >= 6 { main[4..6].parse().ok()? } else { 0 };
    let _ = frac;
    if h > 23 || m > 59 || s > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60 + s))
}

/// datetime.fromisoformat (with the "Z" -> "+00:00" rewrite dashboard.parse_ts
/// does first). Naive values are taken as UTC (parse_ts's rule). Returns the
/// instant in UTC.
pub fn parse_iso_utc(s: &str) -> Option<Dt> {
    let mut t = strip(s).to_string();
    if t.ends_with('Z') {
        t.pop();
        t.push_str("+00:00");
    }
    if t.len() < 10 || !t.is_char_boundary(10) {
        return None;
    }
    let date = NaiveDate::parse_from_str(&t[..10], "%Y-%m-%d").ok()?;
    if t.as_bytes()[4] != b'-' {
        return None;
    }
    let rest = &t[10..];
    if rest.is_empty() {
        return Some(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?));
    }
    let sep = rest.chars().next()?;
    let rest = &rest[sep.len_utf8()..];
    // Python accepts any single character as the date/time separator.
    let _ = sep;
    let (time_part, off) = match rest.find(['+', '-']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let (h, m, sec, us) = parse_hms(time_part)?;
    let naive = if h == 24 {
        if m != 0 || sec != 0 || us != 0 {
            return None;
        }
        date.succ_opt()?.and_hms_micro_opt(0, 0, 0, 0)?
    } else {
        date.and_hms_micro_opt(h, m, sec, us)?
    };
    match off {
        Some(o) => {
            let secs = parse_offset(o)?;
            let tz = FixedOffset::east_opt(secs)?;
            let dt = tz.from_local_datetime(&naive).single()?;
            Some(dt.with_timezone(&Utc))
        }
        None => Some(Utc.from_utc_datetime(&naive)),
    }
}

/// datetime.isoformat() of a UTC-aware value: "...T10:00:00[.ffffff]+00:00".
pub fn iso(dt: &Dt) -> String {
    let base = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
    let us = dt.nanosecond() / 1000;
    if us != 0 {
        format!("{}.{:06}+00:00", base, us)
    } else {
        format!("{}+00:00", base)
    }
}

/// datetime.timestamp() of an aware value (float seconds).
pub fn ts(dt: &Dt) -> f64 {
    let micros = dt.timestamp() as i128 * 1_000_000 + (dt.nanosecond() / 1000) as i128;
    micros as f64 / 1e6
}

/// datetime.fromtimestamp(t, tz=utc) (microseconds rounded half-even).
pub fn from_ts(t: f64) -> Option<Dt> {
    if !t.is_finite() {
        return None;
    }
    let secs = t.floor();
    let frac = t - secs;
    let mut us = (frac * 1e6).round_ties_even() as i64;
    let mut secs = secs as i64;
    if us >= 1_000_000 {
        secs += 1;
        us -= 1_000_000;
    }
    Utc.timestamp_opt(secs, (us * 1000) as u32).single()
}

pub fn iso_from_ts(t: f64) -> Option<String> {
    from_ts(t).map(|d| iso(&d))
}

pub fn now_utc() -> Dt {
    let n = Utc::now();
    // keep microsecond precision like Python
    Utc.timestamp_opt(n.timestamp(), (n.timestamp_subsec_micros()) * 1000).single().unwrap_or(n)
}

pub fn now_ts() -> f64 {
    ts(&now_utc())
}

/// The local-time hour of an instant (ts.astimezone().hour).
pub fn local_hour(dt: &Dt) -> u32 {
    dt.with_timezone(&Local).hour()
}

/// Today's date in local time (now_utc().astimezone().date()).
pub fn local_today() -> NaiveDate {
    Local::now().date_naive()
}

pub fn utc_today() -> NaiveDate {
    Utc::now().date_naive()
}

/// date.fromisoformat for "YYYY-MM-DD" (also accepts the basic YYYYMMDD form).
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        return NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
    }
    if s.len() == 8 && s.bytes().all(|b| b.is_ascii_digit()) {
        return NaiveDate::parse_from_str(s, "%Y%m%d").ok();
    }
    None
}

pub fn date_iso(d: &NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

/// A naive local date-time to UTC (for Cursor clocks without an offset).
pub fn local_to_utc(n: &NaiveDateTime) -> Option<Dt> {
    Local.from_local_datetime(n).earliest().map(|d| d.with_timezone(&Utc))
}

// --------------------------------------------------------------------------
// JSON value semantics
// --------------------------------------------------------------------------

/// Python truthiness of a decoded JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub fn opt_truthy(v: Option<&Value>) -> bool {
    v.map(truthy).unwrap_or(false)
}

/// isinstance(v, int) and not isinstance(v, bool)
pub fn strict_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64(),
        _ => None,
    }
}

/// isinstance(v, (int, float)) and not bool
pub fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn parse_py_int_str(s: &str) -> Option<i64> {
    let t = strip(s);
    let t = t.replace('_', "");
    if t.is_empty() {
        return None;
    }
    let (neg, digits) = match t.as_bytes()[0] {
        b'+' => (false, &t[1..]),
        b'-' => (true, &t[1..]),
        _ => (false, &t[..]),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let v: i64 = digits.parse().ok()?;
    Some(if neg { -v } else { v })
}

/// Python int(v): bools, ints, floats (truncated) and integer strings.
pub fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                let f = n.as_f64()?;
                if f.is_finite() {
                    Some(f.trunc() as i64)
                } else {
                    None
                }
            }
        }
        Value::String(s) => parse_py_int_str(s),
        _ => None,
    }
}

/// Python float(v): bools, numbers and float strings.
pub fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = strip(s).replace('_', "");
            let low = t.to_ascii_lowercase();
            match low.trim_start_matches(['+', '-']) {
                "nan" => return Some(f64::NAN),
                "inf" | "infinity" => {
                    return Some(if low.starts_with('-') { f64::NEG_INFINITY } else { f64::INFINITY })
                }
                _ => {}
            }
            if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
                return None;
            }
            t.parse().ok()
        }
        _ => None,
    }
}

pub fn get_str<'a>(o: &'a Obj, k: &str) -> Option<&'a str> {
    o.get(k).and_then(|v| v.as_str())
}

/// d.get(k) or "" for string fields (non-strings read as "").
pub fn str_or_empty<'a>(o: &'a Obj, k: &str) -> &'a str {
    get_str(o, k).unwrap_or("")
}

// --------------------------------------------------------------------------
// URLs and query strings
// --------------------------------------------------------------------------

fn hexval(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// urllib.parse.unquote (UTF-8, errors="replace").
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexval(b[i + 1]), hexval(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// urllib.parse.unquote_plus
pub fn unquote_plus(s: &str) -> String {
    unquote(&s.replace('+', " "))
}

/// urllib.parse.parse_qs(qs) (keep_blank_values=False), first value per key.
pub fn parse_qs(query: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for part in query.split('&') {
        if part.is_empty() {
            continue;
        }
        let Some((k, v)) = part.split_once('=') else { continue };
        if v.is_empty() {
            continue;
        }
        out.push((unquote_plus(k), unquote_plus(v)));
    }
    out
}

/// qs.get(k, [""])[0] -- the first non-blank value, else None.
pub fn qs_first(query: &str, key: &str) -> Option<String> {
    parse_qs(query).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// urllib.parse.quote(s, safe=safe)
pub fn quote(s: &str, safe: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if b.is_ascii_alphanumeric() || "_.-~".contains(c) || (b < 128 && safe.contains(c)) {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// urllib.parse.quote_plus
pub fn quote_plus(s: &str) -> String {
    quote(s, " ").replace(' ', "+")
}

pub fn token_hex(nbytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; nbytes];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// uuid.uuid4().hex
pub fn uuid4_hex() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    hex::encode(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_and_space() {
        assert!(is_printable('a') && is_printable(' ') && is_printable('é'));
        assert!(!is_printable('\x07') && !is_printable('\t') && !is_printable('\u{202e}') && !is_printable('\u{a0}'));
        assert!(is_space('\x1c') && is_space('\u{3000}') && !is_space('a'));
        assert_eq!(collapse_ws("  a \t b\n"), "a b");
    }

    #[test]
    fn truncation_is_by_codepoint() {
        assert_eq!(truncate("héllo", 3), "hé…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(last_chars("abcdef", 2), "ef");
    }

    #[test]
    fn rounding_and_repr_match_python() {
        assert_eq!(round(0.125, 2), 0.12);
        assert_eq!(round(0.0005, 3), 0.001);
        assert_eq!(round(2.675, 2), 2.67);
        assert_eq!(round_int(2.5), 2);
        assert_eq!(round_int(3.5), 4);
        assert_eq!(float_repr(0.0), "0.0");
        assert_eq!(float_repr(1.5), "1.5");
        assert_eq!(float_repr(1e-7), "1e-07");
        assert_eq!(float_repr(0.0001), "0.0001");
        assert_eq!(float_repr(123456789012345678.0), "1.2345678901234568e+17");
        assert_eq!(float_repr(1e16), "1e+16");
        assert_eq!(float_repr(25.0), "25.0");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(12), "12");
    }

    #[test]
    fn iso_round_trip() {
        let d = parse_iso_utc("2026-09-01T10:00:00.000Z").unwrap();
        assert_eq!(iso(&d), "2026-09-01T10:00:00+00:00");
        let d = parse_iso_utc("2026-09-01T10:00:00.123Z").unwrap();
        assert_eq!(iso(&d), "2026-09-01T10:00:00.123000+00:00");
        let d = parse_iso_utc("2026-09-01T15:30:00+05:30").unwrap();
        assert_eq!(iso(&d), "2026-09-01T10:00:00+00:00");
        assert!(parse_iso_utc("garbage").is_none());
        assert_eq!(iso(&parse_iso_utc("2026-09-01").unwrap()), "2026-09-01T00:00:00+00:00");
        assert_eq!(iso_from_ts(1789999200.0).unwrap(), "2026-09-21T14:00:00+00:00");
        assert_eq!(ts(&parse_iso_utc("2026-09-21T14:00:00Z").unwrap()), 1789999200.0);
    }

    #[test]
    fn query_strings() {
        assert_eq!(qs_first("q=a+b&x=", "q").as_deref(), Some("a b"));
        assert_eq!(qs_first("q=", "q"), None);
        assert_eq!(qs_first("q=%E2%9C%93", "q").as_deref(), Some("✓"));
        assert_eq!(quote("r_a b/c", ""), "r_a%20b%2Fc");
    }

    #[test]
    fn int_coercion() {
        assert_eq!(py_int(&Value::from("  42 ")), Some(42));
        assert_eq!(py_int(&Value::from(3.9)), Some(3));
        assert_eq!(py_int(&Value::from(true)), Some(1));
        assert_eq!(py_int(&Value::from("3.5")), None);
        assert_eq!(py_float(&Value::from("12.5")), Some(12.5));
        assert_eq!(strict_int(&Value::from(1.0)), None);
        assert_eq!(strict_int(&Value::from(true)), None);
    }

    #[test]
    fn lines() {
        assert_eq!(splitlines("a\nb\r\nc\rd"), vec!["a", "b", "c", "d"]);
        assert_eq!(splitlines("a\n"), vec!["a"]);
        assert_eq!(universal_lines("a\r\nb\rc\n").collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }
}
