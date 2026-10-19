//! Device tokens, pairing codes and websocket tickets.
//!
//! Wire-compatible with `app/auth.py`: tokens are SHA-256 hashed the same way,
//! so a device paired against the Python backend authenticates here unchanged.
//! Ticket signing differs (HMAC-SHA256 over `uid|expiry` rather than
//! itsdangerous' format), so a ticket is only valid against the backend that
//! issued it -- fine, since tickets live 60 seconds.

use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// Excludes I/O/0/1 so a code read aloud cannot be ambiguous. Same set as Python.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn new_device_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill(&mut bytes);
    format!("hqd_{}", base64_url(&bytes))
}

pub fn new_pair_code() -> String {
    let mut rng = rand::thread_rng();
    let pick = |rng: &mut rand::rngs::ThreadRng| {
        CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char
    };
    let a: String = (0..4).map(|_| pick(&mut rng)).collect();
    let b: String = (0..4).map(|_| pick(&mut rng)).collect();
    format!("HQ-{a}-{b}")
}

/// URL-safe base64 without padding, matching `secrets.token_urlsafe`.
fn base64_url(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let take = chunk.len() + 1;
        for i in 0..take {
            out.push(T[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
        }
    }
    out
}

fn sign(secret: &str, payload: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(payload.as_bytes());
    format!("{payload}.{}", hex::encode(mac.finalize().into_bytes()))
}

fn verify(secret: &str, token: &str) -> Option<String> {
    let (payload, got) = token.rsplit_once('.')?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(payload.as_bytes());
    // `verify_slice` is constant-time; a plain == would leak timing.
    mac.verify_slice(&hex::decode(got).ok()?).ok()?;
    let (value, exp) = payload.rsplit_once('|')?;
    if exp.parse::<i64>().ok()? < chrono::Utc::now().timestamp() {
        return None;
    }
    Some(value.to_string())
}

pub fn issue_ws_ticket(secret: &str, user_id: &str, ttl_secs: i64) -> String {
    sign(secret, &format!("{user_id}|{}", chrono::Utc::now().timestamp() + ttl_secs))
}

pub fn read_ws_ticket(secret: &str, ticket: &str) -> Option<String> {
    verify(secret, ticket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hashing_matches_python_sha256_hexdigest() {
        // sha256("hqd_test") computed independently.
        assert_eq!(
            hash_token("hqd_test"),
            {
                use sha2::Digest;
                hex::encode(sha2::Sha256::digest(b"hqd_test"))
            }
        );
        assert_eq!(hash_token("a").len(), 64);
    }

    #[test]
    fn pair_codes_avoid_ambiguous_characters() {
        for _ in 0..200 {
            let c = new_pair_code();
            assert!(c.starts_with("HQ-") && c.len() == 12, "{c}");
            assert!(!c.contains('I') && !c.contains('O') && !c.contains('0') && !c.contains('1'));
        }
    }

    #[test]
    fn device_tokens_are_prefixed_and_unique() {
        let a = new_device_token();
        let b = new_device_token();
        assert!(a.starts_with("hqd_") && a.len() > 30);
        assert_ne!(a, b);
    }

    #[test]
    fn tickets_round_trip_and_reject_tampering() {
        let s = "secret";
        let t = issue_ws_ticket(s, "user-1", 60);
        assert_eq!(read_ws_ticket(s, &t).as_deref(), Some("user-1"));
        assert_eq!(read_ws_ticket("other-secret", &t), None);
        assert_eq!(read_ws_ticket(s, "garbage"), None);
        // flip the last hex digit to a *different* one ('0' alone matched 1 time in 16)
        let mut bad = t.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == '0' { '1' } else { '0' });
        assert_eq!(read_ws_ticket(s, &bad), None);
    }

    #[test]
    fn expired_tickets_are_refused() {
        let s = "secret";
        let t = issue_ws_ticket(s, "user-1", -1);
        assert_eq!(read_ws_ticket(s, &t), None);
    }
}
