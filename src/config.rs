//! Environment configuration. Secrets are never printed: `Debug` redacts them.

use std::fmt;
use std::time::Duration;

/// The TypeSafe System One API (docs.typesafe.ai/api).
pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";

pub struct Config {
    pub discord_token: String,
    /// The test guild the `/jev` command is registered in.
    pub guild_id: u64,
    /// TypeSafe API key (`apikey_...`, console.typesafe.ai/keys).
    pub api_key: String,
    pub jev_base_url: String,
    pub jev_timeout: Duration,
    /// TEST SEAM, off by default: a base URL that replaces
    /// `https://discord.com` for Discord's HTTP API (serenity's proxy
    /// support), so the e2e test can run the real binary against a fake
    /// Discord. Only loopback `http://127.0.0.1[:port]` or
    /// `http://localhost[:port]` is accepted - the bot token is sent there.
    /// Deliberately absent from .env.example and the systemd unit.
    pub discord_api_proxy: Option<String>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("discord_token", &"<redacted>")
            .field("guild_id", &self.guild_id)
            .field("api_key", &"<redacted>")
            .field("jev_base_url", &self.jev_base_url)
            .field("jev_timeout", &self.jev_timeout)
            .field("discord_api_proxy", &self.discord_api_proxy)
            .finish()
    }
}

impl Config {
    /// Read `.env` (if present) then the process environment.
    pub fn from_env() -> Result<Self, String> {
        let _ = dotenvy::dotenv();
        let timeout_secs: u64 = optional("JEV_TIMEOUT_SECS")
            .map(|v| {
                v.parse()
                    .map_err(|_| "JEV_TIMEOUT_SECS must be a whole number of seconds".to_string())
            })
            .transpose()?
            .unwrap_or(20);
        if !(1..=60).contains(&timeout_secs) {
            return Err("JEV_TIMEOUT_SECS must be between 1 and 60".into());
        }
        Ok(Config {
            discord_token: required("DISCORD_TOKEN")?,
            guild_id: required("DISCORD_GUILD_ID")?
                .parse()
                .ok()
                .filter(|id| *id != 0)
                .ok_or_else(|| "DISCORD_GUILD_ID must be a nonzero numeric guild id".to_string())?,
            api_key: required("TYPESAFE_API_KEY")?,
            jev_base_url: optional("JEV_BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            jev_timeout: Duration::from_secs(timeout_secs),
            discord_api_proxy: optional("DISCORD_API_PROXY")
                .map(|url| loopback_http(&url).map(|()| url))
                .transpose()?,
        })
    }
}

fn required(name: &str) -> Result<String, String> {
    optional(name).ok_or_else(|| format!("{name} is not set"))
}

fn optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Accept only `http://127.0.0.1` or `http://localhost`, with an optional
/// numeric port and path. Anything else - another host, https, userinfo
/// (`localhost@evil.example`), a lookalike (`127.0.0.1.evil.example`) - is
/// refused, so the test seam cannot send the bot token off the machine.
fn loopback_http(url: &str) -> Result<(), String> {
    let refuse = || {
        Err(format!(
            "DISCORD_API_PROXY must be http://127.0.0.1[:port] or http://localhost[:port] (test use only); got {url:?}"
        ))
    };
    let Some(rest) = url.strip_prefix("http://") else {
        return refuse();
    };
    let authority = rest.split('/').next().unwrap_or("");
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let port_ok = port.is_none_or(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if matches!(host, "127.0.0.1" | "localhost") && port_ok {
        Ok(())
    } else {
        refuse()
    }
}

#[cfg(test)]
mod tests {
    use super::loopback_http;

    #[test]
    fn discord_api_proxy_accepts_only_loopback_http() {
        for ok in [
            "http://127.0.0.1",
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/",
            "http://localhost:3000",
        ] {
            assert!(loopback_http(ok).is_ok(), "{ok}");
        }
        for bad in [
            "https://127.0.0.1:8080",
            "http://example.com",
            "http://127.0.0.1.evil.example",
            "http://localhost.evil.example:80",
            "http://localhost@evil.example",
            "http://evil.example#@127.0.0.1",
            "http://127.0.0.1:80@evil.example",
            "http://127.0.0.1:",
            "http://127.0.0.2",
            "127.0.0.1:8080",
            "",
        ] {
            assert!(loopback_http(bad).is_err(), "{bad}");
        }
    }
}
