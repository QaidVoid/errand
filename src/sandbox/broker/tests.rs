//! Tests for the egress broker, ported from `broker_test.ts`.

use std::collections::BTreeMap;
use std::str::FromStr as _;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::server::{Broker, BrokerTemplate, dial, proxy_authorization, read_request_head};
use super::{
    NoAddress, ProviderRoute, ProviderUpstream, Resolve, carries_nonce, host_allowed,
    is_private_address, parse_connect, parse_forward, public_addresses,
};
use crate::log::{LogFields, Logger};

fn silent() -> Logger {
    Logger::new(LogFields::new(), Arc::new(|_level, _line| {}))
}

fn resolver(addresses: &[&str]) -> Resolve {
    let addresses: Vec<String> = addresses
        .iter()
        .map(|address| address.to_owned().to_owned())
        .collect();
    Arc::new(move |_name| {
        let addresses = addresses.clone();
        Box::pin(async move { Ok(addresses.clone()) })
    })
}

#[test]
fn a_bare_allow_rule_matches_only_itself() {
    let allow = vec!["github.com".to_owned()];
    assert!(host_allowed("github.com", &allow));
    assert!(host_allowed("GitHub.com", &allow));
    assert!(!host_allowed("evil.com", &allow));
    // A suffix that is not a subdomain must not match a bare rule.
    assert!(!host_allowed("notgithub.com", &allow));
    assert!(!host_allowed("github.com.evil.com", &allow));
}

#[test]
fn a_wildcard_rule_matches_subdomains_but_not_the_apex() {
    let allow = vec!["*.githubusercontent.com".to_owned()];
    assert!(host_allowed("codeload.githubusercontent.com", &allow));
    assert!(host_allowed("a.b.githubusercontent.com", &allow));
    // The apex is not a subdomain of itself; a wildcard is written when the
    // apex is not what is talked to.
    assert!(!host_allowed("githubusercontent.com", &allow));
    // A lookalike that merely ends in the string but is not a subdomain.
    assert!(!host_allowed("evilgithubusercontent.com", &allow));
}

#[test]
fn a_star_without_its_dot_is_a_literal_rule_and_matches_nothing() {
    // Validation refuses such a rule, so reaching here means one was written
    // straight into an allowlist. It is compared literally rather than read as
    // a wildcard somebody did not write.
    let allow = vec!["*githubusercontent.com".to_owned()];
    assert!(!host_allowed("codeload.githubusercontent.com", &allow));
    assert!(!host_allowed("githubusercontent.com", &allow));
    assert!(!host_allowed("evilgithubusercontent.com", &allow));
}

#[test]
fn an_empty_allowlist_admits_nothing() {
    assert!(!host_allowed("github.com", &[]));
    assert!(!host_allowed("", &["github.com".to_owned()]));
}

#[test]
fn a_lone_star_admits_any_host_but_not_the_empty_host() {
    let allow = vec!["*".to_owned()];
    assert!(host_allowed("github.com", &allow));
    assert!(host_allowed("derp1.tailscale.com", &allow));
    assert!(host_allowed(
        "anything.example",
        &["a.com".to_owned(), "*".to_owned()]
    ));
    // The catch-all still does not conjure a host out of nothing.
    assert!(!host_allowed("", &allow));
}

#[test]
fn a_connect_line_yields_its_host_and_port_or_nothing() {
    assert_eq!(
        parse_connect("CONNECT github.com:443 HTTP/1.1"),
        Some(super::Target {
            host: "github.com".to_owned(),
            port: 443
        })
    );
    assert_eq!(
        parse_connect("connect github.com:443 HTTP/1.1"),
        Some(super::Target {
            host: "github.com".to_owned(),
            port: 443
        })
    );
    // Not CONNECT, no port, junk port, out of range, and a smuggled path.
    assert_eq!(parse_connect("GET / HTTP/1.1"), None);
    assert_eq!(parse_connect("CONNECT github.com HTTP/1.1"), None);
    assert_eq!(parse_connect("CONNECT github.com:https HTTP/1.1"), None);
    assert_eq!(parse_connect("CONNECT github.com:99999 HTTP/1.1"), None);
    assert_eq!(parse_connect("CONNECT evil.com/path:443 HTTP/1.1"), None);
}

#[test]
fn a_proxied_http_request_is_rewritten_for_the_origin() {
    let forward = parse_forward(
        "POST http://mirror.example:8080/simple/pkg?x=1 HTTP/1.1\r\n\
Host: mirror.example:8080\r\n\
Proxy-Connection: keep-alive\r\n\
Proxy-Authorization: Basic c2VjcmV0\r\n\
Content-Length: 4\r\n\r\n",
    )
    .expect("a proxied request");
    assert_eq!(
        forward.target,
        super::Target {
            host: "mirror.example".to_owned(),
            port: 8080
        }
    );
    assert_eq!(
        forward.head,
        "POST /simple/pkg?x=1 HTTP/1.1\r\n\
Host: mirror.example:8080\r\n\
Content-Length: 4\r\n\
Connection: close\r\n\r\n"
    );

    let bare = parse_forward("GET http://mirror.example HTTP/1.1\r\n\r\n").expect("a request");
    assert_eq!(bare.target.port, 80);
    assert!(bare.head.starts_with("GET / HTTP/1.1\r\n"));

    // A tunnel, a provider call, TLS, a bad port, and smuggled credentials.
    assert_eq!(
        parse_forward("CONNECT github.com:443 HTTP/1.1\r\n\r\n"),
        None
    );
    assert_eq!(parse_forward("GET /provider/v1 HTTP/1.1\r\n\r\n"), None);
    assert_eq!(
        parse_forward("GET https://github.com/ HTTP/1.1\r\n\r\n"),
        None
    );
    assert_eq!(
        parse_forward("GET http://github.com:0/ HTTP/1.1\r\n\r\n"),
        None
    );
    assert_eq!(
        parse_forward("GET http://me@github.com/ HTTP/1.1\r\n\r\n"),
        None
    );
}

/// Connects to the broker as an `HTTPS_PROXY` client would, sends one CONNECT.
async fn try_connect(port: u16, authority: &str) -> String {
    let mut conn = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the broker");
    conn.write_all(
        format!("CONNECT {authority} HTTP/1.1\r\nProxy-Authorization: {PROXY_CREDENTIAL}\r\n\r\n")
            .as_bytes(),
    )
    .await
    .expect("written");
    let mut buf = vec![0_u8; 128];
    let read = conn.read(&mut buf).await.unwrap_or(0);
    String::from_utf8_lossy(&buf[..read])
        .split("\r\n")
        .next()
        .unwrap_or("")
        .to_owned()
}

#[tokio::test]
async fn the_broker_refuses_what_is_not_allowed_by_name_port_and_address() {
    // Loopback is allowlisted here on purpose: even so, it is refused, because
    // the allowlist decides which names, and the address filter decides that
    // the host's own network is never one of them.
    let mut broker = Broker::new(
        vec!["127.0.0.1".to_owned()],
        vec![443],
        silent(),
        Vec::new(),
        false,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");
    // Allowed by name, but an internal address, so refused all the same.
    assert!(try_connect(port, "127.0.0.1:443").await.contains("403"));
    // Not on the allowlist.
    assert!(
        try_connect(port, "derp1.tailscale.com:443")
            .await
            .contains("403")
    );
    // A port the broker will not open, refused before the address is weighed.
    assert!(try_connect(port, "127.0.0.1:5432").await.contains("403"));
    broker.close();
}

#[tokio::test]
async fn the_broker_refuses_a_non_connect_opener() {
    let mut broker = Broker::new(
        vec!["github.com".to_owned()],
        vec![443],
        silent(),
        Vec::new(),
        false,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");

    let mut conn = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the broker");
    conn.write_all(b"GET / HTTP/1.1\r\n\r\n")
        .await
        .expect("written");
    let mut buf = vec![0_u8; 64];
    let read = conn.read(&mut buf).await.unwrap_or(0);
    let line = String::from_utf8_lossy(&buf[..read]);
    assert!(line.contains("400"));
    broker.close();
}

#[tokio::test]
async fn the_head_is_read_whole_leaving_the_tunnelled_bytes_untouched() {
    // A real client sends headers after the CONNECT line, then the blank line,
    // then its TLS. The broker must consume up to and including the blank line
    // and no further, or the leftover header bytes would be piped to the
    // upstream ahead of the ClientHello and break the handshake.
    let wire = b"CONNECT open.example.com:443 HTTP/1.1\r\n\
Host: open.example.com:443\r\n\
Proxy-Connection: keep-alive\r\n\r\n\
TLS-CLIENT-HELLO";
    let mut reader: &[u8] = wire;
    let head = read_request_head(&mut reader).await.expect("a head");
    assert_eq!(
        head.split('\n').next().unwrap_or("").trim_end_matches('\r'),
        "CONNECT open.example.com:443 HTTP/1.1"
    );
    // Everything after the blank line is still there for the tunnel to carry.
    assert_eq!(String::from_utf8_lossy(reader), "TLS-CLIENT-HELLO");
}

/// A stand-in provider that reports what key it was actually handed, under
/// each header a session may send one in.
async fn start_upstream(seen: Arc<Mutex<BTreeMap<String, String>>>, key: &'static str) -> u16 {
    let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let seen = Arc::clone(&seen);
        async move {
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("")
                    .to_owned()
            };
            let mut seen = seen.lock().unwrap();
            seen.insert(key.to_owned(), header("authorization"));
            seen.insert(format!("{key}/x-api-key"), header("x-api-key"));
            (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                "{}",
            )
        }
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bound");
    let port = listener.local_addr().expect("an address").port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serves");
    });
    port
}

#[tokio::test]
async fn the_credential_is_put_on_at_the_broker_never_given_to_the_session() {
    let seen: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let upstream_port = start_upstream(Arc::clone(&seen), "one").await;

    let mut broker = Broker::new(
        Vec::new(),
        vec![443],
        silent(),
        vec![ProviderRoute {
            prefix: "/provider".to_owned(),
            upstream: format!("http://127.0.0.1:{upstream_port}/v4"),
            nonce: "the-session-nonce".to_owned(),
            credential: "the-real-key".to_owned(),
            credential_name: None,
        }],
        true,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");

    let client = reqwest::Client::new();
    let allowed = client
        .post(format!("http://127.0.0.1:{port}/provider/chat/completions"))
        .header("authorization", "Bearer the-session-nonce")
        .body("{}")
        .send()
        .await
        .expect("answered");
    assert_eq!(allowed.status().as_u16(), 200);
    // The session never held this, and the provider still received it.
    assert_eq!(
        seen.lock().unwrap().get("one").map(String::as_str),
        Some("Bearer the-real-key")
    );

    // What a session could read out of its own environment is the nonce, and
    // a nonce the broker does not know is worth nothing.
    let refused = client
        .post(format!("http://127.0.0.1:{port}/provider/chat/completions"))
        .header("authorization", "Bearer not-the-nonce")
        .body("{}")
        .send()
        .await
        .expect("answered");
    assert_eq!(refused.status().as_u16(), 401);
    broker.close();
}

/// The session's key arrives in whichever header its provider kind sends one
/// in: `Authorization` for an OpenAI-shaped provider, `x-api-key` for an
/// Anthropic-shaped one.
#[test]
fn a_nonce_in_either_key_header_is_this_session() {
    let keyed = |name: &str, value: &str| {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            name.parse::<axum::http::header::HeaderName>()
                .expect("a header name"),
            value
                .parse::<axum::http::header::HeaderValue>()
                .expect("a value"),
        );
        headers
    };

    assert!(carries_nonce(&keyed("authorization", "Bearer n"), "n"));
    assert!(carries_nonce(&keyed("authorization", "n"), "n"));
    assert!(carries_nonce(&keyed("x-api-key", "n"), "n"));
    assert!(carries_nonce(&keyed("x-api-key", "Bearer n"), "n"));
    assert!(!carries_nonce(&keyed("authorization", "Bearer x"), "n"));
    assert!(!carries_nonce(&keyed("x-api-key", "x"), "n"));
    assert!(!carries_nonce(&axum::http::HeaderMap::new(), "n"));
}

/// A session addressing an Anthropic-shaped provider sends its key in
/// `x-api-key`. The broker accepts the nonce there too, and the provider
/// receives the real key under both headers: its own in `x-api-key`, and the
/// bearer errand always puts on.
#[tokio::test]
async fn a_session_key_in_x_api_key_is_accepted_and_translated() {
    let seen: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let upstream_port = start_upstream(Arc::clone(&seen), "one").await;

    let mut broker = Broker::new(
        Vec::new(),
        vec![443],
        silent(),
        vec![ProviderRoute {
            prefix: "/provider".to_owned(),
            upstream: format!("http://127.0.0.1:{upstream_port}/v1"),
            nonce: "the-session-nonce".to_owned(),
            credential: "the-real-key".to_owned(),
            credential_name: None,
        }],
        true,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");

    let client = reqwest::Client::new();
    let allowed = client
        .post(format!("http://127.0.0.1:{port}/provider/v1/messages"))
        .header("x-api-key", "the-session-nonce")
        .body("{}")
        .send()
        .await
        .expect("answered");
    assert_eq!(allowed.status().as_u16(), 200);
    // The nonce never reaches the provider, whichever header carried it.
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.get("one/x-api-key").map(String::as_str),
            Some("the-real-key")
        );
        assert_eq!(
            seen.get("one").map(String::as_str),
            Some("Bearer the-real-key")
        );
    }

    // A key that is not this session's is refused in either header.
    let refused = client
        .post(format!("http://127.0.0.1:{port}/provider/v1/messages"))
        .header("x-api-key", "not-the-nonce")
        .body("{}")
        .send()
        .await
        .expect("answered");
    assert_eq!(refused.status().as_u16(), 401);
    broker.close();
}

/// A nonce is per provider within a session, so reading one out of a session
/// buys nothing against another provider the same broker serves.
#[tokio::test]
async fn each_provider_has_its_own_route_and_its_own_nonce() {
    let seen: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let one_port = start_upstream(Arc::clone(&seen), "one").await;
    let two_port = start_upstream(Arc::clone(&seen), "two").await;

    let mut broker = Broker::new(
        Vec::new(),
        vec![443],
        silent(),
        vec![
            ProviderRoute {
                prefix: "/provider/zai".to_owned(),
                upstream: format!("http://127.0.0.1:{one_port}/v4"),
                nonce: "nonce-zai".to_owned(),
                credential: "key-zai".to_owned(),
                credential_name: None,
            },
            ProviderRoute {
                prefix: "/provider/meta".to_owned(),
                upstream: format!("http://127.0.0.1:{two_port}/v1"),
                nonce: "nonce-meta".to_owned(),
                credential: "key-meta".to_owned(),
                credential_name: None,
            },
        ],
        true,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");

    let client = reqwest::Client::new();
    let a = client
        .post(format!("http://127.0.0.1:{port}/provider/zai/chat"))
        .header("authorization", "Bearer nonce-zai")
        .body("{}")
        .send()
        .await
        .expect("answered");
    let b = client
        .post(format!("http://127.0.0.1:{port}/provider/meta/chat"))
        .header("authorization", "Bearer nonce-meta")
        .body("{}")
        .send()
        .await
        .expect("answered");
    assert_eq!(a.status().as_u16(), 200);
    assert_eq!(b.status().as_u16(), 200);

    // Each upstream got its own key, and neither got the other's.
    assert_eq!(
        seen.lock().unwrap().get("one").map(String::as_str),
        Some("Bearer key-zai")
    );
    assert_eq!(
        seen.lock().unwrap().get("two").map(String::as_str),
        Some("Bearer key-meta")
    );

    // One provider's nonce is refused on another's route.
    let crossed = client
        .post(format!("http://127.0.0.1:{port}/provider/meta/chat"))
        .header("authorization", "Bearer nonce-zai")
        .body("{}")
        .send()
        .await
        .expect("answered");
    assert_eq!(crossed.status().as_u16(), 401);
    broker.close();
}

#[test]
fn host_internal_addresses_are_refused_whatever_the_allowlist_says() {
    for address in [
        "127.0.0.1",
        "10.0.0.1",
        "192.168.0.1",
        "169.254.169.254",
        "100.64.0.1",
    ] {
        assert!(is_private_address(address), "{address}");
    }
    for address in ["8.8.8.8", "1.1.1.1", "140.82.121.4", "2606:4700:4700::1111"] {
        assert!(!is_private_address(address), "{address}");
    }
    // Reserved rather than host-internal, but nothing a session legitimately
    // talks to answers on them.
    for address in [
        "192.0.2.9",     // TEST-NET-1
        "198.51.100.9",  // TEST-NET-2
        "203.0.113.9",   // TEST-NET-3
        "192.0.0.8",     // IETF protocol assignments
        "192.88.99.1",   // 6to4 relay anycast
        "198.18.0.1",    // benchmarking
        "198.19.255.1",  // benchmarking, the far end
        "168.63.129.16", // Azure fabric, public by number
    ] {
        assert!(is_private_address(address), "{address}");
    }
    // Loopback and link-local in v6, and a v4 loopback wearing a v6 coat.
    for address in ["::1", "fe80::1", "fd00::1", "::ffff:127.0.0.1"] {
        assert!(is_private_address(address), "{address}");
    }
    // The whole of `this network`, multicast, and everything reserved above
    // it, none of which is the internet a session is allowed out to.
    for address in [
        "0.0.0.0",
        "0.1.2.3",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
    ] {
        assert!(is_private_address(address), "{address}");
    }
    // Not an address the broker can read, so not one it should dial. A
    // spelling with a leading zero is read as octal by some resolvers and as
    // decimal by others, which is reason enough to refuse it.
    for address in [
        "",
        "1.2.3",
        "1.2.3.4.5",
        "256.0.0.1",
        "1.2.3.04",
        "not-an-address",
    ] {
        assert!(is_private_address(address), "{address}");
    }
}

/// A name on the allowlist that points at the host is still refused.
#[tokio::test]
async fn a_name_is_judged_by_where_it_resolves_not_by_its_spelling() {
    // Resolves to loopback: nothing to dial.
    assert_eq!(
        public_addresses("rebind.test", false, Some(&resolver(&["127.0.0.1"]))).await,
        Err(NoAddress::Internal)
    );
    // Mixed: the public ones are what get dialled, in resolver order, and they
    // are addresses, so what was judged is what is used rather than the name
    // resolved a second time.
    assert_eq!(
        public_addresses(
            "mixed.test",
            false,
            Some(&resolver(&["10.0.0.1", "9.9.9.9", "1.1.1.1"]))
        )
        .await,
        Ok(vec!["9.9.9.9".to_owned(), "1.1.1.1".to_owned()])
    );
    // A literal internal target does not even reach the resolver.
    assert_eq!(
        public_addresses("169.254.169.254", false, None).await,
        Err(NoAddress::Internal)
    );
    // Azure's fabric address is refused as a literal and as an answer.
    assert_eq!(
        public_addresses("168.63.129.16", false, None).await,
        Err(NoAddress::Internal)
    );
    assert_eq!(
        public_addresses("fabric.test", false, Some(&resolver(&["168.63.129.16"]))).await,
        Err(NoAddress::Internal)
    );
}

/// A name that does not resolve is not called internal. The fix is this
/// host's resolver, not the allowlist, and saying otherwise sent an operator
/// looking in the wrong place.
#[tokio::test]
async fn a_name_that_does_not_resolve_is_not_called_internal() {
    let broken: Resolve = Arc::new(|_name| {
        Box::pin(async { Err("failed to lookup address information".to_owned()) })
    });
    assert_eq!(
        public_addresses("opencode.ai", true, Some(&broken)).await,
        Err(NoAddress::Unresolved(
            "failed to lookup address information".to_owned()
        ))
    );
    assert!(matches!(
        public_addresses("opencode.ai", false, Some(&resolver(&[]))).await,
        Err(NoAddress::Unresolved(_))
    ));
}

/// The broker refuses to tunnel to the host's own loopback.
#[tokio::test]
async fn a_tunnel_to_loopback_is_refused_even_under_a_lone_star() {
    let mut broker = Broker::new(
        vec!["*".to_owned()],
        vec![443],
        silent(),
        Vec::new(),
        false,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");
    let reply = try_connect(port, "127.0.0.1:443").await;
    assert!(reply.contains("403"));
    broker.close();
}

/// An operator who names an internal address outright has said which machine
/// they mean. A name pointing there has not, because what it points at is not
/// theirs to decide, so it stays refused even with the flag on.
#[tokio::test]
async fn allow_internal_admits_a_literal_address_never_a_name() {
    assert_eq!(
        public_addresses("10.0.0.5", true, None).await,
        Ok(vec!["10.0.0.5".to_owned()])
    );
    assert_eq!(
        public_addresses("127.0.0.1", true, None).await,
        Ok(vec!["127.0.0.1".to_owned()])
    );
    assert_eq!(
        public_addresses("10.0.0.5", false, None).await,
        Err(NoAddress::Internal)
    );

    // A name that resolves internally is refused whether the flag is on or off.
    let mirror = resolver(&["10.0.0.5"]);
    assert_eq!(
        public_addresses("mirror.internal", true, Some(&mirror)).await,
        Err(NoAddress::Internal)
    );
    assert_eq!(
        public_addresses("mirror.internal", false, Some(&mirror)).await,
        Err(NoAddress::Internal)
    );
}

#[tokio::test]
async fn an_allowlisted_internal_address_is_dialled_only_when_allowed_on_purpose() {
    let mut off = Broker::new(
        vec!["127.0.0.1".to_owned()],
        vec![443],
        silent(),
        Vec::new(),
        false,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let off_port = off.listen("127.0.0.1").await.expect("bound");
    assert!(try_connect(off_port, "127.0.0.1:443").await.contains("403"));
    off.close();

    let mut on = Broker::new(
        vec!["127.0.0.1".to_owned()],
        vec![443],
        silent(),
        Vec::new(),
        true,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let on_port = on.listen("127.0.0.1").await.expect("bound");
    // Admitted now, so it gets as far as dialling: nothing listens on 443, so
    // it fails upstream rather than being refused at the gate.
    let reply = try_connect(on_port, "127.0.0.1:443").await;
    assert!(!reply.contains("403"), "{reply}");
    assert!(!reply.contains("407"), "{reply}");
    on.close();
}

/// A provider pointed at this machine is a setting, not a quiet exception.
#[tokio::test]
async fn a_provider_at_an_internal_address_is_refused_unless_allowed_on_purpose() {
    let seen: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let upstream_port = start_upstream(Arc::clone(&seen), "unused").await;

    let mut broker = Broker::new(
        Vec::new(),
        vec![443],
        silent(),
        vec![ProviderRoute {
            prefix: "/provider".to_owned(),
            upstream: format!("http://127.0.0.1:{upstream_port}/v1"),
            nonce: "n".to_owned(),
            credential: "k".to_owned(),
            credential_name: None,
        }],
        false,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");

    let client = reqwest::Client::new();
    let refused = client
        .post(format!("http://127.0.0.1:{port}/provider/chat"))
        .header("authorization", "Bearer n")
        .body("{}")
        .send()
        .await
        .expect("answered");
    assert_eq!(refused.status().as_u16(), 502);
    broker.close();
}

/// The same address has many spellings. Matching the text let the spelling
/// decide: `::ffff:7f00:1` is loopback written in hex, and it was dialled as
/// 127.0.0.1 while reading as public.
#[test]
fn a_v4_address_in_a_v6_coat_is_judged_as_the_v4_it_carries() {
    for address in [
        "::ffff:7f00:1",         // 127.0.0.1 in hex
        "::ffff:127.0.0.1",      // the same, dotted
        "::FFFF:7F00:1",         // the same, upper case
        "0:0:0:0:0:ffff:7f00:1", // the same, unabbreviated
        "::ffff:a9fe:a9fe",      // 169.254.169.254, the metadata address
        "::ffff:0a00:1",         // 10.0.0.1
        "::ffff:c0a8:1",         // 192.168.0.1
        "::ffff:ac10:1",         // 172.16.0.1
        "::7f00:1",              // v4-compatible loopback
        "64:ff9b::7f00:1",       // NAT64 of loopback
    ] {
        assert!(is_private_address(address), "{address} must be refused");
    }

    // A public v4 in a v6 coat is still public, in either spelling.
    assert!(!is_private_address("::ffff:0808:0808"));
    assert!(!is_private_address("::ffff:8.8.8.8"));
}

#[test]
fn v6_forms_that_are_internal_in_their_own_right() {
    for address in [
        "::1",
        "::",
        "fe80::1",
        "febf::1",
        "fc00::1",
        "fd12:3456::1",
        "ff02::1",
    ] {
        assert!(is_private_address(address), "{address} must be refused");
    }
    for address in ["2606:4700:4700::1111", "2001:4860:4860::8888"] {
        assert!(!is_private_address(address), "{address} must be allowed");
    }
    // Something that is not an address at all is not one to dial.
    assert!(is_private_address("::ffff:zzzz:1"));
    assert!(is_private_address("1:2:3:4:5:6:7:8:9"));
}

/// The provider route is what the JSON definitions compile into.
#[allow(dead_code)]
fn _route_shape(route: &ProviderRoute) -> serde_json::Value {
    json!({ "prefix": route.prefix })
}

#[tokio::test]
async fn closing_the_broker_stops_it_accepting() {
    let mut broker = Broker::new(
        vec!["github.com".to_owned()],
        vec![443],
        silent(),
        Vec::new(),
        false,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_ok());

    broker.close();

    // The accept loop learns of the close on the runtime, so the port is
    // allowed a moment to go rather than being read the instant after.
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the broker kept accepting after it was closed");
}

/// A head that never ends is not a request. Handing back what accumulated
/// would serve a client that never finished asking as though it had.
#[tokio::test]
async fn a_head_that_never_ends_is_refused_rather_than_cut() {
    let endless = vec![b'x'; super::server::MAX_HEAD_BYTES + 64];
    let mut reader: &[u8] = &endless;
    assert_eq!(read_request_head(&mut reader).await, None);

    // One that ends just inside the cap is still read whole.
    let mut head = "CONNECT open.example.com:443 HTTP/1.1\r\nX-Pad: ".to_owned();
    head.push_str(&"p".repeat(super::server::MAX_HEAD_BYTES - head.len() - 8));
    head.push_str("\r\n\r\n");
    assert!(head.len() <= super::server::MAX_HEAD_BYTES);
    let bytes = head.clone().into_bytes();
    let mut reader: &[u8] = &bytes;
    assert_eq!(read_request_head(&mut reader).await, Some(head));
}

/// An address that refuses does not end the dial while another is left.
#[tokio::test]
async fn a_dial_moves_past_an_address_that_refuses() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bound");
    let port = listener.local_addr().expect("an address").port();
    let addresses = ["127.0.0.2".to_owned(), "127.0.0.1".to_owned()];
    let deadline = tokio::time::Instant::now() + super::DIAL_BUDGET;
    let stream = dial(&addresses, port, deadline)
        .await
        .expect("the second address");
    assert_eq!(
        stream.peer_addr().expect("a peer").ip().to_string(),
        "127.0.0.1"
    );
    assert!(dial(&addresses[..1], port, deadline).await.is_err());
}

/// A spent budget ends the dial with addresses left, so the broker refuses
/// while the client is still waiting rather than after it gave up.
#[tokio::test]
async fn a_dial_stops_at_its_deadline() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bound");
    let port = listener.local_addr().expect("an address").port();
    let addresses = ["127.0.0.1".to_owned()];
    let error = dial(&addresses, port, tokio::time::Instant::now())
        .await
        .expect_err("no time left to dial");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("0 of 1 addresses tried"));
}

/// A plain HTTP request reaches an allowed origin in origin form, and the
/// answer comes back whole even when the origin ends it by closing.
#[tokio::test]
async fn a_plain_http_request_is_forwarded_only_on_an_allowed_port() {
    let origin = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bound");
    let origin_port = origin.local_addr().expect("an address").port();
    let received = tokio::spawn(async move {
        let (mut conn, _) = origin.accept().await.expect("a connection");
        let head = read_request_head(&mut conn).await.expect("a head");
        conn.write_all(b"HTTP/1.1 200 OK\r\n\r\nhello")
            .await
            .expect("answered");
        head
    });

    let mut broker = Broker::new(
        vec!["127.0.0.1".to_owned()],
        vec![origin_port],
        silent(),
        Vec::new(),
        true,
        PROXY_CREDENTIAL.to_owned(),
        None,
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");

    let mut conn = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the broker");
    conn.write_all(
        format!("GET http://127.0.0.1:{origin_port}/a?b=c HTTP/1.1\r\nHost: 127.0.0.1\r\nProxy-Authorization: {PROXY_CREDENTIAL}\r\n\r\n")
            .as_bytes(),
    )
    .await
    .expect("written");
    let mut answer = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        conn.read_to_string(&mut answer),
    )
    .await
    .expect("the answer ends when the origin closes")
    .expect("read");
    assert_eq!(answer, "HTTP/1.1 200 OK\r\n\r\nhello");
    assert!(
        received
            .await
            .expect("the origin ran")
            .starts_with("GET /a?b=c HTTP/1.1\r\n")
    );

    let mut conn = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the broker");
    conn.write_all(
        format!(
            "GET http://127.0.0.1:1/ HTTP/1.1\r\nProxy-Authorization: {PROXY_CREDENTIAL}\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .expect("written");
    let mut refused = String::new();
    let _ = conn.read_to_string(&mut refused).await;
    assert!(refused.starts_with("HTTP/1.1 403"));
    broker.close();
}

fn provider_route(prefix: &str, upstream: &str) -> ProviderRoute {
    ProviderRoute {
        prefix: prefix.to_owned(),
        upstream: upstream.to_owned(),
        nonce: "session-nonce".to_owned(),
        credential: "operator-key".to_owned(),
        credential_name: None,
    }
}

fn provider_state(routes: Vec<ProviderRoute>) -> super::ProviderState {
    super::ProviderState {
        routes,
        allow: Vec::new(),
        ports: vec![443],
        proxy_authorization: PROXY_CREDENTIAL.to_owned(),
        allow_internal: false,
        log: silent(),
        resolve: None,
        provider_port: 0,
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ledger: None,
        denied: std::sync::Mutex::default(),
    }
}

#[tokio::test]
async fn malformed_provider_routes_are_refused_before_resolution_or_forwarding() {
    let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    let mut state = provider_state(vec![provider_route(
        "/provider/anthropic",
        "https://api.anthropic.com/v1",
    )]);
    state.resolve = Some(Arc::new(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(vec!["1.1.1.1".to_owned()]) })
    }));
    let state = Arc::new(state);
    for (suffix, status) in [
        (".attacker.example/", 404),
        ("@attacker.example/", 404),
        (":8443/", 404),
        ("x/v1/messages", 404),
        ("//attacker.example/", 400),
        ("/../outside", 400),
        ("/%2e%2e/outside", 400),
        ("/%2Fattacker.example/", 400),
        ("/%5cattacker.example/", 400),
    ] {
        let request = http::Request::builder()
            .uri(format!("/provider/anthropic{suffix}"))
            .header("authorization", "Bearer session-nonce")
            .body(axum::body::Body::empty())
            .unwrap();
        let response = super::serve_provider_request(Arc::clone(&state), request).await;
        assert_eq!(response.status().as_u16(), status, "{suffix}");
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

/// Records attempted upstream names and fails resolution before any socket
/// can be opened or pooled, keeping forwarding assertions independent of
/// the host's network and running services.
#[derive(Clone)]
struct RecordingResolve {
    attempted_hosts: Arc<Mutex<Vec<String>>>,
}

impl reqwest::dns::Resolve for RecordingResolve {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        self.attempted_hosts
            .lock()
            .unwrap()
            .push(name.as_str().to_owned());
        Box::pin(async {
            Err(std::io::Error::other("injected upstream resolution failure").into())
        })
    }
}

/// Refusals are logged before either resolver is consulted. The benign
/// control reaches the recording client resolver and fails there, proving
/// that forwarding attempts are observable without making a connection.
#[tokio::test]
async fn hostile_provider_paths_never_reach_any_upstream() {
    let attempted_hosts = Arc::new(Mutex::new(Vec::new()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);

    let mut state = provider_state(vec![provider_route(
        "/provider/anthropic",
        "https://upstream.test/v1",
    )]);
    state.client = reqwest::Client::builder()
        .dns_resolver(Arc::new(RecordingResolve {
            attempted_hosts: Arc::clone(&attempted_hosts),
        }))
        .no_proxy()
        .build()
        .unwrap();
    state.resolve = Some(Arc::new(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(vec!["9.9.9.9".to_owned()]) })
    }));
    let recorded = Arc::clone(&lines);
    state.log = Logger::new(
        LogFields::new(),
        Arc::new(move |_level, line| recorded.lock().unwrap().push(line.to_owned())),
    );
    let state = Arc::new(state);

    let request_for = |suffix: &str| {
        http::Request::builder()
            .method(http::Method::POST)
            .uri(format!("/provider/anthropic{suffix}"))
            .header("authorization", "Bearer session-nonce")
            .body(axum::body::Body::empty())
            .unwrap()
    };

    let answer =
        super::serve_provider_request(Arc::clone(&state), request_for("/v1/messages?beta=true"))
            .await;
    assert_eq!(answer.status().as_u16(), 502);
    assert_eq!(
        *attempted_hosts.lock().unwrap(),
        vec!["upstream.test".to_owned()]
    );
    let judged = lookups.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(judged, 1);

    for (suffix, status) in [
        (".attacker.example/", 404),
        ("@attacker.example/", 404),
        (":8443/", 404),
        ("x/v1/messages", 404),
        ("//attacker.example/", 400),
        ("/../outside", 400),
        ("/%2e%2e/outside", 400),
        ("/%2Fattacker.example/", 400),
        ("/%5cattacker.example/", 400),
    ] {
        let response = super::serve_provider_request(Arc::clone(&state), request_for(suffix)).await;
        assert_eq!(response.status().as_u16(), status, "{suffix}");
    }

    assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), judged);
    assert_eq!(
        *attempted_hosts.lock().unwrap(),
        vec!["upstream.test".to_owned()],
        "a forward was attempted"
    );

    // The refusals are on the log: one line per unmatched route, one per
    // path that cannot be a path, naming the probe's path.
    let lines = lines.lock().unwrap();
    let unserved = lines
        .iter()
        .filter(|line| line.contains("does not serve"))
        .count();
    assert_eq!(unserved, 4, "unmatched routes must be logged");
    let malformed = lines
        .iter()
        .filter(|line| line.contains("cannot be a path"))
        .count();
    assert_eq!(malformed, 5, "malformed paths must be logged");
    assert!(
        lines.iter().any(|line| line.contains("attacker.example")),
        "the probe's path must be on the log"
    );
}

/// A broken upstream is the operator's failure: answered 502 rather than a
/// 400 that blames the session, and refused before resolution is consulted.
#[tokio::test]
async fn a_broken_upstream_is_a_502_refused_before_resolution() {
    let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    let mut state = provider_state(vec![provider_route(
        "/provider/anthropic",
        "https://user:pass@api.anthropic.com/v1",
    )]);
    state.resolve = Some(Arc::new(move |_| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(vec!["1.1.1.1".to_owned()]) })
    }));
    let state = Arc::new(state);
    let request = http::Request::builder()
        .uri("/provider/anthropic/v1/messages")
        .header("authorization", "Bearer session-nonce")
        .body(axum::body::Body::empty())
        .unwrap();
    let response = super::serve_provider_request(state, request).await;
    assert_eq!(response.status().as_u16(), 502);
    assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn provider_names_and_longest_segment_prefix_are_preserved() {
    let routes = ["a", "a/nested", "a-b", "a.b", "a2"]
        .map(|name| provider_route(&format!("/provider/{name}"), "https://provider.example"));
    let state = provider_state(routes.to_vec());
    for route in routes {
        for suffix in ["", "/", "/v1/messages"] {
            let path = format!("{}{suffix}", route.prefix);
            assert_eq!(
                super::route_for(&state, &path).unwrap().prefix,
                route.prefix
            );
        }
    }
}

#[test]
fn provider_targets_keep_the_configured_origin_and_base_path() {
    for upstream in [
        "https://api.example.com",
        "https://api.example.com:8443/v1/",
        "http://127.0.0.1:8000/api",
        "https://[2001:4860:4860::8888]:8443/v1",
    ] {
        let route = provider_route("/provider/test", upstream);
        let base = reqwest::Url::parse(upstream).unwrap();
        for rest in ["", "/", "/messages", "/x/../messages", "/user@example:8443"] {
            let target =
                super::provider_target(&route, rest, Some("q=//other.example/%2f")).unwrap();
            let request = super::build_forwarded(
                &provider_state(vec![route.clone()]),
                http::Request::new(axum::body::Body::empty()),
                &route,
                &target,
                &Arc::default(),
            )
            .build()
            .unwrap();
            assert_eq!(request.url().host(), base.host());
            assert_eq!(
                request.url().port_or_known_default(),
                base.port_or_known_default()
            );
            assert_eq!(request.url().scheme(), base.scheme());
            assert!(super::under_prefix(
                request.url().path(),
                base.path().trim_end_matches('/')
            ));
            assert_eq!(request.url().query(), Some("q=//other.example/%2f"));
        }
    }
}

#[test]
fn ambiguous_provider_paths_and_userinfo_are_refused() {
    let route = provider_route("/provider/test", "https://api.example.com/v1");
    for rest in [
        ".other.example/",
        "@other.example/",
        ":8443/",
        "//other.example/",
        "/\\other.example/",
        "/%2fother.example/",
        "/%5Cother.example/",
        "/../outside",
        "/%2e%2e/outside",
        "/.%2E/outside",
        "/%2e./outside",
        "/../v10/messages",
    ] {
        assert!(
            matches!(
                super::provider_target(&route, rest, None),
                Err(super::TargetError::Malformed(_))
            ),
            "{rest}"
        );
    }
    for upstream in [
        "https://user@api.example.com",
        "https://user:pass@api.example.com",
        "file:///v1",
        // A query or fragment on the upstream is a misconfiguration to
        // refuse, not one to normalise away.
        "https://api.example.com/v1?x=1",
        "https://api.example.com/v1#frag",
    ] {
        assert!(
            matches!(
                super::provider_target(
                    &provider_route("/provider/test", upstream),
                    "/messages",
                    None
                ),
                Err(super::TargetError::Upstream(_))
            ),
            "{upstream}"
        );
    }
}

#[test]
fn provider_forwarding_preserves_authority_path_query_and_replaces_credentials() {
    let state = provider_state(vec![provider_route(
        "/provider/anthropic",
        "https://api.example.com:8443/api",
    )]);
    let request = http::Request::builder()
        .method("POST")
        .uri("/provider/anthropic/v1/messages?beta=true")
        .header("host", "broker.example:9000")
        .header("authorization", "Bearer session-nonce")
        .header("x-api-key", "session-nonce")
        .body(axum::body::Body::from("{}"))
        .unwrap();
    let route = super::route_for(&state, request.uri().path()).unwrap();
    let rest = request.uri().path().strip_prefix(&route.prefix).unwrap();
    let target = super::provider_target(route, rest, request.uri().query()).unwrap();
    let forwarded = super::build_forwarded(&state, request, route, &target, &Arc::default())
        .build()
        .unwrap();

    assert_eq!(forwarded.method(), http::Method::POST);
    assert_eq!(forwarded.url().scheme(), "https");
    assert_eq!(forwarded.url().host_str(), Some("api.example.com"));
    assert_eq!(forwarded.url().port(), Some(8443));
    assert_eq!(forwarded.url().path(), "/api/v1/messages");
    assert_eq!(forwarded.url().query(), Some("beta=true"));
    assert_eq!(forwarded.headers()["authorization"], "Bearer operator-key");
    assert_eq!(forwarded.headers()["x-api-key"], "operator-key");
    assert!(!forwarded.headers().contains_key("host"));
}

/// The upstream is asked for an answer it has not compressed, whatever the
/// session asked for, so the broker can scrub what comes back.
#[test]
fn the_sessions_accept_encoding_is_replaced_with_identity() {
    let state = provider_state(vec![provider_route(
        "/provider/zai",
        "https://api.example.com/v4",
    )]);
    let request = http::Request::builder()
        .uri("/provider/zai/chat")
        .header("authorization", "Bearer session-nonce")
        .header("accept-encoding", "gzip, br")
        .body(axum::body::Body::empty())
        .unwrap();
    let route = super::route_for(&state, request.uri().path()).unwrap();
    let target = super::provider_target(route, "/chat", None).unwrap();
    let forwarded = super::build_forwarded(&state, request, route, &target, &Arc::default())
        .build()
        .unwrap();

    let encodings: Vec<_> = forwarded
        .headers()
        .get_all("accept-encoding")
        .iter()
        .collect();
    assert_eq!(encodings, ["identity"]);
}

const PROXY_CREDENTIAL: &str = "Basic c2Vzc2lvbjpzZWNyZXQ=";

struct ProxyExchange {
    answer: String,
    lookups: usize,
    dials: usize,
    forwarded: String,
    logged: Vec<String>,
}

async fn proxy_exchange(head: &str) -> ProxyExchange {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let lookups = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    let mut state = provider_state(Vec::new());
    state.allow = vec!["upstream.example".to_owned()];
    state.ports = vec![80, 443];
    state.resolve = Some(Arc::new(move |host| {
        assert_eq!(host, "upstream.example");
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(vec!["1.1.1.1".to_owned()]) })
    }));
    let logged = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&logged);
    state.log = Logger::new(
        LogFields::new(),
        Arc::new(move |_level, line| recorded.lock().unwrap().push(line.to_owned())),
    );
    let (mut client, stream) = tokio::io::duplex(16_384);
    let (upstream, mut origin) = tokio::io::duplex(16_384);
    let dials = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&dials);
    let handle = super::server::handle(stream, Arc::new(state), move |addresses, port, _| {
        assert_eq!(addresses, ["1.1.1.1"]);
        assert!([80, 443].contains(&port));
        counted.fetch_add(1, Ordering::SeqCst);
        async move { Ok(upstream) }
    });
    let exchange = async {
        client.write_all(head.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).await.unwrap();
        answer
    };
    let origin = async {
        let mut forwarded = String::new();
        origin.read_to_string(&mut forwarded).await.unwrap();
        if !forwarded.is_empty() {
            origin
                .write_all(b"HTTP/1.1 200 OK\r\n\r\nupstream answer")
                .await
                .unwrap();
            origin.shutdown().await.unwrap();
        }
        forwarded
    };
    let ((), answer, forwarded) = tokio::join!(handle, exchange, origin);
    ProxyExchange {
        answer,
        lookups: lookups.load(Ordering::SeqCst),
        dials: dials.load(Ordering::SeqCst),
        forwarded,
        logged: logged.lock().unwrap().clone(),
    }
}

/// The refusal is logged naming the target, so an operator can tell which
/// request a tool sent without the credential.
fn assert_refusal_names(exchange: &ProxyExchange, port: &str) {
    assert!(
        exchange
            .logged
            .iter()
            .any(|line| line.contains("proxy authentication refused")
                && line.contains("host=upstream.example")
                && line.contains(port)),
        "{:?}",
        exchange.logged
    );
}

#[tokio::test]
async fn a_connect_without_the_credential_is_refused() {
    let exchange = proxy_exchange("CONNECT upstream.example:443 HTTP/1.1\r\n\r\nping").await;
    assert_refusal_names(&exchange, "port=443");
    assert!(
        exchange
            .answer
            .starts_with("HTTP/1.1 407 Proxy Authentication Required\r\n"),
        "{}",
        exchange.answer
    );
    assert!(
        exchange
            .answer
            .contains("Proxy-Authenticate: Basic realm=\"errand\"\r\n")
    );
    // git's first CONNECT never carries the credential, and it reconnects
    // with one only when the 407 says the connection is closing.
    assert!(
        exchange
            .answer
            .ends_with("Connection: close\r\nContent-Length: 0\r\n\r\n"),
        "{}",
        exchange.answer
    );
    assert_eq!((exchange.lookups, exchange.dials), (0, 0));
    assert!(exchange.forwarded.is_empty());
}

#[tokio::test]
async fn a_connect_with_the_credential_is_admitted() {
    let exchange = proxy_exchange(&format!(
        "CONNECT upstream.example:443 HTTP/1.1\r\nProxy-Authorization: {PROXY_CREDENTIAL}\r\n\r\nping"
    ))
    .await;
    assert!(
        exchange
            .answer
            .starts_with("HTTP/1.1 200 Connection Established\r\n\r\n")
    );
    assert!(exchange.answer.ends_with("upstream answer"));
    assert_eq!((exchange.lookups, exchange.dials), (1, 1));
    assert_eq!(exchange.forwarded, "ping");
}

#[tokio::test]
async fn a_connect_with_the_wrong_credential_is_refused() {
    let exchange = proxy_exchange(
        "CONNECT upstream.example:443 HTTP/1.1\r\nProxy-Authorization: Basic wrong\r\n\r\nping",
    )
    .await;
    assert!(exchange.answer.starts_with("HTTP/1.1 407"));
    assert_eq!((exchange.lookups, exchange.dials), (0, 0));
    assert!(exchange.forwarded.is_empty());
}

#[tokio::test]
async fn the_credential_header_is_read_leniently_and_strictly() {
    let token = PROXY_CREDENTIAL.strip_prefix("Basic ").unwrap();
    for scheme in ["basic", "BASIC", "bAsIc"] {
        let exchange = proxy_exchange(&format!(
            "CONNECT upstream.example:443 HTTP/1.1\r\nproxy-authorization: {scheme}\t{token}  \r\n\r\nping"
        ))
        .await;
        assert!(exchange.answer.starts_with("HTTP/1.1 200"), "{scheme}");
        assert_eq!((exchange.lookups, exchange.dials), (1, 1));
    }
    for headers in [
        format!("Proxy-Authorization: {PROXY_CREDENTIAL}\r\nProxy-Authorization: Basic wrong"),
        format!("Proxy-Authorization: Basic wrong\r\nProxy-Authorization: {PROXY_CREDENTIAL}"),
        format!(
            "Proxy-Authorization: {PROXY_CREDENTIAL}\r\nproxy-authorization: {PROXY_CREDENTIAL}"
        ),
        "Proxy-Authorization: Basic".to_owned(),
        "Proxy-Authorization: Basic ".to_owned(),
        format!("Proxy-Authorization: Bearer {token}"),
    ] {
        let exchange = proxy_exchange(&format!(
            "CONNECT upstream.example:443 HTTP/1.1\r\n{headers}\r\n\r\nping"
        ))
        .await;
        assert!(exchange.answer.starts_with("HTTP/1.1 407"), "{headers}");
        assert_eq!((exchange.lookups, exchange.dials), (0, 0));
        assert!(exchange.forwarded.is_empty());
    }
}

#[tokio::test]
async fn a_forward_without_the_credential_is_refused() {
    let exchange = proxy_exchange(
        "POST http://upstream.example/upload HTTP/1.1\r\nContent-Length: 4\r\n\r\nping",
    )
    .await;
    assert_refusal_names(&exchange, "port=80");
    assert!(exchange.answer.starts_with("HTTP/1.1 407"));
    assert!(
        exchange
            .answer
            .contains("Proxy-Authenticate: Basic realm=\"errand\"\r\n")
    );
    assert_eq!((exchange.lookups, exchange.dials), (0, 0));
    assert!(exchange.forwarded.is_empty());
}

#[tokio::test]
async fn an_authenticated_forward_does_not_relay_the_proxy_credential() {
    let exchange = proxy_exchange(&format!(
        "POST http://upstream.example/upload HTTP/1.1\r\nHost: upstream.example\r\nProxy-Authorization: {PROXY_CREDENTIAL}\r\nContent-Length: 4\r\n\r\nping"
    ))
    .await;
    assert!(exchange.answer.starts_with("HTTP/1.1 200 OK"));
    assert_eq!((exchange.lookups, exchange.dials), (1, 1));
    assert_eq!(
        exchange.forwarded,
        "POST /upload HTTP/1.1\r\nHost: upstream.example\r\nContent-Length: 4\r\nConnection: close\r\n\r\nping"
    );
    assert!(!exchange.forwarded.contains(PROXY_CREDENTIAL));
}

#[tokio::test]
async fn a_forward_with_invalid_credentials_is_refused_before_resolution() {
    for (headers, body) in [
        ("Proxy-Authorization: Basic wrong".to_owned(), "ping"),
        ("Proxy-Authorization: Basic".to_owned(), "ping"),
        (
            format!("Proxy-Authorization: Bearer {}", &PROXY_CREDENTIAL[6..]),
            "ping",
        ),
        (
            format!("Proxy-Authorization: {PROXY_CREDENTIAL}\r\nProxy-Authorization: Basic wrong"),
            "ping",
        ),
        (
            "Content-Type: text/plain".to_owned(),
            "Proxy-Authorization: Basic c2Vzc2lvbjpzZWNyZXQ=\r\n",
        ),
    ] {
        let exchange = proxy_exchange(&format!(
            "POST http://upstream.example/upload HTTP/1.1\r\n{headers}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ))
        .await;
        assert!(exchange.answer.starts_with("HTTP/1.1 407"), "{headers}");
        assert_eq!((exchange.lookups, exchange.dials), (0, 0));
        assert!(exchange.forwarded.is_empty());
    }
}

/// A template over one provider at a stand-in upstream on loopback.
fn template_for(upstream_port: u16, log: Logger) -> BrokerTemplate {
    BrokerTemplate {
        allow: vec!["api.example.com".to_owned()],
        ports: vec![443],
        allow_internal: true,
        log,
        providers: vec![ProviderUpstream {
            name: "zai".to_owned(),
            prefix: "/provider/zai".to_owned(),
            upstream: format!("http://127.0.0.1:{upstream_port}/v4"),
            credential: "the-real-key".to_owned(),
        }],
        credential_names: BTreeMap::new(),
        receipts: None,
        now: Arc::new(|| 0),
    }
}

/// What a provider call with `nonce` on a session broker's `port` answers.
async fn call_provider(port: u16, nonce: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/provider/zai/chat"))
        .header("authorization", format!("Bearer {nonce}"))
        .body("{}")
        .send()
        .await
        .expect("answered")
        .status()
        .as_u16()
}

/// Whether nothing accepts on `port` any more, allowing the accept loop a
/// moment to learn of a close on the runtime.
async fn stops_listening(port: u16) -> bool {
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).await.is_err() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    false
}

/// Two sessions started from the same template share no listener, no proxy
/// credential, and no nonce, so what is read out of one is refused by the
/// other.
#[tokio::test]
async fn sessions_share_no_listener_and_no_nonce() {
    let seen: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let upstream_port = start_upstream(Arc::clone(&seen), "one").await;
    let template = template_for(upstream_port, silent());

    let a = template.start("s-a").await.expect("a broker for a");
    let b = template.start("s-b").await.expect("a broker for b");

    assert_ne!(a.port, b.port);
    assert_ne!(a.proxy_secret, b.proxy_secret);
    assert_ne!(a.nonces["zai"], b.nonces["zai"]);

    assert_eq!(call_provider(a.port, &a.nonces["zai"]).await, 200);
    assert_eq!(call_provider(b.port, &b.nonces["zai"]).await, 200);
    assert_eq!(call_provider(b.port, &a.nonces["zai"]).await, 401);
    assert_eq!(call_provider(a.port, &b.nonces["zai"]).await, 401);
}

/// A session broker that is dropped, as one is when a launch fails part way
/// or a handle is let go without a stop, stops listening all the same.
#[tokio::test]
async fn a_dropped_session_broker_stops_listening() {
    let template = template_for(9, silent());
    let started = template.start("s-1").await.expect("a broker");
    let port = started.port;
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_ok());

    drop(started);

    assert!(
        stops_listening(port).await,
        "a dropped broker kept accepting"
    );
}

/// The broker's own log lines name the session whose listener the request
/// arrived on, a tunnel included, though a tunnel carries no nonce.
#[tokio::test]
async fn what_a_session_broker_logs_names_the_session() {
    let logged = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorded = Arc::clone(&logged);
    let log = Logger::new(
        LogFields::new(),
        Arc::new(move |_level, line| recorded.lock().unwrap().push(line.to_owned())),
    );
    let template = template_for(9, log);
    let started = template.start("s-7").await.expect("a broker");

    let mut conn = TcpStream::connect(("127.0.0.1", started.port))
        .await
        .expect("the broker");
    conn.write_all(
        format!(
            "CONNECT elsewhere.example:443 HTTP/1.1\r\nProxy-Authorization: {}\r\n\r\n",
            proxy_authorization(&started.proxy_secret)
        )
        .as_bytes(),
    )
    .await
    .expect("written");
    let mut refused = String::new();
    let _ = conn.read_to_string(&mut refused).await;
    assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");

    assert_eq!(call_provider(started.port, "not-the-nonce").await, 401);

    let logged = logged.lock().unwrap();
    for message in ["egress refused", "without this session's key"] {
        assert!(
            logged
                .iter()
                .any(|line| line.contains(message) && line.contains("session=s-7")),
            "{message}: {logged:?}"
        );
    }
}

/// A connection the session opened before it was stopped is still its to
/// write on, so a request arriving on it after the close is refused rather
/// than admitted with the credential it carries.
#[tokio::test]
async fn a_request_after_the_close_is_refused_on_an_earlier_connection() {
    let exchange = |closed: bool| async move {
        let mut state = provider_state(Vec::new());
        state.allow = vec!["upstream.example".to_owned()];
        state.resolve = Some(resolver(&["1.1.1.1"]));
        state
            .closed
            .store(closed, std::sync::atomic::Ordering::SeqCst);
        let (mut client, stream) = tokio::io::duplex(4096);
        let (upstream, origin) = tokio::io::duplex(4096);
        let dialled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let marked = Arc::clone(&dialled);
        let handle = super::server::handle(stream, Arc::new(state), move |_, _, _| {
            marked.store(true, std::sync::atomic::Ordering::SeqCst);
            async move { Ok(upstream) }
        });
        let ask = async {
            client
                .write_all(
                    format!(
                        "CONNECT upstream.example:443 HTTP/1.1\r\nProxy-Authorization: \
                         {PROXY_CREDENTIAL}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut buf = vec![0_u8; 128];
            let read = client.read(&mut buf).await.unwrap_or(0);
            // Both ends hung up, so an admitted tunnel has nothing to copy.
            drop(client);
            drop(origin);
            String::from_utf8_lossy(&buf[..read]).into_owned()
        };
        let ((), answer) = tokio::join!(handle, ask);
        (answer, dialled.load(std::sync::atomic::Ordering::SeqCst))
    };

    let (open, dialled) = exchange(false).await;
    assert!(open.starts_with("HTTP/1.1 200"), "{open}");
    assert!(dialled);

    let (closed, dialled) = exchange(true).await;
    assert!(closed.starts_with("HTTP/1.1 503"), "{closed}");
    assert!(!dialled, "a stopped session's request was dialled");
}

/// The provider endpoint refuses a request after the close too, as a
/// connection kept alive to it would otherwise carry one.
#[tokio::test]
async fn a_provider_call_after_the_close_is_refused() {
    let state = provider_state(vec![provider_route(
        "/provider/zai",
        "https://api.example.com/v4",
    )]);
    state
        .closed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let request = http::Request::builder()
        .uri("/provider/zai/chat")
        .header("authorization", "Bearer session-nonce")
        .body(axum::body::Body::empty())
        .unwrap();
    let answered = super::serve_provider_request(Arc::new(state), request).await;
    assert_eq!(answered.status().as_u16(), 503);
}

/// A session stopped while a tunnel's dial was still running gets no tunnel:
/// the close is weighed again once the dial returns, before the tunnel is
/// confirmed.
#[tokio::test]
async fn a_session_stopped_during_a_dial_gets_no_tunnel() {
    let mut state = provider_state(Vec::new());
    state.allow = vec!["*".to_owned()];
    state.allow_internal = true;
    let closed = Arc::clone(&state.closed);
    let (mut client, stream) = tokio::io::duplex(4096);
    let (upstream, _far) = tokio::io::duplex(64);
    let handle = super::server::handle(stream, Arc::new(state), move |_, _, _| {
        closed.store(true, std::sync::atomic::Ordering::SeqCst);
        async move { Ok(upstream) }
    });
    let session = async {
        client
            .write_all(
                format!(
                    "CONNECT 127.0.0.1:443 HTTP/1.1\r\nProxy-Authorization: {PROXY_CREDENTIAL}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).await.unwrap();
        answer
    };

    let (answer, ()) = tokio::join!(session, handle);

    assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
    assert!(!answer.contains("200 Connection Established"), "{answer}");
}

/// What a session is handed reads as errand's, not as a provider key, and
/// is exactly the shape every redacted surface scrubs.
#[tokio::test]
async fn what_a_session_is_handed_is_prefixed_and_scrubbed() {
    let started = template_for(9, silent())
        .start("s-1")
        .await
        .expect("a broker");
    for secret in [&started.proxy_secret, &started.nonces["zai"]] {
        let digits = secret
            .strip_prefix(crate::config::redact::MINTED_PREFIX)
            .expect("the prefix");
        assert_eq!(digits.len(), 64, "{secret}");
        assert!(digits.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            crate::config::redact::redact_text(secret, &[]),
            crate::config::redact::REDACTION
        );
    }
}

/// A provider that quotes the key it was sent, as an error page might, hands
/// the session none of it: the answer is scrubbed on the way back through
/// the broker.
#[tokio::test]
async fn an_upstream_that_echoes_the_key_hands_the_session_none_of_it() {
    let app = axum::Router::new().fallback(|headers: axum::http::HeaderMap| async move {
        let sent = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        (
            axum::http::StatusCode::BAD_REQUEST,
            [("x-echo", sent.clone())],
            format!("{{\"error\":\"rejected {sent}\"}}"),
        )
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bound");
    let upstream_port = listener.local_addr().expect("an address").port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serves");
    });
    let started = template_for(upstream_port, silent())
        .start("s-1")
        .await
        .expect("a broker");

    let answered = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/provider/zai/chat",
            started.port
        ))
        .header("authorization", format!("Bearer {}", started.nonces["zai"]))
        .body("{}")
        .send()
        .await
        .expect("answered");

    assert_eq!(answered.status().as_u16(), 400);
    assert_eq!(answered.headers()["x-echo"], "Bearer [redacted]");
    let body = answered.text().await.expect("a body");
    assert_eq!(body, "{\"error\":\"rejected Bearer [redacted]\"}");
}

/// An upstream that records how many requests reached it, and answers with
/// `status` and, when given, a `Location`.
async fn counting_upstream(status: u16, location: Option<String>) -> (u16, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    let app = axum::Router::new().fallback(move || {
        let counted = Arc::clone(&counted);
        let location = location.clone();
        async move {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut answer = axum::http::Response::builder().status(status);
            if let Some(location) = location {
                answer = answer.header("location", location);
            }
            answer.body(axum::body::Body::empty()).unwrap()
        }
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bound");
    let port = listener.local_addr().expect("an address").port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serves");
    });
    (port, hits)
}

/// A provider's redirect reaches the session as sent, and the host it names
/// is never dialled with the key.
#[tokio::test]
async fn a_redirect_is_not_followed_and_the_second_host_sees_nothing() {
    let (second_port, second) = counting_upstream(200, None).await;
    let (first_port, first) =
        counting_upstream(302, Some(format!("http://127.0.0.1:{second_port}/stolen"))).await;
    let started = template_for(first_port, silent())
        .start("s-1")
        .await
        .expect("a broker");

    // The session's own client is told not to follow either, so what is
    // seen is what the broker relayed.
    let answered = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
        .post(format!(
            "http://127.0.0.1:{}/provider/zai/chat",
            started.port
        ))
        .header("authorization", format!("Bearer {}", started.nonces["zai"]))
        .send()
        .await
        .expect("answered");
    assert_eq!(answered.status().as_u16(), 302);
    assert_eq!(first.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(second.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// The client's resolver answers only addresses that pass the judgment, and
/// never admits a name on an internal answer.
#[tokio::test]
async fn the_judged_resolver_answers_only_public_addresses() {
    use reqwest::dns::Resolve as _;

    let lookup = |answers: &[&str]| {
        let resolver = super::JudgedResolver {
            resolve: Some(resolver(answers)),
        };
        resolver.resolve(reqwest::dns::Name::from_str("provider.test").unwrap())
    };

    let addresses: Vec<_> = lookup(&["10.0.0.1", "9.9.9.9"])
        .await
        .expect("a public address")
        .collect();
    assert_eq!(addresses, ["9.9.9.9:0".parse().unwrap()]);
    assert!(lookup(&["127.0.0.1"]).await.is_err());
}

/// The client the broker calls providers with judges the address it dials,
/// so a name that answers internal when the client resolves it, whatever it
/// answered before, finds nothing to connect to.
#[tokio::test]
async fn a_name_that_answers_internal_at_dial_time_is_not_dialled() {
    let (port, hits) = counting_upstream(200, None).await;
    let client = super::server::provider_client(Some(resolver(&["127.0.0.1"]))).unwrap();

    let sent = client
        .get(format!("http://rebind.test:{port}/"))
        .send()
        .await;

    let error = sent.expect_err("dialled");
    let mut chain = Some(&error as &dyn std::error::Error);
    let mut reasons = Vec::new();
    while let Some(link) = chain {
        reasons.push(link.to_string());
        chain = link.source();
    }
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("host-internal")),
        "{reasons:?}"
    );
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// The broker dials HTTPS, or plain HTTP only to this host's own loopback,
/// and says what is wrong with a base URL it will not dial.
#[test]
fn a_provider_upstream_is_https_or_loopback() {
    for good in [
        "https://api.example.com/v4",
        " https://api.example.com/v4 ",
        "http://127.0.0.1:8080/v1",
        "http://[::1]:8080/v1",
        "http://[::ffff:127.0.0.1]:8080/v1",
    ] {
        assert!(super::upstream_url(good).is_ok(), "{good}");
    }
    for (bad, why) in [
        ("http://api.example.com/v4", "not HTTPS"),
        ("http://localhost:8080/v1", "not HTTPS"),
        ("api.example.com/v4", "no scheme"),
        ("ftp://api.example.com/v4", "not http or https"),
        ("https://user:pass@api.example.com/v4", "credentials"),
        ("https://api.example.com/v4?key=1", "query"),
    ] {
        let refused = super::upstream_url(bad).expect_err(bad);
        assert!(refused.contains(why), "{bad}: {refused}");
    }
}

/// A call to a provider at a plain HTTP or schemeless base URL is refused
/// before any lookup, with the reason in the answer.
#[tokio::test]
async fn a_provider_call_to_an_unusable_base_url_names_the_reason() {
    for (upstream, why) in [
        ("http://api.example.test/v1", "not HTTPS"),
        ("api.example.test/v1", "no scheme"),
    ] {
        let mut state = provider_state(vec![provider_route("/provider/zai", upstream)]);
        state.resolve = Some(Arc::new(|_| panic!("nothing is resolved")));
        let request = http::Request::builder()
            .uri("/provider/zai/chat")
            .header("authorization", "Bearer session-nonce")
            .body(axum::body::Body::empty())
            .unwrap();

        let answered = super::serve_provider_request(Arc::new(state), request).await;

        assert_eq!(answered.status().as_u16(), 502);
        let body = axum::body::to_bytes(answered.into_body(), 4096)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains(why), "{upstream}");
    }
}

/// A session broker over `template_for`, with a receipt store in `root`.
fn recorded_template(upstream_port: u16, root: &std::path::Path) -> BrokerTemplate {
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(1_000));
    let mut template = template_for(upstream_port, silent());
    template.receipts = Some(Arc::new(
        super::ReceiptStore::open(root.join(super::RECEIPTS_FILENAME)).unwrap(),
    ));
    template.now = Arc::new(move || clock.fetch_add(10, std::sync::atomic::Ordering::SeqCst));
    template.credential_names = BTreeMap::from([("zai".to_owned(), "ZAI_KEY".to_owned())]);
    template
}

/// The receipts a session's broker wrote, newest first.
fn receipts_of(root: &std::path::Path, session: &str) -> Vec<super::receipts::Receipt> {
    super::receipts::receipts_for(&root.join(super::RECEIPTS_FILENAME), session, 20).unwrap()
}

/// Every provider call a session's broker handles leaves one receipt naming
/// the session, the provider and the credential's name, and what came of it,
/// but never the credential, the stand-in, or the query.
#[tokio::test]
async fn each_provider_call_leaves_one_receipt() {
    let root = tempfile::tempdir().unwrap();
    let seen: Arc<Mutex<BTreeMap<String, String>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let upstream_port = start_upstream(Arc::clone(&seen), "one").await;
    let started = recorded_template(upstream_port, root.path())
        .start("s-1")
        .await
        .expect("a broker");
    let nonce = started.nonces["zai"].clone();

    let answered = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/provider/zai/chat?key=SECRET",
            started.port
        ))
        .header("authorization", format!("Bearer {nonce}"))
        .body("{\"q\":1}")
        .send()
        .await
        .expect("answered");
    assert_eq!(answered.status().as_u16(), 200);
    assert_eq!(answered.text().await.unwrap(), "{}");
    assert_eq!(call_provider(started.port, "not-the-nonce").await, 401);

    // The receipt is written once the answer has been relayed, so give the
    // stream's end a moment on the runtime.
    let mut receipts = Vec::new();
    for _ in 0..100 {
        receipts = receipts_of(root.path(), "s-1");
        if receipts.len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(receipts.len(), 2, "{receipts:?}");
    let denied = &receipts[0];
    assert_eq!(denied.outcome, super::receipts::Outcome::Denied);
    assert_eq!(denied.status, Some(401));
    let completed = &receipts[1];
    assert_eq!(completed.outcome, super::receipts::Outcome::Completed);
    assert_eq!(completed.provider, "/provider/zai");
    assert_eq!(completed.credential_name.as_deref(), Some("ZAI_KEY"));
    assert_eq!(completed.upstream_host.as_deref(), Some("127.0.0.1"));
    assert_eq!(completed.method, "POST");
    assert_eq!(completed.path, "/chat");
    assert_eq!(completed.query_bytes, Some(10));
    assert_eq!(completed.status, Some(200));
    assert_eq!(completed.bytes_out, 7);
    assert_eq!(completed.bytes_in, 2);
    assert!(completed.elapsed_ms > 0);

    let everything = format!("{receipts:?}");
    for secret in ["SECRET", "the-real-key", nonce.as_str()] {
        assert!(!everything.contains(secret), "{secret} was recorded");
    }
}

/// A call the broker would not forward, and one whose provider could not be
/// reached, are recorded too. A tunnel carries no brokered credential and is
/// not.
#[tokio::test]
async fn refused_and_unreachable_calls_are_recorded_and_tunnels_are_not() {
    let root = tempfile::tempdir().unwrap();
    let mut template = recorded_template(9, root.path());
    template.providers.push(ProviderUpstream {
        name: "plain".to_owned(),
        prefix: "/provider/plain".to_owned(),
        upstream: "http://api.example.test/v1".to_owned(),
        credential: "key".to_owned(),
    });
    let started = template.start("s-2").await.expect("a broker");

    assert_eq!(
        call_provider(started.port, &started.nonces["zai"]).await,
        502
    );
    let refused = reqwest::Client::new()
        .post(format!(
            "http://127.0.0.1:{}/provider/plain/chat",
            started.port
        ))
        .header(
            "authorization",
            format!("Bearer {}", started.nonces["plain"]),
        )
        .send()
        .await
        .expect("answered");
    assert_eq!(refused.status().as_u16(), 502);
    let mut tunnel = TcpStream::connect(("127.0.0.1", started.port))
        .await
        .unwrap();
    tunnel
        .write_all(
            format!(
                "CONNECT elsewhere.example:443 HTTP/1.1\r\nProxy-Authorization: {}\r\n\r\n",
                proxy_authorization(&started.proxy_secret)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = String::new();
    let _ = tunnel.read_to_string(&mut answer).await;
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");

    let outcomes: Vec<_> = receipts_of(root.path(), "s-2")
        .into_iter()
        .map(|receipt| (receipt.provider, receipt.outcome, receipt.status))
        .collect();
    assert_eq!(
        outcomes,
        [
            (
                "/provider/plain".to_owned(),
                super::receipts::Outcome::Refused,
                Some(502)
            ),
            (
                "/provider/zai".to_owned(),
                super::receipts::Outcome::UpstreamUnreachable,
                Some(502)
            ),
        ]
    );
}

/// A receipt keeps the path a session chose without any stand-in in it, and
/// no longer than a record needs.
#[test]
fn a_recorded_path_is_scrubbed_and_cut() {
    let minted = format!("{}{}", crate::config::redact::MINTED_PREFIX, "a".repeat(64));
    assert_eq!(
        super::recorded_path(&format!("/chat/{minted}")),
        "/chat/[redacted]"
    );
    let long = format!("/{}", "x".repeat(1000));
    let kept = super::recorded_path(&long);
    assert_eq!(kept.len(), 256 + 3);
    assert!(kept.ends_with("..."));
}

/// A method is kept only as a standard name, and a path is decoded before it
/// is scrubbed, so neither can carry a stand-in into a receipt.
#[test]
fn a_recorded_method_and_encoded_path_carry_no_stand_in() {
    let minted = format!("{}{}", crate::config::redact::MINTED_PREFIX, "a".repeat(64));
    assert_eq!(super::recorded_method(&http::Method::POST), "POST");
    let odd = http::Method::from_bytes(minted.as_bytes()).unwrap();
    assert_eq!(super::recorded_method(&odd), "OTHER");
    let encoded = format!("/chat/{}", minted.replacen('-', "%2D", 1));
    assert_eq!(super::recorded_path(&encoded), "/chat/[redacted]");
}

/// A path cannot drive the terminal a receipt is printed in, or carry a
/// query encoded into it past the rule that a query is kept by length.
#[test]
fn a_recorded_path_holds_no_control_character_or_smuggled_query() {
    let painted = super::recorded_path("/chat/%1b%5b2J%0aforged line");
    assert!(!painted.chars().any(char::is_control), "{painted:?}");
    assert_eq!(painted, "/chat/\\u{1b}[2J\\u{a}forged line");
    assert_eq!(super::recorded_path("/chat%3Fkey=hunter2"), "/chat");
}

/// A session that keeps calling without its key is one denied receipt a
/// minute, not one an attempt, and the next one recorded says how many went
/// unrecorded in between.
#[tokio::test]
async fn repeated_calls_without_the_key_are_recorded_once_a_minute() {
    use super::receipts::{Ledger, ReceiptStore, receipts_for};

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipts.db");
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(1_000));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let mut state = provider_state(vec![provider_route(
        "/provider",
        "https://upstream.test/v1",
    )]);
    state.ledger = Some(Ledger {
        store: Arc::new(ReceiptStore::open(&path).unwrap()),
        now: {
            let clock = Arc::clone(&clock);
            Arc::new(move || clock.load(std::sync::atomic::Ordering::SeqCst))
        },
        session_id: "s-1".to_owned(),
    });
    state.log = {
        let lines = Arc::clone(&lines);
        Logger::new(
            LogFields::new(),
            Arc::new(move |_level, line| lines.lock().unwrap().push(line.to_owned())),
        )
    };
    let state = Arc::new(state);
    let denied = || {
        http::Request::builder()
            .uri("/provider/v1/messages")
            .header("authorization", "Bearer not-the-nonce")
            .body(axum::body::Body::empty())
            .unwrap()
    };

    for _ in 0..5 {
        let answer = super::serve_provider_request(Arc::clone(&state), denied()).await;
        assert_eq!(answer.status().as_u16(), 401);
    }
    assert_eq!(receipts_for(&path, "s-1", 20).unwrap().len(), 1);

    clock.fetch_add(super::DENIED_WINDOW_MS, std::sync::atomic::Ordering::SeqCst);
    super::serve_provider_request(Arc::clone(&state), denied()).await;

    assert_eq!(receipts_for(&path, "s-1", 20).unwrap().len(), 2);
    let warned: Vec<_> = lines
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.contains("without this session's key"))
        .cloned()
        .collect();
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert!(
        warned[1].contains("unrecorded_since_last=4"),
        "{}",
        warned[1]
    );
}
