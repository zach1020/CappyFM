# Security and privacy

## Reporting

Do not open a public issue containing a bot token, API key, Lavalink password, user ID dataset, or database. Revoke exposed credentials immediately, then report the issue privately to the repository owner.

## Privacy boundary

CappyFM rejects DMs, bot-authored messages, and messages without a configured command prefix before command logging or dispatch. Logs contain structured IDs and parsed command names, never ordinary message bodies or search-query text.

The recommendation system receives only explicit vibe commands and music activity: requested tracks, plays, skips, likes, dislikes, source metadata, time, and server music history. It does not inspect chat sentiment, voice audio, presence, or unrelated activity.

## Secrets and data

- Secrets belong in `.env` or a deployment secret manager.
- The optional Spotify OAuth refresh token is stored at `data/spotify-refresh-token` with owner-only permissions on Unix and is ignored by Git. Authorization requests only `playlist-read-private`.
- `.env`, key files, databases, runtime data, plugins, and logs are ignored by Git.
- SQLite stores music history and settings, not ordinary Discord conversation.
- Internal DJ audio is served only on the Compose network and held in memory.
- MusicBrainz facts are stored with source URL and confidence.

## Operational controls

Commands are rate-limited per user. Expensive AI and discovery commands have a separate cooldown. Lavalink requests have bounded retries and timeouts; AI/TTS failures back off and music fails open. URL validation rejects arbitrary and private-network playback destinations.
