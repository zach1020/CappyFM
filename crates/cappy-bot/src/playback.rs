use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use cappy_core::{
    command::CommandName,
    resolver::{
        CandidateMetadata, ContentVersion, MusicProvider, candidate_score, classify_input,
        infer_content_version, normalize, preferred_content_version, score_content_version,
    },
    settings::Settings,
};
use cappy_dj::{DjContext, DjService, PersonalityLevel, TalkFrequency, VoicePreset};
use lavalink_rs::{
    model::track::{TrackData, TrackLoadData},
    prelude::{LavalinkClient, NodeBuilder, NodeDistributionStrategy, TrackInQueue},
};
use rand::seq::SliceRandom;
use serenity::all::{
    ChannelId, Colour, Context, CreateEmbed, CreateMessage, GuildId, Http, Message,
};
use sqlx::{Row, SqlitePool};
use thiserror::Error;
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, timeout, timeout_at},
};

use crate::{
    radio::{self, RadioService},
    spotify::{self, SpotifyClient, SpotifyError},
};

const MAX_ARGUMENT_LENGTH: usize = 500;
const MAX_PLAYLIST_ITEMS: usize = 100;
const MAX_QUEUE_ITEMS: usize = 500;
const QUEUE_DISPLAY_ITEMS: usize = 10;
const DEFAULT_VOLUME: u16 = 60;

#[derive(Clone)]
pub struct PlaybackService {
    lavalink: LavalinkClient,
    database: SqlitePool,
    dj: DjService,
    radio: RadioService,
    spotify: Option<SpotifyClient>,
    last_additions: Arc<Mutex<HashMap<u64, u64>>>,
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
    #[error("Spotify playlist authorization is required")]
    SpotifyAuthorizationRequired,
    #[error("Spotify playlist request failed: {0}")]
    SpotifyPlaylist(String),
    #[error("Apple Music catalog authorization is required")]
    AppleMusicAuthorizationRequired,
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
    #[error("DJ operation failed: {0}")]
    Dj(String),
    #[error("invalid command setting")]
    InvalidSetting,
    #[error("administrator permission is required")]
    NotAdministrator,
}

impl PlaybackError {
    pub fn category(&self) -> &'static str {
        match self {
            Self::NotInVoice => "requester_not_in_voice",
            Self::MissingQuery => "missing_query",
            Self::ArgumentTooLong => "argument_too_long",
            Self::UnsupportedUrl => "unsupported_url",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::SpotifyAuthorizationRequired => "spotify_authorization_required",
            Self::SpotifyPlaylist(_) => "spotify_playlist",
            Self::AppleMusicAuthorizationRequired => "apple_music_authorization_required",
            Self::LowConfidence => "low_match_confidence",
            Self::NoTracks => "no_tracks",
            Self::Livestream => "livestream_rejected",
            Self::QueueFull => "queue_full",
            Self::Voice(_) => "voice_connection",
            Self::Lavalink(_) => "lavalink",
            Self::Data(_) => "data",
            Self::Dj(_) => "dj",
            Self::InvalidSetting => "invalid_setting",
            Self::NotAdministrator => "not_administrator",
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
            Self::SpotifyAuthorizationRequired => {
                "Spotify needs a one-time playlist login. Run `./run spotify-login` on the CappyFM computer, then try again."
            }
            Self::SpotifyPlaylist(_) => {
                "Spotify couldn't read that playlist. It must be owned by—or shared collaboratively with—the account authorized through `./run spotify-login`."
            }
            Self::AppleMusicAuthorizationRequired => {
                "Apple Music link support needs `APPLE_MUSIC_API_TOKEN` in `.env`. Add an Apple Music developer token, restart CappyFM, and try the link again."
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
            Self::Dj(_) => "The DJ booth had a small technical moment. Music will keep playing.",
            Self::InvalidSetting => {
                "That setting doesn't exist. Try `cap!help` for the available options."
            }
            Self::NotAdministrator => {
                "You need Manage Server permission to change CappyFM defaults."
            }
        }
    }
}

impl PlaybackService {
    pub async fn connect(settings: &Settings, database: SqlitePool, dj: DjService) -> Self {
        let radio = RadioService::new(database.clone(), dj.clone());
        let events = radio.install_events();
        let node = NodeBuilder {
            hostname: format!("{}:{}", settings.lavalink.host, settings.lavalink.port),
            is_ssl: false,
            events: Default::default(),
            password: settings.lavalink.password.clone(),
            user_id: settings.discord.application_id.into(),
            session_id: None,
        };
        let lavalink =
            LavalinkClient::new(events, vec![node], NodeDistributionStrategy::round_robin()).await;
        Self {
            lavalink,
            database,
            dj,
            radio,
            spotify: SpotifyClient::from_environment(),
            last_additions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn handle(
        &self,
        context: &Context,
        message: &Message,
        command: CommandName,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        let guild_id = message.guild_id.expect("guild command checked by router");
        if command != CommandName::Settings
            && let Some(channel_id) = sqlx::query_scalar::<_, String>(
                "SELECT command_channel_id FROM guild_settings WHERE guild_id=? AND command_channel_id IS NOT NULL",
            )
            .bind(guild_id.get().to_string())
            .fetch_optional(&self.database)
            .await
            .map_err(|error| PlaybackError::Data(error.to_string()))?
            && channel_id != message.channel_id.get().to_string()
        {
            return say(
                message,
                context,
                format!("CappyFM commands are configured for <#{channel_id}>."),
            )
            .await;
        }
        match command {
            CommandName::Play => self.play(context, message, guild_id, arguments).await,
            CommandName::Queue => self.queue(context, message, guild_id).await,
            CommandName::Skip => self.skip(context, message, guild_id).await,
            CommandName::Stop => self.stop(context, message, guild_id).await,
            CommandName::Clear => self.clear(context, message, guild_id).await,
            CommandName::Remove => self.remove(context, message, guild_id, arguments).await,
            CommandName::Move => self.move_track(context, message, guild_id, arguments).await,
            CommandName::Shuffle => self.shuffle(context, message, guild_id).await,
            CommandName::Undo => self.undo(context, message, guild_id).await,
            CommandName::Requested => self.requested(context, message, guild_id).await,
            CommandName::Now => self.now(context, message, guild_id).await,
            CommandName::Pause => self.pause(context, message, guild_id, true).await,
            CommandName::Resume => self.pause(context, message, guild_id, false).await,
            CommandName::Leave => self.leave(context, message, guild_id).await,
            CommandName::Volume => self.volume(context, message, guild_id, arguments).await,
            CommandName::Voice => self.voice(context, message, guild_id, arguments).await,
            CommandName::Personality => {
                self.personality(context, message, guild_id, arguments)
                    .await
            }
            CommandName::Talk => self.talk(context, message, guild_id, arguments).await,
            CommandName::Shutup => self.shutup(context, message, guild_id).await,
            CommandName::Intro => self.intro(context, message, guild_id).await,
            CommandName::Radio | CommandName::Session => {
                self.radio_command(context, message, guild_id, arguments)
                    .await
            }
            CommandName::Vibe => self.vibe(context, message, guild_id, arguments).await,
            CommandName::Surprise => self.discovery(context, message, guild_id, "surprise").await,
            CommandName::Crate => self.discovery(context, message, guild_id, "crate").await,
            CommandName::Similar => self.discovery(context, message, guild_id, "similar").await,
            CommandName::Why => self.why(context, message, guild_id).await,
            CommandName::Fact => self.fact(context, message, guild_id).await,
            CommandName::Like => self.preference(context, message, guild_id, 1).await,
            CommandName::Dislike => self.preference(context, message, guild_id, -1).await,
            CommandName::Favorites => self.favorites(context, message, guild_id).await,
            CommandName::History | CommandName::Memory => {
                self.history(context, message, guild_id).await
            }
            CommandName::Stats => self.stats(context, message, guild_id).await,
            CommandName::Settings => self.settings(context, message, guild_id, arguments).await,
            CommandName::Health => self.health(context, message, guild_id).await,
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
        if let Some(player) = self.lavalink.get_player_context(guild_id) {
            player
                .set_volume(DEFAULT_VOLUME)
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }
        if !self.dj.has_session(guild_id.get()).await && let Some(row) = sqlx::query(
            "SELECT default_voice, default_personality, default_talk_frequency FROM guild_settings WHERE guild_id=?",
        )
        .bind(guild_id.get().to_string())
        .fetch_optional(&self.database)
        .await
        .map_err(|error| PlaybackError::Data(error.to_string()))?
        {
            if let Ok(voice) = row.get::<String, _>("default_voice").parse::<VoicePreset>() {
                self.dj.set_voice(guild_id.get(), voice).await;
            }
            if let Ok(personality) = row
                .get::<String, _>("default_personality")
                .parse::<PersonalityLevel>()
            {
                self.dj.set_personality(guild_id.get(), personality).await;
            }
            if let Ok(frequency) = row
                .get::<String, _>("default_talk_frequency")
                .parse::<TalkFrequency>()
            {
                self.dj.set_frequency(guild_id.get(), frequency).await;
            }
        }
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
        let command_id = message.id.get();
        let provider = classify_input(input).map_err(|_| PlaybackError::UnsupportedUrl)?;
        if provider == MusicProvider::AppleMusic
            && !std::env::var("APPLE_MUSIC_API_TOKEN")
                .ok()
                .is_some_and(|token| !token.trim().is_empty())
        {
            return Err(PlaybackError::AppleMusicAuthorizationRequired);
        }
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
            if provider == MusicProvider::Spotify && spotify::is_playlist_url(input) {
                let spotify = self
                    .spotify
                    .as_ref()
                    .ok_or(PlaybackError::SpotifyAuthorizationRequired)?;
                let playlist = spotify
                    .load_owned_playlist(input, MAX_PLAYLIST_ITEMS)
                    .await
                    .map_err(map_spotify_error)?;
                if playlist.tracks.is_empty() {
                    return Err(PlaybackError::NoTracks);
                }
                (playlist.tracks, Some(playlist.name))
            } else {
                self.load_metadata(guild_id, &query, provider).await?
            };
        let is_playlist = playlist_name.is_some();
        let mut tracks = Vec::new();
        let mut confidences = Vec::new();
        for metadata in metadata_tracks
            .into_iter()
            .take(MAX_PLAYLIST_ITEMS)
            .take(MAX_QUEUE_ITEMS.saturating_sub(current_count))
        {
            let matched = if provider.needs_playable_match() {
                self.match_playable(guild_id, &metadata, provider).await
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
                "original_isrc": metadata.info.isrc,
                "metadata_provider": provider.database_name(),
                "playback_source": playable.info.source_name,
                "match_confidence": confidence,
                "original_url": if provider == MusicProvider::Search { None } else { Some(input) },
                "content_version": content_version_label(preferred_content_version(
                    provider,
                    &metadata.info.title,
                    track_metadata(&metadata).album,
                )),
                "add_command_id": command_id,
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
        let player_before = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        let session_opening = player_before.track.is_none() && current_count == 0;
        let mut interleaved = Vec::with_capacity(tracks.len() + (tracks.len() / 2) + 1);
        let requester_name = requester_display_name(message);
        let mut previous_track = player_before.track.as_ref().map(|track| {
            let (title, artist) = display_metadata(track);
            format!("{title} by {artist}")
        });
        for (index, music_track) in tracks.into_iter().enumerate() {
            let (title, artist) = display_metadata(&music_track.track);
            {
                let intro_result = timeout(Duration::from_secs(12), async {
                    let segment = self
                        .dj
                        .create_intro(
                            guild_id.get(),
                            DjContext {
                                title: &title,
                                artist: &artist,
                                requester: &requester_name,
                                previous_track: previous_track.as_deref(),
                                session_opening: session_opening && index == 0,
                                radio_session: false,
                                personality: self.dj.settings(guild_id.get()).await.personality,
                                skip_transition: false,
                            },
                            false,
                        )
                        .await
                        .ok()??;
                    let audio_uri = segment.audio_uri.as_ref()?;
                    let intro_track = self.load_single_track(guild_id, audio_uri).await.ok()??;
                    Some((segment, intro_track))
                })
                .await;
                match intro_result {
                    Ok(Some((segment, mut intro_track))) => {
                        intro_track.user_data = Some(serde_json::json!({
                            "dj_segment": true,
                            "script": segment.script,
                            "add_command_id": command_id,
                        }));
                        interleaved.push(TrackInQueue::from(intro_track));
                    }
                    Ok(None) => {}
                    Err(_) => {
                        tracing::warn!(guild_id = %guild_id, "DJ generation timed out for one track; playing music without blocking");
                    }
                }
            }
            previous_track = Some(format!("{title} by {artist}"));
            interleaved.push(music_track);
        }
        let mut previous_was_dj = false;
        interleaved.retain(|item| {
            let is_dj = is_dj_track(&item.track);
            let keep = !(is_dj && previous_was_dj);
            if keep {
                previous_was_dj = is_dj;
            }
            keep
        });
        let radio_insert_at = if self
            .radio
            .session(guild_id)
            .await
            .is_some_and(|session| session.enabled)
        {
            let mut first_radio = None;
            for index in 0..current_count {
                if queue
                    .get_track(index)
                    .await
                    .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
                    .is_some_and(|item| is_radio_track(&item.track))
                {
                    first_radio = if index > 0
                        && queue
                            .get_track(index - 1)
                            .await
                            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
                            .is_some_and(|item| is_dj_track(&item.track))
                    {
                        Some(index - 1)
                    } else {
                        Some(index)
                    };
                    break;
                }
            }
            first_radio
        } else {
            None
        };
        let boundary_index = radio_insert_at.unwrap_or(current_count);
        if interleaved
            .first()
            .is_some_and(|item| is_dj_track(&item.track))
        {
            let previous_is_dj = if boundary_index > 0 {
                queue
                    .get_track(boundary_index - 1)
                    .await
                    .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
                    .is_some_and(|item| is_dj_track(&item.track))
            } else {
                player_before.track.as_ref().is_some_and(is_dj_track)
            };
            if previous_is_dj {
                interleaved.remove(0);
            }
        }
        if let Some(mut index) = radio_insert_at {
            for item in interleaved {
                queue
                    .insert(index, item)
                    .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
                index += 1;
            }
        } else {
            queue
                .append(interleaved.into())
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }
        let fair_queue = fair_queue_blocks(queue_blocks(
            queue
                .get_queue()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?,
        ));
        queue
            .replace(fair_queue.into_iter().flatten().collect())
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;

        self.last_additions
            .lock()
            .await
            .insert(guild_id.get(), command_id);

        if player_before.track.is_none() {
            player
                .skip()
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }

        let average_confidence = confidences.iter().sum::<f64>() / confidences.len() as f64;
        let response = if let Some(name) = playlist_name {
            if provider.needs_playable_match() {
                format!(
                    "Added **{added} tracks** from the {} playlist `{}`. Playback source: YouTube (average match confidence {:.0}%). Explicit versions preferred.",
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
                "Added `{title}` by `{artist}` from your {provider} link. Playback source: YouTube (match confidence {:.0}%). Explicit version preferred.",
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

    async fn load_single_track(
        &self,
        guild_id: GuildId,
        uri: &str,
    ) -> Result<Option<TrackData>, PlaybackError> {
        let loaded = self.load_tracks_with_retry(guild_id, uri).await?;
        Ok(match loaded.data {
            Some(TrackLoadData::Track(track)) => Some(track),
            Some(TrackLoadData::Search(mut tracks)) => tracks.drain(..).next(),
            _ => None,
        })
    }

    async fn load_metadata(
        &self,
        guild_id: GuildId,
        query: &str,
        provider: MusicProvider,
    ) -> Result<(Vec<TrackData>, Option<String>), PlaybackError> {
        let loaded = self.load_tracks_with_retry(guild_id, query).await?;
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
        provider: MusicProvider,
    ) -> Result<(TrackData, f64), PlaybackError> {
        let mut candidates = Vec::new();
        let target = track_metadata(metadata);
        let preferred = preferred_content_version(provider, target.title, target.album);
        let version_query = match preferred {
            ContentVersion::Explicit => " explicit",
            ContentVersion::Clean => " clean",
            ContentVersion::Unknown => "",
        };
        if let Some(isrc) = metadata.info.isrc.as_deref() {
            candidates.extend(
                self.search_candidates(guild_id, &format!("ytsearch:{isrc}"))
                    .await?,
            );
            candidates.extend(
                self.search_candidates(guild_id, &format!("ytsearch:{isrc}{version_query}"))
                    .await?,
            );
        }
        candidates.extend(
            self.search_candidates(
                guild_id,
                &format!(
                    "ytsearch:{} {}{}",
                    metadata.info.author, metadata.info.title, version_query
                ),
            )
            .await?,
        );
        candidates.sort_by(|left, right| left.info.identifier.cmp(&right.info.identifier));
        candidates.dedup_by(|left, right| left.info.identifier == right.info.identifier);

        let best = candidates
            .into_iter()
            .filter(|candidate| !candidate.info.is_stream)
            .map(|candidate| {
                let metadata_score = candidate_score(&target, &track_metadata(&candidate));
                let score = score_content_version(
                    metadata_score,
                    preferred,
                    infer_content_version(&candidate.info.title),
                );
                (candidate, score)
            })
            .filter_map(|(candidate, score)| score.map(|score| (candidate, score)))
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
        let loaded = self.load_tracks_with_retry(guild_id, query).await?;
        match loaded.data {
            Some(TrackLoadData::Search(tracks)) => Ok(tracks.into_iter().take(5).collect()),
            Some(TrackLoadData::Track(track)) => Ok(vec![track]),
            _ => Ok(Vec::new()),
        }
    }

    async fn load_tracks_with_retry(
        &self,
        guild_id: GuildId,
        query: &str,
    ) -> Result<lavalink_rs::model::track::Track, PlaybackError> {
        let mut last_error = "audio node request timed out".to_owned();
        for attempt in 0..2 {
            match timeout(
                Duration::from_secs(10),
                self.lavalink.load_tracks(guild_id, query),
            )
            .await
            {
                Ok(Ok(loaded)) => return Ok(loaded),
                Ok(Err(error)) => last_error = error.to_string(),
                Err(_) => last_error = "audio node request timed out".to_owned(),
            }
            if attempt == 0 {
                sleep(Duration::from_millis(200)).await;
            }
        }
        Err(PlaybackError::Lavalink(last_error))
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
        let blocks = queue_blocks(
            player
                .get_queue()
                .get_queue()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?,
        );
        let count = blocks.len();
        let mut lines = vec![format_now_playing(&player_data)];
        let shown = count.min(QUEUE_DISPLAY_ITEMS);
        for (index, block) in blocks.iter().take(shown).enumerate() {
            let item = block_music(block);
            lines.push(format!(
                "{}. `{}` — `{}`{}{}",
                index + 1,
                display_metadata(item).0,
                display_metadata(item).1,
                requester_suffix(&item.user_data),
                if is_radio_track(item) {
                    " · radio"
                } else {
                    ""
                }
            ));
        }
        if count > shown {
            lines.push(format!("…and {} more.", count - shown));
        }
        say(message, context, lines.join("\n")).await
    }

    async fn remove(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let position = parse_queue_position(arguments)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "The queue is empty.".to_owned()).await;
        };
        let queue = player.get_queue();
        let mut blocks = queue_blocks(
            queue
                .get_queue()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?,
        );
        if position > blocks.len() {
            return Err(PlaybackError::InvalidSetting);
        }
        let removed = blocks.remove(position - 1);
        let (title, artist) = display_metadata(block_music(&removed));
        queue
            .replace(blocks.into_iter().flatten().collect())
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            format!("Removed `{title}` by `{artist}`."),
        )
        .await
    }

    async fn undo(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(command_id) = self
            .last_additions
            .lock()
            .await
            .get(&guild_id.get())
            .copied()
        else {
            return say(
                message,
                context,
                "There isn't a recent play command to undo.".to_owned(),
            )
            .await;
        };
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(
                message,
                context,
                "There aren't any songs from that command left to undo.".to_owned(),
            )
            .await;
        };
        let queue = player.get_queue();
        let blocks = queue_blocks(
            queue
                .get_queue()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?,
        );
        let (kept, removed) = remove_command_blocks(blocks, command_id);
        let removed_count = removed.len();
        queue
            .replace(kept.into_iter().flatten().collect())
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;

        let current = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
            .track;
        let stopped_current = current
            .as_ref()
            .is_some_and(|track| add_command_id(&track.user_data) == Some(command_id));
        if stopped_current {
            player
                .skip()
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }
        if removed_count == 0 && !stopped_current {
            return say(
                message,
                context,
                "There aren't any songs from that command left to undo.".to_owned(),
            )
            .await;
        }
        self.last_additions.lock().await.remove(&guild_id.get());
        let current_song_removed =
            stopped_current && current.as_ref().is_some_and(|track| !is_dj_track(track));
        let total = removed_count + usize::from(current_song_removed);
        say(
            message,
            context,
            format!("Undid the last play command and removed {total} song(s)."),
        )
        .await
    }

    async fn move_track(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let positions: Vec<_> = arguments.split_whitespace().collect();
        if positions.len() != 2 {
            return Err(PlaybackError::InvalidSetting);
        }
        let from = parse_queue_position(positions[0])?;
        let to = parse_queue_position(positions[1])?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "The queue is empty.".to_owned()).await;
        };
        let queue = player.get_queue();
        let mut blocks = queue_blocks(
            queue
                .get_queue()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?,
        );
        if from > blocks.len() || to > blocks.len() {
            return Err(PlaybackError::InvalidSetting);
        }
        let moved = blocks.remove(from - 1);
        let (title, _) = display_metadata(block_music(&moved));
        blocks.insert(to - 1, moved);
        queue
            .replace(blocks.into_iter().flatten().collect())
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            format!("Moved `{title}` to position {to}."),
        )
        .await
    }

    async fn shuffle(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "The queue is empty.".to_owned()).await;
        };
        let queue = player.get_queue();
        let blocks = queue_blocks(
            queue
                .get_queue()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?,
        );
        if blocks.len() < 2 {
            return say(
                message,
                context,
                "Not enough queued music to shuffle.".to_owned(),
            )
            .await;
        }
        let (mut requested, mut automatic): (Vec<_>, Vec<_>) = blocks
            .into_iter()
            .partition(|block| requester_id(&block_music(block).user_data).is_some());
        requested.shuffle(&mut rand::rng());
        automatic.shuffle(&mut rand::rng());
        let count = requested.len() + automatic.len();
        requested.extend(automatic);
        queue
            .replace(requested.into_iter().flatten().collect())
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            format!("Shuffled {count} tracks while keeping direct requests ahead of radio."),
        )
        .await
    }

    async fn requested(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(
                message,
                context,
                "No direct requests are queued.".to_owned(),
            )
            .await;
        };
        let blocks = queue_blocks(
            player
                .get_queue()
                .get_queue()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?,
        );
        let lines: Vec<_> = blocks
            .iter()
            .filter_map(|block| {
                let track = block_music(block);
                requester_id(&track.user_data).map(|id| {
                    let (title, artist) = display_metadata(track);
                    format!("`{title}` — `{artist}` · <@{id}>")
                })
            })
            .take(QUEUE_DISPLAY_ITEMS)
            .collect();
        say(
            message,
            context,
            if lines.is_empty() {
                "No direct requests are queued.".to_owned()
            } else {
                format!("Queued requests:\n{}", lines.join("\n"))
            },
        )
        .await
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
        let Some(track) = &player_data.track else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        let (title, artist) = display_metadata(track);
        let mut embed = CreateEmbed::new()
            .title("Now Playing")
            .description(match track.info.uri.as_deref() {
                Some(uri) => format!("[`{title}`]({uri}) — `{artist}`"),
                None => format!("`{title}` — `{artist}`"),
            })
            .colour(Colour::from_rgb(238, 211, 162))
            .field(
                "Progress",
                format!(
                    "{} / {}",
                    format_duration(player_data.state.position),
                    format_duration(track.info.length)
                ),
                true,
            )
            .field(
                "Playback source",
                source_label(&track.info.source_name),
                true,
            );
        if let Some(artwork) = track.info.artwork_url.as_deref() {
            embed = embed.thumbnail(artwork);
        }
        if let Some(id) = requester_id(&track.user_data) {
            embed = embed.field("Requested by", format!("<@{id}>"), true);
        }
        if let Some(reason) = radio::current_reason(track) {
            embed = embed.field("Radio selection", safe(reason), false);
        }
        if let Some(version) = track
            .user_data
            .as_ref()
            .and_then(|data| data.get("content_version"))
            .and_then(serde_json::Value::as_str)
        {
            embed = embed.footer(serenity::all::CreateEmbedFooter::new(format!(
                "Content version: {version}"
            )));
        }
        message
            .channel_id
            .send_message(&context.http, CreateMessage::new().embed(embed))
            .await
            .map_err(|error| PlaybackError::Voice(error.to_string()))?;
        Ok(())
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
        let queue = player.get_queue();
        if queue
            .get_track(0)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
            .is_some_and(|item| is_dj_track(&item.track))
        {
            queue
                .remove(0)
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }
        let next = queue
            .get_track(0)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        let mut spoken_skip = false;
        let mut fallback_copy = None;
        {
            let (title, artist, radio_session) = next
                .as_ref()
                .map(|next| {
                    let (title, artist) = display_metadata(&next.track);
                    (title, artist, is_radio_track(&next.track))
                })
                .unwrap_or_else(|| {
                    (
                        "a quick change of pace".to_owned(),
                        "Cappy".to_owned(),
                        false,
                    )
                });
            let (previous_title, previous_artist) = display_metadata(&current);
            let previous_track = format!("{previous_title} by {previous_artist}");
            let requester_name = requester_display_name(message);
            let segment = self
                .dj
                .create_intro(
                    guild_id.get(),
                    DjContext {
                        title: &title,
                        artist: &artist,
                        requester: &requester_name,
                        previous_track: Some(&previous_track),
                        session_opening: false,
                        radio_session,
                        personality: self.dj.settings(guild_id.get()).await.personality,
                        skip_transition: true,
                    },
                    true,
                )
                .await
                .map_err(|error| PlaybackError::Dj(error.to_string()))?;
            if let Some(segment) = segment {
                fallback_copy = Some(segment.script.clone());
                if let Some(uri) = segment.audio_uri
                    && let Some(mut narration) = self.load_single_track(guild_id, &uri).await?
                {
                    narration.user_data = Some(serde_json::json!({
                        "dj_segment": true,
                        "script": segment.script,
                        "skip_transition": true,
                    }));
                    queue
                        .push_to_front(TrackInQueue::from(narration))
                        .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
                    spoken_skip = true;
                }
            }
        }
        player
            .skip()
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        let response = if spoken_skip {
            format!(
                "Skipped `{}`. Cappy's switching it up now.",
                display_metadata(&current).0
            )
        } else if let Some(copy) = fallback_copy {
            format!(
                "Skipped `{}`. DJ copy (TTS unavailable): {copy}",
                display_metadata(&current).0
            )
        } else {
            format!("Skipped `{}`.", display_metadata(&current).0)
        };
        say(message, context, response).await
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
        self.radio
            .disable(guild_id)
            .await
            .map_err(PlaybackError::Data)?;
        player
            .get_queue()
            .clear()
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        player
            .stop_now()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        self.dj.mark_session_ended(guild_id.get()).await;
        say(
            message,
            context,
            "Stopped playback and cleared the queue.".to_owned(),
        )
        .await
    }

    async fn clear(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "The queue is already empty.".to_owned()).await;
        };
        let queue = player.get_queue();
        let count = queue
            .get_count()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        if count == 0 {
            return say(message, context, "The queue is already empty.".to_owned()).await;
        }
        queue
            .clear()
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            format!("Cleared {count} upcoming queue item(s). The current track keeps playing."),
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

    async fn volume(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        let Some(volume) = parse_volume(arguments)? else {
            let state = player
                .get_player()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            return say(
                message,
                context,
                format!(
                    "Shared CappyFM volume is **{}%**. For a private level, right-click CappyFM in voice and use Discord's User Volume slider.",
                    state.volume
                ),
            )
            .await;
        };
        player
            .set_volume(volume)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        say(
            message,
            context,
            format!("Shared CappyFM volume set to **{volume}%**."),
        )
        .await
    }

    async fn voice(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        let parts: Vec<&str> = arguments.split_whitespace().collect();
        if parts.is_empty() {
            let settings = self.dj.settings(guild_id.get()).await;
            return say(
                message,
                context,
                format!(
                    "Current DJ voice: `{}`. The DJ voice is AI-generated. Use `cap!voice list` to see presets.",
                    settings.voice
                ),
            )
            .await;
        }
        if parts[0].eq_ignore_ascii_case("list") {
            let voices = VoicePreset::ALL
                .into_iter()
                .map(|voice| format!("`{voice}` — {}", voice.description()))
                .collect::<Vec<_>>()
                .join("\n");
            return say(
                message,
                context,
                format!("**DJ voice presets**\n{voices}\nThe voices are AI-generated."),
            )
            .await;
        }
        if parts[0].eq_ignore_ascii_case("preview") {
            let preset = parts
                .get(1)
                .ok_or(PlaybackError::InvalidSetting)?
                .parse::<VoicePreset>()
                .map_err(|_| PlaybackError::InvalidSetting)?;
            return self.preview_voice(context, message, guild_id, preset).await;
        }
        let preset = parts[0]
            .parse::<VoicePreset>()
            .map_err(|_| PlaybackError::InvalidSetting)?;
        self.dj.set_voice(guild_id.get(), preset).await;
        say(
            message,
            context,
            format!("DJ voice set to `{preset}` for this session."),
        )
        .await
    }

    async fn preview_voice(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        preset: VoicePreset,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(
                message,
                context,
                "Start playback before previewing a DJ voice.".to_owned(),
            )
            .await;
        };
        let previous = self.dj.settings(guild_id.get()).await.voice;
        self.dj.set_voice(guild_id.get(), preset).await;
        let segment = self
            .dj
            .create_intro(
                guild_id.get(),
                DjContext {
                    title: "a microphone check",
                    artist: "CappyFM",
                    requester: &requester_display_name(message),
                    previous_track: None,
                    session_opening: false,
                    radio_session: false,
                    personality: self.dj.settings(guild_id.get()).await.personality,
                    skip_transition: false,
                },
                true,
            )
            .await
            .map_err(|error| PlaybackError::Dj(error.to_string()))?;
        self.dj.set_voice(guild_id.get(), previous).await;
        self.enqueue_or_post_segment(context, message, guild_id, player.get_queue(), segment)
            .await
    }

    async fn personality(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        if arguments.trim().is_empty() {
            let settings = self.dj.settings(guild_id.get()).await;
            return say(
                message,
                context,
                format!("Current personality: `{}`.", settings.personality),
            )
            .await;
        }
        let level = arguments
            .trim()
            .parse::<PersonalityLevel>()
            .map_err(|_| PlaybackError::InvalidSetting)?;
        self.dj.set_personality(guild_id.get(), level).await;
        say(
            message,
            context,
            format!("DJ personality set to `{level}` for this session."),
        )
        .await
    }

    async fn talk(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        if arguments.trim().is_empty() {
            let settings = self.dj.settings(guild_id.get()).await;
            return say(
                message,
                context,
                format!("DJ talk frequency: `{}`.", settings.frequency),
            )
            .await;
        }
        let frequency = arguments
            .trim()
            .parse::<TalkFrequency>()
            .map_err(|_| PlaybackError::InvalidSetting)?;
        self.dj.set_frequency(guild_id.get(), frequency).await;
        say(
            message,
            context,
            format!("DJ talk frequency set to `{frequency}` for this session."),
        )
        .await
    }

    async fn shutup(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        self.dj
            .set_frequency(guild_id.get(), TalkFrequency::Off)
            .await;
        say(
            message,
            context,
            "Understood. The capybara will operate the turntables silently.".to_owned(),
        )
        .await
    }

    async fn intro(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "The queue is empty.".to_owned()).await;
        };
        let queue = player.get_queue();
        let Some(next) = queue
            .get_track(0)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
        else {
            return say(
                message,
                context,
                "There is no next track to introduce.".to_owned(),
            )
            .await;
        };
        let (title, artist) = display_metadata(&next.track);
        let segment = self
            .dj
            .create_intro(
                guild_id.get(),
                DjContext {
                    title: &title,
                    artist: &artist,
                    requester: &requester_display_name(message),
                    previous_track: None,
                    session_opening: false,
                    radio_session: false,
                    personality: self.dj.settings(guild_id.get()).await.personality,
                    skip_transition: false,
                },
                true,
            )
            .await
            .map_err(|error| PlaybackError::Dj(error.to_string()))?;
        self.enqueue_or_post_segment(context, message, guild_id, queue, segment)
            .await
    }

    async fn enqueue_or_post_segment(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        queue: lavalink_rs::player_context::QueueRef,
        segment: Option<cappy_dj::DjSegment>,
    ) -> Result<(), PlaybackError> {
        let Some(segment) = segment else {
            return say(
                message,
                context,
                "DJ speech is off for this session.".to_owned(),
            )
            .await;
        };
        let current_is_dj = if let Some(player) = self.lavalink.get_player_context(guild_id) {
            player
                .get_player()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
                .track
                .as_ref()
                .is_some_and(is_dj_track)
        } else {
            false
        };
        let next_is_dj = queue
            .get_track(0)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
            .is_some_and(|item| is_dj_track(&item.track));
        if current_is_dj || next_is_dj {
            return say(
                message,
                context,
                "A DJ segment is already playing or queued next, so I won't stack another one."
                    .to_owned(),
            )
            .await;
        }
        if let Some(audio_uri) = segment.audio_uri
            && let Some(mut track) = self.load_single_track(guild_id, &audio_uri).await?
        {
            track.user_data = Some(serde_json::json!({
                "dj_segment": true,
                "script": segment.script,
            }));
            queue
                .push_to_front(TrackInQueue::from(track))
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            return say(
                message,
                context,
                "DJ intro queued. The voice is AI-generated.".to_owned(),
            )
            .await;
        }
        say(
            message,
            context,
            format!("DJ copy (TTS unavailable): {}", segment.script),
        )
        .await
    }

    async fn radio_command(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        let requested = arguments.trim();
        if requested.eq_ignore_ascii_case("off") {
            self.radio
                .disable(guild_id)
                .await
                .map_err(PlaybackError::Data)?;
            let removed = self.clear_queued_radio_items(guild_id).await?;
            return say(
                message,
                context,
                format!(
                    "Radio autoplay is off. Removed {removed} queued radio track(s) or radio DJ segment(s); direct requests remain."
                ),
            )
            .await;
        }
        requester_voice_channel(context, message, guild_id)?;
        self.ensure_joined(context, message, guild_id).await?;
        let replacing_radio = self
            .radio
            .session(guild_id)
            .await
            .is_some_and(|session| session.enabled);
        let replacing_current_radio = self
            .current_track(guild_id)
            .await?
            .as_ref()
            .is_some_and(is_radio_track);
        let removed_old_items = if replacing_radio {
            self.radio
                .disable(guild_id)
                .await
                .map_err(PlaybackError::Data)?;
            self.clear_queued_radio_items(guild_id).await?
        } else {
            0
        };
        self.dj.mark_session_ended(guild_id.get()).await;
        let vibe = if requested.is_empty() {
            if let Some(session) = self.radio.session(guild_id).await {
                session.vibe
            } else {
                sqlx::query_scalar::<_, String>(
                    "SELECT default_vibe FROM guild_settings WHERE guild_id=?",
                )
                .bind(guild_id.get().to_string())
                .fetch_optional(&self.database)
                .await
                .map_err(|error| PlaybackError::Data(error.to_string()))?
                .unwrap_or_else(|| "open-format".to_owned())
            }
        } else {
            validate_vibe(requested)?
        };
        self.radio
            .enable(
                guild_id,
                message.author.id,
                message.channel_id,
                vibe.clone(),
            )
            .await
            .map_err(PlaybackError::Data)?;
        let added = self
            .radio
            .replenish(&self.lavalink, guild_id, false, None)
            .await
            .map_err(PlaybackError::Lavalink)?;
        if added == 0 {
            self.prepend_radio_chatter(message, guild_id).await?;
        }
        if let Some(player) = self.lavalink.get_player_context(guild_id) {
            let state = player
                .get_player()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            let queued = player
                .get_queue()
                .get_count()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            if (state.track.is_none() && added > 0) || (replacing_current_radio && queued > 0) {
                player
                    .skip()
                    .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            }
        }
        say(
            message,
            context,
            format!(
                "{} radio with vibe `{}`. Removed {removed_old_items} old radio item(s) and added {added} discovery track(s); autoplay will refill below three.",
                if replacing_radio { "Replaced" } else { "Started" },
                safe(&vibe),
            ),
        )
        .await
    }

    async fn vibe(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        if arguments.trim().is_empty() {
            let session = self.radio.session(guild_id).await;
            let response = match session {
                Some(session) => format!(
                    "Current vibe: `{}`. Radio is {}.",
                    safe(&session.vibe),
                    if session.enabled { "on" } else { "off" }
                ),
                None => "Current vibe: `open-format`. Radio is off.".to_owned(),
            };
            return say(message, context, response).await;
        }
        let vibe = validate_vibe(arguments.trim())?;
        self.radio
            .set_vibe(guild_id, vibe.clone())
            .await
            .map_err(PlaybackError::Data)?;
        say(
            message,
            context,
            format!(
                "Vibe set to `{}` using only your explicit instruction.",
                safe(&vibe)
            ),
        )
        .await
    }

    async fn discovery(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        flavor: &str,
    ) -> Result<(), PlaybackError> {
        requester_voice_channel(context, message, guild_id)?;
        self.ensure_joined(context, message, guild_id).await?;
        if self.radio.session(guild_id).await.is_none() {
            self.radio
                .set_vibe(guild_id, "open-format".to_owned())
                .await
                .map_err(PlaybackError::Data)?;
        }
        let added = self
            .radio
            .replenish(&self.lavalink, guild_id, true, Some(flavor))
            .await
            .map_err(PlaybackError::Lavalink)?;
        if let Some(player) = self.lavalink.get_player_context(guild_id) {
            let state = player
                .get_player()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            if state.track.is_none() && added > 0 {
                player
                    .skip()
                    .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            }
        }
        let label = match flavor {
            "crate" => "lesser-known crate pick",
            "similar" => "related track",
            _ => "taste-compatible surprise",
        };
        say(
            message,
            context,
            if added > 0 {
                format!("Queued one {label}.")
            } else {
                format!("I couldn't find a fresh {label} just now.")
            },
        )
        .await
    }

    async fn why(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        let current = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
            .track;
        let Some(track) = current else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        let response = radio::current_reason(&track)
            .map(|reason| format!("Why this track: {reason}"))
            .unwrap_or_else(|| {
                "This was directly requested rather than selected by radio.".to_owned()
            });
        say(message, context, response).await
    }

    async fn fact(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let Some(track) = self.current_track(guild_id).await? else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        if let Some(fact) = self
            .radio
            .verified_fact(&track)
            .await
            .map_err(PlaybackError::Data)?
        {
            return say(message, context, fact).await;
        }
        let (title, artist) = radio::original_metadata(&track);
        let minutes = track.info.length / 60_000;
        let seconds = (track.info.length / 1_000) % 60;
        let source = track
            .user_data
            .as_ref()
            .and_then(|data| data.get("original_url"))
            .and_then(serde_json::Value::as_str)
            .or(track.info.uri.as_deref());
        let isrc = track
            .info
            .isrc
            .as_deref()
            .map(|value| format!(" Its ISRC is `{value}`."))
            .unwrap_or_default();
        let attribution = source
            .map(|url| format!(" Source: <{url}>"))
            .unwrap_or_else(|| " Source: current playback metadata.".to_owned());
        say(
            message,
            context,
            format!(
                "Verified metadata: `{}` by `{}` runs {minutes}:{seconds:02}.{isrc}{attribution}",
                safe(&title),
                safe(&artist)
            ),
        )
        .await
    }

    async fn preference(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        preference: i64,
    ) -> Result<(), PlaybackError> {
        let Some(track) = self.current_track(guild_id).await? else {
            return say(message, context, "Nothing is playing.".to_owned()).await;
        };
        self.radio
            .preference(guild_id, message.author.id, &track, preference)
            .await
            .map_err(PlaybackError::Data)?;
        let (title, artist) = display_metadata(&track);
        say(
            message,
            context,
            format!(
                "{} `{title}` by `{artist}`. This music-only signal will shape future radio picks.",
                if preference > 0 { "Liked" } else { "Disliked" }
            ),
        )
        .await
    }

    async fn favorites(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let items = self
            .radio
            .favorites(guild_id, message.author.id)
            .await
            .map_err(PlaybackError::Data)?;
        say(
            message,
            context,
            if items.is_empty() {
                "No favorites yet. Use `cap!like` while a track is playing.".to_owned()
            } else {
                format!("Your recent favorites:\n{}", items.join("\n"))
            },
        )
        .await
    }

    async fn history(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let items = self
            .radio
            .history(guild_id, 10)
            .await
            .map_err(PlaybackError::Data)?;
        say(
            message,
            context,
            if items.is_empty() {
                "No server music history yet.".to_owned()
            } else {
                format!("Recent server music history:\n{}", items.join("\n"))
            },
        )
        .await
    }

    async fn stats(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let stats = self
            .radio
            .stats(guild_id)
            .await
            .map_err(PlaybackError::Data)?;
        say(message, context, stats).await
    }

    async fn settings(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
        arguments: &str,
    ) -> Result<(), PlaybackError> {
        sqlx::query("INSERT OR IGNORE INTO guild_settings (guild_id) VALUES (?)")
            .bind(guild_id.get().to_string())
            .execute(&self.database)
            .await
            .map_err(|error| PlaybackError::Data(error.to_string()))?;
        let mut parts = arguments.trim().splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or_default().to_ascii_lowercase();
        let value = parts.next().unwrap_or_default().trim();
        if key.is_empty() {
            let row = sqlx::query(
                "SELECT default_vibe, default_voice, default_personality, default_talk_frequency, command_channel_id \
                 FROM guild_settings WHERE guild_id=?",
            )
            .bind(guild_id.get().to_string())
            .fetch_one(&self.database)
            .await
            .map_err(|error| PlaybackError::Data(error.to_string()))?;
            return say(
                message,
                context,
                format!(
                    "Server defaults — vibe: `{}`, voice: `{}`, personality: `{}`, talk: `{}`, command channel: {}.",
                    safe(&row.get::<String, _>("default_vibe")),
                    row.get::<String, _>("default_voice"),
                    row.get::<String, _>("default_personality"),
                    row.get::<String, _>("default_talk_frequency"),
                    row.get::<Option<String>, _>("command_channel_id")
                        .map(|id| format!("<#{id}>") )
                        .unwrap_or_else(|| "any".to_owned())
                ),
            )
            .await;
        }
        require_manage_guild(context, message)?;
        match key.as_str() {
            "vibe" => {
                let vibe = validate_vibe(value)?;
                sqlx::query(
                    "UPDATE guild_settings SET default_vibe=?, updated_at=CURRENT_TIMESTAMP WHERE guild_id=?",
                )
                .bind(&vibe)
                .bind(guild_id.get().to_string())
                .execute(&self.database)
                .await
                .map_err(|error| PlaybackError::Data(error.to_string()))?;
                self.radio
                    .set_vibe(guild_id, vibe.clone())
                    .await
                    .map_err(PlaybackError::Data)?;
            }
            "voice" => {
                let voice = value
                    .parse::<VoicePreset>()
                    .map_err(|_| PlaybackError::InvalidSetting)?;
                sqlx::query(
                    "UPDATE guild_settings SET default_voice=?, updated_at=CURRENT_TIMESTAMP WHERE guild_id=?",
                )
                .bind(voice.to_string())
                .bind(guild_id.get().to_string())
                .execute(&self.database)
                .await
                .map_err(|error| PlaybackError::Data(error.to_string()))?;
                self.dj.set_voice(guild_id.get(), voice).await;
            }
            "personality" => {
                let personality = value
                    .parse::<PersonalityLevel>()
                    .map_err(|_| PlaybackError::InvalidSetting)?;
                sqlx::query(
                    "UPDATE guild_settings SET default_personality=?, updated_at=CURRENT_TIMESTAMP WHERE guild_id=?",
                )
                .bind(personality.to_string())
                .bind(guild_id.get().to_string())
                .execute(&self.database)
                .await
                .map_err(|error| PlaybackError::Data(error.to_string()))?;
                self.dj.set_personality(guild_id.get(), personality).await;
            }
            "talk" => {
                let frequency = value
                    .parse::<TalkFrequency>()
                    .map_err(|_| PlaybackError::InvalidSetting)?;
                sqlx::query(
                    "UPDATE guild_settings SET default_talk_frequency=?, updated_at=CURRENT_TIMESTAMP WHERE guild_id=?",
                )
                .bind(frequency.to_string())
                .bind(guild_id.get().to_string())
                .execute(&self.database)
                .await
                .map_err(|error| PlaybackError::Data(error.to_string()))?;
                self.dj.set_frequency(guild_id.get(), frequency).await;
            }
            "channel" => {
                let channel = match value.to_ascii_lowercase().as_str() {
                    "here" => Some(message.channel_id.get().to_string()),
                    "off" | "any" => None,
                    _ => return Err(PlaybackError::InvalidSetting),
                };
                sqlx::query(
                    "UPDATE guild_settings SET command_channel_id=?, updated_at=CURRENT_TIMESTAMP WHERE guild_id=?",
                )
                .bind(channel)
                .bind(guild_id.get().to_string())
                .execute(&self.database)
                .await
                .map_err(|error| PlaybackError::Data(error.to_string()))?;
            }
            _ => return Err(PlaybackError::InvalidSetting),
        }
        say(message, context, format!("Updated server default `{key}`.")).await
    }

    async fn health(
        &self,
        context: &Context,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&self.database)
            .await
            .map_err(|error| PlaybackError::Data(error.to_string()))?;
        let (player, queued) = if let Some(player) = self.lavalink.get_player_context(guild_id) {
            let playing = player
                .get_player()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
                .track
                .is_some();
            let queued = player
                .get_queue()
                .get_count()
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
            (if playing { "playing" } else { "idle" }, queued)
        } else {
            ("disconnected", 0)
        };
        let radio = self
            .radio
            .session(guild_id)
            .await
            .is_some_and(|session| session.enabled);
        say(
            message,
            context,
            format!(
                "CappyFM health: database healthy, Lavalink player `{player}`, {queued} raw queue item(s), radio {}.",
                if radio { "on" } else { "off" }
            ),
        )
        .await
    }

    async fn current_track(&self, guild_id: GuildId) -> Result<Option<TrackData>, PlaybackError> {
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return Ok(None);
        };
        player
            .get_player()
            .await
            .map(|player| player.track)
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))
    }

    async fn clear_queued_radio_items(&self, guild_id: GuildId) -> Result<usize, PlaybackError> {
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return Ok(0);
        };
        let queue = player.get_queue();
        let count = queue
            .get_count()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        let mut removed = 0;
        for index in (0..count).rev() {
            let should_remove = queue
                .get_track(index)
                .await
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
                .is_some_and(|item| is_radio_queue_item(&item.track));
            if should_remove {
                queue
                    .remove(index)
                    .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn prepend_radio_chatter(
        &self,
        message: &Message,
        guild_id: GuildId,
    ) -> Result<(), PlaybackError> {
        let Some(player) = self.lavalink.get_player_context(guild_id) else {
            return Ok(());
        };
        let state = player
            .get_player()
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        let queue = player.get_queue();
        let Some(next) = queue
            .get_track(0)
            .await
            .map_err(|error| PlaybackError::Lavalink(error.to_string()))?
        else {
            return Ok(());
        };
        if !is_radio_track(&next.track)
            || state.track.as_ref().is_some_and(is_dj_track)
            || is_dj_track(&next.track)
        {
            return Ok(());
        }
        let (title, artist) = display_metadata(&next.track);
        let previous_track = state.track.as_ref().map(|track| {
            let (previous_title, previous_artist) = display_metadata(track);
            format!("{previous_title} by {previous_artist}")
        });
        let result = timeout_at(Instant::now() + Duration::from_secs(12), async {
            let segment = self
                .dj
                .create_intro(
                    guild_id.get(),
                    DjContext {
                        title: &title,
                        artist: &artist,
                        requester: &requester_display_name(message),
                        previous_track: previous_track.as_deref(),
                        session_opening: state.track.is_none(),
                        radio_session: true,
                        personality: self.dj.settings(guild_id.get()).await.personality,
                        skip_transition: false,
                    },
                    false,
                )
                .await
                .ok()??;
            let uri = segment.audio_uri.as_ref()?;
            let track = self.load_single_track(guild_id, uri).await.ok()??;
            Some((segment, track))
        })
        .await;
        if let Ok(Some((segment, mut track))) = result {
            track.user_data = Some(serde_json::json!({
                "dj_segment": true,
                "script": segment.script,
            }));
            queue
                .push_to_front(TrackInQueue::from(track))
                .map_err(|error| PlaybackError::Lavalink(error.to_string()))?;
        }
        Ok(())
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
        self.radio
            .disable(guild_id)
            .await
            .map_err(PlaybackError::Data)?;
        self.dj.mark_session_ended(guild_id.get()).await;
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
        self.dj.reset_session(guild_id.get()).await;
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

fn require_manage_guild(context: &Context, message: &Message) -> Result<(), PlaybackError> {
    let permitted = message.member.as_ref().is_some_and(|member| {
        member
            .permissions
            .or_else(|| {
                context
                    .cache
                    .guild(message.guild_id?)
                    .map(|guild| guild.partial_member_permissions(message.author.id, member))
            })
            .is_some_and(|permissions| permissions.administrator() || permissions.manage_guild())
    });
    if permitted {
        Ok(())
    } else {
        Err(PlaybackError::NotAdministrator)
    }
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

fn validate_vibe(input: &str) -> Result<String, PlaybackError> {
    let value = input.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return Err(PlaybackError::InvalidSetting);
    }
    Ok(value.to_owned())
}

fn map_spotify_error(error: SpotifyError) -> PlaybackError {
    match error {
        SpotifyError::AuthorizationExpired => PlaybackError::SpotifyAuthorizationRequired,
        other => PlaybackError::SpotifyPlaylist(other.to_string()),
    }
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
    if track
        .user_data
        .as_ref()
        .and_then(|value| value.get("dj_segment"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return (
            "CappyFM DJ segment".to_owned(),
            "AI-generated voice".to_owned(),
        );
    }
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

fn parse_volume(input: &str) -> Result<Option<u16>, PlaybackError> {
    if input.trim().is_empty() {
        return Ok(None);
    }
    let volume = input
        .trim()
        .parse::<u16>()
        .map_err(|_| PlaybackError::InvalidSetting)?;
    if volume > 100 {
        return Err(PlaybackError::InvalidSetting);
    }
    Ok(Some(volume))
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

fn content_version_label(version: ContentVersion) -> &'static str {
    match version {
        ContentVersion::Explicit => "explicit",
        ContentVersion::Clean => "clean",
        ContentVersion::Unknown => "unknown",
    }
}

fn source_label(source: &str) -> &'static str {
    match source {
        "soundcloud" => "SoundCloud",
        _ => "YouTube",
    }
}

fn requester_suffix(data: &Option<serde_json::Value>) -> String {
    requester_id(data)
        .map(|id| format!(" · requested by <@{id}>"))
        .unwrap_or_default()
}

fn requester_id(data: &Option<serde_json::Value>) -> Option<u64> {
    data.as_ref()?.get("requester_id")?.as_u64()
}

fn parse_queue_position(input: &str) -> Result<usize, PlaybackError> {
    let position = input
        .trim()
        .parse::<usize>()
        .map_err(|_| PlaybackError::InvalidSetting)?;
    if position == 0 {
        return Err(PlaybackError::InvalidSetting);
    }
    Ok(position)
}

fn queue_blocks(queue: VecDeque<TrackInQueue>) -> Vec<Vec<TrackInQueue>> {
    let mut blocks = Vec::new();
    let mut pending_dj = None;
    for item in queue {
        if is_dj_track(&item.track) {
            if let Some(previous) = pending_dj.replace(item) {
                blocks.push(vec![previous]);
            }
        } else {
            let mut block = Vec::with_capacity(2);
            if let Some(dj) = pending_dj.take() {
                block.push(dj);
            }
            block.push(item);
            blocks.push(block);
        }
    }
    if let Some(dj) = pending_dj {
        blocks.push(vec![dj]);
    }
    blocks
}

fn block_music(block: &[TrackInQueue]) -> &TrackData {
    &block.last().expect("queue blocks are non-empty").track
}

fn fair_queue_blocks(blocks: Vec<Vec<TrackInQueue>>) -> Vec<Vec<TrackInQueue>> {
    let mut order = Vec::new();
    let mut groups: HashMap<u64, VecDeque<Vec<TrackInQueue>>> = HashMap::new();
    let mut automatic = Vec::new();
    for block in blocks {
        if let Some(user_id) = requester_id(&block_music(&block).user_data) {
            if !groups.contains_key(&user_id) {
                order.push(user_id);
            }
            groups.entry(user_id).or_default().push_back(block);
        } else {
            automatic.push(block);
        }
    }
    let mut fair = Vec::new();
    while !order.is_empty() {
        let mut remaining = Vec::new();
        for user_id in order {
            let queue = groups
                .get_mut(&user_id)
                .expect("requester order and groups stay synchronized");
            for _ in 0..3 {
                let Some(block) = queue.pop_front() else {
                    break;
                };
                fair.push(block);
            }
            if !queue.is_empty() {
                remaining.push(user_id);
            }
        }
        order = remaining;
    }
    fair.extend(automatic);
    fair
}

fn is_radio_track(track: &TrackData) -> bool {
    track
        .user_data
        .as_ref()
        .and_then(|value| value.get("radio_track"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn add_command_id(data: &Option<serde_json::Value>) -> Option<u64> {
    data.as_ref()?.get("add_command_id")?.as_u64()
}

fn remove_command_blocks(
    blocks: Vec<Vec<TrackInQueue>>,
    command_id: u64,
) -> (Vec<Vec<TrackInQueue>>, Vec<Vec<TrackInQueue>>) {
    blocks
        .into_iter()
        .partition(|block| add_command_id(&block_music(block).user_data) != Some(command_id))
}

fn is_radio_queue_item(track: &TrackData) -> bool {
    if is_radio_track(track) {
        return true;
    }
    is_radio_dj_data(&track.user_data)
}

fn is_radio_dj_data(data: &Option<serde_json::Value>) -> bool {
    data.as_ref().is_some_and(|value| {
        value.get("dj_segment").and_then(serde_json::Value::as_bool) == Some(true)
            && value
                .get("radio_session")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
    })
}

fn is_dj_track(track: &TrackData) -> bool {
    track
        .user_data
        .as_ref()
        .and_then(|value| value.get("dj_segment"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn requester_display_name(message: &Message) -> String {
    message
        .member
        .as_ref()
        .and_then(|member| member.nick.clone())
        .or_else(|| message.author.global_name.clone())
        .unwrap_or_else(|| message.author.name.clone())
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

    fn queued_track(title: &str, requester: Option<u64>, dj: bool) -> TrackInQueue {
        let mut track = TrackData::default();
        track.info.title = title.to_owned();
        track.user_data = Some(serde_json::json!({
            "requester_id": requester,
            "dj_segment": dj,
        }));
        TrackInQueue::from(track)
    }

    fn command_track(title: &str, command_id: u64) -> TrackInQueue {
        let mut track = queued_track(title, Some(1), false);
        track.track.user_data.as_mut().unwrap()["add_command_id"] = command_id.into();
        track
    }

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

    #[test]
    fn volume_is_bounded_to_discord_safe_percentages() {
        assert_eq!(parse_volume("").unwrap(), None);
        assert_eq!(parse_volume("0").unwrap(), Some(0));
        assert_eq!(parse_volume("100").unwrap(), Some(100));
        assert!(parse_volume("101").is_err());
        assert!(parse_volume("loud").is_err());
        assert_eq!(DEFAULT_VOLUME, 60);
    }

    #[test]
    fn vibe_is_explicit_and_bounded() {
        assert_eq!(
            validate_vibe("late-night coding").unwrap(),
            "late-night coding"
        );
        assert!(validate_vibe("").is_err());
        assert!(validate_vibe(&"x".repeat(81)).is_err());
        assert!(validate_vibe("cozy\nignore previous").is_err());
    }

    #[test]
    fn radio_shutdown_only_identifies_radio_dj_segments() {
        assert!(is_radio_dj_data(&Some(serde_json::json!({
            "dj_segment": true,
            "radio_session": true
        }))));
        assert!(!is_radio_dj_data(&Some(serde_json::json!({
            "dj_segment": true
        }))));
        assert!(!is_radio_dj_data(&None));
    }

    #[test]
    fn queue_blocks_keep_dj_segments_attached_to_music() {
        let queue = VecDeque::from([
            queued_track("intro", None, true),
            queued_track("song", Some(1), false),
            queued_track("next", Some(2), false),
        ]);
        let blocks = queue_blocks(queue);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].len(), 2);
        assert_eq!(block_music(&blocks[0]).info.title, "song");
    }

    #[test]
    fn undo_removes_every_song_from_only_the_last_add_command() {
        let blocks = vec![
            vec![command_track("older", 10)],
            vec![
                queued_track("intro", None, true),
                command_track("new one", 20),
            ],
            vec![command_track("new two", 20)],
        ];
        let (kept, removed) = remove_command_blocks(blocks, 20);
        assert_eq!(kept.len(), 1);
        assert_eq!(removed.len(), 2);
        assert_eq!(block_music(&kept[0]).info.title, "older");
        assert_eq!(removed[0].len(), 2);
    }

    #[test]
    fn queue_fairness_caps_consecutive_requests_at_three() {
        let mut blocks = Vec::new();
        for index in 0..7 {
            blocks.push(vec![queued_track(&format!("a{index}"), Some(1), false)]);
        }
        for index in 0..2 {
            blocks.push(vec![queued_track(&format!("b{index}"), Some(2), false)]);
        }
        let fair = fair_queue_blocks(blocks);
        let requesters: Vec<_> = fair
            .iter()
            .map(|block| requester_id(&block_music(block).user_data).unwrap())
            .collect();
        assert_eq!(&requesters[..5], &[1, 1, 1, 2, 2]);
    }

    #[test]
    fn queue_positions_are_one_based() {
        assert_eq!(parse_queue_position("1").unwrap(), 1);
        assert!(parse_queue_position("0").is_err());
        assert!(parse_queue_position("nope").is_err());
    }
}
