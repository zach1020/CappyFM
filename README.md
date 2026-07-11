# CappyFM

Privacy-first Discord music bot and capybara radio host. CappyFM includes multi-source playback, continuous vibe radio, explainable music-only recommendations, provenance-backed facts, persistent taste signals, rich now-playing output, configurable DJ chatter, and fail-open recovery.

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
./run
```

The launcher starts Docker Desktop on macOS when needed, builds CappyFM, and
runs the bot and Lavalink in the background. Use `./run status`, `./run logs`,
`./run restart`, or `./run stop` for routine local operation.

On Windows, use `run.cmd` instead. It can also start Docker Desktop and accepts
the same `status`, `logs`, `restart`, and `stop` commands.

Join a voice channel, then try `cap!play Burial Archangel`. You can also queue YouTube, SoundCloud, Spotify, and Apple Music links. Spotify and Apple Music are metadata sources and are honestly labeled when matched to YouTube playback. Use `cap!queue`, `cap!requested`, `cap!now`, `cap!remove`, `cap!move`, `cap!shuffle`, `cap!pause`, `cap!resume`, `cap!skip`, `cap!clear`, `cap!stop`, and `cap!leave` for playback control. DJ segments remain attached to their songs during queue edits.

DJ controls include `cap!voice list`, `cap!voice dry`, `cap!voice preview cozy`, `cap!personality quirky`, `cap!talk less`, `cap!shutup`, and `cap!intro`. By default, Cappy opens each listening session and returns after a rotating 2–4 songs with a varied 70–110-word transition or recap. A fresh bot start, an explicit stop/leave, or returning after playback and the queue have gone idle starts a new spoken session when chatter is enabled. DJ copy stays focused on the requested music, supplied facts, and music-related personality tangents. `cap!volume 0-100` adjusts the shared stream; each listener can privately right-click CappyFM in voice and use Discord's User Volume slider. New sessions start at a conservative 60% shared volume.

Add `OPENAI_API_KEY` to `.env` to enable AI-written spoken intros. `OPENAI_TEXT_MODEL` defaults to `gpt-5.4-nano` and `OPENAI_TTS_MODEL` defaults to `tts-1`. Without a key, CappyFM uses validated templates and never blocks music; TTS-dependent commands post the copy in text. The bot discloses that generated DJ voices are AI-generated.

Apple Music is a metadata source, not CappyFM's playback session. Apple links are matched to playable YouTube audio, so starting playback in Apple's Music app does not interrupt the bot.

## Radio mode

Run `cap!radio late-night coding` to start a continuous station, or `cap!radio off` to stop automatic replenishment and remove queued radio tracks and radio DJ segments without touching direct requests. Radio refills the queue when fewer than three playable tracks remain, while direct requests are inserted ahead of radio selections. When a radio selection starts, Cappy posts its title, artist, recommendation reason, and clickable video source in the radio command channel. Recommendation context is limited to the explicit vibe, recent music history, and music controls such as likes, dislikes, and skips; ordinary Discord chat is never inspected.

Use `cap!vibe`, `cap!surprise`, `cap!crate`, `cap!similar`, and `cap!why` to steer or inspect discovery. `cap!like`, `cap!dislike`, `cap!favorites`, `cap!history`, and `cap!stats` manage the transparent taste model. `cap!fact` uses cached, provenance-backed MusicBrainz release data when a confident ISRC match exists, and otherwise returns attributed playback metadata rather than inventing trivia.

## Administration and operations

`cap!settings` shows persistent server defaults. Members with Manage Server can update `vibe`, `voice`, `personality`, `talk`, and an optional command channel; see `cap!help admin`. Defaults are restored when a new voice session connects. `cap!health` checks the server's database, Lavalink player, queue, and radio state.

Commands are limited to five per user per ten seconds, with a five-second cooldown for expensive playback, discovery, and AI operations. Lavalink loads use bounded timeouts and one retry. Repeated TTS failures enter a one-minute backoff, and automatic chatter always fails open to music.

Spotify links require `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET` in `.env`. Public Apple Music links currently resolve without a token, but `APPLE_MUSIC_API_TOKEN` is supported for reliable access. After changing credentials, recreate Lavalink with `docker compose up -d --force-recreate lavalink`.

Spotify's playlist-items API requires user authorization and only exposes playlists owned by or collaboratively shared with that user. Add `http://127.0.0.1:8888/callback` to the Spotify app's redirect URIs, then run `./run spotify-login` (`run.cmd spotify-login` on Windows) once. Open the URL printed by the launcher and approve `playlist-read-private`. CappyFM stores the refresh token at `data/spotify-refresh-token`, which is ignored by Git and mounted only into the bot container. Access tokens are refreshed automatically. Spotify supplies metadata only; playback is still matched to YouTube, and explicit versions remain preferred.

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

Internal operational endpoints are available to the Docker network at `http://bot:8080/healthz` and `http://bot:8080/metrics`. See [DEPLOYMENT.md](DEPLOYMENT.md) for production setup and [SECURITY.md](SECURITY.md) for the privacy and secret-handling model.
