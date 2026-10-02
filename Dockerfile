# syntax=docker/dockerfile:1.7
#
# Multi-stage build: compile in a full Rust image, ship a slim runtime image
# that contains only the binary, static files and content. Templates and SQL
# migrations are compiled into the binary.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# BuildKit cache mounts keep the cargo registry and target dir between builds,
# so only changed crates are recompiled.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
    && cp target/release/system-design-transparent /usr/local/bin/app

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /app app
WORKDIR /app
COPY --from=build /usr/local/bin/app /usr/local/bin/app
COPY static ./static
COPY content ./content

# Never run as root inside the container.
USER app
ENV BIND_ADDR=0.0.0.0:3000 \
    RUST_LOG=info,sqlx=warn,tower_http=info
EXPOSE 3000
HEALTHCHECK --interval=15s --timeout=3s --start-period=30s --retries=3 \
    CMD curl -fsS http://localhost:3000/healthz || exit 1
CMD ["app", "serve"]
