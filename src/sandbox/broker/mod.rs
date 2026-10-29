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

use std::future::Future;
use std::net::Ipv4Addr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use receipts::{Ledger, RECEIPTS_FILENAME, ReceiptStore};
pub use server::{Broker, BrokerTemplate, SessionBroker};

use receipts::{Outcome, Receipt, ReceiptGuard};

use crate::config::route::{Capabilities, Placement, RoutePolicy};
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
    /// The variable the credential is configured under, for receipts.
    pub credential_name: Option<String>,
    /// Which header the key travels in and which calls it may make.
    pub policy: RoutePolicy,
}

/// A provider the broker stands in front of, before any session holds a
/// nonce for it.
#[derive(Clone)]
pub struct ProviderUpstream {
    /// The provider's name, which a session's nonce for it is filed under.
    pub name: String,
    /// Path a session addresses the provider at, such as `/provider/zai`.
    pub prefix: String,
    /// The provider's real base URL, which the prefix stands in for.
    pub upstream: String,
    /// The real credential. Never leaves the daemon.
    pub credential: String,
    /// Which header the key travels in and which calls it may make.
    pub policy: RoutePolicy,
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

/// The header an Anthropic-shaped provider reads its key from.
///
/// An OpenAI-shaped provider reads `Authorization` instead. Which one a route
/// uses is its placement; a route with none accepts the session's nonce in
/// either and puts the real credential back into whichever ones it used.
const API_KEY_HEADER: &str = "x-api-key";

/// Headers some frameworks read as the request's real method or path, in
/// place of the ones the route's capabilities were matched against.
const REROUTING: [&str; 5] = [
    "x-http-method-override",
    "x-method-override",
    "x-http-method",
    "x-original-url",
    "x-rewrite-url",
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

/// Whether exactly one proxy authorization header carries the run credential.
fn carries_proxy_credential(head: &str, expected: &str) -> bool {
    let Some(expected) = expected
        .strip_prefix("Basic ")
        .filter(|token| !token.is_empty())
    else {
        return false;
    };
    let mut credential = None;
    for line in head.lines().skip(1).take_while(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("proxy-authorization") {
            continue;
        }
        if credential.is_some() {
            return false;
        }
        credential = Some(value.trim());
    }
    let Some((scheme, token)) = credential.and_then(|value| value.split_once(char::is_whitespace))
    else {
        return false;
    };
    scheme.eq_ignore_ascii_case("basic") && same_secret(token.trim(), expected)
}

/// Whether a request carries this session's nonce as its key, in the header
/// the route's placement names, or in either when it names none.
///
/// Either form may carry a `Bearer ` prefix, as some clients add one unasked.
fn carries_nonce(headers: &http::header::HeaderMap, nonce: &str, placement: Placement) -> bool {
    let nonce_of = |name: http::header::HeaderName| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.strip_prefix("Bearer ").unwrap_or(value))
            .unwrap_or_default()
    };
    let bearer = || same_secret(nonce_of(http::header::AUTHORIZATION), nonce);
    let key = || {
        same_secret(
            nonce_of(http::header::HeaderName::from_static(API_KEY_HEADER)),
            nonce,
        )
    };
    match placement {
        Placement::Either => bearer() || key(),
        Placement::Bearer => bearer(),
        Placement::ApiKey => key(),
    }
}

/// What a session is told when its key is not where the route takes it.
fn unauthorized(placement: Placement) -> &'static str {
    match placement {
        Placement::Either => "not this session\n",
        Placement::Bearer => {
            "not this session. This provider takes its key as Authorization: Bearer.\n"
        }
        Placement::ApiKey => "not this session. This provider takes its key in x-api-key.\n",
    }
}

/// Why the path after a route's prefix cannot be matched as the upstream
/// would read it, or nothing when it can.
///
/// Refused rather than normalised, so the path a capability was matched
/// against is byte for byte the one forwarded: an empty segment, a `.` or
/// `..` segment, one that decodes once to either or to a slash or backslash,
/// one carrying `;` parameters, which some servers strip before routing, one
/// that still holds an escape after one decoding, which an upstream that
/// decodes twice would read as any of those, or one that decodes to a
/// control character, could each be resolved by the upstream, or by the URL
/// rebuilt here, to a path the match never saw.
fn ambiguous_path(rest: &str) -> Option<&'static str> {
    if rest.contains("//") {
        return Some("the path has an empty segment");
    }
    for segment in rest.split('/').skip(1) {
        let decoded = percent_decoded(segment);
        if decoded == "." || decoded == ".." {
            return Some("the path has a . or .. segment");
        }
        if decoded.contains('/') || decoded.contains('\\') {
            return Some("the path has an encoded slash");
        }
        if decoded.contains(';') {
            return Some("the path has ; parameters");
        }
        if decoded.contains('%') {
            return Some("the path is encoded twice");
        }
        if decoded.chars().any(char::is_control) {
            return Some("the path has a control character");
        }
    }
    None
}

/// How a route's capabilities answer a call: `None` to forward it, or the
/// refusal. A call a list does not name is refused only when the route is
/// strict, and said in the log either way, as is every call on a route that
/// declares nothing, with the entry that would allow it.
fn capability_refusal(
    state: &ProviderState,
    route: &ProviderRoute,
    method: &http::Method,
    rest: &str,
) -> Option<axum::response::Response> {
    use axum::response::IntoResponse;

    let policy = &route.policy;
    let path = recorded_path(rest);
    let shown = recorded_method(method);
    let call = || {
        fields([
            ("provider", route.prefix.as_str().into()),
            ("method", shown.as_str().into()),
            ("path", path.as_str().into()),
        ])
    };
    if policy.capabilities == Capabilities::Unset {
        let mut logged = call();
        logged.insert(
            "capability".to_owned(),
            format!(
                "{{ \"method\": \"{shown}\", \"path\": \"{}\" }}",
                suggested_path(rest)
            )
            .into(),
        );
        state.log.info(
            "a provider call is let through on a route that declares no capabilities, and the \
             capability field is the entry that would name it",
            &logged,
        );
        return None;
    }
    if policy.capabilities.allow(method.as_str(), rest) {
        return None;
    }
    let granted = match &policy.capabilities {
        Capabilities::Declared(granted) if policy.strict => granted,
        _ => {
            state.log.warn(
                "a provider call its route's capabilities do not name is let through, since the \
                 route is not strict",
                &call(),
            );
            return None;
        }
    };
    state.log.warn(
        "a provider call its route's capabilities do not name was refused",
        &call(),
    );
    let granted = granted
        .iter()
        .map(|capability| format!("{} {}", capability.method, capability.path))
        .collect::<Vec<_>>()
        .join(", ");
    Some(
        (
            axum::http::StatusCode::FORBIDDEN,
            format!("this provider route does not allow {method} {path}. It allows {granted}.\n"),
        )
            .into_response(),
    )
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
        // Azure's WireServer and platform address. It is public by number,
        // but the fabric intercepts it inside every guest, so nothing a
        // session legitimately dials answers there.
        || address == Ipv4Addr::new(168, 63, 129, 16)
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

/// How long dialling a target may take across all of its addresses.
///
/// A name with many addresses, several of them unanswering, would otherwise
/// take one [`DIAL_TIMEOUT`] each, and a client commonly gives up at 20s. With
/// the lookup's 5s this keeps the broker's refusal ahead of that, so the
/// client sees a 502 instead of a timeout that says nothing about where it
/// stalled.
pub const DIAL_BUDGET: Duration = Duration::from_secs(10);

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
///
/// A name that does not resolve is told apart from one that resolves only
/// somewhere internal, because the fix for each is different: the first is
/// this host's resolver, the second is the name or the allowlist.
pub async fn public_addresses(
    host: &str,
    allow_internal: bool,
    resolve: Option<&Resolve>,
) -> Result<Vec<String>, NoAddress> {
    let literal = host.split('.').count() == 4 && host.parse::<std::net::Ipv4Addr>().is_ok()
        || host.contains(':');
    if literal {
        if allow_internal || !is_private_address(host) {
            return Ok(vec![host.to_owned()]);
        }
        return Err(NoAddress::Internal);
    }
    let addresses = match resolve {
        Some(resolve) => resolve(host.to_owned()).await,
        None => default_resolve(host).await,
    }
    .map_err(NoAddress::Unresolved)?;
    if addresses.is_empty() {
        return Err(NoAddress::Unresolved(
            "the resolver found no address".to_owned(),
        ));
    }
    let public: Vec<String> = addresses
        .into_iter()
        .filter(|address| !is_private_address(address))
        .collect();
    if public.is_empty() {
        Err(NoAddress::Internal)
    } else {
        Ok(public)
    }
}

/// Why a target has no address the broker may dial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAddress {
    /// The name did not resolve, for the reason given. The broker resolves on
    /// this host, with this host's resolver, not the session's.
    Unresolved(String),
    /// Every address it resolves to is on the host's own network.
    Internal,
}

async fn default_resolve(name: &str) -> Result<Vec<String>, String> {
    tokio::net::lookup_host((name, 0_u16))
        .await
        .map(|addrs| addrs.map(|addr| addr.ip().to_string()).collect())
        .map_err(|error| error.to_string())
}

/// What one broker shares across its connection tasks and its server.
pub(crate) struct ProviderState {
    /// The routes, longest prefix served first.
    pub routes: Vec<ProviderRoute>,
    /// The egress allowlist, by host.
    pub allow: Vec<String>,
    /// The upstream ports the broker will open.
    pub ports: Vec<u16>,
    /// The expected Basic proxy authorization header for this session.
    pub proxy_authorization: String,
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
    /// Whether the broker has been closed, after which nothing is served.
    pub closed: Arc<AtomicBool>,
    /// Where provider calls are recorded, when they are.
    pub ledger: Option<Ledger>,
    /// When a call without the session's key was last recorded, so a session
    /// that repeats one is a record a minute rather than one an attempt.
    pub denied: std::sync::Mutex<DeniedCalls>,
}

/// How long a recorded denial holds off recording the next.
pub(crate) const DENIED_WINDOW_MS: i64 = 60_000;

/// When a call without the session's key was last recorded, and how many
/// have been refused since without being.
#[derive(Debug, Default)]
pub(crate) struct DeniedCalls {
    last_at: Option<i64>,
    unrecorded: u64,
}

impl DeniedCalls {
    /// Notes a denial at `now`: the count refused unrecorded since the last
    /// recorded one when this one is to be recorded, or nothing when it is
    /// one more within the window.
    fn note(&mut self, now: i64) -> Option<u64> {
        if self
            .last_at
            .is_some_and(|at| now.saturating_sub(at) < DENIED_WINDOW_MS)
        {
            self.unrecorded += 1;
            return None;
        }
        self.last_at = Some(now);
        Some(std::mem::take(&mut self.unrecorded))
    }
}

/// Whether a request is served by the provider endpoint, per its path.
fn route_for<'a>(state: &'a ProviderState, path: &str) -> Option<&'a ProviderRoute> {
    let mut routes = state.routes.iter().collect::<Vec<_>>();
    // Longest prefix first, so a provider named under another's path is still
    // reached rather than shadowed by it.
    routes.sort_by_key(|route| std::cmp::Reverse(route.prefix.len()));
    routes
        .into_iter()
        .find(|candidate| under_prefix(path, &candidate.prefix))
}

/// Whether a path is the prefix itself or one of its descendants.
fn under_prefix(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Why a brokered request cannot be given a target to dial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetError {
    /// The request's path is not a path: it is shaped so that some parser
    /// after this one could read an authority into it. Refused rather than
    /// normalised, because it was malformed on purpose.
    Malformed(&'static str),
    /// The operator's upstream is not a URL this broker dials at all.
    Upstream(&'static str),
}

/// A provider's base URL as the broker dials it, or why it will not.
///
/// The one reading of an upstream, for the broker and for the startup check
/// alike. The real key travels on every request, so the scheme must be
/// HTTPS, except to this host's own loopback, where a local gateway may speak
/// plain HTTP.
pub fn upstream_url(upstream: &str) -> Result<reqwest::Url, &'static str> {
    let upstream = upstream.trim();
    if !upstream.contains("://") {
        return Err("the base URL has no scheme, such as https://, in front");
    }
    let base = reqwest::Url::parse(upstream).map_err(|_| "the base URL is not a URL")?;
    match base.scheme() {
        "https" => {}
        "http" if is_loopback(&base) => {}
        "http" => return Err("the base URL is not HTTPS, so the key would cross in the clear"),
        _ => return Err("the scheme is not http or https"),
    }
    if !base.username().is_empty() || base.password().is_some() {
        return Err("the upstream carries credentials");
    }
    if base.query().is_some() || base.fragment().is_some() {
        return Err("the upstream carries a query or fragment");
    }
    Ok(base)
}

/// Whether a URL names this host's own loopback, as a literal address.
///
/// A name such as `localhost` is not counted: the broker never admits a name
/// on an internal answer, so such a provider could not be reached anyway.
fn is_loopback(url: &reqwest::Url) -> bool {
    url.host_str()
        .map(|host| host.trim_matches(['[', ']']))
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.to_canonical().is_loopback())
}

/// The provider client's resolver: it answers only addresses that pass the
/// rule a provider's name is judged by, at the moment the client dials.
///
/// The judgment before a call resolves the name once; without this the
/// client would resolve it again to dial, and a name that answered public to
/// the one could answer internal to the other.
///
/// Only names reach it, since an address literal is dialled without a
/// lookup, and a name is never admitted on an internal answer, so
/// `allowInternal` plays no part here. The client's connect timeout bounds
/// the lookup.
pub(crate) struct JudgedResolver {
    /// The resolver, injected by tests.
    pub resolve: Option<Resolve>,
}

impl reqwest::dns::Resolve for JudgedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        let resolve = self.resolve.clone();
        Box::pin(async move {
            let judged = public_addresses(&host, false, resolve.as_ref())
                .await
                .map_err(|refused| match refused {
                    NoAddress::Internal => std::io::Error::other(
                        "the provider's name resolves only to host-internal addresses",
                    ),
                    NoAddress::Unresolved(why) => std::io::Error::other(why),
                })?;
            let addresses = judged
                .iter()
                .map(|address| {
                    address
                        .parse::<std::net::IpAddr>()
                        .map(|ip| std::net::SocketAddr::new(ip, 0))
                        .map_err(|_| std::io::Error::other(format!("{address} is not an address")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Builds a provider URL without allowing the session to change its origin.
fn provider_target(
    route: &ProviderRoute,
    rest: &str,
    query: Option<&str>,
) -> Result<reqwest::Url, TargetError> {
    let lower = rest.to_ascii_lowercase();
    if (!rest.is_empty() && !rest.starts_with('/'))
        || rest.starts_with("//")
        || rest.contains('\\')
        || lower.contains("%2f")
        || lower.contains("%5c")
    {
        return Err(TargetError::Malformed(
            "the path carries an authority-shaped or encoded piece",
        ));
    }
    let base = upstream_url(&route.upstream).map_err(TargetError::Upstream)?;
    let prefix = base.path().trim_end_matches('/');
    let mut target = base.clone();
    target.set_path(&format!("{prefix}{rest}"));
    target.set_query(query);
    target.set_fragment(None);
    if target.scheme() != base.scheme()
        || target.host() != base.host()
        || target.port_or_known_default() != base.port_or_known_default()
        || !target.username().is_empty()
        || target.password().is_some()
        || !under_prefix(target.path(), prefix)
    {
        return Err(TargetError::Malformed(
            "the rebuilt target left the upstream",
        ));
    }
    Ok(target)
}

/// What a stopped session's broker answers a request with.
const STOPPED: &str = "this session has stopped\n";

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

    if state.closed.load(Ordering::SeqCst) {
        return (axum::http::StatusCode::SERVICE_UNAVAILABLE, STOPPED).into_response();
    }
    let path = request.uri().path().to_owned();
    let Some(route) = route_for(&state, &path) else {
        // An unmatched route is where a probe dies: the session named a
        // provider this broker does not serve, or spelled one in a way that
        // would have escaped below. Say so, with the path.
        state.log.warn(
            "a provider call arrived for a route this broker does not serve",
            &fields([("path", path.into())]),
        );
        return (
            axum::http::StatusCode::NOT_FOUND,
            "not a provider this broker serves\n",
        )
            .into_response();
    };
    let rest = path[route.prefix.len()..].to_owned();
    let mut receipt = ReceiptGuard::begin(
        state.ledger.as_ref(),
        &state.log,
        Receipt {
            at: 0,
            session_id: String::new(),
            provider: route.prefix.clone(),
            credential_name: route.credential_name.clone(),
            upstream_host: None,
            method: recorded_method(request.method()),
            path: recorded_path(&rest),
            query_bytes: request.uri().query().map(|query| query.len() as u64),
            status: None,
            outcome: Outcome::Interrupted,
            elapsed_ms: 0,
            bytes_out: 0,
            bytes_in: 0,
        },
    );
    // The session proves itself with the nonce this broker gave it, before
    // anything about what the route allows is said.
    let placement = route.policy.placement;
    if !carries_nonce(request.headers(), &route.nonce, placement) {
        // A refusal is the end of a request, not of a session that keeps
        // sending them, so it is recorded once a minute with how many went
        // unrecorded, for the log and the store alike.
        let now = state
            .ledger
            .as_ref()
            .map_or_else(crate::log::now_ms, |ledger| (ledger.now)());
        let noted = state.denied.lock().expect("the denied lock").note(now);
        match noted {
            Some(unrecorded) => {
                state.log.warn(
                    "a provider call arrived without this session's key",
                    &fields([
                        ("provider", route.prefix.as_str().into()),
                        ("unrecorded_since_last", unrecorded.into()),
                    ]),
                );
                receipt.answered(None, Some(401));
                receipt.finish(Outcome::Denied);
            }
            None => receipt.discard(),
        }
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            unauthorized(placement),
        )
            .into_response();
    }
    if let Some(why) = ambiguous_path(&rest) {
        state.log.warn(
            "a provider call arrived with a path that cannot be a path",
            &fields([
                ("provider", route.prefix.as_str().into()),
                ("path", recorded_path(&rest).into()),
                ("detail", why.into()),
            ]),
        );
        let response = (
            axum::http::StatusCode::BAD_REQUEST,
            format!("{why}, which the broker does not forward\n"),
        )
            .into_response();
        return refused_call(receipt, response);
    }
    if let Some(refused) = capability_refusal(&state, route, request.method(), &rest) {
        return refused_call(receipt, refused);
    }
    forward_to_provider(&state, request, route, rest, receipt).await
}

/// The path a capability would need to name a call, as the log suggests it.
///
/// Matched as sent, so a segment is suggested only as it arrived. One that is
/// escaped, or holds something minted for the session, is suggested as `*`,
/// and a path too long to show whole as everything beneath its first segment.
fn suggested_path(rest: &str) -> String {
    if rest.len() > RECORDED_PATH_BYTES {
        let first = rest.split('/').nth(1).unwrap_or_default();
        return format!("/{}/**", suggested_segment(first));
    }
    rest.split('/')
        .map(suggested_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// One segment of [`suggested_path`].
fn suggested_segment(segment: &str) -> &str {
    if segment.contains('%') || crate::config::redact::redact_text(segment, &[]) != segment {
        "*"
    } else {
        segment
    }
}

/// The longest path a receipt keeps.
const RECORDED_PATH_BYTES: usize = 256;

/// A call's method as a receipt keeps it. The session chooses it, and any
/// token is a method, so one that is not a standard name is kept as `OTHER`.
fn recorded_method(method: &http::Method) -> String {
    const STANDARD: [&str; 9] = [
        "GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH",
    ];
    if STANDARD.contains(&method.as_str()) {
        method.as_str().to_owned()
    } else {
        "OTHER".to_owned()
    }
}

/// A call's path as a receipt keeps it: decoded and scrubbed of anything
/// minted for a session, since the session chose it and could have encoded
/// one to slip it past, cut at a query it encoded into the path, which is
/// kept only by its length, with control characters written as escapes, so
/// that printing a receipt cannot drive the terminal it is printed to, and
/// cut to a length a record needs.
fn recorded_path(path: &str) -> String {
    let decoded = percent_decoded(path);
    let path = decoded.split('?').next().unwrap_or_default();
    let scrubbed =
        crate::config::redact::escape_controls(&crate::config::redact::redact_text(path, &[]));
    if scrubbed.len() <= RECORDED_PATH_BYTES {
        return scrubbed;
    }
    let mut cut = RECORDED_PATH_BYTES;
    while !scrubbed.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &scrubbed[..cut])
}

/// `text` with each `%XX` turned back into the byte it stands for, lossily
/// where the result is not UTF-8.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |byte: u8| char::from(byte).to_digit(16);
        if bytes[at] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(at + 1).and_then(|byte| hex(*byte)),
                bytes.get(at + 2).and_then(|byte| hex(*byte)),
            )
        {
            out.push(u8::try_from(high * 16 + low).unwrap_or(b'?'));
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Writes a refused call's receipt with the status it was answered with.
fn refused_call(
    mut receipt: ReceiptGuard,
    response: axum::response::Response,
) -> axum::response::Response {
    receipt.answered(None, Some(response.status().as_u16()));
    receipt.finish(Outcome::Refused);
    response
}

/// Judges where a session's call would go, forwards it with the real
/// credential, and relays the answer, finishing its receipt on every path.
async fn forward_to_provider(
    state: &ProviderState,
    request: http::Request<axum::body::Body>,
    route: &ProviderRoute,
    rest: String,
    mut receipt: ReceiptGuard,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let target = match provider_target(route, &rest, request.uri().query()) {
        Ok(target) => target,
        Err(TargetError::Upstream(why)) => {
            state.log.warn(
                "the provider's upstream is not a URL this broker dials",
                &fields([
                    ("provider", route.prefix.as_str().into()),
                    ("detail", why.into()),
                ]),
            );
            let response = (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("the provider is not at a reachable address: {why}\n"),
            )
                .into_response();
            return refused_call(receipt, response);
        }
        Err(TargetError::Malformed(why)) => {
            state.log.warn(
                "a provider call arrived with a path that cannot be a path",
                &fields([
                    ("provider", route.prefix.as_str().into()),
                    ("path", rest.into()),
                    ("detail", why.into()),
                ]),
            );
            let response = (
                axum::http::StatusCode::BAD_REQUEST,
                "the provider path is invalid or leaves its configured base\n",
            )
                .into_response();
            return refused_call(receipt, response);
        }
    };
    if let Some(refused) = provider_refusal(state, route, &target).await {
        return refused_call(receipt, refused);
    }
    // Noted before the call is sent, so a session that hangs up while it is
    // in flight still shows where the credential went.
    receipt.answered(target.host_str().map(str::to_owned), None);

    let method = request.method().to_string();
    let started = std::time::Instant::now();
    let forwarded = build_forwarded(state, request, route, &target, &receipt.bytes_out);
    match forwarded.send().await {
        Ok(answered) => {
            state.log.debug(
                "the provider answered",
                &fields([
                    ("provider", route.prefix.as_str().into()),
                    ("method", method.into()),
                    ("path", rest.into()),
                    ("status", i64::from(answered.status().as_u16()).into()),
                    (
                        "elapsed_ms",
                        u64::try_from(started.elapsed().as_millis())
                            .unwrap_or(u64::MAX)
                            .into(),
                    ),
                ]),
            );
            receipt.answered(
                target.host_str().map(str::to_owned),
                Some(answered.status().as_u16()),
            );
            answer::relay(
                answered,
                route,
                &state.log,
                answer::MAX_ANSWER_BYTES,
                receipt,
            )
        }
        Err(error) => {
            state.log.warn(
                "the provider could not be reached",
                &fields([
                    ("provider", route.prefix.as_str().into()),
                    ("detail", error.to_string().into()),
                ]),
            );
            receipt.answered(target.host_str().map(str::to_owned), Some(502));
            receipt.finish(Outcome::UpstreamUnreachable);
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "the provider could not be reached\n",
            )
                .into_response()
        }
    }
}

/// What to answer the session with when its provider cannot be reached.
async fn provider_refusal(
    state: &ProviderState,
    route: &ProviderRoute,
    target: &reqwest::Url,
) -> Option<axum::response::Response> {
    use axum::response::IntoResponse;

    // The provider is the operator's to name, so this is not a session
    // reaching somewhere it chose. It is still judged by the same rule, so
    // that a provider pointed at this machine is a deliberate setting rather
    // than a quiet exception to where the broker will go.
    let hostname = target.host_str().map(|host| host.trim_matches(['[', ']']));
    let internal = match hostname {
        Some(hostname) => {
            let lookup = public_addresses(hostname, state.allow_internal, state.resolve.as_ref());
            let Ok(addresses) = tokio::time::timeout(LOOKUP_TIMEOUT, lookup).await else {
                state.log.warn(
                    "the provider's name did not resolve in time",
                    &fields([("host", hostname.into())]),
                );
                return Some(
                    (
                        axum::http::StatusCode::GATEWAY_TIMEOUT,
                        "the provider's name did not resolve in time\n",
                    )
                        .into_response(),
                );
            };
            match addresses {
                Ok(_) => false,
                Err(NoAddress::Internal) => true,
                Err(NoAddress::Unresolved(why)) => {
                    state.log.warn(
                        "the provider's name did not resolve on this host",
                        &fields([("host", hostname.into()), ("detail", why.into())]),
                    );
                    return Some(
                        (
                            axum::http::StatusCode::BAD_GATEWAY,
                            "the provider's name did not resolve\n",
                        )
                            .into_response(),
                    );
                }
            }
        }
        None => true,
    };
    if internal {
        state.log.warn(
            "a provider is configured at a host-internal address",
            &fields([("provider", route.prefix.as_str().into())]),
        );
        return Some(
            (
                axum::http::StatusCode::BAD_GATEWAY,
                "the provider is not at a reachable address\n",
            )
                .into_response(),
        );
    }
    None
}

/// The outbound request, with the session's key replaced by the real one.
fn build_forwarded(
    state: &ProviderState,
    request: http::Request<axum::body::Body>,
    route: &ProviderRoute,
    target: &reqwest::Url,
    sent: &Arc<std::sync::atomic::AtomicU64>,
) -> reqwest::RequestBuilder {
    use futures_util::StreamExt;

    let mut forwarded = state.client.request(
        reqwest::Method::from_bytes(request.method().as_str().as_bytes())
            .unwrap_or(reqwest::Method::GET),
        target.clone(),
    );
    // Whether the session sent its key the Anthropic way, which decides,
    // on a route with no placement, whether the provider is also given it
    // that way below.
    let sends_key = request.headers().contains_key(API_KEY_HEADER);
    for (name, value) in request.headers() {
        // Credentials are set on below, replacing whatever the session sent:
        // the nonce is worth nothing past the broker, so it must never reach
        // the provider, whichever header carried it.
        if name == http::header::AUTHORIZATION || name.as_str() == API_KEY_HEADER {
            continue;
        }
        // Replaced below: a compressed answer could not be scrubbed.
        if name == http::header::ACCEPT_ENCODING {
            continue;
        }
        if REROUTING.contains(&name.as_str()) {
            continue;
        }
        if !HOP_BY_HOP.contains(&name.as_str().to_lowercase().as_str()) {
            forwarded = forwarded.header(name.as_str(), value.to_str().unwrap_or(""));
        }
    }
    forwarded = forwarded.header(http::header::ACCEPT_ENCODING, "identity");
    let bearer = format!("Bearer {}", route.credential);
    forwarded = match route.policy.placement {
        Placement::Either if sends_key => forwarded
            .header(http::header::AUTHORIZATION, bearer)
            .header(API_KEY_HEADER, route.credential.clone()),
        Placement::Either | Placement::Bearer => {
            forwarded.header(http::header::AUTHORIZATION, bearer)
        }
        Placement::ApiKey => forwarded.header(API_KEY_HEADER, route.credential.clone()),
    };
    let sent = Arc::clone(sent);
    let body = request
        .into_body()
        .into_data_stream()
        .inspect(move |chunk| {
            if let Ok(chunk) = chunk {
                sent.fetch_add(chunk.len() as u64, Ordering::SeqCst);
            }
        });
    forwarded.body(reqwest::Body::wrap_stream(body))
}

/// Where a resolved name comes back as.
pub(crate) type ResolveFuture = Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send>>;

/// What a provider's answer is relayed to the session as.
mod answer;
/// What a brokered credential was used for, kept after the fact.
pub mod receipts;
/// The listener that admits a session's connections and tunnels them.
pub(crate) mod server;

#[cfg(test)]
mod tests;
