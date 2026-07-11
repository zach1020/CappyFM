mod playback;

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result};
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode, header},
    routing::get,
};
use cappy_core::{
    command::{CommandName, HELP_RESPONSE, PRIVACY_RESPONSE, PrefixParser},
    settings::Settings,
};
use cappy_dj::{AudioCache, DjService};
use serenity::{
    Client,
    all::{Context, CreateAttachment, EditProfile, EventHandler, GatewayIntents, Message, Ready},
    async_trait,
};
use songbird::SerenityInit;
use sqlx::sqlite::SqlitePoolOptions;
use tokio::sync::Mutex;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

struct Handler {
    parser: Arc<PrefixParser>,
    playback: playback::PlaybackService,
    guild_locks: Mutex<HashMap<serenity::all::GuildId, Arc<Mutex<()>>>>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, context: Context, message: Message) {
        // Privacy boundary: reject bots and DMs before parsing or logging content.
        if message.author.bot || message.guild_id.is_none() {
            return;
        }

        // Do not move logging above this gate. Ordinary chat must leave no content trace.
        let Some(command) = self.parser.parse(&message.content) else {
            return;
        };

        info!(
            guild_id = %message.guild_id.expect("guild checked above"),
            channel_id = %message.channel_id,
            user_id = %message.author.id,
            command = %command.name,
            "command received"
        );

        let response = match command.name {
            CommandName::Help => Some(HELP_RESPONSE),
            CommandName::Privacy => Some(PRIVACY_RESPONSE),
            CommandName::Unknown => Some("I don't know that one yet. Try `cap!help`."),
            _ => None,
        };

        if let Some(response) = response
            && let Err(error) = message.channel_id.say(&context.http, response).await
        {
            warn!(error = %error, "failed to send command response");
        }

        if matches!(
            command.name,
            CommandName::Play
                | CommandName::Queue
                | CommandName::Skip
                | CommandName::Stop
                | CommandName::Clear
                | CommandName::Now
                | CommandName::Pause
                | CommandName::Resume
                | CommandName::Leave
                | CommandName::Volume
                | CommandName::Voice
                | CommandName::Personality
                | CommandName::Talk
                | CommandName::Shutup
                | CommandName::Intro
        ) {
            let guild_id = message.guild_id.expect("guild checked above");
            let guild_lock = {
                let mut locks = self.guild_locks.lock().await;
                locks
                    .entry(guild_id)
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            };
            let _guard = guild_lock.lock().await;

            if let Err(error) = self
                .playback
                .handle(&context, &message, command.name, command.arguments)
                .await
            {
                warn!(
                    guild_id = %guild_id,
                    command = %command.name,
                    error_category = error.category(),
                    "playback command failed"
                );
                if let Err(send_error) = message
                    .channel_id
                    .say(&context.http, error.user_message())
                    .await
                {
                    warn!(error = %send_error, "failed to send playback error response");
                }
            }
        }
    }

    async fn ready(&self, _context: Context, ready: Ready) {
        info!(bot_user_id = %ready.user.id, "CappyFM connected and healthy");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    let config_path = std::env::var_os("CAPPY_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config/cappyfm.toml"));
    let settings = Settings::load(config_path).context("invalid CappyFM configuration")?;

    if let Some(avatar_path) = avatar_update_path() {
        let avatar = CreateAttachment::path(&avatar_path)
            .await
            .with_context(|| format!("could not read avatar at {}", avatar_path.display()))?;
        serenity::http::Http::new(&settings.discord.token)
            .edit_profile(&EditProfile::new().avatar(&avatar))
            .await
            .context("Discord rejected the avatar update")?;
        info!(avatar_path = %avatar_path.display(), "Discord bot avatar updated");
        return Ok(());
    }

    let database = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&settings.database.url)
        .await
        .context("could not connect to SQLite")?;
    sqlx::migrate!("../../migrations")
        .run(&database)
        .await
        .context("database migration failed")?;
    info!("database is healthy and migrations are current");

    info!(
        lavalink_host = %settings.lavalink.host,
        lavalink_port = settings.lavalink.port,
        "Lavalink configured"
    );

    let audio_base = std::env::var("CAPPY_AUDIO_PUBLIC_BASE_URL").unwrap_or_else(|_| {
        if settings.lavalink.host == "lavalink" {
            "http://bot:8080".to_owned()
        } else {
            "http://127.0.0.1:8080".to_owned()
        }
    });
    let dj = DjService::from_env(audio_base);
    start_audio_server(dj.audio_cache()).await?;
    let playback = playback::PlaybackService::connect(&settings, database.clone(), dj).await;

    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::GUILD_VOICE_STATES
        | GatewayIntents::MESSAGE_CONTENT;
    let handler = Handler {
        parser: Arc::new(PrefixParser::new(settings.prefixes.values)),
        playback,
        guild_locks: Mutex::new(HashMap::new()),
    };
    let mut client = Client::builder(&settings.discord.token, intents)
        .application_id(settings.discord.application_id.into())
        .event_handler(handler)
        .register_songbird()
        .await
        .context("could not construct Discord client")?;

    if let Err(error) = client.start().await {
        error!(error = %error, "Discord client stopped");
        return Err(error.into());
    }
    Ok(())
}

async fn start_audio_server(cache: AudioCache) -> Result<()> {
    let app = Router::new()
        .route("/audio/{key}", get(cached_audio))
        .with_state(cache);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .context("could not bind internal DJ audio server")?;
    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            error!(error = %error, "DJ audio server stopped");
        }
    });
    Ok(())
}

async fn cached_audio(State(cache): State<AudioCache>, Path(path): Path<String>) -> Response<Body> {
    let key = path.strip_suffix(".mp3").unwrap_or(&path);
    match cache.get(key).await {
        Some(audio) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, audio.content_type)
            .header(header::CACHE_CONTROL, "private, max-age=86400")
            .body(Body::from(audio.bytes))
            .expect("static audio response"),
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .expect("static not-found response"),
    }
}

fn avatar_update_path() -> Option<PathBuf> {
    let mut arguments = std::env::args_os().skip(1);
    match arguments.next()?.to_str() {
        Some("--set-avatar") => arguments.next().map(PathBuf::from),
        _ => None,
    }
}
