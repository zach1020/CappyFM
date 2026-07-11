FROM rust:1.96-bookworm AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,id=cappy-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cappy-target,target=/app/target \
    cargo build --locked --release -p cappy-bot && \
    cp /app/target/release/cappy-bot /tmp/cappy-bot

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /tmp/cappy-bot /usr/local/bin/cappy-bot
COPY config ./config
COPY migrations ./migrations
RUN mkdir -p /app/data
CMD ["cappy-bot"]
