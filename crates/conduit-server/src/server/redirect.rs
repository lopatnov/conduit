use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use conduit_core::util::host::host_without_port;
use dashmap::DashMap;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::Result;
use pingora_http::ResponseHeader;
use pingora_proxy::{ProxyHttp, Session};

/// A minimal Pingora proxy that redirects every HTTP request to HTTPS.
///
/// Configured with the HTTPS port so that it can build the redirect URL.
/// Returns `308 Permanent Redirect` to preserve the request method.
///
/// Also serves ACME HTTP-01 challenge tokens from `acme_challenges` so that
/// certificate renewal works on the HTTP port without a separate listener.
pub struct RedirectProxy {
    https_port: u16,
    /// Shared ACME HTTP-01 challenge store.  Checked before redirecting.
    acme_challenges: Arc<DashMap<String, String>>,
    /// Lower-cased hosts of the sites served by this redirect listener, or `None`
    /// when a catch-all site is among them (any valid host is redirected).
    allowed_hosts: Option<Vec<String>>,
}

impl RedirectProxy {
    /// `site_hosts` are the `host` values of the sites that use this listener
    /// (`None`/`"*"` means a catch-all site).  A request for any other host gets
    /// `421 Misdirected Request` instead of a redirect (#556, #588).
    pub fn new<'a>(
        https_port: u16,
        acme_challenges: Arc<DashMap<String, String>>,
        site_hosts: impl IntoIterator<Item = Option<&'a str>>,
    ) -> Self {
        let mut hosts = Vec::new();
        let mut any = false;
        for h in site_hosts {
            match h {
                None | Some("*") => any = true,
                Some(h) => hosts.push(normalize_host(h)),
            }
        }
        Self {
            https_port,
            acme_challenges,
            allowed_hosts: (!any).then_some(hosts),
        }
    }

    /// Redirect proxy for `http_port`: the hosts of every site whose `tls.httpRedirectPort`
    /// is `http_port` decide which hosts may be redirected (#588).
    pub fn for_sites(
        https_port: u16,
        http_port: u16,
        sites: &[crate::config::schema::SiteConfig],
        acme_challenges: Arc<DashMap<String, String>>,
    ) -> Self {
        let hosts = sites
            .iter()
            .filter(|s| s.tls.as_ref().and_then(|t| t.http_redirect_port) == Some(http_port))
            .map(|s| s.host.as_deref());
        Self::new(https_port, acme_challenges, hosts)
    }

    /// Status and `Location` for a non-challenge request: 400 for a `Host` that is not a valid
    /// name (it is reflected into `Location`, #556), 421 for a host no site serves (#588),
    /// otherwise a 308 to the HTTPS port keeping path and query.
    fn decide(&self, host_header: Option<&str>, path_and_query: &str) -> (u16, Option<String>) {
        let Some(host) = host_header.and_then(redirect_host) else {
            return (400, None);
        };
        if !self.host_allowed(&host) {
            return (421, None);
        }
        let location = if self.https_port == 443 {
            format!("https://{host}{path_and_query}")
        } else {
            format!("https://{host}:{}{path_and_query}", self.https_port)
        };
        (308, Some(location))
    }

    fn host_allowed(&self, host: &str) -> bool {
        self.allowed_hosts
            .as_ref()
            .is_none_or(|hosts| hosts.contains(&normalize_host(host)))
    }
}

fn normalize_host(h: &str) -> String {
    h.strip_suffix('.').unwrap_or(h).to_ascii_lowercase()
}

#[async_trait]
impl ProxyHttp for RedirectProxy {
    type CTX = ();

    fn new_ctx(&self) -> Self::CTX {}

    async fn request_filter(&self, session: &mut Session, _ctx: &mut ()) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        let path = session.req_header().uri.path().to_owned();

        // Serve ACME HTTP-01 challenges before redirecting so that certificate
        // renewal works even when the HTTP port is a pure redirect service.
        if let Some(token) = path.strip_prefix("/.well-known/acme-challenge/") {
            let body = match self.acme_challenges.get(token) {
                Some(key_auth) => Bytes::copy_from_slice(key_auth.as_bytes()),
                None => {
                    let mut resp = ResponseHeader::build(404, Some(1))?;
                    resp.insert_header("Content-Length", "20")?;
                    session.write_response_header(Box::new(resp), false).await?;
                    session
                        .write_response_body(
                            Some(Bytes::from_static(b"challenge not found\n")),
                            true,
                        )
                        .await?;
                    return Ok(true);
                }
            };
            let len = body.len().to_string();
            let mut resp = ResponseHeader::build(200, Some(2))?;
            resp.insert_header("Content-Type", "text/plain; charset=utf-8")?;
            resp.insert_header("Content-Length", len.as_str())?;
            session.write_response_header(Box::new(resp), false).await?;
            session.write_response_body(Some(body), true).await?;
            return Ok(true);
        }

        let host_header = session
            .req_header()
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok());
        let path_and_query = session
            .req_header()
            .uri
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let (status, location) = self.decide(host_header, path_and_query);
        let mut resp = ResponseHeader::build(status, Some(2))?;
        if let Some(location) = location {
            resp.insert_header("Location", location)?;
        }
        resp.insert_header("Content-Length", "0")?;
        session.write_response_header(Box::new(resp), true).await?;
        Ok(true)
    }

    /// Never called — all requests are handled in `request_filter`.
    async fn upstream_peer(&self, _session: &mut Session, _ctx: &mut ()) -> Result<Box<HttpPeer>>
    where
        Self::CTX: Send + Sync,
    {
        unreachable!("RedirectProxy always handles requests in request_filter")
    }
}

/// The host (without port) to put in a redirect `Location`, or `None` when the
/// `Host` header is not a valid DNS name, IPv4 address or bracketed IPv6 literal.
fn redirect_host(raw: &str) -> Option<String> {
    let host = host_without_port(raw);
    let valid = if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        !inner.is_empty() && inner.parse::<std::net::Ipv6Addr>().is_ok()
    } else {
        // One trailing dot (a fully qualified name) is fine; empty labels and labels that
        // start or end with a hyphen are not.
        let name = host.strip_suffix('.').unwrap_or(host);
        !name.is_empty()
            && host.len() <= 253
            && name.split('.').all(|label| {
                !label.is_empty()
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
    };
    valid.then(|| host.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{redirect_host, RedirectProxy};
    use std::sync::Arc;

    fn proxy(hosts: &[Option<&str>]) -> RedirectProxy {
        RedirectProxy::new(443, Arc::default(), hosts.iter().copied())
    }

    /// `for_sites` takes hosts only from sites that share this HTTP port.
    #[test]
    fn for_sites_uses_the_hosts_of_sites_on_the_same_http_port() {
        use crate::config::schema::{SiteConfig, TlsConfig};
        let site = |host: &str, port: u16| SiteConfig {
            host: Some(host.into()),
            tls: Some(TlsConfig {
                http_redirect_port: Some(port),
                ..Default::default()
            }),
            ..Default::default()
        };
        let sites = [site("a.test", 80), site("b.test", 8080)];
        let p = RedirectProxy::for_sites(443, 80, &sites, Arc::default());
        assert_eq!(p.decide(Some("a.test"), "/").0, 308);
        assert_eq!(p.decide(Some("b.test"), "/").0, 421);
    }

    /// #588: only hosts of configured sites are redirected.
    #[test]
    fn only_configured_hosts_are_redirected() {
        let p = proxy(&[Some("Example.com"), Some("b.example.com")]);
        assert!(p.host_allowed("example.com"));
        assert!(p.host_allowed("EXAMPLE.com."));
        assert!(p.host_allowed("b.example.com"));
        assert!(!p.host_allowed("evil.example"));
    }

    #[test]
    fn decide_answers_400_421_or_308() {
        let p = proxy(&[Some("example.com")]);
        assert_eq!(p.decide(None, "/"), (400, None));
        assert_eq!(p.decide(Some("a b"), "/"), (400, None));
        assert_eq!(p.decide(Some("evil.example"), "/x"), (421, None));
        assert_eq!(
            p.decide(Some("Example.com:8080"), "/x?y=1"),
            (308, Some("https://Example.com/x?y=1".to_owned()))
        );
        let p8443 = RedirectProxy::new(8443, Arc::default(), [Some("example.com")]);
        assert_eq!(
            p8443.decide(Some("example.com"), "/"),
            (308, Some("https://example.com:8443/".to_owned()))
        );
    }

    #[test]
    fn catch_all_site_allows_any_host() {
        assert!(proxy(&[Some("a.example.com"), None]).host_allowed("anything.test"));
        assert!(proxy(&[Some("*")]).host_allowed("anything.test"));
    }

    #[test]
    fn accepts_plain_hosts_with_or_without_port() {
        assert_eq!(redirect_host("example.com").as_deref(), Some("example.com"));
        assert_eq!(
            redirect_host("example.com:8080").as_deref(),
            Some("example.com")
        );
        assert_eq!(redirect_host("127.0.0.1:80").as_deref(), Some("127.0.0.1"));
        assert_eq!(
            redirect_host("example.com.").as_deref(),
            Some("example.com.")
        );
        assert_eq!(redirect_host("[::1]:8080").as_deref(), Some("[::1]"));
        assert_eq!(
            redirect_host("[2001:db8::1]").as_deref(),
            Some("[2001:db8::1]")
        );
    }

    /// #556: nothing that could change the meaning of the `Location` URL is reflected.
    #[test]
    fn rejects_hosts_that_could_rewrite_the_location() {
        for bad in [
            "",
            "evil.example\\@x",
            "a/b",
            "a b",
            "user@evil.example",
            "evil.example:80@good",
            "evil.example\r\nX: y",
            "[::1",
            "[]",
            "[not-an-ip]",
            "a?b",
            "a..b",
            "-bad.example",
            "bad-.example",
            ".",
            ".example.com",
            "a#b",
        ] {
            assert!(redirect_host(bad).is_none(), "{bad:?} must be refused");
        }
    }
}
