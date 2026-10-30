# docker build -f deploy/Dockerfile -t ultrachain-bot .
# docker run -d --restart unless-stopped -v $PWD/config.toml:/app/config.toml:ro \
#   -v $PWD/data:/app/data -v $PWD/signals.jsonl:/app/signals.jsonl \
#   -e ALERT_WEBHOOK_URL=... ultrachain-bot
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini \
    && rm -rf /var/lib/apt/lists/* && useradd -u 1000 -m bot
WORKDIR /app
COPY --from=build /src/target/release/ultrachain-bot /usr/local/bin/
USER 1000
ENTRYPOINT ["tini", "--", "ultrachain-bot"]
CMD ["run", "-c", "/app/config.toml"]
