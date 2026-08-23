/// Distinguishes a hard config error (must not start/reload) from an
/// advisory one (worth surfacing, but shouldn't block startup/reload) —
/// see issue #191. `conduit validate`'s pre-flight check treats both the
/// same way (exit non-zero on either), by design: its whole purpose is to
/// surface anything worth an operator's attention before a real deploy.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// A single config validation failure: the config path that's wrong, and why.
#[derive(Debug, PartialEq, Clone, serde::Serialize)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
    pub severity: Severity,
}

impl ValidationError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            severity: Severity::Error,
        }
    }

    /// An advisory finding: worth surfacing, but must not by itself block
    /// server startup or `/reload` (issue #191) — e.g. a still-valid cert
    /// that's merely close to expiring. `conduit validate` still treats this
    /// the same as [`Self::new`] (see `Severity`'s doc comment).
    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            severity: Severity::Warning,
        }
    }
}

/// Split validation errors into `(warnings, hard_errors)` by severity (#191).
///
/// Callers that gate startup/reload on validation (`run_server()`,
/// `/reload`) should log `warnings` and continue, only refusing to proceed
/// when `hard_errors` is non-empty. `conduit validate`'s CLI pre-flight
/// check deliberately does *not* use this split — it exits non-zero on
/// either kind, by design (see `Severity`'s doc comment).
pub fn partition_by_severity(
    errors: Vec<ValidationError>,
) -> (Vec<ValidationError>, Vec<ValidationError>) {
    errors
        .into_iter()
        .partition(|e| e.severity == Severity::Warning)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_path_and_message() {
        let e = ValidationError::new("sites[0].port", "port is required");
        assert_eq!(e.path, "sites[0].port");
        assert_eq!(e.message, "port is required");
    }

    #[test]
    fn equality_compares_both_fields() {
        let a = ValidationError::new("x", "y");
        let b = ValidationError::new("x", "y");
        let c = ValidationError::new("x", "z");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
