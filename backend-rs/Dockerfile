# The Rust Arena. Built from the REPO ROOT (it embeds backend/app/*.json at
# compile time):  docker build -f backend-rs/Dockerfile .
# ops/release.sh does this for you with ARENA_IMPL=rs.
FROM rust:1-slim AS build
WORKDIR /src
COPY backend-rs/Cargo.toml backend-rs/Cargo.lock backend-rs/
COPY backend-rs/src backend-rs/src
# sqlx::migrate!() is a compile-time macro: it reads migrations/ during
# `cargo build` and embeds the SQL in the binary. Without this COPY the build
# fails, and Rust now owns the schema, so there is nothing else to apply it.
COPY backend-rs/migrations backend-rs/migrations
COPY backend/app/kart_tracks.json backend/app/platformer_levels.json \
     backend/app/fps_map.json backend/app/golf_courses.json backend/app/strike_map.json backend/app/
# Its own COPY: the creature data sits a directory deeper, and a single COPY
# cannot flatten two destinations.
COPY backend/app/data/pokemon.json backend/app/data/
RUN cd backend-rs && cargo build --release --locked --bin arena

FROM debian:bookworm-slim
RUN apt-get update -qq && apt-get install -y -qq --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/backend-rs/target/release/arena /usr/local/bin/arena
RUN printf '#!/bin/sh\nexec /usr/local/bin/arena --health\n' > /usr/local/bin/arena-health \
    && chmod 0755 /usr/local/bin/arena-health

ARG ARENA_VERSION=dev
ENV ARENA_VERSION=$ARENA_VERSION \
    ARENA_BIND_PORT=8080 \
    RUST_LOG=info
EXPOSE 8080
CMD ["/usr/local/bin/arena"]
