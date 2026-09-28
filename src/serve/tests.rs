//! Tests for where the daemon's own answers are said.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::json;
use serenity::model::id::ChannelId;

use super::{BoxedGateRead, answer_in, brokerable_providers, read_usage, usage_sources};
use crate::config::validate::validate_config;
use crate::provider::models::STORE_FILENAME;
use crate::provider::usage::{Quota, QuotaGate, UsageSource};

/// A question asked in a thread is answered in that thread. Answering in the
/// channel put `!usage` and "this session has ended" in front of everybody
/// except the person who asked.
#[test]
fn an_answer_goes_where_the_question_was_asked() {
    let served = ChannelId::new(111);

    assert_eq!(answer_in("222", served), ChannelId::new(222));
    // Nowhere of its own, as for a message from the interface.
    assert_eq!(answer_in("", served), served);
    // Nothing a channel could be, rather than a channel that is nothing.
    assert_eq!(answer_in("not-an-id", served), served);
    assert_eq!(answer_in("0", served), served);
}

/// A provider the agent has built in is brokered from where the store serves
/// it, since the configuration names no URL for it. Without a route it has no
/// key inside the sandbox, and a switch to it is refused.
#[test]
fn a_built_in_provider_is_brokered_from_the_store() {
    let store = tempfile::tempdir().expect("a store directory");
    std::fs::write(
        store.path().join(STORE_FILENAME),
        json!({
            "zai-coding-cn": { "models": [
                { "id": "glm-5.3", "baseUrl": "https://zai.example/v4" },
                { "id": "glm-5.3-flash", "baseUrl": "https://zai.example/v4" },
            ] },
            "openrouter": { "models": [
                { "id": "a", "baseUrl": "https://or.example/api" },
                { "id": "b", "baseUrl": "https://or.example/api/v1" },
                { "id": "c", "baseUrl": "https://or.example/api/v1" },
            ] },
        })
        .to_string(),
    )
    .expect("written");
    let config = validate_config(&json!({
        "chat": { "token": "t", "channelId": "c", "allowedUserIds": ["u"] },
        "agent": {
            "provider": "gateway",
            "providers": {
                "gateway": { "baseUrl": "https://gateway.example/v1", "credential": "g-key" },
                "zai-coding-cn": { "credential": "z-key", "models": [{ "id": "glm-5.3-flash" }] },
                "openrouter": { "credential": "o-key" },
                "free": { "extension": true, "models": [{ "id": "big-pickle" }] },
            },
        },
        "projectRoot": "/tmp/p",
        "stateDir": "/tmp/s",
    }))
    .expect("resolves");

    let store_dir = store.path().display().to_string();
    assert_eq!(
        brokerable_providers(&config.agent, Some(&store_dir)),
        [
            (
                "gateway".to_owned(),
                "https://gateway.example/v1".to_owned(),
                "g-key".to_owned()
            ),
            (
                "zai-coding-cn".to_owned(),
                "https://zai.example/v4".to_owned(),
                "z-key".to_owned()
            ),
            (
                "openrouter".to_owned(),
                "https://or.example/api/v1".to_owned(),
                "o-key".to_owned()
            ),
        ]
    );
}

/// A provider is metered because its definition says how, never because of
/// its name, whether or not sessions start on it, and the one they start on is
/// listed first.
#[test]
fn every_metered_provider_is_asked_the_default_first() {
    let config = validate_config(&json!({
        "chat": { "token": "t", "channelId": "c", "allowedUserIds": ["u"] },
        "agent": {
            "provider": "gateway",
            "providers": {
                "zai-coding-cn": { "credential": "z-key", "usage": "zai" },
                "zai-unmetered": { "credential": "u-key" },
                "mapped": {
                    "baseUrl": "https://mapped.example/v1",
                    "credential": "m-key",
                    "usage": { "percent": "left", "percentIs": "left" },
                },
                "plain": { "baseUrl": "https://plain.example/v1", "credential": "p-key" },
                "gateway": {
                    "baseUrl": "https://gateway.example/v1",
                    "usage": "gateway",
                    "credential": "g-key",
                },
            },
        },
        "projectRoot": "/tmp/p",
        "stateDir": "/tmp/s",
    }))
    .expect("resolves");

    let asked: Vec<String> = usage_sources(&config)
        .into_iter()
        .map(|source| source.provider)
        .collect();
    assert_eq!(asked, ["gateway", "zai-coding-cn", "mapped"]);
}

/// One source whose provider answers `percentage`, counting every read so the
/// test can tell a read that happened from one the gate held.
///
/// `resets_at` is set in the future, which is what makes a spent window
/// holdable: without a time to wait for there is nothing to trust, so the gate
/// asks again rather than believing what it was told.
fn counted_source(
    provider: &str,
    percentage: f64,
    reads: Arc<AtomicI64>,
) -> UsageSource<BoxedGateRead> {
    let read: BoxedGateRead = Box::new(move || {
        let reads = Arc::clone(&reads);
        Box::pin(async move {
            reads.fetch_add(1, Ordering::Relaxed);
            Some(Quota {
                percentage,
                resets_at: Some(500_000),
            })
        }) as Pin<Box<dyn Future<Output = Option<Quota>> + Send>>
    });
    UsageSource {
        provider: provider.to_owned(),
        gate: QuotaGate::new(read, || 1_000),
    }
}

/// A spent window is held until it rolls over, so a provider that resets early
/// goes unread. A refresh is what makes the daemon ask again, and read it is
/// where the flag has to be acted on rather than merely carried.
#[tokio::test]
async fn a_refresh_makes_the_gate_ask_where_a_plain_read_does_not() {
    let reads = Arc::new(AtomicI64::new(0));
    let mut sources = vec![counted_source("spent", 100.0, Arc::clone(&reads))];

    read_usage(&mut sources, false).await;
    read_usage(&mut sources, false).await;
    assert_eq!(reads.load(Ordering::Relaxed), 1, "the second read was held");

    read_usage(&mut sources, true).await;
    read_usage(&mut sources, true).await;
    assert_eq!(
        reads.load(Ordering::Relaxed),
        3,
        "a refresh must not be answered from what was held"
    );
}

/// Every provider is refreshed, not only the first, and each row keeps the
/// provider it belongs to.
#[tokio::test]
async fn a_refresh_covers_every_provider() {
    let reads = Arc::new(AtomicI64::new(0));
    let mut sources = vec![
        counted_source("one", 100.0, Arc::clone(&reads)),
        counted_source("two", 100.0, Arc::clone(&reads)),
        counted_source("three", 100.0, Arc::clone(&reads)),
    ];

    read_usage(&mut sources, false).await;
    assert_eq!(reads.load(Ordering::Relaxed), 3);

    read_usage(&mut sources, true).await;
    assert_eq!(reads.load(Ordering::Relaxed), 6);
    let rows = read_usage(&mut sources, true).await;
    assert_eq!(
        rows.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        ["one", "two", "three"]
    );
}

/// A reading of nothing is carried through as nothing, so a provider that
/// cannot be reached is absent from the table rather than shown as full.
#[tokio::test]
async fn a_provider_that_answers_nothing_is_reported_as_nothing() {
    let reads = Arc::new(AtomicI64::new(0));
    let read: BoxedGateRead = Box::new(move || {
        let reads = Arc::clone(&reads);
        Box::pin(async move {
            reads.fetch_add(1, Ordering::Relaxed);
            None
        }) as Pin<Box<dyn Future<Output = Option<Quota>> + Send>>
    });
    let mut sources = vec![UsageSource {
        provider: "quiet".to_owned(),
        gate: QuotaGate::new(read, || 1_000),
    }];

    let rows = read_usage(&mut sources, true).await;
    assert_eq!(rows, vec![("quiet".to_owned(), None)]);
}
