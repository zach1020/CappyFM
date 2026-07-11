CREATE TABLE IF NOT EXISTS tracks (
    id TEXT PRIMARY KEY,
    canonical_artist TEXT NOT NULL,
    canonical_title TEXT NOT NULL,
    album TEXT,
    duration_ms INTEGER,
    isrc TEXT,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS track_sources (
    id TEXT PRIMARY KEY,
    track_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    provider_track_id TEXT,
    original_url TEXT,
    playable_uri TEXT,
    confidence REAL,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY(track_id) REFERENCES tracks(id)
);

CREATE INDEX IF NOT EXISTS track_sources_track_id_idx ON track_sources(track_id);
CREATE INDEX IF NOT EXISTS track_sources_provider_track_id_idx
    ON track_sources(provider, provider_track_id);
