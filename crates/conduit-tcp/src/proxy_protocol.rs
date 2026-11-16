//! The PROXY protocol, versions 1 and 2 (HAProxy `proxy-protocol.txt`): parse the header a load balancer sends at the start
//! of a connection to say who the real client is, and write one to an upstream that expects it (#757).
//!
//! The parser is strict, because the header decides which address the rest of Conduit trusts for a client (IP filter, rate
//! limit, per-client connection caps, logs):
//!
//! * it is read **once**, at the start of the connection, and only from a peer the operator listed as trusted — the caller
//!   decides that, this module never sees an untrusted peer;
//! * its length is capped (`max`), and a header that is not complete when the cap is reached is refused, not waited for;
//! * a v2 header must be exactly the length it declares, its address block must fit the family, and its TLVs must tile the
//!   rest of the header; anything else is [`Error::Invalid`], never guessed at;
//! * `LOCAL` (v2) and `UNKNOWN` (v1) carry no address: the caller keeps the address of the socket (the spec's rule for
//!   a load balancer's own health checks), and an `UNSPEC` or `UNIX` family does the same;
//! * an IPv4-mapped IPv6 source is turned into the IPv4 address it carries, so that the CIDRs of the IP filter match it.
//!
//! TLVs are checked and then ignored: nothing in them is used for a decision.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// `\r\n\r\n\0\r\nQUIT\n`, the first 12 bytes of a v2 header.
pub const V2_SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

/// The longest v1 header, with its `\r\n` (the spec: 107 bytes).
pub const V1_MAX_LEN: usize = 107;

/// The default for `maxHeaderBytes`: room for v2 with the TLVs a cloud load balancer adds (an AWS endpoint id, an SSL block).
pub const DEFAULT_MAX_HEADER_BYTES: usize = 4096;

/// What a header says about the connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    /// No client address: a health check of the load balancer itself (v2 `LOCAL`, v1 `UNKNOWN`), or an address family that
    /// is not an IP. Keep the address of the socket.
    Local,
    /// The client (`source`) and the address it connected to (`destination`).
    Proxy {
        source: SocketAddr,
        destination: SocketAddr,
    },
}

/// A header at the start of a buffer: what it says and how many bytes it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    pub header: Header,
    pub len: usize,
}

/// Why a buffer is not (yet) a header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Everything so far is a valid start of a header; read more.
    Incomplete,
    /// The bytes do not start a PROXY protocol header.
    NotProxyProtocol,
    /// A header that breaks the specification (the text says how); the connection is refused.
    Invalid(&'static str),
    /// A header longer than the cap.
    TooLong,
}

/// Parse the header at the start of `buf`. `max` caps the header (a v1 header never needs more than [`V1_MAX_LEN`]).
pub fn parse(buf: &[u8], max: usize) -> Result<Parsed, Error> {
    let common = buf.len().min(V2_SIGNATURE.len());
    if buf[..common] == V2_SIGNATURE[..common] {
        if buf.len() < V2_SIGNATURE.len() {
            return Err(Error::Incomplete);
        }
        return parse_v2(buf, max);
    }
    let common = buf.len().min(6);
    if buf[..common] == b"PROXY "[..common] {
        if buf.len() < 6 {
            return Err(Error::Incomplete);
        }
        return parse_v1(buf);
    }
    Err(Error::NotProxyProtocol)
}

fn parse_v1(buf: &[u8]) -> Result<Parsed, Error> {
    let window = &buf[..buf.len().min(V1_MAX_LEN)];
    let Some(end) = window.windows(2).position(|w| w == b"\r\n") else {
        return if buf.len() >= V1_MAX_LEN {
            Err(Error::TooLong)
        } else {
            Err(Error::Incomplete)
        };
    };
    let len = end + 2;
    let line =
        std::str::from_utf8(&buf[6..end]).map_err(|_| Error::Invalid("v1 header is not ASCII"))?;
    if !line.is_ascii() {
        return Err(Error::Invalid("v1 header is not ASCII"));
    }
    // "UNKNOWN" may be followed by anything up to the line end (the spec leaves room for future fields).
    if line == "UNKNOWN" || line.starts_with("UNKNOWN ") {
        return Ok(Parsed {
            header: Header::Local,
            len,
        });
    }
    let mut parts = line.split(' ');
    let family = parts.next().unwrap_or_default();
    let (src, dst, sport, dport) = match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (Some(s), Some(d), Some(sp), Some(dp), None) => (s, d, sp, dp),
        _ => {
            return Err(Error::Invalid(
                "v1 header needs exactly four fields after the protocol",
            ))
        }
    };
    let source = v1_endpoint(family, src, sport)?;
    let destination = v1_endpoint(family, dst, dport)?;
    Ok(Parsed {
        header: Header::Proxy {
            source: canonical(source),
            destination: canonical(destination),
        },
        len,
    })
}

/// One address and port of a v1 header, of the family the header names.
fn v1_endpoint(family: &str, addr: &str, port: &str) -> Result<SocketAddr, Error> {
    let ip: IpAddr = match family {
        "TCP4" => IpAddr::V4(
            addr.parse::<Ipv4Addr>()
                .map_err(|_| Error::Invalid("v1 TCP4 address is not IPv4"))?,
        ),
        "TCP6" => IpAddr::V6(
            addr.parse::<Ipv6Addr>()
                .map_err(|_| Error::Invalid("v1 TCP6 address is not IPv6"))?,
        ),
        _ => return Err(Error::Invalid("v1 protocol is not TCP4, TCP6 or UNKNOWN")),
    };
    // The spec: decimal, no sign, no leading zeros (except 0 itself), 0-65535.
    let valid = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && (port == "0" || !port.starts_with('0'));
    let port: u16 = if valid {
        port.parse()
            .map_err(|_| Error::Invalid("v1 port is above 65535"))?
    } else {
        return Err(Error::Invalid("v1 port is not a decimal number"));
    };
    Ok(SocketAddr::new(ip, port))
}

fn parse_v2(buf: &[u8], max: usize) -> Result<Parsed, Error> {
    if buf.len() < 16 {
        return Err(Error::Incomplete);
    }
    let (version, command) = (buf[12] >> 4, buf[12] & 0x0f);
    if version != 2 {
        return Err(Error::Invalid("v2 header has a version other than 2"));
    }
    if command > 1 {
        return Err(Error::Invalid(
            "v2 header has a command other than LOCAL or PROXY",
        ));
    }
    let (family, protocol) = (buf[13] >> 4, buf[13] & 0x0f);
    let declared = usize::from(u16::from_be_bytes([buf[14], buf[15]]));
    let total = 16 + declared;
    if total > max {
        return Err(Error::TooLong);
    }
    if buf.len() < total {
        return Err(Error::Incomplete);
    }
    let body = &buf[16..total];
    if command == 0 {
        // LOCAL: the address block and the TLVs are ignored, whatever they hold.
        return Ok(Parsed {
            header: Header::Local,
            len: total,
        });
    }
    // PROXY. Over a TCP listener only a stream (or unspecified) transport makes sense; a datagram header is a mistake.
    if protocol > 1 {
        return Err(Error::Invalid(
            "v2 header is for a transport other than a stream",
        ));
    }
    let (addresses, rest) = match family {
        // UNSPEC: the receiver ignores whatever the block holds and uses the address of the socket.
        0 => {
            return Ok(Parsed {
                header: Header::Local,
                len: total,
            });
        }
        1 => split_block(body, 12)?,
        2 => split_block(body, 36)?,
        // UNIX (two 108-byte socket paths): not an IP address Conduit can use, but the TLVs after them are still checked.
        3 => {
            let (_, tlvs) = split_block(body, 216)?;
            check_tlvs(tlvs)?;
            return Ok(Parsed {
                header: Header::Local,
                len: total,
            });
        }
        _ => return Err(Error::Invalid("v2 header has an unknown address family")),
    };
    check_tlvs(rest)?;
    let (source, destination) = if family == 1 {
        let ip = |o: &[u8]| IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], o[3]));
        (
            SocketAddr::new(
                ip(&addresses[0..4]),
                u16::from_be_bytes([addresses[8], addresses[9]]),
            ),
            SocketAddr::new(
                ip(&addresses[4..8]),
                u16::from_be_bytes([addresses[10], addresses[11]]),
            ),
        )
    } else {
        let ip = |o: &[u8]| {
            let mut b = [0u8; 16];
            b.copy_from_slice(o);
            IpAddr::V6(Ipv6Addr::from(b))
        };
        (
            SocketAddr::new(
                ip(&addresses[0..16]),
                u16::from_be_bytes([addresses[32], addresses[33]]),
            ),
            SocketAddr::new(
                ip(&addresses[16..32]),
                u16::from_be_bytes([addresses[34], addresses[35]]),
            ),
        )
    };
    Ok(Parsed {
        header: Header::Proxy {
            source: canonical(source),
            destination: canonical(destination),
        },
        len: total,
    })
}

/// The address block of `size` bytes at the start of a v2 body, and the TLVs after it.
fn split_block(body: &[u8], size: usize) -> Result<(&[u8], &[u8]), Error> {
    if body.len() < size {
        return Err(Error::Invalid(
            "v2 address block is shorter than its family needs",
        ));
    }
    Ok(body.split_at(size))
}

/// The TLVs of a v2 header (`type: u8, length: u16, value`) must tile `tlvs` exactly. The values are not read.
fn check_tlvs(mut tlvs: &[u8]) -> Result<(), Error> {
    while !tlvs.is_empty() {
        if tlvs.len() < 3 {
            return Err(Error::Invalid("v2 TLV is cut short"));
        }
        let len = usize::from(u16::from_be_bytes([tlvs[1], tlvs[2]]));
        if tlvs.len() < 3 + len {
            return Err(Error::Invalid("v2 TLV is longer than the header"));
        }
        tlvs = &tlvs[3 + len..];
    }
    Ok(())
}

/// An IPv4-mapped IPv6 address as the IPv4 address it carries.
fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Why [`read_header`] gave up.
#[derive(Debug)]
pub enum ReadError {
    /// The connection did not start with a valid header (the text says what was wrong).
    Refused(&'static str),
    /// No complete header within the deadline.
    Timeout,
    /// The peer closed the connection, or the read failed, before the header was complete.
    Io(std::io::Error),
}

/// Read the header from the start of `stream` within `deadline` (one deadline for the whole header: a client that sends a byte
/// at a time does not extend it). Returns what it says and the bytes read after it, which belong to the payload and must be
/// handed on (to the upstream, or back into the stream) before anything else is read.
pub async fn read_header<R: AsyncRead + Unpin>(
    stream: &mut R,
    max: usize,
    deadline: Duration,
) -> Result<(Header, Vec<u8>), ReadError> {
    let read = async {
        let mut buf = Vec::with_capacity(256);
        let mut chunk = [0u8; 512];
        loop {
            match parse(&buf, max) {
                Ok(parsed) => return Ok((parsed.header, buf.split_off(parsed.len))),
                Err(Error::Incomplete) => {}
                Err(Error::NotProxyProtocol) => {
                    return Err(ReadError::Refused(
                        "the connection does not start with a PROXY protocol header",
                    ))
                }
                Err(Error::Invalid(why)) => return Err(ReadError::Refused(why)),
                Err(Error::TooLong) => {
                    return Err(ReadError::Refused(
                        "the PROXY protocol header is longer than maxHeaderBytes",
                    ))
                }
            }
            // Never read past the cap: a header still incomplete there is refused by the next parse.
            let room = max
                .max(V1_MAX_LEN)
                .saturating_sub(buf.len())
                .min(chunk.len());
            if room == 0 {
                return Err(ReadError::Refused(
                    "the PROXY protocol header is longer than maxHeaderBytes",
                ));
            }
            match stream.read(&mut chunk[..room]).await {
                Ok(0) => {
                    return Err(ReadError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the connection closed before the PROXY protocol header was complete",
                    )))
                }
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(ReadError::Io(e)),
            }
        }
    };
    tokio::time::timeout(deadline, read)
        .await
        .unwrap_or(Err(ReadError::Timeout))
}

/// A header for the upstream: v1 text or v2 binary (`"v1"` / `"v2"` in the config).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Version {
    V1,
    V2,
}

/// The header to write to an upstream for a connection from `source` to `destination`. Mixed families are written as IPv6
/// (an IPv4 address as its IPv4-mapped form): the header has one family for both ends.
pub fn encode(version: Version, source: SocketAddr, destination: SocketAddr) -> Vec<u8> {
    let (source, destination) = same_family(source, destination);
    match (version, source.ip()) {
        (Version::V1, IpAddr::V4(_)) => format!(
            "PROXY TCP4 {} {} {} {}\r\n",
            source.ip(),
            destination.ip(),
            source.port(),
            destination.port()
        )
        .into_bytes(),
        (Version::V1, IpAddr::V6(_)) => format!(
            "PROXY TCP6 {} {} {} {}\r\n",
            source.ip(),
            destination.ip(),
            source.port(),
            destination.port()
        )
        .into_bytes(),
        (Version::V2, ip) => {
            let mut out = Vec::with_capacity(16 + 36);
            out.extend_from_slice(&V2_SIGNATURE);
            out.push(0x21); // version 2, PROXY
            match (ip, destination.ip()) {
                (IpAddr::V4(s), IpAddr::V4(d)) => {
                    out.push(0x11); // INET, STREAM
                    out.extend_from_slice(&12u16.to_be_bytes());
                    out.extend_from_slice(&s.octets());
                    out.extend_from_slice(&d.octets());
                }
                (IpAddr::V6(s), IpAddr::V6(d)) => {
                    out.push(0x21); // INET6, STREAM
                    out.extend_from_slice(&36u16.to_be_bytes());
                    out.extend_from_slice(&s.octets());
                    out.extend_from_slice(&d.octets());
                }
                _ => unreachable!("same_family made both ends one family"),
            }
            out.extend_from_slice(&source.port().to_be_bytes());
            out.extend_from_slice(&destination.port().to_be_bytes());
            out
        }
    }
}

/// The header for a connection with no known client: v2 `LOCAL`, or `PROXY UNKNOWN`.
pub fn encode_local(version: Version) -> Vec<u8> {
    match version {
        Version::V1 => b"PROXY UNKNOWN\r\n".to_vec(),
        Version::V2 => {
            let mut out = Vec::with_capacity(16);
            out.extend_from_slice(&V2_SIGNATURE);
            out.extend_from_slice(&[0x20, 0x00, 0x00, 0x00]); // version 2, LOCAL, UNSPEC, no body
            out
        }
    }
}

fn same_family(a: SocketAddr, b: SocketAddr) -> (SocketAddr, SocketAddr) {
    let v6 = |s: SocketAddr| match s.ip() {
        IpAddr::V4(ip) => SocketAddr::new(IpAddr::V6(ip.to_ipv6_mapped()), s.port()),
        IpAddr::V6(_) => s,
    };
    if a.is_ipv4() == b.is_ipv4() {
        (a, b)
    } else {
        (v6(a), v6(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn proxy(src: &str, dst: &str) -> Header {
        Header::Proxy {
            source: sa(src),
            destination: sa(dst),
        }
    }

    #[test]
    fn v1_tcp4_and_tcp6() {
        let p = parse(b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\nGET /", 4096).unwrap();
        assert_eq!(p.header, proxy("203.0.113.9:51000", "192.0.2.1:443"));
        assert_eq!(p.len, 44); // the payload after the CRLF is not part of the header
        let p = parse(b"PROXY TCP6 2001:db8::1 2001:db8::2 1 2\r\n", 4096).unwrap();
        assert_eq!(p.header, proxy("[2001:db8::1]:1", "[2001:db8::2]:2"));
    }

    #[test]
    fn v1_unknown_is_local_and_may_carry_anything() {
        assert_eq!(
            parse(b"PROXY UNKNOWN\r\n", 4096).unwrap().header,
            Header::Local
        );
        let p = parse(b"PROXY UNKNOWN ffff::1 ffff::2 65535 65535\r\nrest", 4096).unwrap();
        assert_eq!(p.header, Header::Local);
        assert_eq!(p.len, 43);
    }

    #[test]
    fn v1_incomplete_until_the_line_ends() {
        for n in 0..b"PROXY TCP4 1.2.3.4 5.6.7.8 1 2\r\n".len() {
            let buf = &b"PROXY TCP4 1.2.3.4 5.6.7.8 1 2\r\n"[..n];
            assert_eq!(parse(buf, 4096), Err(Error::Incomplete), "{n} bytes");
        }
    }

    #[test]
    fn v1_longer_than_107_bytes_is_refused() {
        let mut line = b"PROXY UNKNOWN ".to_vec();
        line.extend(std::iter::repeat_n(b'x', 120));
        assert_eq!(parse(&line, 4096), Err(Error::TooLong));
        // 107 bytes with the CRLF is the longest allowed.
        let mut ok = b"PROXY UNKNOWN ".to_vec();
        ok.extend(std::iter::repeat_n(b'x', 107 - 14 - 2));
        ok.extend_from_slice(b"\r\n");
        assert_eq!(ok.len(), 107);
        assert_eq!(parse(&ok, 4096).unwrap().header, Header::Local);
    }

    #[test]
    fn v1_rejects_malformed_fields() {
        for bad in [
            &b"PROXY TCP4 1.2.3.4 5.6.7.8 1\r\n"[..],  // a field missing
            b"PROXY TCP4 1.2.3.4 5.6.7.8 1 2 3\r\n",   // a field too many
            b"PROXY TCP4 1.2.3.4  5.6.7.8 1 2\r\n",    // double space
            b"PROXY TCP4 ::1 5.6.7.8 1 2\r\n",         // family mismatch
            b"PROXY TCP6 1.2.3.4 5.6.7.8 1 2\r\n",     // family mismatch
            b"PROXY UDP4 1.2.3.4 5.6.7.8 1 2\r\n",     // not TCP
            b"PROXY TCP4 1.2.3.4 5.6.7.8 65536 2\r\n", // port too large
            b"PROXY TCP4 1.2.3.4 5.6.7.8 +1 2\r\n",    // a sign
            b"PROXY TCP4 1.2.3.4 5.6.7.8 01 2\r\n",    // a leading zero
            b"PROXY TCP4 1.2.3.4 5.6.7.8 -1 2\r\n",    // negative
            b"PROXY TCP4 1.2.3.4 5.6.7.8 1 2\n", // LF only: never ends the line, so it is read until the cap
        ] {
            assert!(
                parse(bad, 4096).is_err(),
                "{:?} must not parse",
                String::from_utf8_lossy(bad)
            );
        }
        assert!(matches!(
            parse(b"PROXY TCP4 1.2.3.4 5.6.7.8 65536 2\r\n", 4096),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            parse("PROXY TCP4 1.2.3.4 5.6.7.8 \u{e9} 2\r\n".as_bytes(), 4096),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn something_else_is_not_a_proxy_header() {
        assert_eq!(
            parse(b"GET / HTTP/1.1\r\n", 4096),
            Err(Error::NotProxyProtocol)
        );
        assert_eq!(parse(b"\x16\x03\x01", 4096), Err(Error::NotProxyProtocol));
        assert_eq!(parse(b"PROXX ", 4096), Err(Error::NotProxyProtocol));
        // A prefix of the v1 or v2 start waits for more.
        assert_eq!(parse(b"", 4096), Err(Error::Incomplete));
        assert_eq!(parse(b"PRO", 4096), Err(Error::Incomplete));
        assert_eq!(parse(&V2_SIGNATURE[..5], 4096), Err(Error::Incomplete));
    }

    fn v2(command: u8, fam_proto: u8, body: &[u8]) -> Vec<u8> {
        let mut out = V2_SIGNATURE.to_vec();
        out.push(0x20 | command);
        out.push(fam_proto);
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn v4_block() -> Vec<u8> {
        let mut b = vec![203, 0, 113, 9, 192, 0, 2, 1];
        b.extend_from_slice(&51000u16.to_be_bytes());
        b.extend_from_slice(&443u16.to_be_bytes());
        b
    }

    #[test]
    fn v2_inet_and_inet6() {
        let p = parse(&v2(1, 0x11, &v4_block()), 4096).unwrap();
        assert_eq!(p.header, proxy("203.0.113.9:51000", "192.0.2.1:443"));
        assert_eq!(p.len, 28);
        let mut b = Ipv6Addr::LOCALHOST.octets().to_vec();
        b.extend_from_slice(&"2001:db8::2".parse::<Ipv6Addr>().unwrap().octets());
        b.extend_from_slice(&7u16.to_be_bytes());
        b.extend_from_slice(&8u16.to_be_bytes());
        let p = parse(&v2(1, 0x21, &b), 4096).unwrap();
        assert_eq!(p.header, proxy("[::1]:7", "[2001:db8::2]:8"));
    }

    #[test]
    fn v2_local_ignores_the_body() {
        assert_eq!(
            parse(&v2(0, 0x00, &[]), 4096).unwrap().header,
            Header::Local
        );
        // A LOCAL header may carry garbage; it is skipped by its length and used for nothing.
        let p = parse(&v2(0, 0x11, &[1, 2, 3]), 4096).unwrap();
        assert_eq!(p.header, Header::Local);
        assert_eq!(p.len, 19);
    }

    #[test]
    fn v2_unspec_and_unix_have_no_usable_address() {
        assert_eq!(
            parse(&v2(1, 0x00, &[]), 4096).unwrap().header,
            Header::Local
        );
        assert_eq!(
            parse(&v2(1, 0x31, &[0u8; 216]), 4096).unwrap().header,
            Header::Local
        );
    }

    #[test]
    fn v2_tlvs_are_checked_and_skipped() {
        let mut body = v4_block();
        body.extend_from_slice(&[0x04, 0x00, 0x02, 0xAA, 0xBB]); // NOOP with two bytes
        body.extend_from_slice(&[0xEA, 0x00, 0x00]); // an empty vendor TLV
        let p = parse(&v2(1, 0x11, &body), 4096).unwrap();
        assert_eq!(p.header, proxy("203.0.113.9:51000", "192.0.2.1:443"));
        assert_eq!(p.len, 16 + body.len());
        // A TLV cut short, or longer than the header: refused.
        let mut cut = v4_block();
        cut.extend_from_slice(&[0x04, 0x00]);
        assert!(matches!(
            parse(&v2(1, 0x11, &cut), 4096),
            Err(Error::Invalid(_))
        ));
        let mut long = v4_block();
        long.extend_from_slice(&[0x04, 0x00, 0x09, 1, 2]);
        assert!(matches!(
            parse(&v2(1, 0x11, &long), 4096),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn v2_refuses_what_the_spec_forbids() {
        // Version other than 2, command other than LOCAL/PROXY.
        let mut bad_version = v2(1, 0x11, &v4_block());
        bad_version[12] = 0x11;
        assert!(matches!(parse(&bad_version, 4096), Err(Error::Invalid(_))));
        let mut bad_command = v2(1, 0x11, &v4_block());
        bad_command[12] = 0x22;
        assert!(matches!(parse(&bad_command, 4096), Err(Error::Invalid(_))));
        // An address block shorter than the family needs.
        assert!(matches!(
            parse(&v2(1, 0x11, &v4_block()[..11]), 4096),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            parse(&v2(1, 0x21, &[0u8; 35]), 4096),
            Err(Error::Invalid(_))
        ));
        // A datagram transport, an unknown family.
        assert!(matches!(
            parse(&v2(1, 0x12, &v4_block()), 4096),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            parse(&v2(1, 0x41, &[]), 4096),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn v2_waits_for_the_declared_length_and_refuses_what_exceeds_the_cap() {
        let whole = v2(1, 0x11, &v4_block());
        for n in 0..whole.len() {
            assert_eq!(
                parse(&whole[..n], 4096),
                Err(Error::Incomplete),
                "{n} bytes"
            );
        }
        // The length field says 65535: over any sensible cap, refused at once, not waited for.
        let mut huge = V2_SIGNATURE.to_vec();
        huge.extend_from_slice(&[0x21, 0x11, 0xff, 0xff]);
        assert_eq!(parse(&huge, 4096), Err(Error::TooLong));
        // The cap itself: exactly at it passes, one over does not.
        assert!(parse(&whole, whole.len()).is_ok());
        assert_eq!(parse(&whole, whole.len() - 1), Err(Error::TooLong));
    }

    #[test]
    fn an_ipv4_mapped_source_is_the_ipv4_address() {
        let mapped = "::ffff:198.51.100.7".parse::<Ipv6Addr>().unwrap();
        let mut b = mapped.octets().to_vec();
        b.extend_from_slice(&mapped.octets());
        b.extend_from_slice(&[0, 1, 0, 2]);
        let p = parse(&v2(1, 0x21, &b), 4096).unwrap();
        assert_eq!(p.header, proxy("198.51.100.7:1", "198.51.100.7:2"));
    }

    #[test]
    fn encode_round_trips_through_parse() {
        for (src, dst) in [
            ("203.0.113.9:51000", "192.0.2.1:443"),
            ("[2001:db8::1]:1", "[2001:db8::2]:2"),
        ] {
            for version in [Version::V1, Version::V2] {
                let bytes = encode(version, sa(src), sa(dst));
                let p = parse(&bytes, 4096).unwrap();
                assert_eq!(p.header, proxy(src, dst), "{version:?}");
                assert_eq!(p.len, bytes.len(), "{version:?}");
            }
        }
        for version in [Version::V1, Version::V2] {
            let bytes = encode_local(version);
            assert_eq!(
                parse(&bytes, 4096).unwrap(),
                Parsed {
                    header: Header::Local,
                    len: bytes.len()
                }
            );
        }
    }

    #[test]
    fn mixed_families_are_written_as_ipv6() {
        // The IPv4 end becomes ::ffff:a.b.c.d, which the reader turns back into the IPv4 address.
        let bytes = encode(Version::V2, sa("203.0.113.9:1"), sa("[2001:db8::2]:2"));
        assert_eq!(bytes[13], 0x21);
        let p = parse(&bytes, 4096).unwrap();
        assert_eq!(p.header, proxy("203.0.113.9:1", "[2001:db8::2]:2"));
        let text = encode(Version::V1, sa("203.0.113.9:1"), sa("[2001:db8::2]:2"));
        assert!(text.starts_with(b"PROXY TCP6 ::ffff:203.0.113.9 2001:db8::2 1 2\r\n"));
    }

    #[test]
    fn the_v1_text_is_what_haproxy_writes() {
        assert_eq!(
            encode(Version::V1, sa("203.0.113.9:51000"), sa("192.0.2.1:443")),
            b"PROXY TCP4 203.0.113.9 192.0.2.1 51000 443\r\n"
        );
    }

    #[tokio::test]
    async fn read_header_returns_the_bytes_after_the_header() {
        use tokio::io::AsyncWriteExt;
        let (mut client, mut server) = tokio::io::duplex(1024);
        let mut sent = encode(Version::V2, sa("203.0.113.9:1"), sa("192.0.2.1:2"));
        sent.extend_from_slice(b"GET / HTTP/1.1\r\n");
        // Written one byte at a time: the header arrives in pieces.
        tokio::spawn(async move {
            for b in sent {
                client.write_all(&[b]).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let (header, rest) = read_header(&mut server, 4096, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(header, proxy("203.0.113.9:1", "192.0.2.1:2"));
        // Whatever came after the header in the reads that completed it is handed back, nothing is lost or repeated.
        assert!(b"GET / HTTP/1.1\r\n".starts_with(&rest[..]));
    }

    #[tokio::test]
    async fn read_header_refuses_a_connection_that_does_not_start_with_one() {
        let mut input: &[u8] = b"GET / HTTP/1.1\r\n\r\n";
        let err = read_header(&mut input, 4096, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, ReadError::Refused(_)), "{err:?}");
    }

    #[tokio::test]
    async fn read_header_gives_up_at_the_deadline_and_on_a_closed_connection() {
        let (_client, mut server) = tokio::io::duplex(64);
        let err = read_header(&mut server, 4096, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(matches!(err, ReadError::Timeout), "{err:?}");
        let mut closed: &[u8] = b"PROXY TCP4 1.2.3";
        let err = read_header(&mut closed, 4096, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, ReadError::Io(_)), "{err:?}");
    }

    #[tokio::test]
    async fn read_header_stops_reading_when_the_header_is_refused() {
        // A v1 line with no end: 4000 bytes are offered, the reader refuses after one chunk and does not drain the rest.
        let input = [b"PROXY UNKNOWN ".as_slice(), &[b'x'; 4000]].concat();
        let mut slice: &[u8] = &input;
        let err = read_header(&mut slice, 4096, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, ReadError::Refused(_)), "{err:?}");
        assert!(
            slice.len() >= 4000 + 14 - 512,
            "{} bytes left: read more than one chunk",
            slice.len()
        );
    }
}
