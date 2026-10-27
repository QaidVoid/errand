use serde_json::{Value, json};

use super::is_music_link;

fn agent(agent: &Value) -> crate::config::schema::AgentConfig {
    let settings = json!({
        "chat": {
            "token": "a.token.value",
            "channelId": "chan",
            "allowedUserIds": ["100000000000000001"],
        },
        "agent": agent,
        "projectRoot": "/tmp/errand-projects",
        "stateDir": "/tmp/errand-state",
    });
    crate::config::validate::validate_config(&settings)
        .expect("the test configuration is accepted")
        .agent
}

#[test]
fn only_a_link_on_the_music_providers_host_is_fetched() {
    let config = agent(&json!({
        "provider": "chat",
        "model": "m",
        "providers": {
            "chat": { "credential": "k", "baseUrl": "https://chat.example/v1", "models": [{ "id": "m" }] },
            "songs": { "credential": "k", "baseUrl": "https://music.example/v1", "music": { "model": "music" } },
        },
    }));
    assert!(is_music_link(&config, "https://music.example/f/a.mp3"));
    for link in [
        "https://chat.example/f/a.mp3",
        "https://music.example.evil/f/a.mp3",
        "https://music.example@evil.test/a.mp3",
        "http://music.example/f/a.mp3",
        "https://music.example",
    ] {
        assert!(!is_music_link(&config, link), "{link}");
    }
}

#[test]
fn without_a_music_provider_nothing_is_fetched() {
    let config = agent(&json!({
        "provider": "chat",
        "model": "m",
        "providers": {
            "chat": { "credential": "k", "baseUrl": "https://chat.example/v1", "models": [{ "id": "m" }] },
        },
    }));
    assert!(!is_music_link(&config, "https://chat.example/f/a.mp3"));
}
