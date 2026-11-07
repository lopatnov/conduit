# syntax=docker/dockerfile:1
# Multi-stage build: compile with musl for a fully-static binary,
# then package into a minimal scratch image.
#
# Build arguments:
#   FEATURES — optional comma-separated feature flags to enable.
#              Leave empty (default) for the minimal build (default = [] —
#              core proxy only, no jwt/consumers/forward-auth/cache/acme).
#              The published images (release.yml `docker` job) are built with
#              FEATURES=full (the plain tags), standard, gateway and static-server,
#              see Cargo.toml [features]. FEATURES=full enables every optional feature.
#   NO_DEFAULT_FEATURES — "true" adds --no-default-features (gateway, static-server).
#
# Examples:
#   docker build .                                  # minimal (default = [])
#   docker build --build-arg FEATURES=standard .    # standard (published as :latest-standard)
#   docker build --build-arg FEATURES=full .        # full (published as :latest)
#   docker build --build-arg FEATURES=gateway --build-arg NO_DEFAULT_FEATURES=true .  # gateway
#   docker build --build-arg FEATURES=otlp,wasm .   # custom subset

# ── Stage 1: build ────────────────────────────────────────────────────────────
# Use the latest stable Rust on Alpine — keeps in sync with the toolchain
# used by CI builds so dependencies never require a newer rustc than available.
FROM rust:alpine AS builder

# build-base  — gcc, g++, make, musl-dev
# cmake        — required by libz-ng-sys (builds zlib-ng from source)
# perl         — required by some OpenSSL/ring build scripts
# linux-headers — kernel headers for musl target
RUN apk add --no-cache build-base cmake perl linux-headers

WORKDIR /src
COPY . .

# Accept optional feature flags at build time.
ARG FEATURES=""
# Set to "true" to build with --no-default-features (the gateway and static-server bundles are meant for that).
ARG NO_DEFAULT_FEATURES=""

# No --target: on Alpine the host target is musl (x86_64-unknown-linux-musl on an amd64 builder,
# aarch64-unknown-linux-musl on an arm64 one), which gives a fully static binary for the architecture the
# image is built on. The release workflow builds each architecture on its own native runner.
RUN flags=""; \
    if [ "$NO_DEFAULT_FEATURES" = "true" ]; then flags="--no-default-features"; fi; \
    if [ -n "$FEATURES" ]; then \
      cargo build --release $flags --features "$FEATURES"; \
    else \
      cargo build --release $flags; \
    fi

# ── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM scratch

# Copy the static binary.
COPY --from=builder /src/target/release/conduit /conduit

# Expose the default proxy port.
EXPOSE 8080

# Default config path inside the container.
ENV CONDUIT_CONFIG=/etc/conduit/conduit.json

# Run as an unprivileged user (scratch has no /etc/passwd, so use a numeric UID).
# UID 65534 is the conventional "nobody" UID on Linux.
USER 65534:65534

ENTRYPOINT ["/conduit"]
CMD ["-c", "/etc/conduit/conduit.json"]
