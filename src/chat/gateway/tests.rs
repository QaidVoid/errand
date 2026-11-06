//! Gateway tests, ported from `gateway_test.ts`.
//!
//! The TypeScript suite also carries the reconnection delay table and the
//! string-matched error classes. Both are superseded by the chat library
//! decision: serenity re-sits a dead shard on its own fixed pace, and the two
//! permanent failures arrive as typed gateway errors. The give-up is what
//! remains of the policy, and it is tested with the stage machine driven
//! directly: the stages the library reports mid-run are `Resuming` and
//! `Connecting`, and the others are here because the gateway treats every
//! non-`Connected` landing the same.
//!
//! Library objects are built by deserializing the shapes the gateway sends,
//! which is as far from a live connection as the TypeScript fakes were.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::json;
use serenity::gateway::ConnectionStage;
use serenity::model::channel::Message;
use serenity::model::id::ChannelId;

use super::{Gateway, GatewayHandlers, is_permanent, to_raw};
use crate::config::schema::ChatConfig;
use crate::log::LogLevel;
use crate::log::logger;

/// A gateway configuration, for building a gateway without a connection.
fn config() -> ChatConfig {
    ChatConfig {
        token: "bot-token".to_owned(),
        channel_id: "served-channel".to_owned(),
        allowed_user_ids: vec!["u-1".to_owned()],
        blocked_user_ids: Vec::new(),
        operator_user_ids: Vec::new(),
        start_on_mention: false,
    }
}

/// What the connection callbacks did, for a test to assert on.
#[derive(Default)]
struct Calls {
    connected: usize,
    disconnected: usize,
    gave_up: usize,
}

/// Handlers that record every connection callback into `calls`.
fn recording_handlers(calls: &Arc<Mutex<Calls>>) -> GatewayHandlers {
    let connected = Arc::clone(calls);
    let disconnected = Arc::clone(calls);
    let gave_up = Arc::clone(calls);
    GatewayHandlers {
        on_message: Arc::new(|_, _| {}),
        on_command: Arc::new(|_, _| {}),
        on_thread_closed: Arc::new(|_| {}),
        on_withdrawn: Arc::new(|_, _| {}),
        on_connected: Arc::new(move || {
            connected.lock().expect("the calls lock").connected += 1;
        }),
        on_disconnected: Arc::new(move || {
            disconnected.lock().expect("the calls lock").disconnected += 1;
        }),
        on_gave_up: Arc::new(move || {
            gave_up.lock().expect("the calls lock").gave_up += 1;
        }),
    }
}

/// A gateway whose callbacks are recorded in `calls`.
fn recorded_gateway(calls: &Arc<Mutex<Calls>>) -> Arc<Gateway> {
    Gateway::new(config(), recording_handlers(calls), logger(LogLevel::Error))
}

/// Builds a library message from the wire shape it arrives in.
fn library_message(value: serde_json::Value) -> Message {
    serde_json::from_value(value).expect("a library message")
}

/// The reference message every reduction test starts from.
fn wire_message() -> serde_json::Value {
    json!({
        "id": "900000000000000001",
        "channel_id": "900000000000000002",
        "author": {
            "id": "800000000000000003",
            "username": "amelia1",
            "global_name": "amelia",
            "discriminator": "1234",
            "avatar": null,
            "bot": false,
        },
        "content": "do the thing",
        "timestamp": "2025-01-01T00:00:00.000+00:00",
        "tts": false,
        "mention_everyone": false,
        "mentions": [],
        "mention_roles": [],
        "attachments": [
            {
                "id": "700000000000000004",
                "filename": "shot.png",
                "url": "https://files/shot.png",
                "proxy_url": "https://files/shot.png",
                "size": 12,
                "content_type": "image/png",
            },
        ],
        "embeds": [],
        "pinned": false,
        "type": 0,
    })
}

/// A connection that never comes back is given up on, however the library
/// spends the wait. The library reports no `Disconnected` stage at all, so
/// the wait is timed from the last moment the connection was known to be
/// away, and the stages it does report must not reset it.
#[tokio::test]
async fn a_connection_away_too_long_is_given_up_on() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let gateway = recorded_gateway(&calls);

    gateway.stage_changed(ConnectionStage::Connected);
    gateway.stage_changed(ConnectionStage::Resuming);
    // The library trying on, and the manager replacing the shard under it,
    // are more of the same outage, not new ones.
    for stage in [
        ConnectionStage::Connecting,
        ConnectionStage::Handshake,
        ConnectionStage::Identifying,
    ] {
        tokio::time::sleep(Duration::from_millis(10)).await;
        gateway.stage_changed(stage);
    }

    tokio::time::timeout(
        Duration::from_secs(2),
        gateway.watch_connection(Duration::from_millis(20), Duration::from_millis(5)),
    )
    .await
    .expect("the watchdog gives up on an absent connection");
    let held = calls.lock().expect("the calls lock");
    assert_eq!(
        held.gave_up, 1,
        "the daemon gives up rather than hanging on"
    );
    assert_eq!(held.disconnected, 1, "only the drop was announced");
}

/// An outage that ends within the window is waited out: the connection
/// coming back clears the wait, and the daemon does not give up.
#[tokio::test]
async fn a_connection_that_comes_back_is_waited_for() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let gateway = recorded_gateway(&calls);

    gateway.stage_changed(ConnectionStage::Connected);
    gateway.stage_changed(ConnectionStage::Resuming);
    tokio::time::sleep(Duration::from_millis(10)).await;
    gateway.stage_changed(ConnectionStage::Connected);

    let waited = tokio::time::timeout(
        Duration::from_millis(60),
        gateway.watch_connection(Duration::from_millis(50), Duration::from_millis(5)),
    )
    .await;
    assert!(waited.is_err(), "the watchdog keeps waiting");
    let held = calls.lock().expect("the calls lock");
    assert_eq!(held.gave_up, 0);
    assert_eq!(held.connected, 2, "the outage is over");
    assert_eq!(held.disconnected, 1, "the drop was announced");
}

/// A connection that has never come up is given up on too: the wait runs
/// from the moment the daemon began, so a service that cannot be reached at
/// all cannot hold sessions open forever.
#[tokio::test]
async fn a_connection_that_never_rises_is_given_up_on() {
    let calls = Arc::new(Mutex::new(Calls::default()));
    let gateway = recorded_gateway(&calls);

    tokio::time::timeout(
        Duration::from_secs(2),
        gateway.watch_connection(Duration::from_millis(20), Duration::from_millis(5)),
    )
    .await
    .expect("the watchdog gives up on a connection that never came up");
    let held = calls.lock().expect("the calls lock");
    assert_eq!(held.gave_up, 1);
    assert_eq!(
        held.disconnected, 0,
        "there was no working connection to lose"
    );
}

/// A service that is briefly down is waited out; a service that has already
/// decided about this bot is not, because the answer will not change.
#[test]
fn an_outage_is_worth_waiting_out_and_a_refusal_is_not() {
    let permanent =
        |error: serenity::gateway::GatewayError| is_permanent(&serenity::Error::Gateway(error));

    // What configuration looks like: waiting changes neither.
    assert!(permanent(
        serenity::gateway::GatewayError::InvalidAuthentication
    ));
    assert!(permanent(
        serenity::gateway::GatewayError::InvalidGatewayIntents
    ));
    assert!(permanent(
        serenity::gateway::GatewayError::DisallowedGatewayIntents
    ));

    // What an outage looks like: a clean close, a refused handshake.
    assert!(!permanent(serenity::gateway::GatewayError::Closed(None)));
    assert!(!permanent(serenity::gateway::GatewayError::ExpectedHello));
    assert!(!is_permanent(&serenity::Error::Other(
        "503 Service Unavailable"
    )));
}

/// A library message is reduced to the facts the filter needs.
#[test]
fn a_library_message_is_reduced_to_the_facts_the_filter_needs() {
    let message = library_message(wire_message());

    let raw = to_raw(&message, Some(ChannelId::new(800_000_000_000_000_010)));
    assert_eq!(raw.author_id, "800000000000000003");
    assert_eq!(raw.author_name.as_deref(), Some("amelia"));
    assert_eq!(raw.channel_id, "900000000000000002");
    assert_eq!(raw.parent_channel_id.as_deref(), Some("800000000000000010"));
    assert_eq!(raw.attachments[0].name, "shot.png");
    assert_eq!(
        raw.attachments[0].content_type.as_deref(),
        Some("image/png")
    );
}

/// A channel that is not a thread has no parent, and that is not an error.
#[test]
fn a_top_level_message_has_no_parent_channel() {
    let message = library_message(wire_message());

    let raw = to_raw(&message, None);
    assert_eq!(raw.parent_channel_id, None);
    assert_eq!(raw.author_name.as_deref(), Some("amelia"));
}

/// The account name stands in when no display name was set.
#[test]
fn an_author_without_a_display_name_is_named_by_the_account() {
    let mut wire = wire_message();
    wire["author"]["global_name"] = serde_json::Value::Null;
    let message = library_message(wire);

    let raw = to_raw(&message, None);
    assert_eq!(raw.author_name.as_deref(), Some("amelia1"));
}

/// A sticker rides the same attachment list a file does, with the URL the
/// CDN serves it at.
#[test]
fn a_picture_sticker_joins_the_attachments() {
    let mut wire = wire_message();
    wire["sticker_items"] = json!([
        {"id": "600000000000000005", "name": "Cheer", "format_type": 1},
    ]);
    let message = library_message(wire);

    let raw = to_raw(&message, None);
    assert_eq!(raw.attachments.len(), 2);
    let sticker = &raw.attachments[1];
    assert_eq!(sticker.name, "Cheer.png");
    assert_eq!(
        sticker.url,
        "https://cdn.discordapp.com/stickers/600000000000000005.png"
    );
    assert_eq!(sticker.content_type.as_deref(), Some("image/png"));
    assert_eq!(sticker.id, "600000000000000005");
    assert_eq!(sticker.size, 0);
}

/// A Lottie sticker is an animation described in JSON, so it arrives as the
/// JSON the agent can read rather than an image the model is shown.
#[test]
fn a_lottie_sticker_is_taken_as_its_source() {
    let mut wire = wire_message();
    wire["attachments"] = json!([]);
    wire["sticker_items"] = json!([
        {"id": "600000000000000006", "name": "Wave", "format_type": 3},
    ]);
    let message = library_message(wire);

    let raw = to_raw(&message, None);
    assert_eq!(raw.attachments.len(), 1);
    assert_eq!(raw.attachments[0].name, "Wave.json");
    assert_eq!(
        raw.attachments[0].content_type.as_deref(),
        Some("application/json")
    );
}

/// A format the library does not know has no URL to fetch, so it is left
/// behind rather than invented.
#[test]
fn a_sticker_of_an_unknown_format_is_dropped() {
    let mut wire = wire_message();
    wire["attachments"] = json!([]);
    wire["sticker_items"] = json!([
        {"id": "600000000000000007", "name": "Mystery", "format_type": 99},
    ]);
    let message = library_message(wire);

    let raw = to_raw(&message, None);
    assert!(raw.attachments.is_empty());
}

/// A sticker on its own counts as content, so it is not refused as an empty
/// message by the filter downstream.
#[test]
fn a_sticker_only_message_carries_an_attachment() {
    let mut wire = wire_message();
    wire["content"] = json!("");
    wire["attachments"] = json!([]);
    wire["sticker_items"] = json!([
        {"id": "600000000000000005", "name": "Cheer", "format_type": 2},
    ]);
    let message = library_message(wire);

    let raw = to_raw(&message, None);
    assert_eq!(raw.content, "");
    assert_eq!(raw.attachments.len(), 1);
    assert_eq!(raw.attachments[0].name, "Cheer.png");
}
