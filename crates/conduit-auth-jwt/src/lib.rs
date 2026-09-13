//! JWT bearer-token authentication crate for conduit's feature-driven Cargo
//! workspace migration (issue [#114](https://github.com/lopatnov/conduit/issues/114),
//! extracted in [#133](https://github.com/lopatnov/conduit/issues/133)).
//!
//! ## Scope
//!
//! Owns [`JwtAuthConfig`] (the `sites[].jwtAuth` config struct), the
//! `{{ jwt.<claim> }}` header-template expansion (`template` module), and
//! the real JWT validation/guard code (`jwt` + `guard` modules): JWKS
//! caching/fetch, HS256/RS256/ES256 verification, and
//! `guard::JwtGuard` — the request guard that validates the
//! `Authorization: Bearer` header.
//!
//! `JwtAuthConfig` is compiled into **every** conduit build — like
//! `AcmeConfig`/`OtlpConfig`/`TcpConfig`/`UploadConfig`/`FaultInjectionConfig`
//! — because `SiteConfig.jwt_auth` is not itself feature-gated (a config
//! file that sets `jwtAuth` without `--features jwt` must still parse
//! cleanly and get an explicit `feature_warnings()` warning, not a
//! silent-drop or a hard parse error).
//!
//! `template::expand_jwt_templates` is **also** always compiled, for a
//! different reason: `requestTransform.setHeaders` template expansion
//! (`{{ jwt.<claim> }}`) is itself an always-compiled call site in the root
//! crate, so a config referencing that syntax must still parse and expand
//! (to `""` for every claim) even when `--features jwt` is off. See that
//! module's own doc comment for detail.
//!
//! Only the real `jwt`/`guard` modules — JWKS fetch, signature validation,
//! `guard::JwtGuard` — are gated behind this crate's own `jwt` Cargo
//! feature; the root crate's `jwt` feature forwards into it via
//! `lopatnov-conduit-auth-jwt/jwt`.
//!
//! ## Guard-shaped extraction (#132's pattern, reused here)
//!
//! Like `conduit-faults` (#132) and `conduit-acme`'s `challenge` module,
//! `guard::JwtGuard` implements `conduit-core`'s
//! [`RequestFilter`](conduit_core::filter::chain::RequestFilter) chain trait
//! directly, so per `CONTRIBUTING.md`'s crate extraction recipe this crate
//! *does* depend on `lopatnov-conduit-core`, gated behind the same `jwt`
//! feature as the rest of the guard's dependencies. Chain assembly and
//! guard ordering stay in the root crate's `src/filter/chain.rs` (`CLAUDE.md`
//! decision #20) — this crate exports only the filter implementation and
//! its constructor input ([`JwtAuthConfig`]), never a chain position.
//!
//! ## `RequestCtx.jwt` (CLAUDE.md decision #30)
//!
//! Per-request JWT claim state ([`guard::JwtReqState`]) is a
//! `#[cfg(feature = "jwt")]`-gated field on the root crate's `RequestCtx`
//! (`src/proxy/ctx.rs`), not a type-erased extension slot — matching the
//! existing `otel_span`/`early_refresh_upstream_url` pattern documented in
//! `CLAUDE.md` decision #30. This crate only supplies the type and the
//! extraction function; `RequestCtx` itself, and the always-compiled
//! `#[cfg]`-branching accessor the root crate uses to read it from
//! unconditional call sites (header-template expansion), stay in the root
//! crate.

pub mod config;
#[cfg(feature = "jwt")]
pub mod guard;
#[cfg(feature = "jwt")]
pub mod jwt;
pub mod template;

pub use config::JwtAuthConfig;
#[cfg(feature = "jwt")]
pub use jwt::{check_jwt, check_jwt_extracting, JwtCheckResult};
