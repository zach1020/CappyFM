# CappyFM

Privacy-first Discord music bot and capybara radio host. This repository currently implements milestone 0 plus the privacy-safe command shell: configuration, SQLite migrations, Discord connectivity, strict prefix parsing, `help`, `privacy`, and a Lavalink v4 development service.

## Prerequisites

- Rust 1.85 or newer
- Docker with Compose
- A Discord application and bot token

Enable **Message Content Intent** for the bot in Discord's Developer Portal. CappyFM needs it only for the requested `cap!` / `cappy!` prefix interface. The gateway handler immediately discards bot messages, DMs, and any guild message without a configured prefix. It neither logs nor stores ordinary message content.

Invite the bot with the minimal permissions needed for this milestone: View Channels, Send Messages, and Read Message History. Voice permissions will be added with playback.

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

Then try `cap!help` and `cap!privacy` in a guild text channel.

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

Playback, queueing, source resolution, and AI/TTS are intentionally deferred to later milestones from the design document.
