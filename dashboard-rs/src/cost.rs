//! Cost model (ESTIMATE): Anthropic list prices per 1,000,000 tokens.

use crate::py::{py_int, Obj};
use serde_json::Value;

/// Ordered most-specific first: the first key found in the lowercased model id
/// wins, so "opus-5-5" must precede "opus-5".
pub const PRICES: &[(&str, (f64, f64))] = &[
    ("fable-5-1", (10.0, 50.0)),
    ("mythos-5-1", (10.0, 50.0)),
    ("fable", (10.0, 50.0)),
    ("mythos", (10.0, 50.0)),
    ("opus-5-5", (4.0, 20.0)),
    ("opus-5", (5.0, 25.0)),
    ("opus-4-8", (5.0, 25.0)),
    ("opus-4-7", (5.0, 25.0)),
    ("opus-4-6", (5.0, 25.0)),
    ("opus-4-5", (5.0, 25.0)),
    ("opus", (15.0, 75.0)),
    ("sonnet-5", (2.0, 10.0)),
    ("sonnet", (3.0, 15.0)),
    ("haiku-4", (1.0, 5.0)),
    ("haiku", (0.8, 4.0)),
];
pub const DEFAULT_PRICE: (f64, f64) = (3.0, 15.0);

const CACHE_READ_MULT: &[(&str, f64)] = &[("fable-5-1", 0.025), ("mythos-5-1", 0.025), ("opus-5-5", 0.05)];
const CACHE_WRITE_5M: f64 = 1.25;
const CACHE_WRITE_1H: f64 = 2.0;

/// (input_price, output_price) per 1e6 tokens.
pub fn price_for(model: &str) -> (f64, f64) {
    let m = model.to_lowercase();
    PRICES.iter().find(|(k, _)| m.contains(k)).map(|(_, p)| *p).unwrap_or(DEFAULT_PRICE)
}

pub fn cache_read_mult(model: &str) -> f64 {
    let m = model.to_lowercase();
    CACHE_READ_MULT.iter().find(|(k, _)| m.contains(k)).map(|(_, x)| *x).unwrap_or(0.1)
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    pub cost: f64,
    pub output: i64,
    pub input: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
}

fn num(d: &Obj, k: &str) -> i64 {
    match d.get(k) {
        Some(v) if crate::py::truthy(v) => py_int(v).unwrap_or(0).max(0),
        _ => 0,
    }
}

/// One record's cost and token counts. Cache writes are priced by TTL when
/// the usage carries the cache_creation breakdown.
pub fn usage_cost(model: &str, usage: Option<&Value>) -> Usage {
    let Some(Value::Object(u)) = usage else { return Usage::default() };
    let (in_price, out_price) = price_for(model);
    let it = num(u, "input_tokens");
    let ot = num(u, "output_tokens");
    let cr = num(u, "cache_read_input_tokens");
    let mut cc = num(u, "cache_creation_input_tokens");
    let (mut c5, mut c1h) = (0, 0);
    if let Some(Value::Object(brk)) = u.get("cache_creation") {
        c5 = num(brk, "ephemeral_5m_input_tokens");
        c1h = num(brk, "ephemeral_1h_input_tokens");
    }
    cc = cc.max(c5 + c1h);
    let rest = cc - c5 - c1h;
    let cost = (it as f64 * in_price
        + cr as f64 * in_price * cache_read_mult(model)
        + (c5 + rest) as f64 * in_price * CACHE_WRITE_5M
        + c1h as f64 * in_price * CACHE_WRITE_1H
        + ot as f64 * out_price)
        / 1_000_000.0;
    Usage { cost, output: ot, input: it, cache_read: cr, cache_creation: cc }
}

/// Normalize a model id to a short family label for breakdowns.
pub fn model_family(model: &str) -> String {
    let m = model.to_lowercase();
    if m.is_empty() {
        "unknown".into()
    } else if m.contains("opus") {
        "Opus".into()
    } else if m.contains("sonnet") {
        "Sonnet".into()
    } else if m.contains("haiku") {
        "Haiku".into()
    } else if m.contains("fable") {
        "Fable".into()
    } else {
        model.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn prices_match_by_substring() {
        assert_eq!(price_for("claude-opus-4-8"), (5.0, 25.0));
        assert_eq!(price_for("claude-sonnet-5"), (2.0, 10.0));
        assert_eq!(price_for("claude-haiku-4-5"), (1.0, 5.0));
        assert_eq!(price_for("some-fable-thing"), (10.0, 50.0));
        assert_eq!(price_for("gpt-4o"), (3.0, 15.0));
        assert_eq!(price_for(""), (3.0, 15.0));
        assert_eq!(price_for("CLAUDE-OPUS"), (15.0, 75.0));
        assert_eq!(price_for("claude-opus-5-5"), (4.0, 20.0));
        assert_eq!(price_for("claude-opus-5"), (5.0, 25.0));
        assert_eq!(price_for("claude-opus-4-1-20250805"), (15.0, 75.0));
        assert_eq!(price_for("claude-sonnet-5-5"), (2.0, 10.0));
        assert_eq!(price_for("claude-sonnet-4-6"), (3.0, 15.0));
    }

    #[test]
    fn cache_read_multipliers() {
        assert_eq!(cache_read_mult("claude-opus-5-5"), 0.05);
        assert_eq!(cache_read_mult("claude-fable-5-1"), 0.025);
        assert_eq!(cache_read_mult("claude-opus-5"), 0.1);
        assert_eq!(cache_read_mult(""), 0.1);
    }

    #[test]
    fn usage_costs() {
        assert_eq!(usage_cost("opus", None), Usage::default());
        assert_eq!(usage_cost("opus", Some(&json!("nope"))), Usage::default());
        let u = usage_cost("opus", Some(&json!({"input_tokens": 1_000_000, "output_tokens": 1_000_000})));
        assert!(close(u.cost, 90.0));
        assert_eq!((u.output, u.input, u.cache_read, u.cache_creation), (1_000_000, 1_000_000, 0, 0));
        let u = usage_cost("sonnet", Some(&json!({"cache_read_input_tokens": 1_000_000, "cache_creation_input_tokens": 1_000_000})));
        assert!(close(u.cost, 4.05));
        assert_eq!(usage_cost("haiku", Some(&json!({}))), Usage::default());
    }

    #[test]
    fn ttl_cache_writes() {
        let u = usage_cost("claude-sonnet-4-6", Some(&json!({"cache_creation_input_tokens": 3_000_000,
            "cache_creation": {"ephemeral_5m_input_tokens": 1_000_000, "ephemeral_1h_input_tokens": 2_000_000}})));
        assert!(close(u.cost, 15.75));
        assert_eq!(u.cache_creation, 3_000_000);
        let u = usage_cost("claude-sonnet-4-6", Some(&json!({"cache_creation_input_tokens": 2_000_000,
            "cache_creation": {"ephemeral_1h_input_tokens": 1_000_000}})));
        assert!(close(u.cost, 9.75));
        let u = usage_cost("claude-sonnet-4-6", Some(&json!({"cache_creation": {"ephemeral_1h_input_tokens": 1_000_000}})));
        assert!(close(u.cost, 6.0));
        assert_eq!(u.cache_creation, 1_000_000);
        let u = usage_cost("claude-sonnet-4-6", Some(&json!({"cache_creation_input_tokens": 1_000_000, "cache_creation": "x"})));
        assert!(close(u.cost, 3.75));
        let u = usage_cost("claude-sonnet-4-6", Some(&json!({"cache_creation": {"ephemeral_1h_input_tokens": "lots"}})));
        assert_eq!(u.cost, 0.0);
        let u = usage_cost("claude-opus-5-5", Some(&json!({"cache_read_input_tokens": 1_000_000})));
        assert!(close(u.cost, 0.2));
    }
}
