use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub discord: DiscordSettings,
    pub database: DatabaseSettings,
    pub prefixes: PrefixSettings,
    pub privacy: PrivacySettings,
    pub lavalink: LavalinkSettings,
    #[serde(default = "default_environment")]
    pub environment: String,
    #[serde(default)]
    pub log_search_queries: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiscordSettings {
    pub token: String,
    pub application_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DatabaseSettings {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrefixSettings {
    pub values: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrivacySettings {
    pub ignore_non_commands: bool,
    pub store_command_bodies: bool,
    pub store_search_queries: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LavalinkSettings {
    pub host: String,
    pub port: u16,
    pub password: String,
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("could not load configuration: {0}")]
    Load(#[from] config::ConfigError),
    #[error("privacy invariant violated: {0}")]
    PrivacyInvariant(&'static str),
    #[error("at least one non-empty command prefix is required")]
    MissingPrefix,
}

impl Settings {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SettingsError> {
        let settings: Self = config::Config::builder()
            .add_source(config::File::from(path.as_ref()).required(false))
            .add_source(
                config::Environment::default()
                    .separator("__")
                    .try_parsing(true),
            )
            .set_override_option("discord.token", std::env::var("DISCORD_TOKEN").ok())?
            .set_override_option(
                "discord.application_id",
                std::env::var("DISCORD_APPLICATION_ID").ok(),
            )?
            .set_override_option("database.url", std::env::var("DATABASE_URL").ok())?
            .set_override_option("lavalink.host", std::env::var("LAVALINK_HOST").ok())?
            .set_override_option("lavalink.port", std::env::var("LAVALINK_PORT").ok())?
            .set_override_option("lavalink.password", std::env::var("LAVALINK_PASSWORD").ok())?
            .set_override_option("environment", std::env::var("CAPPY_ENV").ok())?
            .set_override_option(
                "log_search_queries",
                std::env::var("CAPPY_LOG_SEARCH_QUERIES").ok(),
            )?
            .build()?
            .try_deserialize()?;

        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> Result<(), SettingsError> {
        if !self.privacy.ignore_non_commands {
            return Err(SettingsError::PrivacyInvariant(
                "ignore_non_commands must remain enabled",
            ));
        }
        if self.privacy.store_command_bodies {
            return Err(SettingsError::PrivacyInvariant(
                "store_command_bodies must remain disabled",
            ));
        }
        if self.environment.eq_ignore_ascii_case("production")
            && (self.log_search_queries || self.privacy.store_search_queries)
        {
            return Err(SettingsError::PrivacyInvariant(
                "search query logging must be disabled in production",
            ));
        }
        if self.prefixes.values.is_empty()
            || self.prefixes.values.iter().any(|prefix| prefix.is_empty())
        {
            return Err(SettingsError::MissingPrefix);
        }
        Ok(())
    }
}

fn default_environment() -> String {
    "development".to_owned()
}
