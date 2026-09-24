//! Tests for where the daemon's own answers are said.

use serde_json::json;
use serenity::model::id::ChannelId;

use super::{answer_in, brokerable_providers, usage_sources};
use crate::config::validate::validate_config;
use crate::provider::models::STORE_FILENAME;

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
