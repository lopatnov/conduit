#!/usr/bin/env bash
# What is in Conduit's `--no-default-features` binary? Measures (1) the tip built with the repo's release profile minus
# strip so cargo-bloat can read symbols, (2) its stripped size, (3) the smallest Pingora proxy built with the SAME profile.
# Output: ~/perf/size_report.txt (progress in ~/perf/size_progress.txt). Needs ~/perf/conduit (a clone) and ~/perf/pp (floors/pingora_min.rs as a crate).
set -uo pipefail
cd ~/perf
P=~/perf/size_progress.txt; R=~/perf/size_report.txt
: > "$P"; : > "$R"
say() { echo "[$(date +%H:%M:%S)] $*" >> "$P"; }

say "cargo install cargo-bloat"
cargo install cargo-bloat --locked >> "$P" 2>&1 || { say "cargo-bloat install FAILED"; }

say "fetch tip + worktree"
git -C ~/perf/conduit fetch -q origin claude/cargo-workspace-features-23qxfr >> "$P" 2>&1
if [ ! -d ~/perf/size ]; then
  git -C ~/perf/conduit worktree add --detach ~/perf/size origin/claude/cargo-workspace-features-23qxfr >> "$P" 2>&1
else
  git -C ~/perf/size checkout -q --detach origin/claude/cargo-workspace-features-23qxfr >> "$P" 2>&1
fi
say "tip: $(git -C ~/perf/size log --oneline -1)"

say "build no-default-features (profile: lto=true cgu=1, strip=none for symbols)"
cd ~/perf/size
export CARGO_PROFILE_RELEASE_STRIP=none
cargo build --release --no-default-features -p lopatnov-conduit >> "$P" 2>&1 || say "conduit build FAILED"
BIN=target/release/conduit
if [ -x "$BIN" ]; then
  T=$(mktemp -d); cp "$BIN" "$T/c"; strip "$T/c"
  {
    echo "== conduit --no-default-features, tip $(git log --oneline -1 | cut -c1-7)"
    echo "unstripped bytes: $(stat -c %s "$BIN")"
    echo "stripped   bytes: $(stat -c %s "$T/c")"
  } >> "$R"
  rm -rf "$T"
  say "cargo bloat --crates"
  cargo bloat --release --no-default-features -p lopatnov-conduit --crates -n 60 >> "$R" 2>> "$P" || say "bloat crates FAILED"
  echo >> "$R"; echo "== top 25 functions" >> "$R"
  cargo bloat --release --no-default-features -p lopatnov-conduit -n 25 >> "$R" 2>> "$P" || say "bloat fns FAILED"
fi

say "build smallest Pingora proxy with the same profile"
rm -rf ~/perf/pp_fat && cp -r ~/perf/pp ~/perf/pp_fat && cd ~/perf/pp_fat && rm -rf target
export CARGO_PROFILE_RELEASE_LTO=true CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 CARGO_PROFILE_RELEASE_STRIP=none
cargo build --release >> "$P" 2>&1 || say "pp build FAILED"
PB=target/release/pp
if [ -x "$PB" ]; then
  T=$(mktemp -d); cp "$PB" "$T/p"; strip "$T/p"
  {
    echo
    echo "== smallest Pingora ProxyHttp proxy (pingora-core+proxy 0.9, rustls), lto=true cgu=1"
    echo "stripped   bytes: $(stat -c %s "$T/p")"
  } >> "$R"
  rm -rf "$T"
fi
say "DONE"
