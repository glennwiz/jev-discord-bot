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
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("discord_token", &"<redacted>")
            .field("guild_id", &self.guild_id)
            .field("api_key", &"<redacted>")
            .field("jev_base_url", &self.jev_base_url)
            .field("jev_timeout", &self.jev_timeout)
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
