//! Browser-equivalent credential scoping for captured downloads.
//!
//! A capture carries the `Cookie`/`Authorization` the browser would send to
//! the URL it was captured for. aria2 replays its `header` option to every
//! redirect target, so handing it the original URL would leak those
//! credentials to whatever host a redirect points to. Instead the redirect
//! chain is followed here, hop by hop, sending the credentials only while the
//! origin is unchanged (the rule browsers and curl apply), and aria2 receives
//! the final URL with the credentials that are still valid for it.
//! [`crate::aria2::build_add_options`] forbids aria2 any further redirect while
//! it carries credentials.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use minidl_ipc::{CaptureJob, DownloadKind};

const MAX_HOPS: usize = 10;
/// One deadline for a whole redirect chain, DNS included.
const RESOLVE_DEADLINE: Duration = Duration::from_secs(30);
const LOOKUP_DEADLINE: Duration = Duration::from_secs(10);

fn embedded_v4(segments: &[u16]) -> Ipv4Addr {
    Ipv4Addr::new(
        (segments[0] >> 8) as u8,
        segments[0] as u8,
        (segments[1] >> 8) as u8,
        segments[1] as u8,
    )
}

pub fn is_non_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || a >= 224
                || ip.is_documentation()
        }
        std::net::IpAddr::V6(ip) => {
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || segments[0] & 0xfe00 == 0xfc00 // unique-local
                || segments[0] & 0xffc0 == 0xfe80 // link-local
                || segments[0] & 0xffc0 == 0xfec0 // deprecated site-local
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || segments[..4] == [0x0100, 0, 0, 0] // discard-only
                || (segments[..3] == [0x0064, 0xff9b, 0x0001]) // local-use NAT64
                // IPv4 carried inside IPv6: mapped/compatible, NAT64, 6to4,
                // Teredo client (stored inverted). Judge the IPv4 address.
                || ip
                    .to_ipv4()
                    .map(IpAddr::V4)
                    .is_some_and(is_non_public_ip)
                || (segments[..6] == [0x0064, 0xff9b, 0, 0, 0, 0]
                    && is_non_public_ip(embedded_v4(&segments[6..]).into()))
                || (segments[0] == 0x2002 && is_non_public_ip(embedded_v4(&segments[1..3]).into()))
                || (segments[..2] == [0x2001, 0]
                    && is_non_public_ip(embedded_v4(&[!segments[6], !segments[7]]).into()))
        }
    }
}

pub async fn ensure_public_target(raw: &str) -> Result<(), String> {
    use url::Host;

    let parsed = url::Url::parse(raw).map_err(|_| "invalid target URL".to_string())?;
    let host = parsed
        .host()
        .ok_or_else(|| "target URL has no host".to_string())?;
    let blocked = match host {
        Host::Ipv4(ip) => is_non_public_ip(ip.into()),
        Host::Ipv6(ip) => is_non_public_ip(ip.into()),
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.');
            if domain.eq_ignore_ascii_case("localhost") || domain.ends_with(".localhost") {
                true
            } else {
                let port = parsed.port_or_known_default().unwrap_or(443);
                let resolved =
                    tokio::time::timeout(LOOKUP_DEADLINE, tokio::net::lookup_host((domain, port)))
                        .await
                        .map_err(|_| "resolving the target host timed out".to_string())?
                        .map_err(|error| format!("could not resolve target host: {error}"))?
                        .collect::<Vec<_>>();
                if resolved.is_empty() {
                    return Err("target host resolved to no addresses".into());
                }
                resolved
                    .into_iter()
                    .any(|address| is_non_public_ip(address.ip()))
            }
        }
    };
    if blocked {
        Err(
            "target host resolves to a private, local, or reserved address (blocked in settings)"
                .into(),
        )
    } else {
        Ok(())
    }
}

/// DNS for the redirect probe. With `block_private` it drops every
/// non-public address from the answer, so the address that was vetted is the
/// address connected to (one lookup per connection, no check-then-resolve
/// window for DNS rebinding). `hosts` is a fixed answer table for tests.
#[derive(Clone, Default)]
struct VettedResolver {
    block_private: bool,
    hosts: Arc<HashMap<String, Vec<IpAddr>>>,
}

impl reqwest::dns::Resolve for VettedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = self.clone();
        let host = name.as_str().trim_end_matches('.').to_ascii_lowercase();
        Box::pin(async move {
            let found: Vec<IpAddr> = match this.hosts.get(&host) {
                Some(fixed) => fixed.clone(),
                None => tokio::time::timeout(
                    LOOKUP_DEADLINE,
                    tokio::net::lookup_host((host.as_str(), 0)),
                )
                .await
                .map_err(|_| "DNS lookup timed out")??
                .map(|address| address.ip())
                .collect(),
            };
            let localhost = host == "localhost" || host.ends_with(".localhost");
            let usable: Vec<SocketAddr> = found
                .into_iter()
                .filter(|ip| !this.block_private || (!localhost && !is_non_public_ip(*ip)))
                .map(|ip| SocketAddr::new(ip, 0))
                .collect();
            if usable.is_empty() {
                return Err(
                    "target host resolves to a private, local, or reserved address (blocked in settings)"
                        .into(),
                );
            }
            Ok(Box::new(usable.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// True when the job would hand aria2 origin-bound credentials.
pub fn carries_credentials(job: &CaptureJob) -> bool {
    job.cookie.is_some() || !job.extra_headers.is_empty()
}

/// Keep the credentials only while `next` has the same origin as `current`.
fn scope_to(job: &mut CaptureJob, next: &url::Url) {
    let same_origin =
        url::Url::parse(&job.url).is_ok_and(|current| current.origin() == next.origin());
    if !same_origin {
        job.cookie = None;
        job.extra_headers.clear();
    }
    job.url = next.to_string();
}

/// Follow `job`'s redirect chain and return the job aria2 should run: the
/// final URL, with the captured credentials only if every hop stayed on their
/// origin. Jobs without credentials, or that are not plain HTTP(S), are
/// returned unchanged (aria2 may follow their redirects: nothing to leak).
/// When `block_private` is set every hop must be a public address. A network
/// failure returns the job unchanged; aria2 then gets no redirect allowance.
pub async fn resolve(
    job: &CaptureJob,
    proxy: Option<&str>,
    block_private: bool,
) -> Result<CaptureJob, String> {
    let resolver = VettedResolver {
        block_private,
        hosts: Arc::default(),
    };
    tokio::time::timeout(RESOLVE_DEADLINE, resolve_with(job, proxy, resolver))
        .await
        .map_err(|_| "following the download's redirects timed out".to_string())?
}

async fn resolve_with(
    job: &CaptureJob,
    proxy: Option<&str>,
    resolver: VettedResolver,
) -> Result<CaptureJob, String> {
    let block_private = resolver.block_private;
    let mut job = job.clone();
    let is_http =
        url::Url::parse(&job.url).is_ok_and(|url| matches!(url.scheme(), "http" | "https"));
    if job.kind != DownloadKind::Http || !is_http || !carries_credentials(&job) {
        return Ok(job);
    }
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(20));
    let proxy = proxy.filter(|value| !value.is_empty());
    match proxy {
        // The proxy resolves the target itself, so its address cannot be
        // vetted here; hops are checked by name before each request instead.
        Some(proxy) => {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|error| error.to_string())?)
        }
        None => builder = builder.no_proxy().dns_resolver(Arc::new(resolver)),
    }
    let client = builder.build().map_err(|error| error.to_string())?;
    for _ in 0..MAX_HOPS {
        if block_private {
            // IP literals never reach the resolver; names do when no proxy is set.
            let literal = url::Url::parse(&job.url)
                .ok()
                .is_some_and(|url| !matches!(url.host(), Some(url::Host::Domain(_))));
            if literal || proxy.is_some() {
                ensure_public_target(&job.url).await?;
            }
        }
        let location = match next_hop(&client, &job).await {
            Ok(location) => location,
            Err(error) if block_private && is_blocked(&error) => {
                return Err(
                    "target host resolves to a private, local, or reserved address (blocked in settings)"
                        .into(),
                )
            }
            Err(_) => None,
        };
        let Some(location) = location else {
            return Ok(job);
        };
        let current = url::Url::parse(&job.url).map_err(|error| error.to_string())?;
        let next = current
            .join(&location)
            .map_err(|_| "invalid redirect target".to_string())?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err("redirect to an unsupported URL scheme".into());
        }
        scope_to(&mut job, &next);
    }
    Err("too many redirects".into())
}

/// The `Location` of a redirect response for `job.url`, or `None` when it is
/// not a redirect (or the request failed). HEAD first so a single-use signed
/// URL is not consumed; servers that refuse HEAD get a one-byte ranged GET.
fn is_blocked(error: &reqwest::Error) -> bool {
    let mut source: Option<&dyn std::error::Error> = Some(error);
    while let Some(error) = source {
        if error.to_string().contains("blocked in settings") {
            return true;
        }
        source = error.source();
    }
    false
}

async fn next_hop(
    client: &reqwest::Client,
    job: &CaptureJob,
) -> Result<Option<String>, reqwest::Error> {
    let request = |method: reqwest::Method| {
        let mut request = client.request(method, &job.url);
        if let Some(cookie) = &job.cookie {
            request = request.header("Cookie", cookie);
        }
        for (name, value) in &job.extra_headers {
            request = request.header(name, value);
        }
        if let Some(referrer) = &job.referrer {
            request = request.header("Referer", referrer);
        }
        if let Some(agent) = &job.user_agent {
            request = request.header("User-Agent", agent);
        }
        request
    };
    let mut response = request(reqwest::Method::HEAD).send().await?;
    if response.status().is_client_error() || response.status().is_server_error() {
        response = request(reqwest::Method::GET)
            .header("Range", "bytes=0-0")
            .send()
            .await?;
    }
    if !response.status().is_redirection() {
        return Ok(None);
    }
    Ok(response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(url: &str) -> CaptureJob {
        CaptureJob {
            url: url.into(),
            filename: None,
            referrer: None,
            user_agent: None,
            cookie: Some("sid=secret".into()),
            extra_headers: vec![("Authorization".into(), "Bearer t".into())],
            kind: DownloadKind::Http,
            mime: None,
            size: None,
            page_url: None,
            cookie_store_id: None,
            torrent_b64: None,
            batch_id: None,
            batch_name: None,
        }
    }

    #[test]
    fn credentials_follow_only_same_origin_hops() {
        let mut capture = job("https://app.example/dl?id=1");
        scope_to(
            &mut capture,
            &url::Url::parse("https://app.example/files/1").unwrap(),
        );
        assert!(carries_credentials(&capture));
        for other in [
            "https://cdn.example/files/1",
            "http://app.example/files/1",
            "https://app.example:8443/files/1",
        ] {
            let mut capture = job("https://app.example/dl?id=1");
            scope_to(&mut capture, &url::Url::parse(other).unwrap());
            assert!(!carries_credentials(&capture), "{other} kept credentials");
            assert_eq!(capture.url, other);
        }
    }

    #[tokio::test]
    async fn jobs_without_credentials_are_not_probed() {
        let mut capture = job("https://unresolvable.invalid/file");
        capture.cookie = None;
        capture.extra_headers.clear();
        let resolved = resolve(&capture, None, false).await.unwrap();
        assert_eq!(resolved.url, capture.url);
    }

    #[test]
    fn ipv4_embedded_in_ipv6_is_judged_by_the_ipv4_address() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        for private in [
            "64:ff9b::7f00:1",                      // NAT64 of 127.0.0.1
            "64:ff9b::a00:1",                       // NAT64 of 10.0.0.1
            "64:ff9b:1::1",                         // local-use NAT64
            "2002:c0a8:101::1",                     // 6to4 of 192.168.1.1
            "2002:7f00:1::",                        // 6to4 of 127.0.0.1
            "2001:0:4136:e378:8000:63bf:80ff:fffe", // Teredo client 127.0.0.1
            "::ffff:10.1.2.3",
            "100::1",
        ] {
            assert!(is_non_public_ip(ip(private)), "{private} passed as public");
        }
        for public in ["64:ff9b::5db8:d822", "2002:5db8:d822::1", "2606:4700::1111"] {
            assert!(!is_non_public_ip(ip(public)), "{public} judged private");
        }
    }

    /// DNS rebinding: the probe must connect only to an address it vetted.
    /// The name answers with a public and a private address; the private one
    /// (where a fixture listens) must never be contacted, and a name with only
    /// private answers is refused before any connection.
    #[tokio::test]
    async fn the_probe_connects_only_to_vetted_addresses() {
        use reqwest::dns::Resolve;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let hosts = HashMap::from([
            (
                "mixed.test".to_string(),
                vec![ip4(93, 184, 216, 34), ip4(127, 0, 0, 1)],
            ),
            (
                "private.test".to_string(),
                vec![ip4(127, 0, 0, 1), "64:ff9b::7f00:1".parse().unwrap()],
            ),
        ]);
        let resolver = VettedResolver {
            block_private: true,
            hosts: Arc::new(hosts),
        };
        let answer: Vec<_> = resolver
            .resolve("mixed.test".parse().unwrap())
            .await
            .unwrap()
            .map(|address| address.ip())
            .collect();
        assert_eq!(answer, [ip4(93, 184, 216, 34)]);

        let error = resolve_with(
            &job(&format!("http://private.test:{port}/f")),
            None,
            resolver,
        )
        .await
        .unwrap_err();
        assert!(error.contains("blocked in settings"), "{error}");
        assert!(
            listener.accept().is_err(),
            "the private address was contacted"
        );
    }

    fn ip4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    #[tokio::test]
    async fn private_hops_are_refused_when_blocked() {
        assert!(resolve(&job("http://127.0.0.1:9/f"), None, true)
            .await
            .is_err());
    }
}
