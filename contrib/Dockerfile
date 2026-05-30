# syntax=docker/dockerfile:1
# Multi-stage build: compile with musl for a fully-static binary,
# then package into a minimal scratch image.

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

RUN cargo build --release --target x86_64-unknown-linux-musl

# ── Stage 2: runtime ──────────────────────────────────────────────────────────
FROM scratch

# Copy the static binary.
COPY --from=builder /src/target/x86_64-unknown-linux-musl/release/conduit /conduit

# Expose the default proxy port.
EXPOSE 8080

# Default config path inside the container.
ENV CONDUIT_CONFIG=/etc/conduit/conduit.json

# Run as an unprivileged user (scratch has no /etc/passwd, so use a numeric UID).
# UID 65534 is the conventional "nobody" UID on Linux.
USER 65534:65534

ENTRYPOINT ["/conduit"]
CMD ["-c", "/etc/conduit/conduit.json"]
