# CappyFM

Privacy-first Discord music bot and capybara radio host. The current DJ milestone includes multi-source playback, scored metadata resolution, per-guild queues, safe volume control, five DJ voice presets, bounded AI-written intros, TTS, session-scoped talk controls, and silent fallback.

## Prerequisites

- Rust 1.85 or newer
- Docker with Compose
- A Discord application and bot token

Enable **Message Content Intent** for the bot in Discord's Developer Portal. CappyFM needs it only for the requested `cap!` / `capy!` / `cappy!` prefix interface. The gateway handler immediately discards bot messages, DMs, and any guild message without a configured prefix. It neither logs nor stores ordinary message content.

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

Join a voice channel, then try `cap!play Burial Archangel`. You can also queue YouTube, SoundCloud, Spotify, and Apple Music links. Spotify and Apple Music are metadata sources and are honestly labeled when matched to YouTube playback. Use `cap!queue`, `cap!now`, `cap!pause`, `cap!resume`, `cap!skip`, `cap!clear`, `cap!stop`, and `cap!leave` for playback control. `cap!clear` removes upcoming items while leaving the current song alone.

DJ controls include `cap!voice list`, `cap!voice dry`, `cap!voice preview cozy`, `cap!personality quirky`, `cap!talk less`, `cap!shutup`, and `cap!intro`. By default, Cappy opens each listening session and returns after a rotating 2–4 songs with a varied 70–110-word transition or recap. `cap!volume 0-100` adjusts the shared stream; each listener can privately right-click CappyFM in voice and use Discord's User Volume slider. New sessions start at a conservative 60% shared volume.

Add `OPENAI_API_KEY` to `.env` to enable AI-written spoken intros. `OPENAI_TEXT_MODEL` defaults to `gpt-5.4-nano`, `OPENAI_TTS_MODEL` defaults to `tts-1`, and `CAPPY_DJ_GAIN=1.18` gives speech a modest music-independent boost with clipping protection. Without a key, CappyFM uses validated templates and never blocks music; TTS-dependent commands post the copy in text. The bot discloses that generated DJ voices are AI-generated.

Apple Music is a metadata source, not CappyFM's playback session. Apple links are matched to playable YouTube audio, so starting playback in Apple's Music app does not interrupt the bot.

Spotify links require `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET` in `.env`. Public Apple Music links currently resolve without a token, but `APPLE_MUSIC_API_TOKEN` is supported for reliable access. After changing credentials, recreate Lavalink with `docker compose up -d --force-recreate lavalink`.

## Bot avatar

The canonical bot artwork is [assets/cappyfm-logo.png](assets/cappyfm-logo.png). After filling in `.env`, apply it to the Discord bot once with:

```sh
cargo run -p cappy-bot -- --set-avatar assets/cappyfm-logo.png
```

This updates the bot profile through Discord's API and exits. Normal startup never rewrites the avatar.

## Privacy invariants

- Only guild messages beginning with `cap!`, `capy!`, or `cappy!` reach command dispatch.
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
