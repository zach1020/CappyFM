use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use cappy_core::resolver::normalize;
use cappy_dj::{DjContext, DjService};
use lavalink_rs::{
    model::{
        BoxFuture,
        events::{Events, TrackEnd, TrackEndReason, TrackStart, WebSocketClosed},
        track::{TrackData, TrackLoadData},
    },
    prelude::{LavalinkClient, TrackInQueue},
};
use serenity::all::{ChannelId, GuildId, Http, UserId};
use sqlx::{Row, SqlitePool};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, Instant, sleep, timeout, timeout_at};
use tracing::warn;

const RADIO_QUEUE_TARGET: usize = 4;

#[derive(Debug, Clone)]
pub struct RadioSession {
    pub vibe: String,
    pub enabled: bool,
    pub text_channel_id: u64,
}

struct RadioDjContext<'a> {
    title: &'a str,
    artist: &'a str,
    previous_track: Option<&'a str>,
    session_opening: bool,
    deadline: Instant,
}

#[derive(Clone)]
pub struct RadioService {
    database: SqlitePool,
    http: reqwest::Client,
    dj: DjService,
    sessions: Arc<RwLock<HashMap<u64, RadioSession>>>,
    replenishing: Arc<Mutex<HashSet<u64>>>,
}

static RADIO_RUNTIME: OnceLock<RadioService> = OnceLock::new();

impl RadioService {
    pub fn new(database: SqlitePool, dj: DjService) -> Self {
        Self {
            database,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .user_agent(
                    std::env::var("MUSICBRAINZ_USER_AGENT")
                        .unwrap_or_else(|_| "CappyFM/0.1 (local Discord music bot)".to_owned()),
                )
                .build()
                .unwrap_or_else(|_| unreachable!("static HTTP client configuration")),
            dj,
            sessions: Arc::default(),
            replenishing: Arc::default(),
        }
    }

    pub fn install_events(&self) -> Events {
        let _ = RADIO_RUNTIME.set(self.clone());
        Events {
            track_start: Some(track_start),
            track_end: Some(track_end),
            websocket_closed: Some(websocket_closed),
            ..Events::default()
        }
    }

    pub async fn enable(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        channel_id: ChannelId,
        vibe: String,
    ) -> Result<(), String> {
        self.sessions.write().await.insert(
            guild_id.get(),
            RadioSession {
                vibe: vibe.clone(),
                enabled: true,
                text_channel_id: channel_id.get(),
            },
        );
        sqlx::query(
            "INSERT INTO radio_sessions (guild_id, enabled, vibe, started_by_user_id, text_channel_id) \
             VALUES (?, 1, ?, ?, ?) ON CONFLICT(guild_id) DO UPDATE SET enabled=1, \
             vibe=excluded.vibe, started_by_user_id=excluded.started_by_user_id, \
             text_channel_id=excluded.text_channel_id, started_at=CURRENT_TIMESTAMP, \
             updated_at=CURRENT_TIMESTAMP",
        )
        .bind(guild_id.get().to_string())
        .bind(vibe)
        .bind(user_id.get().to_string())
        .bind(channel_id.get().to_string())
        .execute(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub async fn disable(&self, guild_id: GuildId) -> Result<(), String> {
        if let Some(session) = self.sessions.write().await.get_mut(&guild_id.get()) {
            session.enabled = false;
        }
        sqlx::query(
            "UPDATE radio_sessions SET enabled=0, updated_at=CURRENT_TIMESTAMP WHERE guild_id=?",
        )
        .bind(guild_id.get().to_string())
        .execute(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub async fn session(&self, guild_id: GuildId) -> Option<RadioSession> {
        self.sessions.read().await.get(&guild_id.get()).cloned()
    }

    pub async fn set_vibe(&self, guild_id: GuildId, vibe: String) -> Result<(), String> {
        let mut sessions = self.sessions.write().await;
        let session = sessions.entry(guild_id.get()).or_insert(RadioSession {
            vibe: vibe.clone(),
            enabled: false,
            text_channel_id: 0,
        });
        session.vibe.clone_from(&vibe);
        drop(sessions);
        sqlx::query(
            "INSERT INTO radio_sessions (guild_id, enabled, vibe) VALUES (?, 0, ?) \
             ON CONFLICT(guild_id) DO UPDATE SET vibe=excluded.vibe, updated_at=CURRENT_TIMESTAMP",
        )
        .bind(guild_id.get().to_string())
        .bind(vibe)
        .execute(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub async fn replenish(
        &self,
        client: &LavalinkClient,
        guild_id: GuildId,
        force: bool,
        flavor: Option<&str>,
    ) -> Result<usize, String> {
        let Some(session) = self.session(guild_id).await else {
            return Ok(0);
        };
        if !session.enabled && !force {
            return Ok(0);
        }
        let Some(player) = client.get_player_context(guild_id) else {
            return Ok(0);
        };
        let queue = player.get_queue();
        let raw_count = queue.get_count().await.map_err(|error| error.to_string())?;
        let mut playable_count = 0;
        for index in 0..raw_count {
            if queue
                .get_track(index)
                .await
                .map_err(|error| error.to_string())?
                .is_some_and(|item| !is_dj_segment(&item.track))
            {
                playable_count += 1;
            }
        }
        if !force && playable_count >= 3 {
            return Ok(0);
        }

        {
            let mut active = self.replenishing.lock().await;
            if !active.insert(guild_id.get()) {
                return Ok(0);
            }
        }
        let result = self
            .replenish_inner(client, guild_id, &session.vibe, flavor, playable_count)
            .await;
        self.replenishing.lock().await.remove(&guild_id.get());
        result
    }

    async fn replenish_inner(
        &self,
        client: &LavalinkClient,
        guild_id: GuildId,
        vibe: &str,
        flavor: Option<&str>,
        current_count: usize,
    ) -> Result<usize, String> {
        let target = if flavor.is_some() {
            1
        } else {
            RADIO_QUEUE_TARGET.saturating_sub(current_count).max(2)
        };
        let taste_artist = self.preferred_artist(guild_id).await.ok().flatten().or(self
            .recent_artist(guild_id)
            .await
            .ok()
            .flatten());
        let query = discovery_query(vibe, flavor, taste_artist.as_deref());
        let loaded = self
            .load_tracks_with_retry(client, guild_id, &format!("ytsearch:{query}"))
            .await?;
        let candidates = match loaded.data {
            Some(TrackLoadData::Search(tracks)) => tracks,
            Some(TrackLoadData::Track(track)) => vec![track],
            _ => Vec::new(),
        };

        let mut avoid = HashSet::new();
        let current = player_data(client, guild_id).await?;
        let mut previous_track = current.as_ref().map(|track| {
            let (title, artist) = original_metadata(track);
            format!("{title} by {artist}")
        });
        let mut previous_was_dj = current.as_ref().is_some_and(is_dj_segment);
        if let Some(track) = &current
            && !is_dj_segment(track)
        {
            avoid.insert(track.info.identifier.clone());
        }
        let queue = client
            .get_player_context(guild_id)
            .expect("player checked before replenishment")
            .get_queue();
        let queue_count = queue.get_count().await.map_err(|error| error.to_string())?;
        for index in 0..queue_count {
            if let Some(track) = queue
                .get_track(index)
                .await
                .map_err(|error| error.to_string())?
            {
                previous_was_dj = is_dj_segment(&track.track);
                if !previous_was_dj {
                    avoid.insert(track.track.info.identifier.clone());
                    let (title, artist) = original_metadata(&track.track);
                    previous_track = Some(format!("{title} by {artist}"));
                }
            }
        }

        let reason = recommendation_reason(vibe, flavor, taste_artist.as_deref());
        let mut added = 0;
        for mut track in candidates {
            if added >= target
                || track.info.is_stream
                || !avoid.insert(track.info.identifier.clone())
            {
                continue;
            }
            if self.preference_score(guild_id, &track).await? < 0 {
                continue;
            }
            let (title, artist) = original_metadata(&track);
            track.user_data = Some(serde_json::json!({
                "radio_track": true,
                "radio_vibe": vibe,
                "radio_reason": reason,
                "original_title": track.info.title,
                "original_artist": track.info.author,
                "playback_source": track.info.source_name,
            }));
            self.ensure_track(&track).await?;
            let session_opening = current.is_none() && queue_count == 0 && added == 0;
            if !previous_was_dj
                && let Some(dj_track) = self
                    .radio_dj_track(
                        client,
                        guild_id,
                        RadioDjContext {
                            title: &title,
                            artist: &artist,
                            previous_track: previous_track.as_deref(),
                            session_opening,
                            deadline: Instant::now() + Duration::from_secs(12),
                        },
                    )
                    .await
            {
                queue
                    .push_to_back(TrackInQueue::from(dj_track))
                    .map_err(|error| error.to_string())?;
            }
            queue
                .push_to_back(TrackInQueue::from(track))
                .map_err(|error| error.to_string())?;
            previous_track = Some(format!("{title} by {artist}"));
            previous_was_dj = false;
            added += 1;
        }
        Ok(added)
    }

    async fn radio_dj_track(
        &self,
        client: &LavalinkClient,
        guild_id: GuildId,
        context: RadioDjContext<'_>,
    ) -> Option<TrackData> {
        timeout_at(context.deadline, async {
            let segment = self
                .dj
                .create_intro(
                    guild_id.get(),
                    DjContext {
                        title: context.title,
                        artist: context.artist,
                        requester: "radio",
                        previous_track: context.previous_track,
                        session_opening: context.session_opening,
                        radio_session: true,
                        personality: self.dj.settings(guild_id.get()).await.personality,
                        skip_transition: false,
                    },
                    false,
                )
                .await
                .ok()??;
            let uri = segment.audio_uri.as_ref()?;
            let loaded = self
                .load_tracks_with_retry(client, guild_id, uri)
                .await
                .ok()?;
            let mut track = match loaded.data? {
                TrackLoadData::Track(track) => track,
                TrackLoadData::Search(mut tracks) => tracks.drain(..).next()?,
                _ => return None,
            };
            track.user_data = Some(serde_json::json!({
                "dj_segment": true,
                "script": segment.script,
                "radio_session": true,
            }));
            Some(track)
        })
        .await
        .ok()
        .flatten()
    }

    async fn load_tracks_with_retry(
        &self,
        client: &LavalinkClient,
        guild_id: GuildId,
        query: &str,
    ) -> Result<lavalink_rs::model::track::Track, String> {
        let mut last_error = "audio node request timed out".to_owned();
        for attempt in 0..2 {
            match timeout(Duration::from_secs(10), client.load_tracks(guild_id, query)).await {
                Ok(Ok(loaded)) => return Ok(loaded),
                Ok(Err(error)) => last_error = error.to_string(),
                Err(_) => last_error = "audio node request timed out".to_owned(),
            }
            if attempt == 0 {
                sleep(Duration::from_millis(200)).await;
            }
        }
        Err(last_error)
    }

    pub async fn preference(
        &self,
        guild_id: GuildId,
        user_id: UserId,
        track: &TrackData,
        preference: i64,
    ) -> Result<(), String> {
        let track_id = self.ensure_track(track).await?;
        sqlx::query(
            "INSERT INTO user_track_preferences (guild_id, user_id, track_id, preference) \
             VALUES (?, ?, ?, ?) ON CONFLICT(guild_id, user_id, track_id) DO UPDATE SET \
             preference=excluded.preference, updated_at=CURRENT_TIMESTAMP",
        )
        .bind(guild_id.get().to_string())
        .bind(user_id.get().to_string())
        .bind(&track_id)
        .bind(preference)
        .execute(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        self.record_event(
            guild_id,
            track,
            if preference > 0 { "liked" } else { "disliked" },
        )
        .await
    }

    pub async fn history(&self, guild_id: GuildId, limit: i64) -> Result<Vec<String>, String> {
        let rows = sqlx::query(
            "SELECT t.canonical_title, t.canonical_artist, pe.event_type, pe.occurred_at \
             FROM play_events pe JOIN tracks t ON t.id=pe.track_id \
             WHERE pe.guild_id=? AND pe.event_type IN ('started','completed','skipped') \
             ORDER BY pe.id DESC LIMIT ?",
        )
        .bind(guild_id.get().to_string())
        .bind(limit)
        .fetch_all(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(|row| {
                format!(
                    "`{}` — `{}` ({})",
                    row.get::<String, _>("canonical_title"),
                    row.get::<String, _>("canonical_artist"),
                    row.get::<String, _>("event_type")
                )
            })
            .collect())
    }

    pub async fn favorites(
        &self,
        guild_id: GuildId,
        user_id: UserId,
    ) -> Result<Vec<String>, String> {
        let rows = sqlx::query(
            "SELECT t.canonical_title, t.canonical_artist FROM user_track_preferences p \
             JOIN tracks t ON t.id=p.track_id WHERE p.guild_id=? AND p.user_id=? \
             AND p.preference > 0 ORDER BY p.updated_at DESC LIMIT 10",
        )
        .bind(guild_id.get().to_string())
        .bind(user_id.get().to_string())
        .fetch_all(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(rows
            .into_iter()
            .map(|row| {
                format!(
                    "`{}` — `{}`",
                    row.get::<String, _>("canonical_title"),
                    row.get::<String, _>("canonical_artist")
                )
            })
            .collect())
    }

    pub async fn stats(&self, guild_id: GuildId) -> Result<String, String> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS plays, COUNT(DISTINCT track_id) AS tracks FROM play_events \
             WHERE guild_id=? AND event_type='started'",
        )
        .bind(guild_id.get().to_string())
        .fetch_one(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        let artist = self
            .recent_artist(guild_id)
            .await?
            .unwrap_or_else(|| "none yet".to_owned());
        Ok(format!(
            "{} recorded plays across {} distinct tracks. Most recent artist: `{artist}`.",
            row.get::<i64, _>("plays"),
            row.get::<i64, _>("tracks")
        ))
    }

    pub async fn verified_fact(&self, track: &TrackData) -> Result<Option<String>, String> {
        let track_id = self.ensure_track(track).await?;
        if let Some(row) =
            sqlx::query("SELECT fact_text, source_url FROM verified_facts WHERE track_id=?")
                .bind(&track_id)
                .fetch_optional(&self.database)
                .await
                .map_err(|error| error.to_string())?
        {
            return Ok(Some(format!(
                "{} Source: <{}>",
                row.get::<String, _>("fact_text"),
                row.get::<String, _>("source_url")
            )));
        }

        let isrc = track
            .user_data
            .as_ref()
            .and_then(|data| data.get("original_isrc"))
            .and_then(serde_json::Value::as_str)
            .or(track.info.isrc.as_deref());
        let Some(isrc) = isrc else {
            return Ok(None);
        };
        let body: serde_json::Value = self
            .http
            .get("https://musicbrainz.org/ws/2/recording")
            .query(&[
                ("query", format!("isrc:{isrc}")),
                ("fmt", "json".to_owned()),
                ("limit", "1".to_owned()),
            ])
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?
            .json()
            .await
            .map_err(|error| error.to_string())?;
        let Some(recording) = body
            .get("recordings")
            .and_then(serde_json::Value::as_array)
            .and_then(|recordings| recordings.first())
        else {
            return Ok(None);
        };
        let score = recording
            .get("score")
            .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
            .unwrap_or(0);
        if score < 90 {
            return Ok(None);
        }
        let Some(recording_id) = recording.get("id").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let Some(date) = recording
            .get("first-release-date")
            .and_then(serde_json::Value::as_str)
            .filter(|date| !date.is_empty())
        else {
            return Ok(None);
        };
        let (title, artist) = original_metadata(track);
        let fact = format!(
            "MusicBrainz lists `{title}` by `{artist}` with a first release date of `{date}`."
        );
        let source_url = format!("https://musicbrainz.org/recording/{recording_id}");
        sqlx::query(
            "INSERT INTO verified_facts (track_id, fact_text, source_url, provider, confidence) \
             VALUES (?, ?, ?, 'musicbrainz', ?) ON CONFLICT(track_id) DO UPDATE SET \
             fact_text=excluded.fact_text, source_url=excluded.source_url, \
             confidence=excluded.confidence, fetched_at=CURRENT_TIMESTAMP",
        )
        .bind(track_id)
        .bind(&fact)
        .bind(&source_url)
        .bind(score as f64 / 100.0)
        .execute(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(Some(format!("{fact} Source: <{source_url}>")))
    }

    async fn recent_artist(&self, guild_id: GuildId) -> Result<Option<String>, String> {
        sqlx::query_scalar(
            "SELECT t.canonical_artist FROM play_events pe JOIN tracks t ON t.id=pe.track_id \
             WHERE pe.guild_id=? AND pe.event_type IN ('started','liked') \
             ORDER BY pe.id DESC LIMIT 1",
        )
        .bind(guild_id.get().to_string())
        .fetch_optional(&self.database)
        .await
        .map_err(|error| error.to_string())
    }

    async fn preferred_artist(&self, guild_id: GuildId) -> Result<Option<String>, String> {
        sqlx::query_scalar(
            "SELECT t.canonical_artist FROM user_track_preferences p \
             JOIN tracks t ON t.id=p.track_id WHERE p.guild_id=? GROUP BY t.canonical_artist \
             HAVING SUM(p.preference) > 0 ORDER BY SUM(p.preference) DESC, MAX(p.updated_at) DESC LIMIT 1",
        )
        .bind(guild_id.get().to_string())
        .fetch_optional(&self.database)
        .await
        .map_err(|error| error.to_string())
    }

    async fn preference_score(&self, guild_id: GuildId, track: &TrackData) -> Result<i64, String> {
        let (title, artist) = original_metadata(track);
        let id = track_id(&artist, &title, track.info.isrc.as_deref());
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(preference), 0) FROM user_track_preferences WHERE guild_id=? AND track_id=?",
        )
        .bind(guild_id.get().to_string())
        .bind(id)
        .fetch_one(&self.database)
        .await
        .map_err(|error| error.to_string())
    }

    async fn record_event(
        &self,
        guild_id: GuildId,
        track: &TrackData,
        event_type: &str,
    ) -> Result<(), String> {
        if is_dj_segment(track) {
            return Ok(());
        }
        let track_id = self.ensure_track(track).await?;
        let requested_by = track
            .user_data
            .as_ref()
            .and_then(|data| data.get("requester_id"))
            .and_then(serde_json::Value::as_u64)
            .map(|id| id.to_string());
        sqlx::query(
            "INSERT INTO play_events (guild_id, track_id, requested_by_user_id, source_provider, event_type, detail_json) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(guild_id.get().to_string())
        .bind(track_id)
        .bind(requested_by)
        .bind(&track.info.source_name)
        .bind(event_type)
        .bind(track.user_data.as_ref().map(serde_json::Value::to_string))
        .execute(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn announce_radio_track(
        &self,
        client: &LavalinkClient,
        guild_id: GuildId,
        track: &TrackData,
    ) -> Result<(), String> {
        if !is_radio_track(track) {
            return Ok(());
        }
        let Some(session) = self
            .session(guild_id)
            .await
            .filter(|session| session.enabled)
        else {
            return Ok(());
        };
        if session.text_channel_id == 0 {
            return Ok(());
        }
        let Some(player) = client.get_player_context(guild_id) else {
            return Ok(());
        };
        let data = player
            .data::<(ChannelId, Arc<Http>)>()
            .map_err(|error| error.to_string())?;
        let (title, artist) = original_metadata(track);
        let reason = current_reason(track).unwrap_or("Selected by the active radio vibe.");
        let source = track
            .info
            .uri
            .as_deref()
            .unwrap_or("Source URL unavailable");
        ChannelId::new(session.text_channel_id)
            .say(
                &data.1,
                format!(
                    "📻 **Radio picked:** `{}` — `{}`\n{}\n{}",
                    discord_safe(&title),
                    discord_safe(&artist),
                    discord_safe(reason),
                    source
                ),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn ensure_track(&self, track: &TrackData) -> Result<String, String> {
        let (title, artist) = original_metadata(track);
        let track_id = track_id(&artist, &title, track.info.isrc.as_deref());
        let metadata = serde_json::to_string(track).map_err(|error| error.to_string())?;
        sqlx::query(
            "INSERT INTO tracks (id, canonical_artist, canonical_title, duration_ms, isrc, metadata_json) \
             VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET \
             canonical_artist=excluded.canonical_artist, canonical_title=excluded.canonical_title, \
             duration_ms=excluded.duration_ms, metadata_json=excluded.metadata_json, updated_at=CURRENT_TIMESTAMP",
        )
        .bind(&track_id)
        .bind(artist)
        .bind(title)
        .bind(track.info.length as i64)
        .bind(track.info.isrc.as_deref())
        .bind(metadata)
        .execute(&self.database)
        .await
        .map_err(|error| error.to_string())?;
        Ok(track_id)
    }
}

fn track_start(
    client: LavalinkClient,
    _session_id: String,
    event: &TrackStart,
) -> BoxFuture<'static, ()> {
    let event = event.clone();
    Box::pin(async move {
        tokio::spawn(async move {
            let Some(radio) = RADIO_RUNTIME.get().cloned() else {
                return;
            };
            let guild_id = GuildId::new(event.guild_id.0);
            if let Err(error) = radio.record_event(guild_id, &event.track, "started").await {
                warn!(%error, "failed to record track start");
            }
            if let Err(error) = radio
                .announce_radio_track(&client, guild_id, &event.track)
                .await
            {
                warn!(%error, "failed to announce radio track source");
            }
            if let Err(error) = radio.replenish(&client, guild_id, false, None).await {
                warn!(%error, "radio replenishment failed");
            }
        });
    })
}

fn track_end(
    client: LavalinkClient,
    _session_id: String,
    event: &TrackEnd,
) -> BoxFuture<'static, ()> {
    let event = event.clone();
    Box::pin(async move {
        tokio::spawn(async move {
            let Some(radio) = RADIO_RUNTIME.get().cloned() else {
                return;
            };
            let guild_id = GuildId::new(event.guild_id.0);
            let event_type = match event.reason {
                TrackEndReason::Finished => "completed",
                TrackEndReason::Replaced | TrackEndReason::Stopped => "skipped",
                TrackEndReason::LoadFailed | TrackEndReason::Cleanup => "failed",
            };
            if let Err(error) = radio.record_event(guild_id, &event.track, event_type).await {
                warn!(%error, "failed to record track end");
            }
            if let Err(error) = radio.replenish(&client, guild_id, false, None).await {
                warn!(%error, "radio replenishment failed");
            }
        });
    })
}

fn websocket_closed(
    client: LavalinkClient,
    _session_id: String,
    event: &WebSocketClosed,
) -> BoxFuture<'static, ()> {
    let event = event.clone();
    Box::pin(async move {
        tokio::spawn(async move {
            warn!(
                guild_id = event.guild_id.0,
                code = event.code,
                by_remote = event.by_remote,
                "Discord voice websocket closed; Lavalink recovery remains active"
            );
            sleep(Duration::from_secs(2)).await;
            let Some(radio) = RADIO_RUNTIME.get().cloned() else {
                return;
            };
            let guild_id = GuildId::new(event.guild_id.0);
            if let Err(error) = radio.replenish(&client, guild_id, false, None).await {
                warn!(%error, "post-recovery radio check failed");
            }
        });
    })
}

async fn player_data(
    client: &LavalinkClient,
    guild_id: GuildId,
) -> Result<Option<TrackData>, String> {
    let Some(player) = client.get_player_context(guild_id) else {
        return Ok(None);
    };
    player
        .get_player()
        .await
        .map(|player| player.track)
        .map_err(|error| error.to_string())
}

pub fn current_reason(track: &TrackData) -> Option<&str> {
    track.user_data.as_ref()?.get("radio_reason")?.as_str()
}

pub fn original_metadata(track: &TrackData) -> (String, String) {
    let data = track.user_data.as_ref();
    let title = data
        .and_then(|value| value.get("original_title"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&track.info.title);
    let artist = data
        .and_then(|value| value.get("original_artist"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&track.info.author);
    (title.to_owned(), artist.to_owned())
}

fn is_dj_segment(track: &TrackData) -> bool {
    track
        .user_data
        .as_ref()
        .and_then(|value| value.get("dj_segment"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn is_radio_track(track: &TrackData) -> bool {
    track
        .user_data
        .as_ref()
        .and_then(|value| value.get("radio_track"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn discord_safe(value: &str) -> String {
    value
        .replace('`', "'")
        .replace('@', "@\u{200b}")
        .replace(['\r', '\n'], " ")
}

fn track_id(artist: &str, title: &str, isrc: Option<&str>) -> String {
    isrc.map(|value| format!("isrc:{}", value.to_ascii_uppercase()))
        .unwrap_or_else(|| format!("track:{}:{}", normalize(artist), normalize(title)))
}

fn discovery_query(vibe: &str, flavor: Option<&str>, recent_artist: Option<&str>) -> String {
    match flavor {
        Some("crate") => format!("{vibe} underground hidden gem music"),
        Some("similar") => format!("{} similar artists music", recent_artist.unwrap_or(vibe)),
        Some("surprise") => format!("{vibe} music discovery"),
        _ => match recent_artist {
            Some(artist) => format!("{vibe} music similar to {artist}"),
            None => format!("{vibe} official audio music"),
        },
    }
}

fn recommendation_reason(vibe: &str, flavor: Option<&str>, recent_artist: Option<&str>) -> String {
    match flavor {
        Some("crate") => format!("A lesser-known pick for the `{vibe}` vibe."),
        Some("similar") => format!(
            "Selected for musical proximity to {}.",
            recent_artist.unwrap_or("the recent session")
        ),
        Some("surprise") => format!("A discovery pick compatible with `{vibe}`."),
        _ => match recent_artist {
            Some(artist) => format!("Fits `{vibe}` and follows the recent {artist} direction."),
            None => format!("Selected for the explicit `{vibe}` radio vibe."),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendation_queries_use_only_music_context() {
        assert_eq!(
            discovery_query("late-night", Some("crate"), Some("Burial")),
            "late-night underground hidden gem music"
        );
        assert!(recommendation_reason("cozy", None, Some("Burial")).contains("Burial"));
    }

    #[test]
    fn canonical_track_ids_are_stable() {
        assert_eq!(
            track_id("Burial", "Archangel", None),
            track_id("BURIAL", "Archangel!", None)
        );
        assert_eq!(track_id("x", "y", Some("abc")), "isrc:ABC");
    }

    #[test]
    fn radio_announcements_escape_discord_metadata() {
        assert_eq!(
            discord_safe("`song` by @everyone\nnow"),
            "'song' by @\u{200b}everyone now"
        );
    }
}
