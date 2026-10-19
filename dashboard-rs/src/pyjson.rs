//! json.dumps with Python's exact output: ", " / ": " separators (or the
//! compact ones), ensure_ascii escaping, indent=2 files and float repr. Using it
//! for responses and state files keeps both servers byte-compatible.

use crate::py::float_repr;
use serde_json::Value;

#[derive(Clone, Copy)]
pub struct Style {
    pub indent: Option<usize>,
    pub compact: bool,
    pub ensure_ascii: bool,
}

pub const DEFAULT: Style = Style { indent: None, compact: false, ensure_ascii: true };
pub const COMPACT: Style = Style { indent: None, compact: true, ensure_ascii: true };
pub const INDENT2: Style = Style { indent: Some(2), compact: false, ensure_ascii: true };

/// json.dumps(v)
pub fn dumps(v: &Value) -> String {
    dumps_with(v, DEFAULT)
}

pub fn dumps_with(v: &Value, style: Style) -> String {
    let mut out = String::new();
    write(&mut out, v, style, 0);
    out
}

fn write_str(out: &mut String, s: &str, ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", u));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline(out: &mut String, style: Style, level: usize) {
    if let Some(n) = style.indent {
        out.push('\n');
        for _ in 0..n * level {
            out.push(' ');
        }
    }
}

fn write(out: &mut String, v: &Value, style: Style, level: usize) {
    let item_sep = if style.compact || style.indent.is_some() { "," } else { ", " };
    let kv_sep = if style.compact { ":" } else { ": " };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                out.push_str(&n.to_string());
            } else {
                out.push_str(&float_repr(n.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(s) => write_str(out, s, style.ensure_ascii),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, style, level + 1);
                write(out, x, style, level + 1);
            }
            newline(out, style, level);
            out.push(']');
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                newline(out, style, level + 1);
                write_str(out, k, style.ensure_ascii);
                out.push_str(kv_sep);
                write(out, x, style, level + 1);
            }
            newline(out, style, level);
            out.push('}');
        }
    }
}

/// json.dumps(v, sort_keys=True) -- used only for change signatures.
pub fn dumps_sorted(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                let mut m = serde_json::Map::new();
                for k in keys {
                    m.insert(k.clone(), sort(&o[k]));
                }
                Value::Object(m)
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            x => x.clone(),
        }
    }
    dumps(&sort(v))
}

/// Stands in for a NaN / Infinity literal that json.loads accepts and serde
/// cannot hold: a string, so every "is it a number" check rejects it, which is
/// what Python's range checks do with those values.
pub const NON_FINITE: &str = "\u{0}non-finite";

/// json.loads for a state file Python may have written: like serde_json, but
/// the NaN / Infinity / -Infinity literals parse (as NON_FINITE).
pub fn loads_lenient(s: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str(s) {
        return Some(v);
    }
    let mut out = String::with_capacity(s.len());
    let (mut in_str, mut esc) = (false, false);
    let mut rest = s;
    let marker = serde_json::to_string(NON_FINITE).unwrap();
    while let Some(c) = rest.chars().next() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else {
            let hit = ["-Infinity", "Infinity", "NaN"].into_iter().find(|t| rest.starts_with(t));
            if let Some(t) = hit {
                out.push_str(&marker);
                rest = &rest[t.len()..];
                continue;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    serde_json::from_str(&out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lenient_loads_take_python_non_finite_literals() {
        let v = loads_lenient(r#"{"a": [NaN, -Infinity, Infinity, 1], "s": "NaN \" Infinity"}"#).unwrap();
        assert_eq!(v, json!({"a": [NON_FINITE, NON_FINITE, NON_FINITE, 1], "s": "NaN \" Infinity"}));
        assert!(loads_lenient("{nope").is_none());
    }

    #[test]
    fn matches_python_dumps() {
        let v = json!({"a": 1, "b": [1.0, 2.5, true, null], "c": "é\n\"x\"", "d": {}, "e": []});
        let e = format!("{}u00e9", '\\');
        assert_eq!(dumps(&v), format!(r#"{{"a": 1, "b": [1.0, 2.5, true, null], "c": "{}\n\"x\"", "d": {{}}, "e": []}}"#, e));
        assert_eq!(dumps_with(&v, COMPACT), format!(r#"{{"a":1,"b":[1.0,2.5,true,null],"c":"{}\n\"x\"","d":{{}},"e":[]}}"#, e));
        assert_eq!(dumps_with(&json!({"a": [1, 2], "b": {}}), INDENT2), "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}");
        assert_eq!(dumps(&json!("\u{1F600}")), format!("\"{0}ud83d{0}ude00\"", '\\'));
    }
}
