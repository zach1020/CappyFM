use std::sync::Arc;

use cappy_core::{
    command::CommandName,
    resolver::{CandidateMetadata, MusicProvider, candidate_score, classify_input, normalize},
    settings::Settings,
};
use lavalink_rs::{
    model::{
        events,
        track::{TrackData, TrackLoadData},
    },
    prelude::{LavalinkClient, NodeBuilder, NodeDistributionStrategy, TrackInQueue},
};
use serenity::all::{ChannelId, Context, GuildId, Http, Message};
use sqlx::SqlitePool;
use thiserror::Error;

const MAX_ARGUMENT_LENGTH: usize = 500;
const MAX_PLAYLIST_ITEMS: usize = 100;
const MAX_QUEUE_ITEMS: usize = 500;
const QUEUE_DISPLAY_ITEMS: usize = 10;

#[derive(Clone)]
pub struct PlaybackService {
    lavalink: LavalinkClient,
    database: SqlitePool,
}

#[derive(Debug, Error)]
pub enum PlaybackError {
    #[error("the requester is not connected to voice")]
    NotInVoice,
    #[error("the command needs an argument")]
    MissingQuery,
    #[error("the command argument is too long")]
    ArgumentTooLong,
    #[error("the supplied URL is not a supported music URL")]
    UnsupportedUrl,
    #[error("the metadata provider is not configured or unavailable")]
    ProviderUnavailable,
    #[error("no confident playable match was found")]
    LowConfidence,
    #[error("no playable track was found")]
    NoTracks,
    #[error("livestreams are not supported in the MVP")]
    Livestream,
    #[error("the queue is full")]
    QueueFull,
    #[error("voice connection failed: {0}")]
    Voice(String),
    #[error("audio node request failed: {0}")]
    Lavalink(String),
    #[error("music metadata persistence failed: {0}")]
    Data(String),
}

impl PlaybackError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::NotInVoice => "requester_not_in_voice",
            Self::MissingQuery => "missing_query",
            Self::ArgumentTooLong => "argument_too_long",
            Self::UnsupportedUrl => "unsupported_url",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::LowConfidence => "low_match_confidence",
            Self::NoTracks => "no_tracks",
            Self::Livestream => "livestream_rejected",
            Self::QueueFull => "queue_full",
            Self::Voice(_) => "voice_connection",
            Self::Lavalink(_) => "lavalink",
            Self::Data(_) => "data",
        }
    }

    pub fn user_message(&self) -> &'static str {
        match self {
            Self::NotInVoice => {
                "You need to be in a voice channel before handing the capybara the aux."
            }
            Self::MissingQuery => {
                "Give me a music URL or search, like `cap!play Burial Archangel`."
            }
            Self::ArgumentTooLong => "That request is too long. Keep it under 500 characters.",
            Self::UnsupportedUrl => {
                "Use a YouTube, SoundCloud, Spotify, or Apple Music URL—or a plain-text search."
            }
            Self::ProviderUnavailable => {
                "I recognize that link, but its metadata provider isn't configured or available. Check the provider credentials in `.env`."
            }
            Self::LowConfidence => {
                "I found the track metadata, but I couldn't locate a confident playable match. Try a YouTube or SoundCloud link."
            }
            Self::NoTracks => "I couldn't find a playable track for that request.",
            Self::Livestream => "Livestreams are staying out of the pool for the MVP.",
            Self::QueueFull => "The queue is full. The capybara only has so many paws.",
            Self::Voice(_) => {
                "I couldn't join that voice channel. Check my Connect and Speak permissions."
            }
            Self::Lavalink(_) => "The audio node took an unscheduled swim. Try again in a moment.",
            Self::Data(_) => {
                "The music is ready, but I couldn't save its history. Try again in a moment."
            }
        }
    }
}

impl PlaybackService {
    pub async fn connect(settings: &Settings, database: SqlitePool) -> Self {
        let node = NodeBuilder {
            hostname: format!("{}:{}", settings.lavalink.host, settings.lavalink.port),
            is_ssl: false,
            events: events::Events::default(),
            password: settings.lavalink.password.clone(),
            user_id: settings.discord.application_id.into(),
            session_id: None,
        };
        let lavalink = LavalinkClient::new(
            events::Events::default(),
            vec![node],
            NodeDistributionStrategy::round_robin(),
        )
        .await;
        Self { lavalink, database }
    }

    pub async fn handle(
        &self,
        context: &Context,
        message: &Message,
        command: CommandName,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        let guild_id = message.guild_id.expect("guild command checked by router");
        match command {
            CommandName::Play => self.play(context, message, guild_id, arguments).await,
            CommandName::Queue => self.queue(context, message, guild_id).await,
            CommandName::Skip => self.skip(context, message, guild_id).await,
            CommandName::Stop => self.stop(context, message, guild_id).await,
            CommandName::Now => self.now(context, message, guild_id).await,
            CommandName::Pause => self.pause(context, message, guild_id, true).await,
            CommandName::Resume => self.pause(context, message, guild_id, false).await,
            CommandName::Leave => self.leave(context, message, guild_id).await,
            _ => Ok(()),
        }
    }

    async fn ensure_joined(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let voice_channel = requester_voice_channel(context, message, guild_id)?;
        if self.lavalink.get_player_context(guild_id).is_some() {
            return Ok(());
        }
        let manager = songbird::get(context)
            .await
            .ok_or_else(|| PlaybackError::Voice("Songbird is not registered".to_owned()))?
            .clone();
        let (connection_info, _) = manager
            .join_gateway(guild_id, voice_channel)
            .await
            .map_err(|error| PlaybackError::Voice(error.to_string()))?;

        self.lavalink
            .create_player_context_with_data::<(ChannelId, Arc<Http>)>(
                guild_id,
                connection_info,
                Arc::new((message.channel_id, context.http.clone())),
            )
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        Ok(())
    }

    async fn play(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        let input = validate_input(arguments)?;
        let provider = classify_input(input).map_err(|_| PlaybackError::UnsupportedUrl)?;
        self.ensure_joined(context, message, guild_id).await?;
        let player = self
            .lavalink
            .get_player_context(guild_id)
            .ok_or_else(|| PlaybackError::Voice("player context was not created".to_owned()))?;
        let queue = player.get_queue();
        let current_count = queue
            .get_count()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        if current_count >= MAX_QUEUE_ITEMS {
            return Err(PlaybackError::QueueFull);
        }

        let query = if provider == MusicProvider::Search {
            format!("ytsearch:{input}")
        } else {
            input.to_owned()
        };
        let (metadata_tracks, playlist_name) =
            self.load_metadata(guild_id, &query, provider).await?;
        let is_playlist = playlist_name.is_some();
        let mut tracks = Vec::new();
        let mut confidences = Vec::new();
        for metadata in metadata_tracks
            .into_iter()
            .take(MAX_PLAYLIST_ITEMS)
            .take(MAX_QUEUE_ITEMS.saturating_sub(current_count))
        {
            let matched = if provider.needs_playable_match() {
                self.match_playable(guild_id, &metadata).await
            } else {
                Ok((metadata.clone(), 1.0))
            };
            let (mut playable, confidence) = match matched {
                Err(PlaybackError::LowConfidence | PlaybackError::NoTracks) if is_playlist => {
                    continue;
                }
                result => result?,
            };
            if playable.info.is_stream {
                continue;
            }
            playable.user_data = Some(serde_json::json!({
                "requester_id": message.author.id.get(),
                "original_title": metadata.info.title,
                "original_artist": metadata.info.author,
                "metadata_provider": provider.database_name(),
                "playback_source": playable.info.source_name,
                "match_confidence": confidence,
                "original_url": if provider == MusicProvider::Search { None } else { Some(input) },
            }));
            self.persist_resolution(
                &metadata,
                &playable,
                provider,
                if provider == MusicProvider::Search {
                    None
                } else {
                    Some(input)
                },
                confidence,
            )
            .await?;
            confidences.push(confidence);
            tracks.push(TrackInQueue::from(playable));
        }
        tracks.retain(|track| !track.track.info.is_stream);
        if tracks.is_empty() {
            return Err(if provider.needs_playable_match() {
                PlaybackError::LowConfidence
            } else {
                PlaybackError::Livestream
            });
        }

        let first = tracks
            .first()
            .expect("non-empty after validation")
            .track
            .clone();
        let added = tracks.len();
        queue
            .append(tracks.into())
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;

        let player_data = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        if player_data.track.is_none() {
            player
                .skip()
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }

        let average_confidence = confidences.iter().sum::<f64>() / confidences.len() as f64;
        let response = if let Some(name) = playlist_name {
            if provider.needs_playable_match() {
                format!(
                    "Added **{added} tracks** from the {} playlist `{}`. Playback source: YouTube (average match confidence {:.0}%).",
                    provider,
                    safe(&name),
                    average_confidence * 100.0
                )
            } else {
                format!(
                    "Added **{added} tracks** from the {} playlist `{}`.",
                    provider,
                    safe(&name)
                )
            }
        } else if provider.needs_playable_match() {
            let (title, artist) = display_metadata(&first);
            format!(
                "Added `{title}` by `{artist}` from your {provider} link. Playback source: YouTube (match confidence {:.0}%).",
                average_confidence * 100.0
            )
        } else {
            format!(
                "Added `{}` by `{}`. Playback source: {}.",
                safe(&first.info.title),
                safe(&first.info.author),
                source_label(&first.info.source_name)
            )
        };
        say(message, context, response).await
    }

    async fn load_metadata(
        &self,
        guild_id: GuildId,
        query: &str,
        provider: MusicProvider,
    ) -> Result<(Vec<TrackData>, Option<String>), PlaybackError> {
        let loaded = self
            .lavalink
            .load_tracks(guild_id, query)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        match loaded.data {
            Some(TrackLoadData::Track(track)) => Ok((vec![track], None)),
            Some(TrackLoadData::Search(tracks)) => tracks
                .into_iter()
                .next()
                .map(|track| (vec![track], None))
                .ok_or(PlaybackError::NoTracks),
            Some(TrackLoadData::Playlist(playlist)) => {
                Ok((playlist.tracks, Some(playlist.info.name)))
            }
            Some(TrackLoadData::Error(_)) | None if provider.needs_playable_match() => {
                Err(PlaybackError::ProviderUnavailable)
            }
            Some(TrackLoadData::Error(error)) => Err(PlaybackError::Lavalink(error.message)),
            None => Err(PlaybackError::NoTracks),
        }
    }

    async fn match_playable(
        &self,
        guild_id: GuildId,
        metadata: &TrackData,
    ) -> Result<(TrackData, f64), PlaybackError> {
        let mut candidates = Vec::new();
        if let Some(isrc) = metadata.info.isrc.as_deref() {
            candidates.extend(
                self.search_candidates(guild_id, &format!("ytsearch:{isrc}"))
                    .await?,
            );
        }
        candidates.extend(
            self.search_candidates(
                guild_id,
                &format!("ytsearch:{} {}", metadata.info.author, metadata.info.title),
            )
            .await?,
        );
        candidates.sort_by(|left, right| left.info.identifier.cmp(&right.info.identifier));
        candidates.dedup_by(|left, right| left.info.identifier == right.info.identifier);

        let target = track_metadata(metadata);
        let best = candidates
            .into_iter()
            .filter(|candidate| !candidate.info.is_stream)
            .map(|candidate| {
                let score = candidate_score(&target, &track_metadata(&candidate));
                (candidate, score)
            })
            .max_by(|(_, left), (_, right)| left.total_cmp(right));
        match best {
            Some((candidate, score)) if score >= 0.60 => Ok((candidate, score)),
            _ => Err(PlaybackError::LowConfidence),
        }
    }

    async fn search_candidates(
        &self,
        guild_id: GuildId,
        query: &str,
    ) -> Result<Vec<TrackData>, PlaybackError> {
        let loaded = self
            .lavalink
            .load_tracks(guild_id, query)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        match loaded.data {
            Some(TrackLoadData::Search(tracks)) => Ok(tracks.into_iter().take(5).collect()),
            Some(TrackLoadData::Track(track)) => Ok(vec![track]),
            _ => Ok(Vec::new()),
        }
    }

    async fn persist_resolution(
        &self,
        metadata: &TrackData,
        playable: &TrackData,
        provider: MusicProvider,
        original_url: Option<&str>,
        confidence: f64,
    ) -> Result<(), PlaybackError> {
        let track_id = metadata
            .info
            .isrc
            .as_deref()
            .map(|isrc| format!("isrc:{}", isrc.to_ascii_uppercase()))
            .unwrap_or_else(|| {
                format!(
                    "track:{}:{}",
                    normalize(&metadata.info.author),
                    normalize(&metadata.info.title)
                )
            });
        let metadata_json = serde_json::to_string(metadata)
            .map_err(|error| PlaybackError::Data(error.to_string()))?;
        sqlx::query(
            "INSERT INTO tracks (id, canonical_artist, canonical_title, duration_ms, isrc, metadata_json) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET canonical_artist=excluded.canonical_artist, \
             canonical_title=excluded.canonical_title, duration_ms=excluded.duration_ms, \
             isrc=excluded.isrc, metadata_json=excluded.metadata_json, updated_at=CURRENT_TIMESTAMP",
        )
        .bind(&track_id)
        .bind(&metadata.info.author)
        .bind(&metadata.info.title)
        .bind(metadata.info.length as i64)
        .bind(metadata.info.isrc.as_deref())
        .bind(&metadata_json)
        .execute(&self.database)
        .await
        .map_err(|error| PlaybackError::Data(error.to_string()))?;

        self.persist_source(
            &track_id,
            provider.database_name(),
            &metadata.info.identifier,
            original_url,
            if provider.needs_playable_match() {
                None
            } else {
                playable.info.uri.as_deref()
            },
            if provider.needs_playable_match() {
                None
            } else {
                Some(confidence)
            },
            &metadata_json,
        )
        .await?;
        if provider.needs_playable_match() {
            let playable_json = serde_json::to_string(playable)
                .map_err(|error| PlaybackError::Data(error.to_string()))?;
            self.persist_source(
                &track_id,
                "youtube",
                &playable.info.identifier,
                None,
                playable.info.uri.as_deref(),
                Some(confidence),
                &playable_json,
            )
            .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn persist_source(
        &self,
        track_id: &str,
        provider: &str,
        provider_track_id: &str,
        original_url: Option<&str>,
        playable_uri: Option<&str>,
        confidence: Option<f64>,
        metadata_json: &str,
    ) -> Result<(), PlaybackError> {
        let source_id = format!("{provider}:{provider_track_id}");
        sqlx::query(
            "INSERT INTO track_sources \
             (id, track_id, provider, provider_track_id, original_url, playable_uri, confidence, metadata_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET track_id=excluded.track_id, original_url=excluded.original_url, \
             playable_uri=excluded.playable_uri, confidence=excluded.confidence, metadata_json=excluded.metadata_json",
        )
        .bind(source_id)
        .bind(track_id)
        .bind(provider)
        .bind(provider_track_id)
        .bind(original_url)
        .bind(playable_uri)
        .bind(confidence)
        .bind(metadata_json)
        .execute(&self.database)
        .await
        .map_err(|error| PlaybackError::Data(error.to_string()))?;
        Ok(())
    }

    async fn queue(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "The queue is empty.".to_owned()).await;
        };
        let player_data = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        let queue = player.get_queue();
        let count = queue
            .get_count()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        let mut lines = vec![format_now_playing(&player_data)];
        let shown = count.min(QUEUE_DISPLAY_ITEMS);
        for index in 0..shown {
            if let Some(item) = queue
                .get_track(index)
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
            {
                lines.push(format!(
                    "{}. `{}` — `{}`{}",
                    index + 1,
                    display_metadata(&item.track).0,
                    display_metadata(&item.track).1,
                    requester_suffix(&item.track.user_data)
                ));
            }
        }
        if count > shown {
            lines.push(format!("…and {} more.", count - shown));
        }
        say(message, context, lines.join("\n")).await
    }

    async fn now(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        let player_data = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(message, context, format_now_playing(&player_data)).await
    }

    async fn skip(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "Nothing to skip.".to_owned()).await;
        };
        let current = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
            .track;
        let Some(current) = current else {
            return say(message, context, "Nothing to skip.".to_owned()).await;
        };
        player
            .skip()
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            format!("Skipped `{}`.", display_metadata(&current).0),
        )
        .await
    }

    async fn stop(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        player
            .get_queue()
            .clear()
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        player
            .stop_now()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            "Stopped playback and cleared the queue.".to_owned(),
        )
        .await
    }

    async fn pause(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        paused: bool,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        player
            .set_pause(paused)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            if paused {
                "Paused. The capybara is holding the needle.".to_owned()
            } else {
                "Playback resumed.".to_owned()
            },
        )
        .await
    }

    async fn leave(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        if self.lavalink.get_player_context(guild_id).is_some() {
            self.lavalink
                .delete_player(guild_id)
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }
        let manager = songbird::get(context)
            .await
            .ok_or_else(|| PlaybackError::Voice("Songbird is not registered".to_owned()))?
            .clone();
        if manager.get(guild_id).is_some() {
            manager
                .remove(guild_id)
                .await
                .map_err(|error| PlaybackError::Voice(error.to_string()))?;
        }
        say(
            message,
            context,
            "Left voice. The aux is now unattended.".to_owned(),
        )
        .await
    }
}

fn requester_voice_channel(
    context: &Context,
    message: &Message,
    guild_id: GuildId,
) -> Result<ChannelId, PlaybackError> {
    context
        .cache
        .guild(guild_id)
        .and_then(|guild| {
            guild
                .voice_states
                .get(&message.author.id)
                .and_then(|state| state.channel_id)
        })
        .ok_or(PlaybackError::NotInVoice)
}

fn validate_input(input: &str) -> Result<&str, PlaybackError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(PlaybackError::MissingQuery);
    }
    if input.chars().count() > MAX_ARGUMENT_LENGTH {
        return Err(PlaybackError::ArgumentTooLong);
    }
    classify_input(input).map_err(|_| PlaybackError::UnsupportedUrl)?;
    Ok(input)
}

fn format_now_playing(player: &lavalink_rs::model::player::Player) -> String {
    let Some(track) = &player.track else {
        return "Nothing is playing.".to_owned();
    };
    let position = format_duration(player.state.position);
    let duration = format_duration(track.info.length);
    let (title, artist) = display_metadata(track);
    let source = playback_source_suffix(&track.user_data);
    format!(
        "Now playing: `{title}` — `{artist}` [{position}/{duration}]{source}{}",
        requester_suffix(&track.user_data)
    )
}

fn track_metadata(track: &TrackData) -> CandidateMetadata<'_> {
    let album = track
        .plugin_info
        .as_ref()
        .and_then(|value| value.get("albumName").or_else(|| value.get("album")))
        .and_then(serde_json::Value::as_str);
    CandidateMetadata {
        title: &track.info.title,
        artist: &track.info.author,
        album,
        duration_ms: Some(track.info.length),
        isrc: track.info.isrc.as_deref(),
    }
}

fn display_metadata(track: &TrackData) -> (String, String) {
    let original = track.user_data.as_ref();
    let title = original
        .and_then(|value| value.get("original_title"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&track.info.title);
    let artist = original
        .and_then(|value| value.get("original_artist"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&track.info.author);
    (safe(title), safe(artist))
}

fn playback_source_suffix(data: &Option<serde_json::Value>) -> String {
    let Some(data) = data else {
        return String::new();
    };
    let metadata = data
        .get("metadata_provider")
        .and_then(serde_json::Value::as_str);
    let playback = data
        .get("playback_source")
        .and_then(serde_json::Value::as_str);
    match (metadata, playback) {
        (Some(metadata), Some(playback)) if metadata != playback => {
            format!(
                " · {} link → {} playback",
                provider_label(metadata),
                source_label(playback)
            )
        }
        _ => String::new(),
    }
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        "spotify" => "Spotify",
        "apple_music" => "Apple Music",
        "soundcloud" => "SoundCloud",
        _ => "YouTube",
    }
}

fn source_label(source: &str) -> &'static str {
    match source {
        "soundcloud" => "SoundCloud",
        _ => "YouTube",
    }
}

fn requester_suffix(data: &Option<serde_json::Value>) -> String {
    data.as_ref()
        .and_then(|value| value.get("requester_id"))
        .and_then(serde_json::Value::as_u64)
        .map(|id| format!(" · requested by <@{id}>"))
        .unwrap_or_default()
}

fn format_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn safe(value: &str) -> String {
    value
        .replace('`', "'")
        .replace('@', "@\u{200b}")
        .replace(['\r', '\n'], " ")
}

async fn say(message: &Message, context: &Context, response: String) -> Result<(), PlaybackError> {
    message
        .channel_id
        .say(&context.http, response)
        .await
        .map_err(|error| PlaybackError::Voice(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_search_is_valid() {
        assert_eq!(
            validate_input("Burial Archangel").unwrap(),
            "Burial Archangel"
        );
    }

    #[test]
    fn youtube_urls_are_allowed() {
        let url = "https://www.youtube.com/watch?v=abcdef";
        assert_eq!(validate_input(url).unwrap(), url);
        assert!(validate_input("https://youtu.be/abcdef").is_ok());
        assert!(validate_input("https://music.youtube.com/watch?v=abcdef").is_ok());
        assert!(validate_input("https://soundcloud.com/artist/track").is_ok());
        assert!(validate_input("https://open.spotify.com/track/abcdef").is_ok());
    }

    #[test]
    fn arbitrary_and_private_urls_are_rejected() {
        assert!(matches!(
            validate_input("http://127.0.0.1/secret"),
            Err(PlaybackError::UnsupportedUrl)
        ));
        assert!(matches!(
            validate_input("https://example.com/audio.mp3"),
            Err(PlaybackError::UnsupportedUrl)
        ));
    }

    #[test]
    fn empty_and_oversized_queries_are_rejected() {
        assert!(matches!(
            validate_input(""),
            Err(PlaybackError::MissingQuery)
        ));
        assert!(matches!(
            validate_input(&"a".repeat(501)),
            Err(PlaybackError::ArgumentTooLong)
        ));
    }

    #[test]
    fn metadata_is_made_safe_for_discord() {
        assert_eq!(
            safe("`track` by @everyone\nnow"),
            "'track' by @\u{200b}everyone now"
        );
    }
}
