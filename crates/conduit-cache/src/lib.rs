//! HTTP response caching crate for conduit's feature-driven Cargo workspace
//! migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#135](https://github.com/lopatnov/conduit/issues/135)).
//!
//! ## Scope
//!
//! Owns [`config::CacheConfig`] (the `proxy.*.cache` config struct), the
//! always-compiled cache-key/policy logic (`cache` module — `build_cache_key`,
//! `should_cache_request`, `response_cacheable`, `cache_storage`,
//! `cache_lock`, `should_early_refresh`), the disk storage backend (`disk`
//! module), the Redis storage backend (`redis` module, gated behind this
//! crate's own `redis` feature), and [`ctx::CacheReqState`] — the per-request
//! cache state struct.
//!
//! ## Always-compiled vs. gated (mirrors `conduit-faults`/`conduit-auth-jwt`)
//!
//! `CacheConfig` is compiled into **every** conduit build — like
//! `OtlpConfig`/`AcmeConfig`/`TcpConfig`/`FaultInjectionConfig`/
//! `JwtAuthConfig` — because `ProxyRouteConfig.cache` is not itself
//! feature-gated (a config file that sets `cache` without `--features cache`
//! must still parse cleanly and get an explicit `feature_warnings()` warning,
//! not a silent-drop or a hard parse error).
//!
//! Most of the `cache` module is **also** always compiled, matching its
//! pre-extraction gating exactly (`src/proxy/cache.rs` had no
//! `#![cfg(feature = "cache")]` file-level gate before this move): Pingora's
//! `ProxyHttp` trait calls `cache_key_callback`/`response_cache_filter` on
//! every request regardless of whether the `cache` feature is compiled in
//! (the root crate's `request_cache_filter` never *enables* Pingora's cache
//! without the feature, so these calls are no-ops in practice, but the code
//! backing them — `build_cache_key`, `response_cacheable`, `cache_storage`,
//! `should_cache_request`, `cache_lock` — still has to compile). The Admin
//! API's `DELETE /cache/purge` handler (`src/admin/api.rs`) also calls
//! `cache::build_cache_key`/`cache::cache_storage` unconditionally. The
//! `disk` module is likewise always compiled, matching the pre-extraction
//! `src/proxy/cache_disk.rs` (also no feature gate) — it's only ever
//! *invoked* from behind the root crate's own `#[cfg(feature = "cache")]`
//! block in `request_phase.rs`.
//!
//! Only two things are genuinely gated behind this crate's own Cargo
//! features, matching their pre-extraction gating exactly:
//! - `cache::should_early_refresh` — behind `cache` (was
//!   `#[cfg(feature = "cache")]` on that one function in the pre-extraction
//!   file).
//! - the entire `redis` module — behind `redis` (was
//!   `#![cfg(feature = "redis")]` on `src/proxy/cache_redis.rs`).
//!
//! `cache`/`disk-cache` are declared as marker features here (mirroring the
//! pre-extraction root `cache = []` / `disk-cache = ["cache"]` — neither
//! actually gates code in this crate beyond `should_early_refresh` above);
//! the root crate's own `cache`/`disk-cache` features forward into them
//! purely for documentation/consistency with every other extracted crate.
//! `redis` is forwarded from the root's own `redis` feature
//! (`lopatnov-conduit-cache/redis`) — a *weak* dependency feature
//! (`lopatnov-conduit-cache?/redis`) was considered to further insulate this
//! from `lopatnov-conduit-ratelimit`'s own `redis` feature (issue #137
//! slice 2), which uses the same `redis` crate for a different purpose, but
//! Cargo's `?` syntax only applies to *optional* dependencies and
//! `lopatnov-conduit-cache` is mandatory (like every other extracted feature
//! crate — `CacheConfig` must stay always-compiled). The separation is still
//! structural: this crate's `redis` feature is its own Cargo-feature
//! namespace, independent of `conduit-ratelimit`'s. (The root crate itself
//! has no `dep:redis` of its own any more — its last direct usage moved out
//! with `rate_limit_redis.rs`.) See
//! `CONTRIBUTING.md`'s crate-extraction recipe and #114's own "Cargo feature
//! unification" risk note.
//!
//! ## `RequestCtx.cache` (CLAUDE.md decision #30)
//!
//! Per-request cache state ([`ctx::CacheReqState`]) is a
//! `#[cfg(feature = "cache")]`-gated field on the root crate's `RequestCtx`
//! (`src/proxy/ctx.rs`), not a type-erased extension slot — matching the
//! existing `otel_span`/`jwt` pattern. This crate only supplies the state
//! type; `RequestCtx` itself, and the always-compiled `#[cfg]`-branching
//! accessor the root crate uses to read `cache_age_secs` from the
//! unconditional `ResponseCtx` trait impl (`src/filter/response_chain.rs`),
//! stay in the root crate.
//!
//! ## No `conduit-core` dependency
//!
//! Unlike `conduit-faults`/`conduit-auth-jwt` (guard-shaped extractions),
//! nothing in this crate implements `RequestFilter`/`ResponseFilter` — the
//! cache hooks are Pingora `ProxyHttp` trait-method bodies that stay in the
//! root crate's `request_phase.rs`/`response_phase.rs`, calling into this
//! crate's plain functions. Per `CONTRIBUTING.md`'s "conduit-core dependency
//! is opt-in, not automatic", this crate has no dependency on
//! `lopatnov-conduit-core` at all.

pub mod cache;
pub mod common;
pub mod config;
#[cfg(feature = "cache")]
pub mod ctx;
pub mod disk;
#[cfg(feature = "redis")]
pub mod redis;

pub use config::CacheConfig;
#[cfg(feature = "cache")]
pub use ctx::CacheReqState;
