use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::Context;

const DEV_IP_HASH_SECRET: &str = "dev-only-insecure-secret-change-me";

/// Runtime configuration, read from environment variables (12-factor style).
/// See `.env.example` for documentation of every variable.
#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: SocketAddr,
    pub content_dir: PathBuf,
    pub static_dir: PathBuf,
    /// Key for hashing voter IPs. Rotating it effectively resets "who voted".
    pub ip_hash_secret: String,
    /// Number of reverse proxies in front of the app that append to
    /// `X-Forwarded-For`. 0 means "use the TCP peer address" (no proxy).
    pub trusted_proxy_hops: usize,
    /// Set the `Secure` flag on cookies. Must be true when served over HTTPS.
    pub cookie_secure: bool,
    /// Public origin of the site, used for absolute links in feeds/sitemaps.
    pub public_url: String,
    /// Base URL used for "Edit this page on GitHub" links.
    pub repo_url: String,
    pub max_votes_per_poll: i64,
    pub max_suggestions_per_poll: i64,
    pub run_background_jobs: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
        let bind_addr = env_or("BIND_ADDR", "0.0.0.0:3000").parse().context("BIND_ADDR must look like 0.0.0.0:3000")?;
        let ip_hash_secret = env_or("IP_HASH_SECRET", DEV_IP_HASH_SECRET);
        if ip_hash_secret == DEV_IP_HASH_SECRET {
            tracing::warn!("IP_HASH_SECRET is not set; using an insecure development default");
        }

        Ok(Self {
            database_url,
            bind_addr,
            content_dir: env_or("CONTENT_DIR", "content").into(),
            static_dir: env_or("STATIC_DIR", "static").into(),
            ip_hash_secret,
            trusted_proxy_hops: env_or("TRUSTED_PROXY_HOPS", "0")
                .parse()
                .context("TRUSTED_PROXY_HOPS must be a non-negative integer")?,
            cookie_secure: env_bool("COOKIE_SECURE", false)?,
            public_url: env_or("PUBLIC_URL", "http://localhost:3000"),
            repo_url: env_or("REPO_URL", "https://github.com/testapp00001/system-design-transparent"),
            max_votes_per_poll: 3,
            max_suggestions_per_poll: 3,
            run_background_jobs: env_bool("RUN_BACKGROUND_JOBS", true)?,
        })
    }

    /// Configuration used by integration tests. Trusts one proxy hop so tests
    /// can simulate different visitors with `X-Forwarded-For`.
    pub fn for_tests() -> Self {
        Self {
            database_url: String::new(),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            content_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/content").into(),
            static_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/static").into(),
            ip_hash_secret: "test-secret".into(),
            trusted_proxy_hops: 1,
            cookie_secure: false,
            public_url: "http://localhost:3000".into(),
            repo_url: "https://github.com/example/repo".into(),
            max_votes_per_poll: 3,
            max_suggestions_per_poll: 3,
            run_background_jobs: false,
        }
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| default.to_string())
}

fn env_bool(key: &str, default: bool) -> anyhow::Result<bool> {
    match env::var(key).ok().as_deref().map(str::trim) {
        None | Some("") => Ok(default),
        Some("1" | "true" | "TRUE" | "yes") => Ok(true),
        Some("0" | "false" | "FALSE" | "no") => Ok(false),
        Some(other) => anyhow::bail!("{key} must be true or false, got {other:?}"),
    }
}
