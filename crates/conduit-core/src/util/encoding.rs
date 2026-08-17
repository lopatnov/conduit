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
            // Skip encodings explicitly disabled with q=0 or q=0.0.
            let is_zero_q = segments.any(|seg| {
                let seg = seg.trim();
                seg.eq_ignore_ascii_case("q=0") || seg.eq_ignore_ascii_case("q=0.0")
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
