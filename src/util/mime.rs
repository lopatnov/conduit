//! Content-Type detection facade.
//!
//! Extracted into `crates/conduit-static` (issue #114/#139) — the only
//! caller (`handler/static_files.rs`) moved there too, so this facade is
//! compiled only when the `static` feature is enabled. See that crate's
//! `src/lib.rs` doc comment, "What moved out of `conduit-core`".
#[cfg(feature = "static")]
pub use conduit_static::mime::content_type;
