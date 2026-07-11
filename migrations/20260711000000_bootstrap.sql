CREATE TABLE IF NOT EXISTS guild_settings (
    guild_id TEXT PRIMARY KEY,
    command_channel_id TEXT,
    dj_role_id TEXT,
    default_vibe TEXT NOT NULL DEFAULT 'open-format',
    default_voice TEXT NOT NULL DEFAULT 'late-night',
    default_personality TEXT NOT NULL DEFAULT 'quirky',
    default_talk_frequency TEXT NOT NULL DEFAULT 'normal',
    max_queue_per_user INTEGER NOT NULL DEFAULT 20,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

