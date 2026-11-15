//! What a processor may do to the request headers, and the rules that keep a processor from doing more.
//!
//! The rules follow Envoy's `mutation_rules` (`source/extensions/filters/common/mutation_rules`, applied by
//! `mutation_utils.cc`): header names and values are validated, the number and size of mutations are capped, and a
//! small set of headers cannot be touched at all because changing them changes how the request is framed or routed:
//!
//! * pseudo-headers (`:method`, `:path`, ...) and `host`: the request line and the virtual host are decided by the
//!   proxy before the processor is asked;
//! * `content-length`, `transfer-encoding`, `connection`, `keep-alive`, `proxy-connection`, `te`, `trailer`, `upgrade`:
//!   message framing and hop-by-hop headers, the ingredients of request smuggling.
//!
//! A mutation of a forbidden header is ignored and reported (Envoy's default, `disallow_is_error: false`). A mutation that
//! is not well formed (a bad name, a value with a control character, an unknown action, too many mutations, a value over
//! the size cap) refuses the whole answer: applying half of it would leave the request in a state nobody asked for.

use http::{HeaderName, HeaderValue};

use crate::proto::{append_action, HeaderMutation};

/// How a header is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetMode {
    /// Add a value, keeping existing ones.
    Append,
    /// Add the header only when it is absent.
    AddIfAbsent,
    /// Replace every existing value, or add the header.
    Overwrite,
    /// Replace every existing value, only when the header exists.
    OverwriteIfExists,
}

/// One header to set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderSet {
    /// The name, lower case.
    pub name: HeaderName,
    /// The value.
    pub value: HeaderValue,
    /// How to set it.
    pub mode: SetMode,
}

/// What to do to the request headers: the validated result of a [`HeaderMutation`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderChanges {
    /// Headers to set, in the order the processor sent them.
    pub set: Vec<HeaderSet>,
    /// Headers to remove (applied after the sets, as Envoy does).
    pub remove: Vec<HeaderName>,
}

impl HeaderChanges {
    /// `true` when there is nothing to do.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty()
    }
}

/// The caps on one answer.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Sets plus removals.
    pub max_mutations: usize,
    /// Bytes of one header value.
    pub max_value_bytes: usize,
}

/// A mutation that cannot be applied, with what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The result of reading a [`HeaderMutation`]: the changes to make and the names of the forbidden headers it asked to touch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// What to do.
    pub changes: HeaderChanges,
    /// Forbidden headers the processor tried to set or remove (for the logs).
    pub ignored: Vec<String>,
}

/// `true` for the headers a processor may not set or remove.
fn forbidden(name: &str) -> bool {
    name.starts_with(':')
        || matches!(
            name,
            "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "keep-alive"
                | "proxy-connection"
                | "te"
                | "trailer"
                | "upgrade"
        )
}

/// A header name from a processor: lower case, a valid HTTP token, or the reason it is not. `Ok(None)` for a forbidden one.
fn name_of(raw: &str, ignored: &mut Vec<String>) -> Result<Option<HeaderName>, Rejected> {
    let lower = raw.to_ascii_lowercase();
    if lower.is_empty() {
        return Err(Rejected("a header mutation has an empty name".into()));
    }
    if forbidden(&lower) {
        ignored.push(lower);
        return Ok(None);
    }
    HeaderName::from_bytes(lower.as_bytes())
        .map(Some)
        .map_err(|_| Rejected(format!("{raw:?} is not a valid header name")))
}

/// Read and validate `mutation` under `limits`.
pub fn plan(mutation: &HeaderMutation, limits: Limits) -> Result<Plan, Rejected> {
    let total = mutation.set_headers.len() + mutation.remove_headers.len();
    if total > limits.max_mutations {
        return Err(Rejected(format!(
            "{total} header mutations, more than the {} allowed",
            limits.max_mutations
        )));
    }
    let mut plan = Plan::default();
    for option in &mutation.set_headers {
        let header = option
            .header
            .as_ref()
            .ok_or_else(|| Rejected("a header mutation has no header".into()))?;
        let Some(name) = name_of(&header.key, &mut plan.ignored)? else {
            continue;
        };
        let bytes: &[u8] = if header.raw_value.is_empty() {
            header.value.as_bytes()
        } else {
            &header.raw_value
        };
        if bytes.len() > limits.max_value_bytes {
            return Err(Rejected(format!(
                "the value of header {name} is {} bytes, more than the {} allowed",
                bytes.len(),
                limits.max_value_bytes
            )));
        }
        let mode = match option.append_action {
            append_action::APPEND_IF_EXISTS_OR_ADD => {
                // the deprecated `append: false` still means overwrite
                if option.append.as_ref().is_some_and(|b| !b.value) {
                    SetMode::Overwrite
                } else {
                    SetMode::Append
                }
            }
            append_action::ADD_IF_ABSENT => SetMode::AddIfAbsent,
            append_action::OVERWRITE_IF_EXISTS_OR_ADD => SetMode::Overwrite,
            append_action::OVERWRITE_IF_EXISTS => SetMode::OverwriteIfExists,
            other => {
                return Err(Rejected(format!(
                    "header {name} has an unknown append_action {other}"
                )))
            }
        };
        if bytes.is_empty() && !option.keep_empty_value {
            // Envoy: an empty value is dropped unless keep_empty_value; for a header that is overwritten that means removed
            if matches!(mode, SetMode::Overwrite | SetMode::OverwriteIfExists) {
                plan.changes.remove.push(name);
            }
            continue;
        }
        let value = HeaderValue::from_bytes(bytes).map_err(|_| {
            Rejected(format!(
                "the value of header {name} has a control character"
            ))
        })?;
        plan.changes.set.push(HeaderSet { name, value, mode });
    }
    for raw in &mutation.remove_headers {
        if let Some(name) = name_of(raw, &mut plan.ignored)? {
            plan.changes.remove.push(name);
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{BoolValue, HeaderValue as PbValue, HeaderValueOption};

    const LIMITS: Limits = Limits {
        max_mutations: 8,
        max_value_bytes: 16,
    };

    fn set(key: &str, value: &[u8], action: i32) -> HeaderValueOption {
        HeaderValueOption {
            header: Some(PbValue {
                key: key.into(),
                value: String::new(),
                raw_value: value.to_vec(),
            }),
            append: None,
            append_action: action,
            keep_empty_value: false,
        }
    }

    fn mutation(set_headers: Vec<HeaderValueOption>, remove: &[&str]) -> HeaderMutation {
        HeaderMutation {
            set_headers,
            remove_headers: remove.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn plain_sets_and_removals_are_planned_in_order_with_lower_case_names() {
        let plan = plan(
            &mutation(
                vec![
                    set("X-Team", b"blue", append_action::OVERWRITE_IF_EXISTS_OR_ADD),
                    set("x-trace", b"1", append_action::ADD_IF_ABSENT),
                    set("x-list", b"a", append_action::APPEND_IF_EXISTS_OR_ADD),
                    set("x-only", b"b", append_action::OVERWRITE_IF_EXISTS),
                ],
                &["X-Remove"],
            ),
            LIMITS,
        )
        .unwrap();
        let summary: Vec<_> = plan
            .changes
            .set
            .iter()
            .map(|s| (s.name.as_str(), s.value.to_str().unwrap(), s.mode))
            .collect();
        assert_eq!(
            summary,
            [
                ("x-team", "blue", SetMode::Overwrite),
                ("x-trace", "1", SetMode::AddIfAbsent),
                ("x-list", "a", SetMode::Append),
                ("x-only", "b", SetMode::OverwriteIfExists),
            ]
        );
        assert_eq!(plan.changes.remove[0].as_str(), "x-remove");
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn the_value_is_read_from_raw_value_and_falls_back_to_value() {
        let mut option = set("x-a", b"", 0);
        option.header.as_mut().unwrap().value = "from-value".into();
        let plan = plan(&mutation(vec![option], &[]), LIMITS).unwrap();
        assert_eq!(plan.changes.set[0].value, "from-value");
    }

    #[test]
    fn the_deprecated_append_false_means_overwrite() {
        let mut option = set("x-a", b"v", append_action::APPEND_IF_EXISTS_OR_ADD);
        option.append = Some(BoolValue { value: false });
        let plan = plan(&mutation(vec![option], &[]), LIMITS).unwrap();
        assert_eq!(plan.changes.set[0].mode, SetMode::Overwrite);
    }

    #[test]
    fn headers_that_frame_or_route_the_request_cannot_be_touched() {
        for name in [
            "Host",
            ":authority",
            ":path",
            ":method",
            "content-length",
            "Transfer-Encoding",
            "connection",
            "keep-alive",
            "proxy-connection",
            "te",
            "trailer",
            "upgrade",
        ] {
            let plan = plan(&mutation(vec![set(name, b"x", 2)], &[name]), LIMITS).unwrap();
            assert!(plan.changes.is_empty(), "{name} must not be changed");
            assert_eq!(
                plan.ignored.len(),
                2,
                "{name}: set and remove are both reported"
            );
        }
        // the neighbours of the forbidden names are fine
        let plan = plan(
            &mutation(
                vec![set("x-host", b"x", 2), set("content-type", b"x", 2)],
                &[],
            ),
            LIMITS,
        )
        .unwrap();
        assert_eq!(plan.changes.set.len(), 2);
    }

    #[test]
    fn a_value_with_a_control_character_refuses_the_whole_answer() {
        for bad in [&b"a\r\nx-evil: 1"[..], b"a\nb", b"a\0b", b"a\x7fb"] {
            let err = plan(
                &mutation(vec![set("x-ok", b"fine", 2), set("x-bad", bad, 2)], &[]),
                LIMITS,
            )
            .unwrap_err();
            assert!(err.0.contains("control character"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn bad_names_unknown_actions_and_a_missing_header_are_refused() {
        for key in ["", "bad name", "bad\r\nname", "na:me", "naïve"] {
            assert!(
                plan(&mutation(vec![set(key, b"v", 2)], &[]), LIMITS).is_err(),
                "{key:?}"
            );
        }
        assert!(plan(&mutation(vec![], &["bad name"]), LIMITS).is_err());
        assert!(plan(&mutation(vec![set("x-a", b"v", 9)], &[]), LIMITS).is_err());
        let no_header = HeaderValueOption {
            header: None,
            append: None,
            append_action: 0,
            keep_empty_value: false,
        };
        assert!(plan(&mutation(vec![no_header], &[]), LIMITS).is_err());
    }

    #[test]
    fn the_count_and_the_value_size_are_capped() {
        let many: Vec<_> = (0..9).map(|i| set(&format!("x-{i}"), b"v", 2)).collect();
        assert!(plan(&mutation(many, &[]), LIMITS)
            .unwrap_err()
            .0
            .contains("more than the 8"));
        // sets and removals count together
        let some: Vec<_> = (0..5).map(|i| set(&format!("x-{i}"), b"v", 2)).collect();
        assert!(plan(&mutation(some, &["a", "b", "c", "d"]), LIMITS).is_err());
        let at_limit = vec![b'a'; 16];
        assert!(plan(&mutation(vec![set("x-a", &at_limit, 2)], &[]), LIMITS).is_ok());
        let over = vec![b'a'; 17];
        assert!(plan(&mutation(vec![set("x-a", &over, 2)], &[]), LIMITS)
            .unwrap_err()
            .0
            .contains("17 bytes"));
    }

    #[test]
    fn an_empty_value_is_dropped_unless_kept_and_removes_when_overwriting() {
        // add modes: nothing to add
        let plan_add = plan(
            &mutation(vec![set("x-a", b"", 0), set("x-b", b"", 1)], &[]),
            LIMITS,
        )
        .unwrap();
        assert!(plan_add.changes.is_empty());
        // overwrite modes: the header goes away
        let plan_over = plan(
            &mutation(vec![set("x-a", b"", 2), set("x-b", b"", 3)], &[]),
            LIMITS,
        )
        .unwrap();
        assert!(plan_over.changes.set.is_empty());
        assert_eq!(plan_over.changes.remove.len(), 2);
        // keep_empty_value keeps it
        let mut kept = set("x-a", b"", 2);
        kept.keep_empty_value = true;
        let plan_kept = plan(&mutation(vec![kept], &[]), LIMITS).unwrap();
        assert_eq!(plan_kept.changes.set[0].value, "");
    }
}
