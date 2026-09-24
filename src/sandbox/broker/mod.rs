//! The egress broker: the single endpoint a session in `proxy` mode may reach.
//!
//! A session's network namespace is locked so that the broker is the only
//! thing it can connect to. The broker then decides, per connection, whether
//! the host a session asked for is on the allowlist, and refuses everything
//! else. This is where egress stops being "any host on port 443" and becomes
//! "these hosts and no others", which is what keeps a session from dialing a
//! relay it does not need and turning an outbound allowance into a two-way
//! channel.
//!
//! A session speaks to it as an ordinary HTTP proxy, so `HTTPS_PROXY` and
//! `HTTP_PROXY` are all a well-behaved client needs. A `CONNECT` tunnel is
//! opaque once established: the broker gates on the host in the CONNECT line
//! and then copies bytes, it does not read inside the TLS. A plain `http://`
//! request is gated the same way on the host in its URL. Credential injection for the
//! provider is a separate, terminating path, served on this module's second
//! listener; this file is the gate.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub use server::Broker;

use crate::log::Logger;
use crate::log::fields;

/// Whether `host` is permitted by an allowlist of names and `*.` wildcards.
///
/// A lone `*` admits any host, turning the broker into an audit pass-through:
/// it still gates the port and logs every connection, but restricts no host. A
/// bare name matches only itself. A `*.example.com` rule matches any single or
/// multi-label subdomain of `example.com` but not the bare `example.com`,
/// since a wildcard is written when the apex is not what a session talks to.
/// The comparison is case-folded, because a hostname is.
pub fn host_allowed(host: &str, allow: &[String]) -> bool {
    let candidate = host.trim().to_lowercase();
    if candidate.is_empty() {
        return false;
    }
    for rule in allow {
        if rule == "*" {
            return true;
        }
        if let Some(apex) = rule.strip_prefix("*.") {
            let suffix = format!(".{}", apex.to_lowercase());
            if candidate.len() > suffix.len() && candidate.ends_with(&suffix) {
                return true;
            }
        } else if candidate == rule.to_lowercase() {
            return true;
        }
    }
    false
}

/// The host and port a session asked the broker to reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The host asked for.
    pub host: String,
    /// The port asked for.
    pub port: u16,
}

/// Reads the target out of an HTTP `CONNECT` request line.
///
/// The line is `CONNECT host:port HTTP/1.1`. Anything else, a missing port, a
/// port that is not a number or is out of range, is refused by returning
/// nothing rather than guessing, since a target the broker had to guess at is
/// one it cannot claim to have checked.
pub fn parse_connect(request_line: &str) -> Option<Target> {
    let mut parts = request_line.split_whitespace();
    if !parts.next()?.eq_ignore_ascii_case("CONNECT") {
        return None;
    }
    parse_authority(parts.next()?, None)
}

/// A plain HTTP request a session sent through the broker as its proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    /// Where the request goes.
    pub target: Target,
    /// The head as the origin is to receive it.
    pub head: String,
}

/// Headers that concern the hop to the broker rather than the origin.
///
/// The framing headers stay, because the body is relayed as it arrived.
const PROXY_HOP_HEADERS: [&str; 6] = [
    "connection",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "upgrade",
];

/// Reads a proxied `http://` request and rewrites its head for the origin.
///
/// A proxy is sent `GET http://host/path HTTP/1.1` and an origin expects
/// `GET /path HTTP/1.1`. The connection is closed after one exchange because
/// what follows the head is relayed as bytes to the host that was judged, so
/// a client reusing it for another host would otherwise reach this one.
pub fn parse_forward(head: &str) -> Option<Forward> {
    let mut lines = head.lines();
    let mut parts = lines.next()?.split_whitespace();
    let (method, uri, version) = (parts.next()?, parts.next()?, parts.next()?);
    let rest = uri.strip_prefix("http://")?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |at| rest.split_at(at));
    let target = parse_authority(authority, Some(80))?;

    let mut rewritten = format!("{method} {path} {version}\r\n");
    for line in lines.take_while(|line| !line.is_empty()) {
        let name = line.split(':').next().unwrap_or("").trim();
        if !PROXY_HOP_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            rewritten.push_str(line);
            rewritten.push_str("\r\n");
        }
    }
    rewritten.push_str("Connection: close\r\n\r\n");
    Some(Forward {
        target,
        head: rewritten,
    })
}

/// Reads `host:port`, or a bare host when there is a `default_port`.
///
/// IPv6 literals would be bracketed; a session reaches named hosts, so a
/// bracketed authority is not something the allowlist can match and is
/// refused rather than parsed into a host. So is one carrying credentials.
fn parse_authority(authority: &str, default_port: Option<u16>) -> Option<Target> {
    if authority.contains(['/', '[', '@']) {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok().filter(|port| *port != 0)?),
        None => (authority, default_port?),
    };
    if host.is_empty() {
        return None;
    }
    Some(Target {
        host: host.to_owned(),
        port,
    })
}

/// Where the model provider really is, and what stands in for its key.
///
/// A session is given `nonce` in place of the credential, so the credential
/// itself never enters a sandbox and nothing read out of a session's
/// environment can be replayed anywhere else. The nonce is worth only what the
/// broker will do with it, and the broker is reachable only from the session's
/// own namespace.
#[derive(Debug, Clone)]
pub struct ProviderRoute {
    /// Path a session addresses the provider at, such as `/provider`.
    pub prefix: String,
    /// The provider's real base URL, which the prefix stands in for.
    pub upstream: String,
    /// What a session sends as its key.
    pub nonce: String,
    /// The real credential. Never leaves the daemon.
    pub credential: String,
}

/// Headers that describe one hop and must not be forwarded to the next.
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
];

/// Compares without letting the time taken say how much of it matched.
fn same_secret(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let differences = a
        .bytes()
        .zip(b.bytes())
        .fold(0_u8, |accumulated, (left, right)| {
            accumulated | (left ^ right)
        });
    differences == 0
}

/// Whether an IPv4 address is one the broker must not connect to.
///
/// The broker runs on the host, so a connection it makes reaches the host's
/// own network from the host's position. Loopback, the private ranges,
/// link-local, and the carrier range are the host and its neighbours, not the
/// internet a session is allowed out to. Reaching them through the broker
/// would be a way back into the host that the network namespace was built to
/// close.
///
/// The ranges reserved for documentation, benchmarking, protocol assignments
/// and 6to4 relaying are refused as well. They are not the host's own, but
/// nothing a session legitimately talks to answers on them, so a name that
/// resolves there resolved to nothing worth dialling.
pub fn is_private_v4(address: &str) -> bool {
    // An address the broker cannot read is not one it should dial, so a
    // spelling the parser refuses counts as internal rather than as public.
    let Ok(address) = address.parse::<Ipv4Addr>() else {
        return true;
    };
    let [first, second, third, _] = address.octets();
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_multicast()
        // The TEST-NET ranges, which exist to appear in prose.
        || address.is_documentation()
        // `this network`, which std has no stable name for.
        || first == 0
        // Carrier-grade NAT, 100.64.0.0/10, likewise unnamed on stable.
        || (first == 100 && (64..=127).contains(&second))
        // IETF protocol assignments, 192.0.0.0/24.
        || (first == 192 && second == 0 && third == 0)
        // The 6to4 relay anycast address, 192.88.99.0/24.
        || (first == 192 && second == 88 && third == 99)
        // Benchmarking, 198.18.0.0/15.
        || (first == 198 && (18..=19).contains(&second))
        // Everything above multicast is reserved, and the last of it is the
        // broadcast address.
        || first >= 240
}

/// The eight groups of an IPv6 address, or nothing when it is not one.
///
/// Parsed rather than matched. The same address has many spellings, and
/// `::ffff:7f00:1`, `::ffff:127.0.0.1` and `0:0:0:0:0:ffff:7f00:1` are all
/// loopback: judging the text instead of the value lets the spelling decide
/// whether a host is internal.
pub fn v6_groups(address: &str) -> Option<Vec<u32>> {
    let bare = address
        .to_lowercase()
        .split('%')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if bare.is_empty() {
        return None;
    }
    // A trailing dotted quad carries the low 32 bits, as in `::ffff:127.0.0.1`.
    let mut text = bare.clone();
    if let Some((head, quad)) = bare.rsplit_once(':') {
        let octets: Vec<&str> = quad.split('.').collect();
        if octets.len() == 4
            && octets
                .iter()
                .all(|part| part.parse::<i64>().is_ok_and(|n| (0..=255).contains(&n)))
        {
            let numbers: Vec<i64> = octets
                .iter()
                .map(|part| part.parse().expect("checked above"))
                .collect();
            let high = (numbers[0] << 8) | numbers[1];
            let low = (numbers[2] << 8) | numbers[3];
            text = format!("{head}:{high:x}:{low:x}");
        }
    }

    let halves: Vec<&str> = text.split("::").collect();
    if halves.len() > 2 {
        return None;
    }
    let read = |part: &str| -> Option<Vec<u32>> {
        if part.is_empty() {
            return Some(Vec::new());
        }
        let mut groups = Vec::new();
        for piece in part.split(':') {
            let valid = (1..=4).contains(&piece.len())
                && piece.bytes().all(|byte| byte.is_ascii_hexdigit());
            if !valid {
                return None;
            }
            groups.push(u32::from_str_radix(piece, 16).expect("hex digits"));
        }
        Some(groups)
    };

    let head = read(halves[0])?;
    if halves.len() == 1 {
        return (head.len() == 8).then_some(head);
    }

    let tail = read(halves[1])?;
    let gap = 8_usize.checked_sub(head.len() + tail.len())?;
    if gap < 1 {
        return None;
    }
    let mut groups = head;
    groups.extend(std::iter::repeat_n(0, gap));
    groups.extend(tail);
    Some(groups)
}

/// Whether an IPv6 address is one the broker must not connect to.
#[expect(clippy::many_single_char_names)]
pub fn is_private_v6(address: &str) -> bool {
    let Some(groups) = v6_groups(address) else {
        // Not an address this can read, so not one it should dial.
        return true;
    };
    if groups.len() != 8 {
        return true;
    }
    let [a, b, c, d, e, f, g, h] = groups[..] else {
        return true;
    };
    let leading = a | b | c | d | e;

    // A v4 address wearing a v6 coat reaches the same v4 host, so it is judged
    // as the v4 it carries: `::ffff:a.b.c.d` mapped, `::a.b.c.d` compatible,
    // and the NAT64 prefix, which is a translation of one too.
    let carries_v4 = (leading == 0 && f == 0xffff)
        || (leading == 0 && f == 0 && !(g == 0 && (h == 0 || h == 1)))
        || (a == 0x0064 && b == 0xff9b);
    if carries_v4 {
        let v4 = format!("{}.{}.{}.{}", g >> 8, g & 0xff, h >> 8, h & 0xff);
        return is_private_v4(&v4);
    }

    if leading == 0 && f == 0 && g == 0 && (h == 0 || h == 1) {
        return true; // :: and ::1
    }
    if (a & 0xffc0) == 0xfe80 {
        return true; // link-local
    }
    if (a & 0xfe00) == 0xfc00 {
        return true; // unique local
    }
    if (a & 0xff00) == 0xff00 {
        return true; // multicast
    }
    false
}

/// Whether a literal IP address is a host-internal one.
pub fn is_private_address(address: &str) -> bool {
    if address.contains(':') {
        is_private_v6(address)
    } else {
        is_private_v4(address)
    }
}

/// Resolves names to addresses, injected so a test answers from a table.
pub type Resolve = Arc<dyn Fn(String) -> ResolveFuture + Send + Sync>;

/// How long resolving a name may take before the broker gives up on it.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long one upstream address may take to accept before the next is tried.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolves a target to the addresses the broker may dial, in resolver order.
///
/// A name is resolved here and the result is judged, so a name on the
/// allowlist that points at the host's own network, whether by mistake or to
/// slip past the allowlist, is refused. The connection is then made to the
/// addresses that were judged rather than to the name resolved a second time,
/// so what was checked is what is dialled.
///
/// `allow_internal` relaxes this for a literal address only. An operator
/// naming `10.0.0.5` has said which machine they mean, and no one else can
/// change what that points at. A name resolving somewhere internal stays
/// refused even then, because what a name points at is not the operator's to
/// decide.
pub async fn public_addresses(
    host: &str,
    allow_internal: bool,
    resolve: Option<&Resolve>,
) -> Vec<String> {
    let literal = host.split('.').count() == 4 && host.parse::<std::net::Ipv4Addr>().is_ok()
        || host.contains(':');
    if literal {
        if allow_internal || !is_private_address(host) {
            return vec![host.to_owned()];
        }
        return Vec::new();
    }
    let addresses = match resolve {
        Some(resolve) => resolve(host.to_owned()).await,
        None => default_resolve(host).await,
    };
    addresses
        .into_iter()
        .filter(|address| !is_private_address(address))
        .collect()
}

async fn default_resolve(name: &str) -> Vec<String> {
    match tokio::net::lookup_host((name, 0_u16)).await {
        Ok(addrs) => addrs.map(|addr| addr.ip().to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

/// What one broker shares across its connection tasks and its server.
pub(crate) struct ProviderState {
    /// The routes, longest prefix served first.
    pub routes: Vec<ProviderRoute>,
    /// The egress allowlist, by host.
    pub allow: Vec<String>,
    /// The upstream ports the broker will open.
    pub ports: Vec<u16>,
    /// Whether a literal internal address may be dialled.
    pub allow_internal: bool,
    /// The logger the refusals go to.
    pub log: Logger,
    /// The resolver, injected by tests.
    pub resolve: Option<Resolve>,
    /// The port the provider endpoint answers on.
    pub provider_port: u16,
    /// The client that reaches the provider.
    pub client: reqwest::Client,
}

/// Whether a request is served by the provider endpoint, per its path.
fn route_for<'a>(state: &'a ProviderState, path: &str) -> Option<&'a ProviderRoute> {
    let mut routes = state.routes.iter().collect::<Vec<_>>();
    // Longest prefix first, so a provider named under another's path is still
    // reached rather than shadowed by it.
    routes.sort_by_key(|route| std::cmp::Reverse(route.prefix.len()));
    routes
        .into_iter()
        .find(|candidate| path.starts_with(&candidate.prefix))
}

/// Serves the provider API, with the real credential put on here.
///
/// Run on its own loopback port and reached by handing the connection over,
/// rather than by parsing HTTP on the raw socket: a request body may be
/// streamed and a response is often an event stream, and getting either wrong
/// would show up as a session that hangs rather than one that fails.
pub(crate) async fn serve_provider_request(
    state: Arc<ProviderState>,
    request: http::Request<axum::body::Body>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let path = request.uri().path().to_owned();
    let query = request
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let Some(route) = route_for(&state, &path) else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            "not a provider this broker serves\n",
        )
            .into_response();
    };
    let offered = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let expected = format!("Bearer {}", route.nonce);
    if !same_secret(&offered, &expected) {
        state.log.warn(
            "a provider call arrived without this session's key",
            &BTreeMap::new(),
        );
        return (axum::http::StatusCode::UNAUTHORIZED, "not this session\n").into_response();
    }

    let rest = path[route.prefix.len()..].to_owned();
    let trimmed_upstream = route.upstream.strip_suffix('/').unwrap_or(&route.upstream);
    let target = format!("{trimmed_upstream}{rest}{query}");
    // The provider is the operator's to name, so this is not a session
    // reaching somewhere it chose. It is still judged by the same rule, so
    // that a provider pointed at this machine is a deliberate setting rather
    // than a quiet exception to where the broker will go.
    let hostname = url_host(&target);
    let internal = match hostname {
        Some(hostname) => {
            let lookup = public_addresses(&hostname, state.allow_internal, None);
            let Ok(addresses) = tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await else {
                state.log.warn(
                    "the provider's name did not resolve in time",
                    &fields([("host", hostname.as_str().into())]),
                );
                return (
                    axum::http::StatusCode::GATEWAY_TIMEOUT,
                    "the provider's name did not resolve in time\n",
                )
                    .into_response();
            };
            addresses.is_empty()
        }
        None => true,
    };
    if internal {
        state.log.warn(
            "a provider is configured at a host-internal address",
            &fields([("provider", route.prefix.as_str().into())]),
        );
        return (
            axum::http::StatusCode::BAD_GATEWAY,
            "the provider is not at a reachable address\n",
        )
            .into_response();
    }

    let forwarded = build_forwarded(&state, request, route, &target);
    match forwarded.send().await {
        Ok(answered) => stream_answer(answered),
        Err(error) => {
            state.log.warn(
                "the provider could not be reached",
                &fields([("detail", error.to_string().into())]),
            );
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "the provider could not be reached\n",
            )
                .into_response()
        }
    }
}

/// The outbound request, with the session's key replaced by the real one.
fn build_forwarded(
    state: &ProviderState,
    request: http::Request<axum::body::Body>,
    route: &ProviderRoute,
    target: &str,
) -> reqwest::RequestBuilder {
    let mut forwarded = state.client.request(
        reqwest::Method::from_bytes(request.method().as_str().as_bytes())
            .unwrap_or(reqwest::Method::GET),
        target,
    );
    for (name, value) in request.headers() {
        // The credential is set on below, replacing whatever the session sent.
        if name == http::header::AUTHORIZATION {
            continue;
        }
        if !HOP_BY_HOP.contains(&name.as_str().to_lowercase().as_str()) {
            forwarded = forwarded.header(name.as_str(), value.to_str().unwrap_or(""));
        }
    }
    forwarded = forwarded.header(
        http::header::AUTHORIZATION,
        format!("Bearer {}", route.credential),
    );
    let body = request.into_body();
    forwarded.body(reqwest::Body::wrap_stream(body.into_data_stream()))
}

/// The provider's answer, with its hop-by-hop headers stripped.
fn stream_answer(answered: reqwest::Response) -> axum::response::Response {
    use axum::response::IntoResponse;
    use futures_util::TryStreamExt;

    let mut builder = axum::http::Response::builder().status(answered.status().as_u16());
    for (name, value) in answered.headers() {
        if !HOP_BY_HOP.contains(&name.as_str().to_lowercase().as_str())
            && let Ok(value) = value.to_str()
        {
            builder = builder.header(name.as_str(), value);
        }
    }
    let stream = answered
        .bytes_stream()
        .map_err(|error| std::io::Error::other(error.to_string()));
    builder
        .body(axum::body::Body::from_stream(stream))
        .unwrap_or_else(|error| {
            (axum::http::StatusCode::BAD_GATEWAY, error.to_string()).into_response()
        })
}

/// Where a resolved name comes back as.
pub(crate) type ResolveFuture = Pin<Box<dyn Future<Output = Vec<String>> + Send>>;

/// The host of a URL, without pulling in a full URL parser for one field.
fn url_host(target: &str) -> Option<String> {
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?']).next()?;
    // A provider is named, never bracketed, so an IPv6 literal here is not
    // reachable through this path either.
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host);
    Some(host.to_owned())
}

/// The listener that admits a session's connections and tunnels them.
pub(crate) mod server;

#[cfg(test)]
mod tests;
