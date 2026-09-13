//! Extracted into `crates/conduit-compression` (issue #114/#138).
//!
//! `CompressionConfig`/`CompressionOptions` (the schema types) moved too, but
//! are re-exported at their *original* location, `crate::config::schema` —
//! see that module for the facade. This file only re-exports the real
//! negotiation logic below.
//!
//! `CompressOptions`/`effective`/`is_compressible_type`/`best_encoding`/
//! `compress_bytes` are gated behind this crate's own `compression` Cargo
//! feature — forwarded from the root crate's own `compression` feature,
//! which (unlike every other extracted optional feature so far) is
//! **default-on** (see the root `Cargo.toml`'s `[features]` `default`/
//! `compression` entries): a plain `cargo build` keeps compressing responses
//! exactly like before this extraction; only `--no-default-features`
//! (without re-adding `compression`) drops it — along with `async-compression`
//! from the dependency tree entirely.
//!
//! `handler/static_files.rs`'s on-the-fly streaming compression
//! (`stream_file_compressed`) stayed in the root crate (out of #138's scope)
//! but is gated behind the same `compression` feature directly.

#[cfg(feature = "compression")]
pub use conduit_compression::logic::{
    best_encoding, compress_bytes, compress_small_body, effective, is_compressible_type,
    CompressOptions,
};
