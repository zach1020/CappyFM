mod playback;
mod radio;
mod spotify;

use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

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
    rate_limits: Mutex<HashMap<(serenity::all::GuildId, serenity::all::UserId), VecDeque<Instant>>>,
    expensive_limits: Mutex<HashMap<(serenity::all::GuildId, serenity::all::UserId), Instant>>,
}

static STARTED_AT: OnceLock<Instant> = OnceLock::new();
static COMMANDS_TOTAL: AtomicU64 = AtomicU64::new(0);
static COMMAND_ERRORS_TOTAL: AtomicU64 = AtomicU64::new(0);
static RATE_LIMITED_TOTAL: AtomicU64 = AtomicU64::new(0);

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
        let guild_id = message.guild_id.expect("guild checked above");
        if self
            .is_rate_limited(guild_id, message.author.id, command.name)
            .await
        {
            RATE_LIMITED_TOTAL.fetch_add(1, Ordering::Relaxed);
            let _ = message
                .channel_id
                .say(
                    &context.http,
                    "Easy, speed racer—the capybara needs a few seconds between command bursts.",
                )
                .await;
            return;
        }
        COMMANDS_TOTAL.fetch_add(1, Ordering::Relaxed);

        info!(
            guild_id = %guild_id,
            channel_id = %message.channel_id,
            user_id = %message.author.id,
            command = %command.name,
            "command received"
        );

        let response = match command.name {
            CommandName::Help => Some(help_response(command.arguments)),
            CommandName::Privacy => Some(PRIVACY_RESPONSE.to_owned()),
            CommandName::Unknown => Some("I don't know that one yet. Try `cap!help`.".to_owned()),
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
                | CommandName::Remove
                | CommandName::Move
                | CommandName::Shuffle
                | CommandName::Undo
                | CommandName::Requested
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
                | CommandName::Radio
                | CommandName::Session
                | CommandName::Vibe
                | CommandName::Surprise
                | CommandName::Crate
                | CommandName::Similar
                | CommandName::Why
                | CommandName::Fact
                | CommandName::Like
                | CommandName::Dislike
                | CommandName::Favorites
                | CommandName::History
                | CommandName::Stats
                | CommandName::Memory
                | CommandName::Settings
                | CommandName::Health
        ) {
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
                COMMAND_ERRORS_TOTAL.fetch_add(1, Ordering::Relaxed);
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

fn help_response(topic: &str) -> String {
    match topic.trim().to_ascii_lowercase().as_str() {
        "radio" => "**Radio help**\n`cap!radio [vibe]` starts or replaces the server station. `cap!radio off` disables it and removes queued radio items.\n`cap!vibe`, `cap!surprise`, `cap!crate`, `cap!similar`, `cap!why`, and `cap!fact` steer or explain discovery.\n`cap!like`, `cap!dislike`, `cap!favorites`, `cap!history`, and `cap!stats` manage music-only taste signals.".to_owned(),
        "dj" => "**DJ help**\n`cap!voice list`, `cap!voice <preset>`, `cap!voice preview <preset>`\n`cap!personality <chill|quirky|unhinged|roast>`\n`cap!talk <off|on|less|normal|more>`, `cap!shutup`, `cap!intro`\nRoast mode playfully roasts every song. Generated voices are AI-generated. Automatic segments never run back-to-back and fail open to music.".to_owned(),
        "admin" => "**Admin help**\nManage Server permission is required for changes.\n`cap!settings` shows defaults.\n`cap!settings vibe <text>`\n`cap!settings voice <preset>`\n`cap!settings personality <level>`\n`cap!settings talk <frequency>`\n`cap!settings channel <here|off>`".to_owned(),
        "queue" | "playback" => "**Queue help**\n`cap!play <URL or search>`, `cap!queue`, `cap!requested`, `cap!now`\n`cap!remove <position>`, `cap!move <from> <to>`, `cap!shuffle`, `cap!undo`, `cap!clear`\nDJ intros stay attached to their songs during queue edits.".to_owned(),
        _ => HELP_RESPONSE.to_owned(),
    }
}

impl Handler {
    async fn is_rate_limited(
        &self,
        guild_id: serenity::all::GuildId,
        user_id: serenity::all::UserId,
        command: CommandName,
    ) -> bool {
        if matches!(
            command,
            CommandName::Help | CommandName::Privacy | CommandName::Health
        ) {
            return false;
        }
        let key = (guild_id, user_id);
        let now = Instant::now();
        let mut limits = self.rate_limits.lock().await;
        let window = limits.entry(key).or_default();
        while window
            .front()
            .is_some_and(|time| now.duration_since(*time) >= Duration::from_secs(10))
        {
            window.pop_front();
        }
        if window.len() >= 5 {
            return true;
        }
        window.push_back(now);
        drop(limits);

        if matches!(
            command,
            CommandName::Play
                | CommandName::Radio
                | CommandName::Session
                | CommandName::Surprise
                | CommandName::Crate
                | CommandName::Similar
                | CommandName::Intro
                | CommandName::Voice
        ) {
            let mut expensive = self.expensive_limits.lock().await;
            if expensive
                .get(&key)
                .is_some_and(|last| now.duration_since(*last) < Duration::from_secs(5))
            {
                return true;
            }
            expensive.insert(key, now);
        }
        false
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = STARTED_AT.set(Instant::now());
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .json()
        .init();

    if std::env::args().any(|argument| argument == "--spotify-login") {
        spotify::run_login_flow().await?;
        return Ok(());
    }

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
        rate_limits: Mutex::new(HashMap::new()),
        expensive_limits: Mutex::new(HashMap::new()),
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
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
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

async fn healthz() -> &'static str {
    "ok\n"
}

async fn metrics() -> String {
    let uptime = STARTED_AT.get().map(Instant::elapsed).unwrap_or_default();
    format!(
        "cappyfm_uptime_seconds {}\ncappyfm_commands_total {}\ncappyfm_command_errors_total {}\ncappyfm_rate_limited_total {}\n",
        uptime.as_secs(),
        COMMANDS_TOTAL.load(Ordering::Relaxed),
        COMMAND_ERRORS_TOTAL.load(Ordering::Relaxed),
        RATE_LIMITED_TOTAL.load(Ordering::Relaxed),
    )
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
