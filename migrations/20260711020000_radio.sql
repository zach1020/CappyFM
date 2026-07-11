CREATE TABLE IF NOT EXISTS radio_sessions (
    guild_id TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL DEFAULT 0,
    vibe TEXT NOT NULL DEFAULT 'open-format',
    started_by_user_id TEXT,
    text_channel_id TEXT,
    started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS play_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    requested_by_user_id TEXT,
    source_provider TEXT NOT NULL,
    event_type TEXT NOT NULL,
    listened_ms INTEGER,
    detail_json TEXT,
    occurred_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(track_id) REFERENCES tracks(id)
);

CREATE INDEX IF NOT EXISTS play_events_guild_time_idx
    ON play_events(guild_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS play_events_track_idx ON play_events(track_id);

CREATE TABLE IF NOT EXISTS user_track_preferences (
    guild_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    preference INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (guild_id, user_id, track_id),
    FOREIGN KEY(track_id) REFERENCES tracks(id)
);

CREATE TABLE IF NOT EXISTS verified_facts (
    track_id TEXT PRIMARY KEY,
    fact_text TEXT NOT NULL,
    source_url TEXT NOT NULL,
    provider TEXT NOT NULL,
    confidence REAL NOT NULL,
    fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(track_id) REFERENCES tracks(id)
);
