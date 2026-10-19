//! Settings from the environment, ported from `app/config.py`.

#[derive(Debug, Clone)]
pub struct Settings {
    pub database_url: String,
    pub secret_key: String,
    pub github_client_id: String,
    pub github_client_secret: String,
    pub public_base_url: String,
    pub bind: String,
    pub max_daily_prompts: i64,
    pub max_daily_tools: i64,
    pub max_backfill_days: i64,
    pub ws_ticket_ttl_secs: i64,
    pub pair_code_ttl_secs: i64,
    /// Arena moderators (map gallery hide/delete), by GitHub handle, from the
    /// comma-separated ARENA_ADMIN_HANDLES. Empty unless set.
    pub admin_handles: Vec<String>,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_num(key: &str, default: i64) -> i64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Settings {
    pub fn from_env() -> Self {
        Self {
            // Same four-slash absolute form the Python uses; sqlx wants a path,
            // so the scheme prefix is stripped in db.rs.
            database_url: env_or("ARENA_DATABASE_URL", "sqlite:///./arena.db"),
            secret_key: env_or("ARENA_SECRET_KEY", "dev-only-insecure-change-me"),
            github_client_id: env_or("ARENA_GITHUB_CLIENT_ID", ""),
            github_client_secret: env_or("ARENA_GITHUB_CLIENT_SECRET", ""),
            public_base_url: env_or("ARENA_PUBLIC_BASE_URL", "http://127.0.0.1:8081"),
            bind: env_or("ARENA_BIND_HOST", "0.0.0.0"),
            max_daily_prompts: env_num("ARENA_MAX_DAILY_PROMPTS", 5_000),
            max_daily_tools: env_num("ARENA_MAX_DAILY_TOOLS", 50_000),
            max_backfill_days: env_num("ARENA_MAX_BACKFILL_DAYS", 400),
            ws_ticket_ttl_secs: env_num("ARENA_WS_TICKET_TTL_SECS", 60),
            pair_code_ttl_secs: env_num("ARENA_PAIR_CODE_TTL_SECS", 900),
            admin_handles: parse_handles(&env_or("ARENA_ADMIN_HANDLES", "")),
        }
    }
}

/// "alice, Bob ,,carol" -> ["alice", "bob", "carol"] (lowercased, blanks dropped).
pub fn parse_handles(raw: &str) -> Vec<String> {
    raw.split(',').map(|h| h.trim().to_ascii_lowercase()).filter(|h| !h.is_empty()).collect()
}

impl Settings {
    /// Is this handle an Arena moderator? Case-insensitive, as GitHub handles are.
    pub fn is_admin(&self, handle: &str) -> bool {
        let h = handle.trim().to_ascii_lowercase();
        !h.is_empty() && self.admin_handles.contains(&h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_handles_parse_and_match_case_insensitively() {
        assert_eq!(parse_handles(""), Vec::<String>::new());
        assert_eq!(parse_handles("alice, Bob ,,carol"), vec!["alice", "bob", "carol"]);
        let mut s = Settings::from_env();
        s.admin_handles = parse_handles("Alice");
        assert!(s.is_admin("alice"));
        assert!(s.is_admin("ALICE"));
        assert!(!s.is_admin("bob"));
        assert!(!s.is_admin(""));
    }
}
