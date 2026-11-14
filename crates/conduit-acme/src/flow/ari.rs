//! ACME Renewal Information (ARI, RFC 9773; issue #594).
//!
//! A CA that supports ARI tells a client, per certificate, in which window it wants the certificate renewed (a
//! mass revocation, a changed lifetime, a plain "early"). Conduit asks at every renewal check and renews when the
//! window says so. The lifetime rule ([`super::cert_renewal_due`]) stays the floor: ARI can only pull a renewal
//! forward, never push it past that rule, so a CA that answers late, wrongly or not at all costs nothing.
//!
//! Everything here is best effort and fails open: no ARI in the directory, a network error, a timeout, an invalid
//! window all mean "no advice", never an error that stops the renewal loop.
use std::borrow::Cow;
use std::time::Duration;

use instant_acme::{Account, CertificateIdentifier};
use x509_parser::extensions::ParsedExtension;
use x509_parser::pem::parse_x509_pem;

/// How long one ARI request may take. The CA is asked on every renewal tick; a hung endpoint must not hold the
/// sequential renewal loop (and with it every other certificate).
const ASK_TIMEOUT: Duration = Duration::from_secs(15);

/// What the CA said about one certificate.
pub(super) struct Advice {
    /// The identifier of the certificate, to send as `replaces` with the order that renews it (RFC 9773 section 5),
    /// which lets the CA exempt the order from its rate limits and link the two certificates.
    pub id: CertificateIdentifier<'static>,
    /// The suggested window asks for a renewal now (see [`window_says_renew`]).
    pub renew_now: bool,
}

/// base64url without padding (RFC 4648 section 5), the encoding of a certificate identifier.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize] as char);
        }
    }
    out
}

/// The identifier of the first certificate in `cert_pem` (RFC 9773 section 4.1): the base64url of the authority key
/// identifier's `keyIdentifier` and of the serial number's DER content, joined by a dot. `None` when the certificate
/// cannot be read or has no authority key identifier (a CA that supports ARI always puts one there).
pub(super) fn certificate_identifier(cert_pem: &str) -> Option<CertificateIdentifier<'static>> {
    let (_, pem) = parse_x509_pem(cert_pem.as_bytes()).ok()?;
    let x509 = pem.parse_x509().ok()?;
    let aki = x509
        .extensions()
        .iter()
        .find_map(|ext| match ext.parsed_extension() {
            ParsedExtension::AuthorityKeyIdentifier(aki) => {
                aki.key_identifier.as_ref().map(|k| k.0)
            }
            _ => None,
        })?;
    Some(CertificateIdentifier {
        authority_key_identifier: Cow::Owned(base64url(aki)),
        serial: Cow::Owned(base64url(x509.tbs_certificate.raw_serial())),
    })
}

/// `true` when the suggested window `[start, end]` (seconds since the Unix epoch) calls for a renewal now.
///
/// RFC 9773 section 4.2 says to pick a uniformly random time in the window and renew then, which spreads a CA's
/// clients over the window. A random pick on every check would instead renew with a growing probability at each of
/// them, so the point is derived from `seed` (a hash of the certificate's serial): the same certificate gets the same
/// point on every check and in every process, different certificates get different ones. A window that would close
/// before the next check (`interval_secs` away) counts as open, so it is never missed by a check landing after it.
/// An inverted window is no advice.
pub(super) fn window_says_renew(
    start: i64,
    end: i64,
    seed: u64,
    now: i64,
    interval_secs: i64,
) -> bool {
    if end < start {
        return false;
    }
    let span = end.saturating_sub(start) as u64 + 1;
    let chosen = start.saturating_add((seed % span) as i64);
    now >= chosen || end <= now.saturating_add(interval_secs)
}

/// Ask the CA what it thinks of `cert_pem`. `None` when there is no usable answer (the directory has no ARI, the
/// request failed or timed out, the certificate has no identifier): the caller then relies on the lifetime rule.
pub(super) async fn advice(
    account: &Account,
    cert_pem: &str,
    now: i64,
    interval_secs: i64,
) -> Option<Advice> {
    let id = certificate_identifier(cert_pem)?;
    let seed = super::fnv1a_64(id.serial.as_bytes());
    match tokio::time::timeout(ASK_TIMEOUT, account.renewal_info(&id)).await {
        Ok(Ok((info, _retry_after))) => {
            let window = info.suggested_window;
            let renew_now = window_says_renew(
                window.start.unix_timestamp(),
                window.end.unix_timestamp(),
                seed,
                now,
                interval_secs,
            );
            tracing::debug!(
                certificate = %id,
                window_start = %window.start,
                window_end = %window.end,
                renew_now,
                explanation = info.explanation_url.as_deref().unwrap_or(""),
                "ACME renewal information"
            );
            Some(Advice { id, renew_now })
        }
        Ok(Err(instant_acme::Error::Unsupported(_))) => {
            tracing::debug!("the ACME server does not offer renewal information (ARI)");
            None
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "ACME renewal information request failed — using the lifetime rule");
            None
        }
        Err(_) => {
            tracing::warn!("ACME renewal information request timed out — using the lifetime rule");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_the_rfc_9773_example() {
        // RFC 9773 appendix A: this AKI and serial give "aYhba4dGQEHhs3uEe6CuLN4ByNQ.AIdlQyE".
        let aki = [
            0x69, 0x88, 0x5b, 0x6b, 0x87, 0x46, 0x40, 0x41, 0xe1, 0xb3, 0x7b, 0x84, 0x7b, 0xa0,
            0xae, 0x2c, 0xde, 0x01, 0xc8, 0xd4,
        ];
        let serial = [0x00, 0x87, 0x65, 0x43, 0x21];
        assert_eq!(base64url(&aki), "aYhba4dGQEHhs3uEe6CuLN4ByNQ");
        assert_eq!(base64url(&serial), "AIdlQyE");
        // every remainder length of the final group
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        // the url-safe alphabet, no padding
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn the_window_opens_at_a_point_inside_it_that_is_stable_for_one_certificate() {
        let (start, end, interval) = (1_000_000, 1_000_000 + 10 * 86_400, 12 * 3600);
        // before the window and more than one check interval before it closes: wait
        assert!(!window_says_renew(start, end, 0, start - 1, interval));
        // the chosen point is `start + seed % span`; one second before it: wait, at it: renew
        let seed = 3 * 86_400;
        let chosen = start + 3 * 86_400;
        assert!(!window_says_renew(start, end, seed, chosen - 1, interval));
        assert!(window_says_renew(start, end, seed, chosen, interval));
        // another certificate (another seed) gets another point in the same window
        assert!(window_says_renew(
            start,
            end,
            seed + 86_400,
            chosen + 86_400,
            interval
        ));
        assert!(!window_says_renew(
            start,
            end,
            seed + 86_400,
            chosen + 86_399,
            interval
        ));
        // a huge seed stays inside the window
        assert!(window_says_renew(start, end, u64::MAX, end, interval));
    }

    #[test]
    fn a_window_that_closes_before_the_next_check_is_open_now() {
        let interval = 12 * 3600;
        let now = 5_000_000;
        // chosen point is a day away, but the window ends in 6 hours: the next check would be too late
        assert!(window_says_renew(
            now + 3600,
            now + 6 * 3600,
            u64::MAX,
            now,
            interval
        ));
        // the same window two days out: wait
        assert!(!window_says_renew(
            now + 2 * 86_400,
            now + 2 * 86_400 + 6 * 3600,
            u64::MAX,
            now,
            interval
        ));
    }

    #[test]
    fn a_window_in_the_past_renews_and_an_inverted_one_is_no_advice() {
        let interval = 12 * 3600;
        assert!(window_says_renew(100, 200, 7, 10_000, interval));
        assert!(!window_says_renew(200, 100, 7, 10_000, interval));
        // a one-second window
        assert!(window_says_renew(500, 500, 9, 500, interval));
    }

    fn pem_with_aki() -> (String, Vec<u8>) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec!["ca.test".to_owned()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);

        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["example.com".to_owned()]).unwrap();
        params.use_authority_key_identifier_extension = true;
        params.serial_number = Some(rcgen::SerialNumber::from(vec![
            0x00, 0x87, 0x65, 0x43, 0x21,
        ]));
        let leaf = params.signed_by(&key, &issuer).unwrap();
        // the CA's subject key identifier, read back from its certificate
        let (_, ca_pem) = parse_x509_pem(ca.pem().as_bytes()).unwrap();
        let ca_x509 = ca_pem.parse_x509().unwrap();
        let ski = ca_x509
            .extensions()
            .iter()
            .find_map(|e| match e.parsed_extension() {
                ParsedExtension::SubjectKeyIdentifier(k) => Some(k.0.to_vec()),
                _ => None,
            })
            .expect("the CA has a subject key identifier");
        (leaf.pem(), ski)
    }

    #[test]
    fn the_identifier_is_the_issuers_key_id_and_the_serial() {
        let (pem, ca_ski) = pem_with_aki();
        let id = certificate_identifier(&pem).expect("identifier");
        assert_eq!(id.authority_key_identifier, base64url(&ca_ski));
        assert_eq!(id.serial, "AIdlQyE");
        assert_eq!(id.to_string(), format!("{}.AIdlQyE", base64url(&ca_ski)));
    }

    #[test]
    fn a_certificate_without_an_authority_key_id_or_unreadable_has_no_identifier() {
        let key = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["example.com".to_owned()]).unwrap();
        let self_signed = params.self_signed(&key).unwrap();
        assert!(certificate_identifier(&self_signed.pem()).is_none());
        assert!(certificate_identifier("not a certificate").is_none());
    }
}
