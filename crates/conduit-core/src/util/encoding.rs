//! Parsed `Accept-Encoding` request header.

#[derive(Debug, Default, Clone)]
pub struct AcceptEncoding {
    pub brotli: bool,
    pub gzip: bool,
    pub deflate: bool,
    pub zstd: bool,
}

impl AcceptEncoding {
    pub fn parse(value: &str) -> Self {
        let mut enc = Self::default();
        for part in value.split(',') {
            let mut segments = part.trim().split(';');
            let token = segments.next().unwrap_or("").trim().to_ascii_lowercase();
            // Skip encodings explicitly disabled with q=0. Parsed numerically
            // rather than matched against specific textual forms (issue
            // #284) -- RFC 9110's qvalue grammar allows up to three
            // fractional digits (`q=0`, `q=0.0`, `q=0.00`, `q=0.000` are all
            // equally valid), and matching only two of those forms let
            // `q=0.00`/`q=0.000` silently re-enable an encoding the client
            // explicitly disabled.
            let is_zero_q = segments.any(|seg| {
                seg.trim()
                    .to_ascii_lowercase()
                    .strip_prefix("q=")
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .is_some_and(|q| q == 0.0)
            });
            if is_zero_q {
                continue;
            }
            match token.as_str() {
                "br" => enc.brotli = true,
                "gzip" => enc.gzip = true,
                "deflate" => enc.deflate = true,
                "zstd" => enc.zstd = true,
                _ => {}
            }
        }
        enc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_enables_nothing() {
        let enc = AcceptEncoding::parse("");
        assert!(!enc.brotli && !enc.gzip && !enc.deflate);
    }

    #[test]
    fn parse_gzip_only() {
        let enc = AcceptEncoding::parse("gzip");
        assert!(enc.gzip);
        assert!(!enc.brotli);
        assert!(!enc.deflate);
    }

    #[test]
    fn parse_br_only() {
        let enc = AcceptEncoding::parse("br");
        assert!(enc.brotli);
        assert!(!enc.gzip);
    }

    #[test]
    fn parse_multiple_encodings() {
        let enc = AcceptEncoding::parse("br, gzip, deflate, zstd");
        assert!(enc.brotli);
        assert!(enc.gzip);
        assert!(enc.deflate);
        assert!(enc.zstd);
    }

    #[test]
    fn parse_zstd_only() {
        let enc = AcceptEncoding::parse("zstd");
        assert!(enc.zstd);
        assert!(!enc.brotli);
        assert!(!enc.gzip);
    }

    #[test]
    fn parse_q_zero_disables_encoding() {
        let enc = AcceptEncoding::parse("gzip;q=0, br");
        assert!(!enc.gzip, "gzip with q=0 must be skipped");
        assert!(enc.brotli);
    }

    #[test]
    fn parse_q_zero_zero_disables_encoding() {
        let enc = AcceptEncoding::parse("gzip;q=0.0");
        assert!(!enc.gzip);
    }

    #[test]
    fn parse_q_zero_two_decimals_disables_encoding() {
        // Issue #284: q=0.00 is an equally valid all-zero qvalue per RFC
        // 9110 but wasn't recognized by the old string-matching check.
        let enc = AcceptEncoding::parse("gzip;q=0.00");
        assert!(!enc.gzip, "q=0.00 must disable the encoding");
    }

    #[test]
    fn parse_q_zero_three_decimals_disables_encoding() {
        let enc = AcceptEncoding::parse("gzip;q=0.000");
        assert!(!enc.gzip, "q=0.000 must disable the encoding");
    }

    #[test]
    fn parse_q_nonzero_does_not_disable_encoding() {
        // A low but nonzero qvalue must NOT be treated as disabled.
        let enc = AcceptEncoding::parse("gzip;q=0.001");
        assert!(enc.gzip, "a nonzero qvalue must not disable the encoding");
    }

    #[test]
    fn parse_case_insensitive() {
        let enc = AcceptEncoding::parse("GZip, BR, Deflate");
        assert!(enc.gzip);
        assert!(enc.brotli);
        assert!(enc.deflate);
    }

    #[test]
    fn parse_unknown_token_ignored() {
        let enc = AcceptEncoding::parse("identity, zstd, gzip");
        assert!(enc.gzip);
        assert!(!enc.brotli);
    }
}
