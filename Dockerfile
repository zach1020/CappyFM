FROM rust:1.96-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --locked --release -p cappy-bot

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/cappy-bot /usr/local/bin/cappy-bot
COPY config ./config
COPY migrations ./migrations
RUN mkdir -p /app/data
CMD ["cappy-bot"]

