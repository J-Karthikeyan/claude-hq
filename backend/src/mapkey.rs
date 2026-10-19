//! Content keys for user-made maps: same kind + same canonical data => same key.
use serde_json::Value;
use sha2::{Digest, Sha256};

/// "c-" + first 12 hex of sha256("<kind>:" + compact JSON of CANONICAL `data` (as returned by <game>::validate_custom)).
#[allow(dead_code)]
pub fn content_key(kind: &str, data: &Value) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b":");
    h.update(serde_json::to_string(data).unwrap_or_default().as_bytes());
    let hex = hex::encode(h.finalize());
    format!("c-{}", &hex[..12])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_input_same_key_and_the_shape() {
        let d = json!({"tiles": "FSSRSSRSSSRSSR", "scenery": "forest"});
        let a = content_key("kart", &d);
        assert_eq!(a, content_key("kart", &d.clone()));
        assert_ne!(a, content_key("plat", &d));
        assert_eq!(a.len(), 14);
        assert!(a.starts_with("c-"));
        assert!(a[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn other_kinds_never_collide() {
        let a = content_key("fps", &json!({"x": 1}));
        assert_eq!(a, content_key("fps", &json!({"x": 1})));
        assert_ne!(a, content_key("kart", &json!({"x": 1})));
        assert!(a.len() == 14 && a.starts_with("c-") && a[2..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn the_same_input_gives_the_same_key() {
        let d = json!({"tiles": "FSSRSSRSSRSR", "scenery": "forest"});
        assert_eq!(content_key("kart", &d), content_key("kart", &d.clone()));
        assert_ne!(content_key("kart", &d), content_key("plat", &d));
    }

    #[test]
    fn the_key_is_c_dash_twelve_hex() {
        let k = content_key("fps", &json!({}));
        assert_eq!(k.len(), 14);
        assert!(k.starts_with("c-"));
        assert!(k[2..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
    }
}
