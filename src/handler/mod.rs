#[cfg(feature = "acme")]
pub mod acme_challenge;
#[cfg(feature = "static")]
pub mod fallback;
pub mod health;
pub mod hot_reload;
pub mod metrics;
pub mod response;
#[cfg(feature = "static")]
pub mod static_files;

// Layer-0 vocabulary (#114/#126). See its doc comment for how to add a new
// handler -- the steps still apply unchanged.
pub use conduit_core::handler::LocalHandlerImpl;
