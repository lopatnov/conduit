//! The byte relay of a `tcp` site: both directions at once, with an idle timeout over the two (issue #757).
//!
//! `tokio::io::copy_bidirectional` has no way to stop a connection that has gone quiet, so the copy loops are written
//! out here. The semantics are the same: a side that reaches end of input shuts down the write half of the other, the
//! other direction goes on until it ends too, and an error on either direction ends the connection. The addition is a
//! deadline that every read or write pushes forward (nginx `proxy_timeout`, HAProxy `timeout tunnel`): once nothing
//! has moved in either direction for the idle time, the connection is closed. A peer that stops reading is idle as
//! well, because a write that cannot finish does not count as activity.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// tokio's clock, not std's: a test with paused time moves it, and so do `sleep`s, so the deadline and the sleep agree.
use tokio::time::Instant;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Buffer of one direction. The same as the 8 KiB of `copy_bidirectional`, doubled: one `read` per segment burst.
const BUF: usize = 16 * 1024;

/// How a relay ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Both directions reached end of input. Bytes client to upstream, bytes upstream to client.
    Closed(u64, u64),
    /// Nothing moved for the idle time.
    Idle,
}

/// Relay between `client` and `upstream` until both directions end, one of them fails, or (with `idle` set) nothing
/// has moved for that long.
pub async fn relay<C, U>(client: C, upstream: U, idle: Option<Duration>) -> io::Result<Outcome>
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (mut client_r, mut client_w) = tokio::io::split(client);
    let (mut upstream_r, mut upstream_w) = tokio::io::split(upstream);
    let epoch = Instant::now();
    // Milliseconds after `epoch` of the last byte that moved in either direction.
    let last = AtomicU64::new(0);

    let both = async {
        tokio::try_join!(
            pump(&mut client_r, &mut upstream_w, &last, epoch),
            pump(&mut upstream_r, &mut client_w, &last, epoch)
        )
    };
    let Some(idle) = idle else {
        return both.await.map(|(up, down)| Outcome::Closed(up, down));
    };
    tokio::select! {
        done = both => done.map(|(up, down)| Outcome::Closed(up, down)),
        () = watchdog(&last, epoch, idle) => Ok(Outcome::Idle),
    }
}

/// Copy `from` into `to` until end of input, then shut down the write half of `to`.
async fn pump<R, W>(from: &mut R, to: &mut W, last: &AtomicU64, epoch: Instant) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; BUF];
    let mut total = 0u64;
    loop {
        let n = from.read(&mut buf).await?;
        if n == 0 {
            // The other side may already be gone: that is not an error of this direction.
            let _ = to.shutdown().await;
            return Ok(total);
        }
        last.store(elapsed_ms(epoch), Ordering::Relaxed);
        to.write_all(&buf[..n]).await?;
        // A buffered stream (Pingora's `Stream` buffers writes) would otherwise hold the bytes until the next read.
        to.flush().await?;
        total += n as u64;
        last.store(elapsed_ms(epoch), Ordering::Relaxed);
    }
}

/// Milliseconds of a duration, saturating: a huge `idleTimeoutSecs` must not wrap to a tiny timeout.
pub(crate) fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn elapsed_ms(epoch: Instant) -> u64 {
    millis(epoch.elapsed())
}

/// Completes once `idle` has passed since the last activity.
async fn watchdog(last: &AtomicU64, epoch: Instant, idle: Duration) {
    let idle_ms = millis(idle);
    loop {
        let quiet_since = last.load(Ordering::Relaxed);
        let deadline = quiet_since.saturating_add(idle_ms);
        let now = elapsed_ms(epoch);
        if now >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(deadline - now)).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_huge_duration_saturates_instead_of_wrapping() {
        // 18446744073709552 s is 2^64 + 384 ms: a plain `as u64` would give 384 ms
        let huge = std::time::Duration::from_secs(18_446_744_073_709_552);
        assert_eq!(millis(huge), u64::MAX);
        assert_eq!(millis(std::time::Duration::from_millis(1500)), 1500);
    }

    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn bytes_go_both_ways_and_the_counts_are_returned() {
        let (mut client, client_side) = duplex(64 * 1024);
        let (mut upstream, upstream_side) = duplex(64 * 1024);
        let relayed = tokio::spawn(relay(client_side, upstream_side, None));

        client.write_all(b"ping").await.unwrap();
        let mut got = [0u8; 4];
        upstream.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"ping");
        upstream.write_all(b"pong!").await.unwrap();
        let mut back = [0u8; 5];
        client.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"pong!");

        drop(client);
        drop(upstream);
        assert_eq!(relayed.await.unwrap().unwrap(), Outcome::Closed(4, 5));
    }

    #[tokio::test]
    async fn a_half_close_still_lets_the_reply_through() {
        let (mut client, client_side) = duplex(1024);
        let (mut upstream, upstream_side) = duplex(1024);
        let relayed = tokio::spawn(relay(client_side, upstream_side, None));

        client.write_all(b"last words").await.unwrap();
        client.shutdown().await.unwrap();
        let mut asked = Vec::new();
        upstream.read_to_end(&mut asked).await.unwrap();
        assert_eq!(
            asked, b"last words",
            "the upstream sees the request and then its end"
        );
        upstream.write_all(b"reply").await.unwrap();
        upstream.shutdown().await.unwrap();
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.unwrap();
        assert_eq!(
            reply, b"reply",
            "the client still gets the reply after it shut down its write half"
        );
        assert_eq!(relayed.await.unwrap().unwrap(), Outcome::Closed(10, 5));
    }

    #[tokio::test]
    async fn a_large_transfer_is_intact() {
        let (mut client, client_side) = duplex(8 * 1024);
        let (mut upstream, upstream_side) = duplex(8 * 1024);
        tokio::spawn(relay(
            client_side,
            upstream_side,
            Some(Duration::from_secs(30)),
        ));
        let blob: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let sent = blob.clone();
        let writer = tokio::spawn(async move {
            client.write_all(&sent).await.unwrap();
            client.shutdown().await.unwrap();
        });
        let mut got = Vec::new();
        upstream.read_to_end(&mut got).await.unwrap();
        writer.await.unwrap();
        assert_eq!(got.len(), blob.len());
        assert!(got == blob, "the bytes changed on the way");
    }

    #[tokio::test(start_paused = true)]
    async fn a_quiet_connection_is_closed_after_the_idle_time() {
        let (_client, client_side) = duplex(1024);
        let (_upstream, upstream_side) = duplex(1024);
        let relayed = tokio::spawn(relay(
            client_side,
            upstream_side,
            Some(Duration::from_secs(60)),
        ));
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert!(!relayed.is_finished(), "still within the idle time");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(relayed.await.unwrap().unwrap(), Outcome::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn traffic_in_either_direction_keeps_the_connection_open() {
        let (mut client, client_side) = duplex(1024);
        let (mut upstream, upstream_side) = duplex(1024);
        let relayed = tokio::spawn(relay(
            client_side,
            upstream_side,
            Some(Duration::from_secs(60)),
        ));
        let mut one = [0u8; 1];
        // a byte every 40 s, alternating directions: 200 s in all, well past one idle time
        for round in 0..5 {
            tokio::time::sleep(Duration::from_secs(40)).await;
            if round % 2 == 0 {
                client.write_all(b"c").await.unwrap();
                upstream.read_exact(&mut one).await.unwrap();
            } else {
                upstream.write_all(b"u").await.unwrap();
                client.read_exact(&mut one).await.unwrap();
            }
            assert!(!relayed.is_finished(), "round {round}");
        }
        tokio::time::sleep(Duration::from_secs(61)).await;
        assert_eq!(relayed.await.unwrap().unwrap(), Outcome::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn a_peer_that_stops_reading_is_idle_too() {
        // The client writes more than the pipe to the upstream holds, and the upstream never reads: the write cannot
        // finish, nothing moves, and the connection ends at the idle time instead of holding the slot for ever.
        let (mut client, client_side) = duplex(1024);
        let (_upstream, upstream_side) = duplex(1024);
        let relayed = tokio::spawn(relay(
            client_side,
            upstream_side,
            Some(Duration::from_secs(30)),
        ));
        let blob = vec![7u8; 256 * 1024];
        let _writer = tokio::spawn(async move {
            let _ = client.write_all(&blob).await;
        });
        tokio::time::sleep(Duration::from_secs(31)).await;
        assert_eq!(relayed.await.unwrap().unwrap(), Outcome::Idle);
    }

    #[tokio::test]
    async fn without_an_idle_time_a_quiet_connection_stays_open() {
        let (_client, client_side) = duplex(1024);
        let (_upstream, upstream_side) = duplex(1024);
        let relayed = tokio::spawn(relay(client_side, upstream_side, None));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!relayed.is_finished());
        relayed.abort();
    }

    /// A stream that holds what is written until it is flushed, like Pingora's `Stream` with `buffer_write` set: a relay
    /// that forgets to flush delivers nothing until the next read of the same direction.
    struct HoldUntilFlush<S> {
        inner: S,
        held: Vec<u8>,
    }

    impl<S: AsyncRead + Unpin> AsyncRead for HoldUntilFlush<S> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for HoldUntilFlush<S> {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            data: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            self.held.extend_from_slice(data);
            std::task::Poll::Ready(Ok(data.len()))
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            while !self.held.is_empty() {
                let this = &mut *self;
                let n = std::task::ready!(
                    std::pin::Pin::new(&mut this.inner).poll_write(cx, &this.held)
                )?;
                this.held.drain(..n);
            }
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn a_stream_that_buffers_writes_still_gets_the_bytes_at_once() {
        // The upstream speaks first (a greeting), the client has sent nothing: the bytes must reach the client without
        // waiting for anything else to happen on the connection. Found with a real Pingora listener (#757).
        let (mut client, client_side) = duplex(1024);
        let (mut upstream, upstream_side) = duplex(1024);
        let held_client = HoldUntilFlush {
            inner: client_side,
            held: Vec::new(),
        };
        tokio::spawn(relay(held_client, upstream_side, None));
        upstream.write_all(b"greeting").await.unwrap();
        let mut got = [0u8; 8];
        tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut got))
            .await
            .expect("the greeting reaches the client without a further read")
            .unwrap();
        assert_eq!(&got, b"greeting");
    }
}
