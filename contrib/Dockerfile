# syntax=docker/dockerfile:1
# Multi-stage build: compile with musl for a fully-static binary,
# then package into a minimal scratch image.

# ── Stage 1: build ────────────────────────────────────────────────────────────
FROM rust:1-alpine AS builder

# musl-dev provides the C linker and libc headers needed for musl builds.
RUN apk add --no-cache musl-dev

WORKDIR /src
COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl

# ── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM scratch

# Copy the static binary.
COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/conduit /conduit

# Expose the default proxy port.
EXPOSE 8080

# Default config path inside the container.
ENV CONDUIT_CONFIG=/etc/conduit/conduit.json

ENTRYPOINT ["/conduit"]
CMD ["-c", "/etc/conduit/conduit.json"]
