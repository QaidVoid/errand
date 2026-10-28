//! Tests for what a provider's answer is relayed as.

use crate::config::route::RoutePolicy;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;

use super::{Scrubber, relay};
use crate::log::{LogFields, Logger};
use crate::sandbox::broker::ProviderRoute;

const KEY: &str = "sk-the-operators-real-key";

fn route(credential: &str) -> ProviderRoute {
    ProviderRoute {
        prefix: "/provider/zai".to_owned(),
        upstream: "https://api.example.com/v4".to_owned(),
        nonce: "session-nonce".to_owned(),
        credential: credential.to_owned(),
        credential_name: None,
        policy: RoutePolicy::default(),
    }
}

/// A receipt for a call nothing records.
fn unrecorded() -> super::super::receipts::ReceiptGuard {
    super::super::receipts::ReceiptGuard::begin(None, &recording().0, blank_receipt())
}

/// A receipt with nothing filled in yet.
fn blank_receipt() -> super::super::receipts::Receipt {
    super::super::receipts::Receipt {
        at: 0,
        session_id: String::new(),
        provider: "/provider/zai".to_owned(),
        credential_name: None,
        upstream_host: None,
        method: "POST".to_owned(),
        path: "/chat".to_owned(),
        query_bytes: None,
        status: None,
        outcome: super::super::receipts::Outcome::Interrupted,
        elapsed_ms: 0,
        bytes_out: 0,
        bytes_in: 0,
    }
}

/// A logger that keeps what it was told.
fn recording() -> (Logger, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&lines);
    let log = Logger::new(
        LogFields::new(),
        Arc::new(move |_level, line| kept.lock().unwrap().push(line.to_owned())),
    );
    (log, lines)
}

/// An upstream answer whose body arrives as `chunks`, one at a time.
fn answer(status: u16, headers: &[(&str, &[u8])], chunks: Vec<Vec<u8>>) -> reqwest::Response {
    let mut builder = http::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, http::HeaderValue::from_bytes(value).unwrap());
    }
    let stream = futures_util::stream::iter(chunks.into_iter().map(Ok::<Vec<u8>, std::io::Error>));
    reqwest::Response::from(builder.body(reqwest::Body::wrap_stream(stream)).unwrap())
}

/// What the session reads: the status, the headers, and the body or the
/// error that ended it.
async fn read(
    relayed: axum::response::Response,
) -> (u16, http::HeaderMap, Result<Vec<u8>, String>) {
    let status = relayed.status().as_u16();
    let headers = relayed.headers().clone();
    let mut body = relayed.into_body().into_data_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        match chunk {
            Ok(chunk) => bytes.extend_from_slice(&chunk),
            Err(error) => return (status, headers, Err(error.to_string())),
        }
    }
    (status, headers, Ok(bytes))
}

/// A key split anywhere across two chunks is still found, and nothing of it
/// survives.
#[test]
fn a_key_split_across_chunks_is_scrubbed_at_every_offset() {
    let body = format!("{{\"echo\":\"Bearer {KEY}\"}}");
    for cut in 0..=body.len() {
        let mut scrubber = Scrubber::new(KEY);
        let mut out = scrubber.push(&body.as_bytes()[..cut]);
        out.extend(scrubber.push(&body.as_bytes()[cut..]));
        out.extend(scrubber.finish());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"echo\":\"Bearer [redacted]\"}",
            "cut at {cut}"
        );
    }
}

/// A chunk that cannot be the start of the key is emitted whole, so a
/// streamed event is not held behind the next one. Only an end that could
/// still begin the key waits.
#[test]
fn only_a_possible_start_of_the_key_is_held_back() {
    let mut scrubber = Scrubber::new(KEY);
    let line = b"data: {\"delta\":\"hello\"}\n\n";
    assert_eq!(scrubber.push(line), line);

    let mut pending = b"data: sk-the-".to_vec();
    assert_eq!(scrubber.push(&pending), b"data: ");
    pending = scrubber.push(b"weather");
    assert_eq!(pending, b"sk-the-weather");
    assert!(scrubber.finish().is_empty());
}

/// A key fed one byte at a time, and a key that overlaps itself, are
/// scrubbed exactly as a whole-body replace would.
#[test]
fn a_key_fed_byte_by_byte_or_overlapping_itself_is_scrubbed() {
    for (key, body) in [
        (KEY, format!("a {KEY} b {KEY}{KEY} c")),
        ("abab", "xxabababab abab".to_owned()),
        ("aaa", "aaaaaaa".to_owned()),
    ] {
        let mut scrubber = Scrubber::new(key);
        let mut out = Vec::new();
        for byte in body.as_bytes() {
            out.extend(scrubber.push(std::slice::from_ref(byte)));
        }
        out.extend(scrubber.finish());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            body.replace(key, "[redacted]"),
            "{key} in {body}"
        );
    }
}

/// An empty credential matches nothing, so the body passes unchanged.
#[test]
fn an_empty_credential_leaves_the_body_alone() {
    let mut scrubber = Scrubber::new("");
    assert_eq!(scrubber.push(b"anything at all"), b"anything at all");
    assert!(scrubber.finish().is_empty());
}

/// An upstream that quotes the key it was sent, in its body and in its
/// headers, hands the session none of it, and the status is untouched.
#[tokio::test]
async fn an_upstream_that_echoes_the_key_is_scrubbed() {
    let quoted = format!("{{\"error\":\"bad key {KEY}\"}}");
    let echoed = format!("Bearer {KEY}");
    let location = format!("https://api.example.com/retry?key={KEY}");
    let upstream = answer(
        401,
        &[
            ("x-request-echo", echoed.as_bytes()),
            ("location", location.as_bytes()),
            ("content-length", quoted.len().to_string().as_bytes()),
            ("etag", b"\"abc\""),
            ("content-md5", b"Q2hlY2sgSW50ZWdyaXR5IQ=="),
        ],
        vec![
            quoted.as_bytes()[..20].to_vec(),
            quoted.as_bytes()[20..].to_vec(),
        ],
    );

    let (status, headers, body) = read(relay(
        upstream,
        &route(KEY),
        &recording().0,
        1 << 20,
        unrecorded(),
    ))
    .await;

    assert_eq!(status, 401);
    assert_eq!(
        String::from_utf8(body.unwrap()).unwrap(),
        "{\"error\":\"bad key [redacted]\"}"
    );
    assert_eq!(headers["x-request-echo"], "Bearer [redacted]");
    assert_eq!(
        headers["location"],
        "https://api.example.com/retry?key=[redacted]"
    );
    assert!(headers.get("content-length").is_none());
    assert!(headers.get("etag").is_none());
    assert!(headers.get("content-md5").is_none());
    for value in headers.values() {
        assert!(!String::from_utf8_lossy(value.as_bytes()).contains(KEY));
    }
}

/// A header value that is not UTF-8 was dropped before; it is relayed, with
/// the key taken out of it all the same.
#[tokio::test]
async fn a_non_utf8_header_is_relayed_and_scrubbed() {
    let mut value = b"caf\xe9 ".to_vec();
    value.extend_from_slice(KEY.as_bytes());
    let upstream = answer(200, &[("x-odd", &value)], Vec::new());

    let (_, headers, _) = read(relay(
        upstream,
        &route(KEY),
        &recording().0,
        1 << 20,
        unrecorded(),
    ))
    .await;

    assert_eq!(headers["x-odd"].as_bytes(), b"caf\xe9 [redacted]");
}

/// A declared size over the ceiling is refused before anything is relayed.
#[tokio::test]
async fn an_answer_declared_over_the_limit_is_refused() {
    let (log, lines) = recording();
    let upstream = answer(200, &[("content-length", b"2048")], vec![vec![b'x'; 2048]]);

    let (status, _, body) = read(relay(upstream, &route(KEY), &log, 1024, unrecorded())).await;

    assert_eq!(status, 502);
    assert!(!String::from_utf8(body.unwrap()).unwrap().contains('x'));
    assert!(
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("declared size") && line.contains("/provider/zai"))
    );
}

/// An answer that declares nothing and streams past the ceiling is cut with
/// an error the session sees, and the cut is logged.
#[tokio::test]
async fn an_answer_streamed_over_the_limit_is_cut() {
    let (log, lines) = recording();
    let upstream = answer(200, &[], vec![vec![b'x'; 600], vec![b'x'; 600]]);

    let (status, _, body) = read(relay(upstream, &route(KEY), &log, 1024, unrecorded())).await;

    assert_eq!(status, 200);
    assert!(body.is_err(), "the body ended cleanly past the limit");
    assert!(
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("cut for its size") && line.contains("/provider/zai"))
    );
}

/// A compressed answer cannot be scrubbed, so it is refused rather than
/// relayed with the key in it, and the refusal is logged.
#[tokio::test]
async fn a_compressed_answer_is_refused_not_relayed_unscrubbed() {
    let (log, lines) = recording();
    let mut bytes = b"\x1f\x8b not really gzip ".to_vec();
    bytes.extend_from_slice(KEY.as_bytes());
    let upstream = answer(200, &[("content-encoding", b"gzip")], vec![bytes]);

    let (status, _, body) = read(relay(upstream, &route(KEY), &log, 1 << 20, unrecorded())).await;

    assert_eq!(status, 502);
    let body = body.unwrap();
    assert!(
        !body
            .windows(KEY.len())
            .any(|window| window == KEY.as_bytes())
    );
    assert!(
        lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("compressed") && line.contains("/provider/zai"))
    );
}

/// An answer with no body, such as a 204, relays as empty without hanging.
#[tokio::test]
async fn an_empty_answer_passes_through_empty() {
    let upstream = answer(204, &[], Vec::new());

    let (status, _, body) = read(relay(
        upstream,
        &route(KEY),
        &recording().0,
        1 << 20,
        unrecorded(),
    ))
    .await;

    assert_eq!(status, 204);
    assert!(body.unwrap().is_empty());
}

/// An empty or identity coding is no compression, so the body is scrubbed
/// as usual rather than waved through.
#[tokio::test]
async fn an_identity_or_empty_coding_is_still_scrubbed() {
    for coding in [&b""[..], b"identity", b"identity, identity"] {
        let upstream = answer(
            200,
            &[("content-encoding", coding)],
            vec![KEY.as_bytes().to_vec()],
        );
        let (_, _, body) = read(relay(
            upstream,
            &route(KEY),
            &recording().0,
            1 << 20,
            unrecorded(),
        ))
        .await;
        assert_eq!(body.unwrap(), b"[redacted]", "{coding:?}");
    }
}

/// The first chunk of a streamed answer reaches the session before the
/// upstream has sent the next, so an event stream is not buffered whole.
#[tokio::test]
async fn a_streamed_answer_arrives_before_the_upstream_finishes() {
    let (send, receive) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let chunks = futures_util::stream::unfold(receive, |mut receive| async move {
        receive
            .recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), receive))
    });
    let upstream = reqwest::Response::from(
        http::Response::builder()
            .status(200)
            .body(reqwest::Body::wrap_stream(chunks))
            .unwrap(),
    );
    let mut body = relay(upstream, &route(KEY), &recording().0, 1 << 20, unrecorded())
        .into_body()
        .into_data_stream();

    send.send(b"data: one\n\n".to_vec()).unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
        .await
        .expect("the first event was held back")
        .expect("a frame")
        .expect("no error");
    assert_eq!(&first[..], b"data: one\n\n");

    send.send(b"data: two\n\n".to_vec()).unwrap();
    drop(send);
    let mut rest = Vec::new();
    while let Some(chunk) = body.next().await {
        rest.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(rest, b"data: two\n\n");
}

/// A streamed answer's receipt is written when the stream ends, not when it
/// starts, and one the session hangs up on part way is recorded as
/// interrupted with what was relayed so far.
#[tokio::test]
async fn a_receipt_waits_for_the_end_of_the_answer() {
    use super::super::receipts::{Ledger, Outcome, ReceiptGuard, ReceiptStore, receipts_for};

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipts.db");
    let ledger = Ledger {
        store: Arc::new(ReceiptStore::open(&path).unwrap()),
        now: Arc::new(|| 0),
        session_id: "s-1".to_owned(),
    };
    let streamed = || {
        let (send, receive) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let chunks = futures_util::stream::unfold(receive, |mut receive| async move {
            receive
                .recv()
                .await
                .map(|chunk| (Ok::<_, std::io::Error>(chunk), receive))
        });
        let upstream = reqwest::Response::from(
            http::Response::builder()
                .status(200)
                .body(reqwest::Body::wrap_stream(chunks))
                .unwrap(),
        );
        (send, upstream)
    };
    let guard = || ReceiptGuard::begin(Some(&ledger), &recording().0, blank_receipt());

    let (send, upstream) = streamed();
    let mut body = relay(upstream, &route(KEY), &recording().0, 1 << 20, guard())
        .into_body()
        .into_data_stream();
    send.send(b"data: one\n\n".to_vec()).unwrap();
    body.next().await.unwrap().unwrap();
    assert!(receipts_for(&path, "s-1", 20).unwrap().is_empty());
    drop(send);
    while body.next().await.is_some() {}
    let read = receipts_for(&path, "s-1", 20).unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].outcome, Outcome::Completed);
    assert_eq!(read[0].bytes_in, 11);

    let (send, upstream) = streamed();
    let mut body = relay(upstream, &route(KEY), &recording().0, 1 << 20, guard())
        .into_body()
        .into_data_stream();
    send.send(b"data: one\n\n".to_vec()).unwrap();
    body.next().await.unwrap().unwrap();
    drop(body);
    let read = receipts_for(&path, "s-1", 20).unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[0].outcome, Outcome::Interrupted);
    assert_eq!(read[0].bytes_in, 11);
}

/// An answer that carries no body is complete as soon as it is relayed: the
/// body is never read, so it could never be seen to end.
#[tokio::test]
async fn a_bodiless_answer_is_recorded_as_completed() {
    use super::super::receipts::{Ledger, Outcome, ReceiptGuard, ReceiptStore, receipts_for};

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipts.db");
    let ledger = Ledger {
        store: Arc::new(ReceiptStore::open(&path).unwrap()),
        now: Arc::new(|| 0),
        session_id: "s-1".to_owned(),
    };
    let guard = ReceiptGuard::begin(Some(&ledger), &recording().0, blank_receipt());

    let relayed = relay(
        answer(204, &[], Vec::new()),
        &route(KEY),
        &recording().0,
        1 << 20,
        guard,
    );

    assert_eq!(relayed.status().as_u16(), 204);
    let read = receipts_for(&path, "s-1", 20).unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].outcome, Outcome::Completed);
}

/// What a receipt says of an answer refused for its size, and of one whose
/// last bytes were held back as a possible start of the key: the refusal is
/// recorded as one, and the held bytes count once they are relayed.
#[tokio::test]
async fn a_size_refusal_and_a_held_tail_are_recorded() {
    use super::super::receipts::{Ledger, Outcome, ReceiptGuard, ReceiptStore, receipts_for};

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipts.db");
    let ledger = Ledger {
        store: Arc::new(ReceiptStore::open(&path).unwrap()),
        now: Arc::new(|| 0),
        session_id: "s-1".to_owned(),
    };
    let guard = || ReceiptGuard::begin(Some(&ledger), &recording().0, blank_receipt());

    let declared = answer(200, &[("content-length", b"2048")], vec![vec![b'x'; 2048]]);
    let _ = read(relay(declared, &route(KEY), &recording().0, 1024, guard())).await;
    let ends_held = answer(200, &[], vec![b"done sk-the".to_vec()]);
    let (_, _, body) = read(relay(
        ends_held,
        &route(KEY),
        &recording().0,
        1 << 20,
        guard(),
    ))
    .await;
    assert_eq!(body.unwrap(), b"done sk-the");

    let read = receipts_for(&path, "s-1", 20).unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[1].outcome, Outcome::Refused);
    assert_eq!(read[1].status, Some(502));
    assert_eq!(read[0].outcome, Outcome::Completed);
    assert_eq!(read[0].bytes_in, 11);
}

/// An answer with no body names a coding for the representation it stands
/// for, and has nothing to scrub, so it is relayed rather than refused.
#[tokio::test]
async fn a_compressed_answer_with_no_body_is_relayed() {
    let upstream = answer(304, &[("content-encoding", b"gzip")], Vec::new());

    let (status, _, body) = read(relay(
        upstream,
        &route(KEY),
        &recording().0,
        1 << 20,
        unrecorded(),
    ))
    .await;

    assert_eq!(status, 304);
    assert!(body.unwrap().is_empty());
}
