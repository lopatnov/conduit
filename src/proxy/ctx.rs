use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use crate::config::schema::StaticOptions;

#[derive(Debug)]
pub struct RequestCtx {
    pub site_idx: usize,
    pub upstream: UpstreamTarget,
    pub start_time: Instant,
    pub accept_enc: AcceptEncoding,
}

impl RequestCtx {
    pub fn new(site_idx: usize, upstream: UpstreamTarget) -> Self {
        Self {
            site_idx,
            upstream,
            start_time: Instant::now(),
            accept_enc: AcceptEncoding::default(),
        }
    }
}

#[derive(Debug)]
pub enum UpstreamTarget {
    Local(LocalHandler),
    Proxy {
        addr: SocketAddr,
        strip_prefix: Option<String>,
    },
    Upload {
        addr: SocketAddr,
    },
}

#[derive(Debug, Clone)]
pub enum LocalHandler {
    Health,
    Fallback,
    Metrics { token: Option<String> },
    StaticFile {
        roots: Vec<PathBuf>,
        options: Arc<StaticOptions>,
        strip_prefix: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct AcceptEncoding {
    pub brotli: bool,
    pub gzip: bool,
    pub deflate: bool,
}

impl AcceptEncoding {
    pub fn parse(value: &str) -> Self {
        let mut enc = Self::default();
        for part in value.split(',') {
            let token = part
                .trim()
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            match token.as_str() {
                "br" => enc.brotli = true,
                "gzip" => enc.gzip = true,
                "deflate" => enc.deflate = true,
                _ => {}
            }
        }
        enc
    }
}
