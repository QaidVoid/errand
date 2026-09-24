//! Tests for the egress broker, ported from `broker_test.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::server::{Broker, dial, read_request_head};
use super::{
    NoAddress, ProviderRoute, Resolve, host_allowed, is_private_address, parse_connect,
    parse_forward, public_addresses,
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
    conn.write_all(format!("CONNECT {authority} HTTP/1.1\r\n\r\n").as_bytes())
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

/// A stand-in provider that reports what key it was actually handed.
async fn start_upstream(seen: Arc<Mutex<BTreeMap<String, String>>>, key: &'static str) -> u16 {
    let app = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let seen = Arc::clone(&seen);
        async move {
            let authorization = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned();
            seen.lock().unwrap().insert(key.to_owned(), authorization);
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
        }],
        true,
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

/// A nonce is per provider, so reading one out of a session buys nothing
/// against another provider the same broker serves.
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
            },
            ProviderRoute {
                prefix: "/provider/meta".to_owned(),
                upstream: format!("http://127.0.0.1:{two_port}/v1"),
                nonce: "nonce-meta".to_owned(),
                credential: "key-meta".to_owned(),
            },
        ],
        true,
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
        "192.0.2.9",    // TEST-NET-1
        "198.51.100.9", // TEST-NET-2
        "203.0.113.9",  // TEST-NET-3
        "192.0.0.8",    // IETF protocol assignments
        "192.88.99.1",  // 6to4 relay anycast
        "198.18.0.1",   // benchmarking
        "198.19.255.1", // benchmarking, the far end
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
    let mut broker = Broker::new(vec!["*".to_owned()], vec![443], silent(), Vec::new(), false);
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
    );
    let on_port = on.listen("127.0.0.1").await.expect("bound");
    // Admitted now, so it gets as far as dialling: nothing listens on 443, so
    // it fails upstream rather than being refused at the gate.
    assert!(!try_connect(on_port, "127.0.0.1:443").await.contains("403"));
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
        }],
        false,
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
    let stream = dial(&addresses, port).await.expect("the second address");
    assert_eq!(
        stream.peer_addr().expect("a peer").ip().to_string(),
        "127.0.0.1"
    );
    assert!(dial(&addresses[..1], port).await.is_err());
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
    );
    let port = broker.listen("127.0.0.1").await.expect("bound");

    let mut conn = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the broker");
    conn.write_all(
        format!("GET http://127.0.0.1:{origin_port}/a?b=c HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
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
    conn.write_all(b"GET http://127.0.0.1:1/ HTTP/1.1\r\n\r\n")
        .await
        .expect("written");
    let mut refused = String::new();
    let _ = conn.read_to_string(&mut refused).await;
    assert!(refused.starts_with("HTTP/1.1 403"));
    broker.close();
}
