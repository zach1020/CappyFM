# CappyFM deployment guide

## Production prerequisites

- A Discord application with Message Content Intent enabled.
- Docker Engine with Compose v2.
- A host with persistent storage for `./data`.
- Spotify credentials for Spotify links and an OpenAI API key for spoken DJ segments.

Copy `.env.example` to `.env` and fill in every required secret. Never commit `.env`. Use a unique Lavalink password and restrict port `2333` at the host firewall; it is published for local diagnostics but should not be internet-accessible.

## Start and update

```sh
docker compose config
docker compose up -d --build
docker compose ps
docker compose logs --tail=100 bot lavalink
```

Database migrations run automatically before the Discord client starts. The SQLite database is persisted at `./data/cappyfm.db`. Back up the database with the bot stopped or through SQLite's online backup tooling.

To update, pull the reviewed revision and rerun `docker compose up -d --build`. Do not delete `./data` during an update.

## Health and metrics

From inside the Compose network, the bot serves:

- `GET http://bot:8080/healthz`
- `GET http://bot:8080/metrics`

The metrics endpoint contains aggregate command, error, rate-limit, and uptime counters. It never exposes command text, search queries, tokens, or ordinary Discord messages.

Use `cap!health` for a server-scoped database, player, queue, and radio check.

## Discord permissions

Grant only View Channels, Send Messages, Embed Links, Read Message History, Connect, Speak, and Use Voice Activity. Administrators need Manage Server to alter `cap!settings` defaults.

## Rollback

Keep the prior container image or Git revision and a database backup. Application rollback is safe when the older binary understands all applied migrations. Never manually edit the SQLx migration ledger.
