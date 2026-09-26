//! Config-file parsing: a facade over `conduit_config::parse` (issue #222), which owns `ConfigFile`
//! and `normalize()` next to the schema they produce.

pub use conduit_config::parse::{from_str, from_yaml, load_config, normalize};
