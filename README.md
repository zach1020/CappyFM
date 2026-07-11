# CappyFM

Privacy-first Discord music bot and capybara radio host. The current resolver milestone includes YouTube search/URL playback, SoundCloud playback, Spotify and Apple Music metadata resolution to scored YouTube matches, canonical music records, per-guild queues, and Lavalink v4 voice.

## Prerequisites

- Rust 1.85 or newer
- Docker with Compose
- A Discord application and bot token

Enable **Message Content Intent** for the bot in Discord's Developer Portal. CappyFM needs it only for the requested `cap!` / `cappy!` prefix interface. The gateway handler immediately discards bot messages, DMs, and any guild message without a configured prefix. It neither logs nor stores ordinary message content.

Invite the bot with View Channels, Send Messages, Read Message History, Connect, Speak, and Use Voice Activity.

## Local setup

```sh
cp .env.example .env
# Fill in DISCORD_TOKEN, DISCORD_APPLICATION_ID, and a strong LAVALINK_PASSWORD.
cargo test
cargo run -p cappy-bot
```

To start both services:

```sh
docker compose up --build
```

Join a voice channel, then try `cap!play Burial Archangel`. You can also queue YouTube, SoundCloud, Spotify, and Apple Music links. Spotify and Apple Music are metadata sources and are honestly labeled when matched to YouTube playback. Use `cap!queue`, `cap!now`, `cap!pause`, `cap!resume`, `cap!skip`, `cap!stop`, and `cap!leave` for playback control.

Spotify links require `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET` in `.env`. Public Apple Music links currently resolve without a token, but `APPLE_MUSIC_API_TOKEN` is supported for reliable access. After changing credentials, recreate Lavalink with `docker compose up -d --force-recreate lavalink`.

## Bot avatar

The canonical bot artwork is [assets/cappyfm-logo.png](assets/cappyfm-logo.png). After filling in `.env`, apply it to the Discord bot once with:

```sh
cargo run -p cappy-bot -- --set-avatar assets/cappyfm-logo.png
```

This updates the bot profile through Discord's API and exits. Normal startup never rewrites the avatar.

## Privacy invariants

- Only guild messages beginning with `cap!` or `cappy!` reach command dispatch.
- DMs and bot-authored messages are ignored.
- Structured production logs contain IDs and parsed command names, never complete message bodies.
- Configuration refuses to start if ordinary-message ignoring or command-body privacy is disabled.
- Search-query logging is rejected in production.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
docker compose config
```

Radio and AI/TTS remain intentionally deferred to later milestones from the design document.
