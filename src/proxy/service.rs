//! Facade over `conduit_runtime` (issue #145): `ConduitProxy`, `AppState` and `ConduitMetrics` moved into `crates/conduit-runtime`.
//! Every item is re-exported here at its original path, so no call site changed.

pub use conduit_runtime::proxy::service::{AppState, ConduitMetrics, ConduitProxy};

// The pipeline lives in `conduit-runtime`, which declares its own copy of these features. Every root feature forwards to
// it, but a forward that is missed compiles fine and silently drops behaviour (a guard never pushed onto the chain, a
// route type ignored) while `feature_warnings()` stays quiet because the root believes the feature is on. Turn any such
// drift into a build error, the same way `config::validate::warnings` does for the feature crates.
const _: () = assert!(
    conduit_runtime::features::PROXY == cfg!(feature = "proxy"),
    "`conduit-runtime`'s `proxy` feature and the root crate's `proxy` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::COMPRESSION == cfg!(feature = "compression"),
    "`conduit-runtime`'s `compression` feature and the root crate's `compression` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::STATIC == cfg!(feature = "static"),
    "`conduit-runtime`'s `static` feature and the root crate's `static` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::HOTRELOAD == cfg!(feature = "hotreload"),
    "`conduit-runtime`'s `hotreload` feature and the root crate's `hotreload` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::JWT == cfg!(feature = "jwt"),
    "`conduit-runtime`'s `jwt` feature and the root crate's `jwt` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::CONSUMERS == cfg!(feature = "consumers"),
    "`conduit-runtime`'s `consumers` feature and the root crate's `consumers` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::FORWARD_AUTH == cfg!(feature = "forward-auth"),
    "`conduit-runtime`'s `forward-auth` feature and the root crate's `forward-auth` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::UPLOAD == cfg!(feature = "upload"),
    "`conduit-runtime`'s `upload` feature and the root crate's `upload` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::REDIS == cfg!(feature = "redis"),
    "`conduit-runtime`'s `redis` feature and the root crate's `redis` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::CACHE == cfg!(feature = "cache"),
    "`conduit-runtime`'s `cache` feature and the root crate's `cache` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::ACME == cfg!(feature = "acme"),
    "`conduit-runtime`'s `acme` feature and the root crate's `acme` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::FAULT_INJECTION == cfg!(feature = "fault-injection"),
    "`conduit-runtime`'s `fault-injection` feature and the root crate's `fault-injection` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::OTLP == cfg!(feature = "otlp"),
    "`conduit-runtime`'s `otlp` feature and the root crate's `otlp` feature must be enabled together"
);
const _: () = assert!(
    conduit_runtime::features::TOKIO_METRICS == cfg!(feature = "tokio-metrics"),
    "`conduit-runtime`'s `tokio-metrics` feature and the root crate's `tokio-metrics` feature must be enabled together"
);
