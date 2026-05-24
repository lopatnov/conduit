use std::io::Cursor;

use async_compression::tokio::bufread::{BrotliEncoder, DeflateEncoder, GzipEncoder};
use async_compression::Level;
use bytes::Bytes;
use tokio::io::AsyncReadExt as _;

use crate::config::schema::CompressionConfig;
use crate::proxy::ctx::AcceptEncoding;

// ── Options ────────────────────────────────────────────────────────────────

/// Resolved compression parameters (flattened from the bool / object shorthand).
pub struct CompressOptions {
    /// Preferred encoding order — first match with the client's Accept-Encoding wins.
    pub algorithms: Vec<String>,
    pub level: u8,
    /// Minimum uncompressed body size in bytes before compression is applied.
    pub min_bytes: u64,
}

/// Resolve a site compression config into effective options, or `None` if disabled.
pub fn effective(cfg: &CompressionConfig) -> Option<CompressOptions> {
    match cfg {
        CompressionConfig::Enabled(false) => None,
        CompressionConfig::Enabled(true) => Some(CompressOptions {
            algorithms: vec!["br".to_owned(), "gzip".to_owned()],
            level: 6,
            min_bytes: 1024,
        }),
        CompressionConfig::Options(opts) => {
            let algorithms = opts
                .algorithms
                .clone()
                .unwrap_or_else(|| vec!["br".to_owned(), "gzip".to_owned()]);
            if algorithms.is_empty() {
                return None;
            }
            Some(CompressOptions {
                algorithms,
                level: opts.level.unwrap_or(6),
                min_bytes: opts.min_bytes.unwrap_or(1024),
            })
        }
    }
}

// ── Encoding selection ─────────────────────────────────────────────────────

/// Choose the best `Content-Encoding` given what the client advertises.
///
/// Returns `None` when:
/// - the body is smaller than `opts.min_bytes`, or
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
        });
        assert!(effective(&cfg).is_none());
    }

    #[test]
    fn best_encoding_below_min_bytes() {
        let opts = CompressOptions {
            algorithms: vec!["gzip".to_owned()],
            level: 6,
            min_bytes: 1024,
        };
        let accept = AcceptEncoding {
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
        };
        let accept = AcceptEncoding {
            brotli: true,
            gzip: true,
            deflate: false,
        };
        assert_eq!(best_encoding(&opts, &accept, 2000), Some("br"));
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
        let mut dec =
            GzipDecoder::new(tokio::io::BufReader::new(std::io::Cursor::new(compressed)));
        let mut decoded = Vec::new();
        dec.read_to_end(&mut decoded).await.unwrap();
        assert_eq!(decoded, original.as_ref());
    }
}
