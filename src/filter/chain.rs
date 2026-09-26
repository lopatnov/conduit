//! Facade over `conduit_runtime` (issue #145): the guard chain and its guards moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

#[cfg(feature = "consumers")]
pub use conduit_runtime::filter::chain::ConsumersGuard;
#[cfg(feature = "fault-injection")]
pub use conduit_runtime::filter::chain::FaultInjectionGuard;
#[cfg(feature = "forward-auth")]
pub use conduit_runtime::filter::chain::ForwardAuthGuard;
#[cfg(feature = "jwt")]
pub use conduit_runtime::filter::chain::JwtGuard;
pub use conduit_runtime::filter::chain::{
    AllowedHostsGuard, ApiKeyGuard, BasicAuthGuard, CorsPreflight, FilterChain, FilterContext,
    FilterOutcome, HealthBypass, IpConnSlotGuard, IpGuard, LimitsGuard, MiddlewareGuard,
    RateLimitGuard, RedirectGuard, RequestFilter, ScriptGuard, XRequestIdGuard,
};
