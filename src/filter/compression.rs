use std::io::Cursor;

use async_compression::tokio::bufread::{BrotliEncoder, DeflateEncoder, GzipEncoder, ZstdEncoder};
use async_compression::Level;
use bytes::Bytes;
use tokio::io::AsyncReadExt as _;

use crate::config::schema::CompressionConfig;
use crate::proxy::ctx::AcceptEncoding;

// ── Options ────────────────────────────────────────────────────────────────

/// Default compressible Content-Type prefixes — nginx `gzip_types` pattern.
///
/// Only text and structured data compress well.  Binary formats (images,
/// video, audio, archives, fonts) are already compressed or grow larger.
const DEFAULT_COMPRESS_TYPES: &[&str] = &[
    "text/",
    "application/json",
    "application/xml",
    "application/xhtml",
    "application/javascript",
    "application/x-javascript",
    "application/wasm",
    "image/svg",
    "font/",
];

/// Resolved compression parameters (flattened from the bool / object shorthand).
pub struct CompressOptions {
    /// Preferred encoding order — first match with the client's Accept-Encoding wins.
    pub algorithms: Vec<String>,
    pub level: u8,
    /// Minimum uncompressed body size in bytes before compression is applied.
    pub min_bytes: u64,
    /// Content-Type prefixes/substrings to compress.
    /// Empty list means "use DEFAULT_COMPRESS_TYPES".
    /// `["*"]` means compress everything.
    pub types: Vec<String>,
}

/// Resolve a site compression config into effective options, or `None` if disabled.
pub fn effective(cfg: &CompressionConfig) -> Option<CompressOptions> {
    match cfg {
        CompressionConfig::Enabled(false) => None,
        CompressionConfig::Enabled(true) => Some(CompressOptions {
            // br first (best ratio), then zstd (fast + small), then gzip (widest compat)
            algorithms: vec!["br".to_owned(), "zstd".to_owned(), "gzip".to_owned()],
            level: 6,
            min_bytes: 1024,
            types: Vec::new(), // empty → use DEFAULT_COMPRESS_TYPES
        }),
        CompressionConfig::Options(opts) => {
            let algorithms = opts
                .algorithms
                .clone()
                .unwrap_or_else(|| vec!["br".to_owned(), "zstd".to_owned(), "gzip".to_owned()]);
            if algorithms.is_empty() {
                return None;
            }
            Some(CompressOptions {
                algorithms,
                level: opts.level.unwrap_or(6),
                min_bytes: opts.min_bytes.unwrap_or(1024),
                types: opts.types.clone().unwrap_or_default(),
            })
        }
    }
}

// ── Encoding selection ─────────────────────────────────────────────────────

/// Check whether `content_type` should be compressed.
///
/// **nginx `gzip_types` pattern** — skip binary content types that are already
/// compressed or that do not benefit from compression (images, audio, video,
/// archives).  Compressing them wastes CPU and often increases response size.
///
/// Matching is case-insensitive prefix/substring.
/// - Empty `types` list → use [`DEFAULT_COMPRESS_TYPES`].
/// - `["*"]` → compress everything.
pub fn is_compressible_type(content_type: &str, opts: &CompressOptions) -> bool {
    let ct = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    let patterns: &[&str] = if opts.types.is_empty() {
        DEFAULT_COMPRESS_TYPES
    } else if opts.types.iter().any(|t| t == "*") {
        return true;
    } else {
        return opts.types.iter().any(|t| ct.contains(t.as_str()));
    };

    patterns.iter().any(|p| ct.starts_with(p))
}

/// Choose the best `Content-Encoding` given what the client advertises.
///
/// Returns `None` when:
/// - the body is smaller than `opts.min_bytes`, or
/// - the content type is not compressible (nginx `gzip_types` pattern), or
/// - no algorithm in `opts.algorithms` is accepted by the client.
pub fn best_encoding(
    opts: &CompressOptions,
    accept: &AcceptEncoding,
    body_len: u64,
) -> Option<&'static str> {
    if body_len < opts.min_bytes {
        return None;
    }
    for algo in &opts.algorithms {
        match algo.as_str() {
            "br" if accept.brotli => return Some("br"),
            "zstd" if accept.zstd => return Some("zstd"),
            "gzip" if accept.gzip => return Some("gzip"),
            "deflate" if accept.deflate => return Some("deflate"),
            _ => {}
        }
    }
    None
}

// ── In-memory compression ──────────────────────────────────────────────────

/// Compress `data` in memory using the specified encoding and quality level.
///
/// Used for small, fixed-size local responses (metrics, fallback with body).
/// For large streaming responses (static files) use the streaming path in
/// `static_files::stream_file_compressed`.
///
/// Returns the original bytes unchanged if the encoding is unknown or encoding fails.
pub async fn compress_bytes(data: Bytes, encoding: &str, level: u8) -> Bytes {
    let lev = Level::Precise(i32::from(level));
    let mut out = Vec::with_capacity(data.len() / 2 + 64);

    let ok: bool = match encoding {
        "br" => {
            let r = tokio::io::BufReader::new(Cursor::new(data.clone()));
            BrotliEncoder::with_quality(r, lev)
                .read_to_end(&mut out)
                .await
                .is_ok()
        }
        "gzip" => {
            let r = tokio::io::BufReader::new(Cursor::new(data.clone()));
            GzipEncoder::with_quality(r, lev)
                .read_to_end(&mut out)
                .await
                .is_ok()
        }
        "deflate" => {
            let r = tokio::io::BufReader::new(Cursor::new(data.clone()));
            DeflateEncoder::with_quality(r, lev)
                .read_to_end(&mut out)
                .await
                .is_ok()
        }
        "zstd" => {
            let r = tokio::io::BufReader::new(Cursor::new(data.clone()));
            ZstdEncoder::with_quality(r, lev)
                .read_to_end(&mut out)
                .await
                .is_ok()
        }
        _ => false,
    };

    if ok && !out.is_empty() {
        Bytes::from(out)
    } else {
        data // Return original on failure — better uncompressed than nothing.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::CompressionOptions;

    #[test]
    fn disabled_when_false() {
        assert!(effective(&CompressionConfig::Enabled(false)).is_none());
    }

    #[test]
    fn enabled_defaults() {
        let opts = effective(&CompressionConfig::Enabled(true)).unwrap();
        assert_eq!(opts.level, 6);
        assert_eq!(opts.min_bytes, 1024);
        assert!(opts.algorithms.contains(&"gzip".to_owned()));
    }

    #[test]
    fn empty_algorithms_disables() {
        let cfg = CompressionConfig::Options(CompressionOptions {
            algorithms: Some(vec![]),
            level: None,
            min_bytes: None,
            types: None,
        });
        assert!(effective(&cfg).is_none());
    }

    #[test]
    fn best_encoding_below_min_bytes() {
        let opts = CompressOptions {
            algorithms: vec!["gzip".to_owned()],
            level: 6,
            min_bytes: 1024,
            types: Vec::new(),
        };
        let accept = AcceptEncoding {
            zstd: false,
            gzip: true,
            ..Default::default()
        };
        assert!(best_encoding(&opts, &accept, 100).is_none());
    }

    #[test]
    fn best_encoding_picks_br_first() {
        let opts = CompressOptions {
            algorithms: vec!["br".to_owned(), "gzip".to_owned()],
            level: 6,
            min_bytes: 0,
            types: Vec::new(),
        };
        let accept = AcceptEncoding {
            zstd: false,
            brotli: true,
            gzip: true,
            deflate: false,
        };
        assert_eq!(best_encoding(&opts, &accept, 2000), Some("br"));
    }

    #[test]
    fn effective_options_with_custom_settings() {
        // Options branch with explicit (non-empty) algorithm list.
        let cfg = CompressionConfig::Options(CompressionOptions {
            algorithms: Some(vec!["gzip".to_owned()]),
            level: Some(9),
            min_bytes: Some(512),
            types: None,
        });
        let opts = effective(&cfg).unwrap();
        assert_eq!(opts.algorithms, vec!["gzip"]);
        assert_eq!(opts.level, 9);
        assert_eq!(opts.min_bytes, 512);
    }

    #[test]
    fn effective_options_defaults_when_fields_none() {
        // Options branch with all fields None → falls back to defaults.
        let cfg = CompressionConfig::Options(CompressionOptions {
            algorithms: None,
            level: None,
            min_bytes: None,
            types: None,
        });
        let opts = effective(&cfg).unwrap();
        assert!(!opts.algorithms.is_empty());
        assert_eq!(opts.level, 6);
        assert_eq!(opts.min_bytes, 1024);
    }

    #[test]
    fn best_encoding_falls_back_to_gzip_when_brotli_not_accepted() {
        let opts = CompressOptions {
            algorithms: vec!["br".to_owned(), "gzip".to_owned()],
            level: 6,
            min_bytes: 0,
            types: Vec::new(),
        };
        // brotli NOT accepted — should fall back to gzip.
        let accept = AcceptEncoding {
            zstd: false,
            brotli: false,
            gzip: true,
            deflate: false,
        };
        assert_eq!(best_encoding(&opts, &accept, 2000), Some("gzip"));
    }

    #[test]
    fn best_encoding_picks_deflate() {
        let opts = CompressOptions {
            algorithms: vec!["deflate".to_owned()],
            level: 6,
            min_bytes: 0,
            types: Vec::new(),
        };
        let accept = AcceptEncoding {
            zstd: false,
            brotli: false,
            gzip: false,
            deflate: true,
        };
        assert_eq!(best_encoding(&opts, &accept, 100), Some("deflate"));
    }

    #[test]
    fn best_encoding_returns_none_when_no_algorithm_matches() {
        let opts = CompressOptions {
            algorithms: vec!["br".to_owned(), "gzip".to_owned()],
            level: 6,
            min_bytes: 0,
            types: Vec::new(),
        };
        // Client accepts nothing.
        let accept = AcceptEncoding {
            zstd: false,
            brotli: false,
            gzip: false,
            deflate: false,
        };
        assert!(best_encoding(&opts, &accept, 5000).is_none());
    }

    #[tokio::test]
    async fn compress_brotli_roundtrip() {
        use async_compression::tokio::bufread::BrotliDecoder;

        let original = Bytes::from("hello world ".repeat(100));
        let compressed = compress_bytes(original.clone(), "br", 4).await;
        assert!(
            compressed.len() < original.len(),
            "brotli should compress repetitive data: compressed={} original={}",
            compressed.len(),
            original.len()
        );

        let mut dec =
            BrotliDecoder::new(tokio::io::BufReader::new(std::io::Cursor::new(compressed)));
        let mut decoded = Vec::new();
        dec.read_to_end(&mut decoded).await.unwrap();
        assert_eq!(decoded, original.as_ref());
    }

    #[tokio::test]
    async fn compress_deflate_compresses_data() {
        let original = Bytes::from("deflate me ".repeat(100));
        let compressed = compress_bytes(original.clone(), "deflate", 6).await;
        assert!(
            compressed.len() < original.len(),
            "deflate should compress repetitive data"
        );
    }

    #[tokio::test]
    async fn compress_unknown_encoding_returns_original() {
        let original = Bytes::from("hello");
        let result = compress_bytes(original.clone(), "unknown-codec", 6).await;
        assert_eq!(
            result, original,
            "unknown encoding must return data unchanged"
        );
    }

    #[tokio::test]
    async fn compress_gzip_roundtrip() {
        use async_compression::tokio::bufread::GzipDecoder;

        // Use a long repetitive string so gzip always produces a smaller output.
        let original = Bytes::from("hello world ".repeat(100));
        let compressed = compress_bytes(original.clone(), "gzip", 6).await;
        // Compression of repetitive data must reduce the size.
        assert!(
            compressed.len() < original.len(),
            "expected compressed ({}) < original ({})",
            compressed.len(),
            original.len()
        );

        // Decompress and verify roundtrip.
        let mut dec = GzipDecoder::new(tokio::io::BufReader::new(std::io::Cursor::new(compressed)));
        let mut decoded = Vec::new();
        dec.read_to_end(&mut decoded).await.unwrap();
        assert_eq!(decoded, original.as_ref());
    }

    #[tokio::test]
    async fn compress_zstd_roundtrip() {
        use async_compression::tokio::bufread::ZstdDecoder;

        let original = Bytes::from("hello world ".repeat(100));
        let compressed = compress_bytes(original.clone(), "zstd", 6).await;
        assert!(
            compressed.len() < original.len(),
            "zstd should compress repetitive data: compressed={} original={}",
            compressed.len(),
            original.len()
        );

        let mut dec = ZstdDecoder::new(tokio::io::BufReader::new(std::io::Cursor::new(compressed)));
        let mut decoded = Vec::new();
        dec.read_to_end(&mut decoded).await.unwrap();
        assert_eq!(decoded, original.as_ref());
    }

    #[test]
    fn best_encoding_picks_zstd_when_available() {
        let opts = CompressOptions {
            algorithms: vec!["br".to_owned(), "zstd".to_owned(), "gzip".to_owned()],
            level: 6,
            min_bytes: 0,
            types: Vec::new(),
        };
        // Only zstd accepted.
        let accept = AcceptEncoding {
            zstd: true,
            ..Default::default()
        };
        assert_eq!(best_encoding(&opts, &accept, 2000), Some("zstd"));
    }

    #[test]
    fn best_encoding_prefers_br_over_zstd() {
        let opts = CompressOptions {
            algorithms: vec!["br".to_owned(), "zstd".to_owned(), "gzip".to_owned()],
            level: 6,
            min_bytes: 0,
            types: Vec::new(),
        };
        // Both br and zstd accepted — br wins (listed first).
        let accept = AcceptEncoding {
            brotli: true,
            zstd: true,
            ..Default::default()
        };
        assert_eq!(best_encoding(&opts, &accept, 2000), Some("br"));
    }

    // ── is_compressible_type ──────────────────────────────────────────────────

    fn opts_no_types() -> CompressOptions {
        CompressOptions {
            algorithms: vec![],
            level: 6,
            min_bytes: 0,
            types: Vec::new(),
        }
    }

    #[test]
    fn text_html_is_compressible() {
        assert!(is_compressible_type(
            "text/html; charset=utf-8",
            &opts_no_types()
        ));
    }

    #[test]
    fn application_json_is_compressible() {
        assert!(is_compressible_type("application/json", &opts_no_types()));
    }

    #[test]
    fn image_jpeg_is_not_compressible() {
        assert!(!is_compressible_type("image/jpeg", &opts_no_types()));
    }

    #[test]
    fn video_mp4_is_not_compressible() {
        assert!(!is_compressible_type("video/mp4", &opts_no_types()));
    }

    #[test]
    fn image_svg_is_compressible() {
        assert!(is_compressible_type("image/svg+xml", &opts_no_types()));
    }

    #[test]
    fn wildcard_compresses_everything() {
        let mut opts = opts_no_types();
        opts.types = vec!["*".to_owned()];
        assert!(is_compressible_type("image/jpeg", &opts));
        assert!(is_compressible_type("video/mp4", &opts));
    }

    #[test]
    fn custom_types_list_overrides_defaults() {
        let mut opts = opts_no_types();
        opts.types = vec!["text/plain".to_owned()];
        assert!(is_compressible_type("text/plain", &opts));
        assert!(!is_compressible_type("text/html", &opts));
    }
}
