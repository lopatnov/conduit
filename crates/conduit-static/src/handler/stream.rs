//! Copying the body of an opened file to the client, plain or compressed on the fly.

#[cfg(feature = "compression")]
use async_compression::tokio::bufread::{BrotliEncoder, DeflateEncoder, GzipEncoder};
#[cfg(feature = "compression")]
use async_compression::Level;
use bytes::Bytes;
use pingora_core::Result;
use pingora_proxy::Session;
#[cfg(feature = "compression")]
use tokio::io::AsyncRead;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// Map a file I/O error to a Pingora-level internal error.
fn io_err(e: std::io::Error) -> Box<pingora_core::Error> {
    pingora_core::Error::explain(pingora_core::ErrorType::InternalError, e.to_string())
}

/// Stream a file compressed with the given encoding and quality level.
///
/// Uses Tokio async encoders so no blocking thread is required.  No
/// Content-Length is sent — the HTTP/1.1 layer uses chunked transfer encoding
/// automatically.
///
/// `compress` (in `handle_static`) is only ever `Some` when the `compression`
/// feature is compiled in (see its `#[cfg(not(feature = "compression"))]`
/// fallback, which forces it to `None`) — so the `#[cfg(not(feature =
/// "compression"))]` variant below is unreachable in practice; it exists only
/// so this function type-checks (and drops `async-compression` from the
/// dependency tree) in a `compression`-less build.
#[cfg(feature = "compression")]
pub(super) async fn stream_file_compressed(
    session: &mut Session,
    mut file: tokio::fs::File,
    offset: u64,
    length: u64,
    encoding: &str,
    level: u8,
) -> Result<()> {
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_err)?;
    }

    // Limit the file reader to `length` bytes.
    let limited = file.take(length);
    let buf_reader = tokio::io::BufReader::new(limited);
    let lev = Level::Precise(i32::from(level));

    match encoding {
        "br" => {
            let encoder = BrotliEncoder::with_quality(buf_reader, lev);
            stream_encoded(session, encoder).await
        }
        "gzip" => {
            let encoder = GzipEncoder::with_quality(buf_reader, lev);
            stream_encoded(session, encoder).await
        }
        "deflate" => {
            let encoder = DeflateEncoder::with_quality(buf_reader, lev);
            stream_encoded(session, encoder).await
        }
        _ => {
            // Unknown encoding — fall back to uncompressed streaming. Nothing has been read yet, so the
            // file is taken back out of the reader and `stream_file` seeks to `offset` again.
            stream_file(
                session,
                buf_reader.into_inner().into_inner(),
                offset,
                length,
            )
            .await
        }
    }
}

/// See the `#[cfg(feature = "compression")]` overload above — unreachable
/// stub for a `compression`-less build (`compress` is always `None` in that
/// build, so `serve_full` never calls this).
#[cfg(not(feature = "compression"))]
pub(super) async fn stream_file_compressed(
    session: &mut Session,
    file: tokio::fs::File,
    offset: u64,
    length: u64,
    _encoding: &str,
    _level: u8,
) -> Result<()> {
    stream_file(session, file, offset, length).await
}

/// Drain an `AsyncRead` encoder in 64 KiB chunks, signalling `done=true` on
/// the last write.
///
/// We use a "one-chunk-ahead" pattern: we always buffer the chunk we just read
/// and send it on the *next* iteration, so we know whether there is more data
/// before we call `write_response_body`.  This lets us set `done=true` on the
/// final chunk without reading an extra zero-length chunk first.
#[cfg(feature = "compression")]
async fn stream_encoded<R: AsyncRead + Unpin>(session: &mut Session, mut reader: R) -> Result<()> {
    const CHUNK: usize = 64 * 1024;
    let mut buf = vec![0u8; CHUNK];
    let mut pending: Option<Bytes> = None;

    loop {
        let n = reader.read(&mut buf).await.map_err(io_err)?;

        if n == 0 {
            // EOF — flush whatever is pending (may be None if the file was empty).
            let chunk = pending.take();
            session.write_response_body(chunk, true).await?;
            return Ok(());
        }

        // Send the previously buffered chunk (not done yet — we have more data).
        if let Some(prev) = pending.take() {
            session.write_response_body(Some(prev), false).await?;
        }

        pending = Some(Bytes::copy_from_slice(&buf[..n]));
    }
}

/// Stream `length` bytes from `file` starting at `offset` in 64 KiB chunks. `file` is already open
/// (issue #618), so no open can fail once the response headers are out.
pub(super) async fn stream_file(
    session: &mut Session,
    mut file: tokio::fs::File,
    offset: u64,
    length: u64,
) -> Result<()> {
    const CHUNK: usize = 64 * 1024;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_err)?;
    }
    let mut remaining = length;
    let mut buf = vec![0u8; CHUNK];
    while remaining > 0 {
        let to_read = (remaining as usize).min(CHUNK);
        let n = file.read(&mut buf[..to_read]).await.map_err(io_err)?;
        if n == 0 {
            break; // Unexpected EOF — client will detect truncation.
        }
        remaining -= n as u64;
        let chunk = Bytes::copy_from_slice(&buf[..n]);
        let done = remaining == 0;
        session.write_response_body(Some(chunk), done).await?;
    }
    Ok(())
}
