//! Response/static-file compression config + negotiation logic for conduit's
//! feature-driven Cargo workspace migration (issue
//! [#114](https://github.com/lopatnov/conduit/issues/114), extracted in
//! [#138](https://github.com/lopatnov/conduit/issues/138)).
//!
//! ## Scope
//!
//! Owns [`CompressionConfig`]/[`CompressionOptions`] (the `sites[].compression`
//! bool-or-object-shorthand config, `false | true | { algorithms, level,
//! minBytes, types }`) and the real negotiation logic in [`logic`] —
//! `CompressOptions` (the resolved/flattened form), `logic::effective()`,
//! `logic::is_compressible_type()`, `logic::best_encoding()`, and
//! `logic::compress_bytes()`. `CompressionConfig`/`CompressionOptions` are
//! compiled into **every** conduit build — like `FaultInjectionConfig`/
//! `JwtAuthConfig`/`AcmeConfig`/... — because `SiteConfig.compression` is not
//! itself feature-gated (a config file that sets `compression` without
//! `--features compression` must still parse cleanly and get an explicit
//! `feature_warnings()` warning, not a silent-drop or a hard parse error).
//! Only [`logic`] is gated behind this crate's own `compression` Cargo
//! feature; the root crate's `compression` feature forwards into it via
//! `lopatnov-conduit-compression/compression`.
//!
//! ## Default-on (unlike every prior extraction)
//!
//! `compression` is the first extracted feature in the Conduit 2.0 migration
//! that stays **default-on** at the root crate — every other optional
//! feature (`jwt`, `cache`, `acme`, ...) defaults *off*. Issue #138 is
//! explicit about this: response compression is a baseline expectation for a
//! reverse proxy/static-file server, so a plain `cargo build` must keep
//! compressing exactly like it did before this extraction. Only
//! `--no-default-features` (without re-adding `compression`) produces the
//! "just static files, no compression" build the issue describes — see the
//! root crate's `[features]` `default`/`compression` entries.
//!
//! ## Out of scope: on-the-fly streaming compression in `handler/static_files.rs`
//!
//! `handler/static_files.rs`'s `stream_file_compressed` (the actual
//! chunk-by-chunk brotli/gzip/deflate encoder pipeline used while streaming a
//! static file response) was **not** moved here — issue #138's scope names
//! only `src/filter/compression.rs`. It still lives in the root crate,
//! directly depending on `async-compression`, gated behind the same
//! `compression` Cargo feature (`#[cfg(feature = "compression")]`) so
//! disabling this feature still drops `async-compression` from the
//! dependency tree entirely, even though its usage is split across two
//! crates.

pub mod config;
#[cfg(feature = "compression")]
pub mod logic;

pub use config::{CompressionConfig, CompressionOptions};
