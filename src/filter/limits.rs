use pingora_proxy::Session;

use crate::config::schema::LimitsConfig;

pub enum CheckResult {
    Ok,
    BodyTooLarge,
    HeaderTooLarge,
}

/// Check declared Content-Length against maxBodyBytes and header size against maxHeaderBytes.
/// Timeout enforcement requires OS-level socket options and is deferred.
pub fn check(config: &LimitsConfig, session: &Session) -> CheckResult {
    if config
        .max_header_bytes
        .is_some_and(|max| header_size(session) > max)
    {
        return CheckResult::HeaderTooLarge;
    }

    if config
        .max_body_bytes
        .zip(declared_content_length(session))
        .is_some_and(|(max, len)| len > max)
    {
        return CheckResult::BodyTooLarge;
    }

    CheckResult::Ok
}

/// Parse the Content-Length request header into a byte count, if present and valid.
fn declared_content_length(session: &Session) -> Option<u64> {
    session
        .req_header()
        .headers
        .get("content-length")?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn header_size(session: &Session) -> u64 {
    let req = session.req_header();
    // Request line approximation: METHOD SP path SP HTTP/1.1 CRLF
    let request_line = req.method.as_str().len() + 1 + req.uri.to_string().len() + 11;
    let fields: usize = req
        .headers
        .iter()
        .map(|(k, v)| k.as_str().len() + 2 + v.len() + 2) // "name: value\r\n"
        .sum();
    (request_line + fields + 2) as u64 // trailing CRLF
}

/// Leaky-bucket minimum-upload-rate step (#51).
///
/// Updates `excess` (surplus bytes above the minimum rate) and returns
/// `true` when the client has fallen more than one second behind the
/// minimum rate and should be rejected with 408.
///
/// # Arguments
/// - `excess` — running surplus in bytes (positive = fast, negative = slow).
///   Modified in place.
/// - `chunk_len` — bytes received in this chunk.
/// - `min_rate` — minimum acceptable rate in bytes per second.
/// - `elapsed_secs` — seconds elapsed since the previous chunk.
///
/// # Algorithm
/// ```text
/// excess += chunk_len − min_rate × elapsed_secs
/// excess  = min(excess, min_rate)   // cap surplus (no unlimited burst credit)
/// reject  = excess < −min_rate       // more than one second of deficit
/// ```
pub(crate) fn upload_rate_step(
    excess: &mut f64,
    chunk_len: usize,
    min_rate: u64,
    elapsed_secs: f64,
) -> bool {
    *excess += chunk_len as f64 - min_rate as f64 * elapsed_secs;
    *excess = excess.min(min_rate as f64);
    *excess < -(min_rate as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── upload_rate_step (leaky-bucket minimum rate, #51) ────────────────────

    #[test]
    fn upload_rate_step_at_exact_min_rate_keeps_excess_zero() {
        let mut excess = 0.0f64;
        let min_rate = 1024u64; // 1 KiB/s
                                // Exactly 1024 bytes in exactly 1 second → excess stays at 0.
        let rejected = upload_rate_step(&mut excess, 1024, min_rate, 1.0);
        assert!(!rejected, "exactly at min rate must not reject");
        assert!(
            (excess - 0.0).abs() < 0.01,
            "excess should be ~0, got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_above_min_rate_accumulates_surplus() {
        let mut excess = 0.0f64;
        let min_rate = 1024u64;
        // 2048 bytes in 1 second (twice the minimum rate) → surplus = 1024.
        let rejected = upload_rate_step(&mut excess, 2048, min_rate, 1.0);
        assert!(!rejected, "above min rate must not reject");
        // Surplus capped at min_rate (1024).
        assert!(
            (excess - 1024.0).abs() < 0.01,
            "surplus capped at min_rate: got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_surplus_is_capped_at_one_second() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // Enormous burst: 1_000_000 bytes in 0.01 seconds.
        upload_rate_step(&mut excess, 1_000_000, min_rate, 0.01);
        // Surplus must be capped at min_rate (1000) — not the raw 999_990.
        assert!(
            excess <= min_rate as f64 + 0.01,
            "surplus must be capped at min_rate: got {excess}"
        );
    }

    #[test]
    fn upload_rate_step_below_min_rate_accumulates_deficit() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // 100 bytes in 1 second (1/10 of min rate) → deficit grows.
        let rejected = upload_rate_step(&mut excess, 100, min_rate, 1.0);
        // deficit = 100 - 1000 = -900; not yet past -1000 threshold.
        assert!(
            !rejected,
            "single slow chunk below min rate but deficit < min_rate"
        );
        assert!(excess < 0.0, "excess must be negative (deficit): {excess}");
    }

    #[test]
    fn upload_rate_step_rejects_when_deficit_exceeds_one_second() {
        let mut excess = -(1000f64 - 1.0); // just below the rejection threshold
        let min_rate = 1000u64;
        // One more tiny chunk with a 1-second gap: excess += 1 - 1000 → -1999+1 = -1999
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 1.0);
        assert!(rejected, "deficit > min_rate must trigger rejection");
        assert!(
            excess < -(min_rate as f64),
            "excess must be below -min_rate: {excess}"
        );
    }

    #[test]
    fn upload_rate_step_carries_over_surplus_for_slow_periods() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // First chunk: big burst that fills the surplus bucket.
        upload_rate_step(&mut excess, 10_000, min_rate, 0.0);
        // Surplus capped at 1000.
        assert!((excess - 1000.0).abs() < 0.01, "surplus capped: {excess}");

        // Second chunk: very slow (1 byte in 1 second).
        // excess += 1 - 1000 → 1000 + 1 - 1000 = 1.
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 1.0);
        assert!(!rejected, "surplus from burst must absorb one slow chunk");
        assert!(excess >= 0.0, "excess should remain non-negative: {excess}");
    }

    #[test]
    fn upload_rate_step_zero_elapsed_never_rejects() {
        let mut excess = 0.0f64;
        let min_rate = 1000u64;
        // First call always has elapsed=0 (first chunk in request_body_filter).
        let rejected = upload_rate_step(&mut excess, 1, min_rate, 0.0);
        assert!(!rejected, "first chunk (elapsed=0) must never reject");
    }
}
