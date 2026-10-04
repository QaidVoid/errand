//! Gateway tests, ported from `gateway_test.ts`.
//!
//! The TypeScript suite also carries the reconnection delay table and the
//! string-matched error classes. Both are superseded by the chat library
//! decision: serenity re-sits a dead shard on its own fixed pace, and the two
//! permanent failures arrive as typed gateway errors. The give-up counter is
//! what remains of the policy, and it is tested here.
//!
//! Library objects are built by deserializing the shapes the gateway sends,
//! which is as far from a live connection as the TypeScript fakes were.

use serde_json::json;
use serenity::model::channel::Message;
use serenity::model::id::ChannelId;

use super::{GiveUp, is_permanent, to_raw};

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

#[test]
fn the_counter_gives_up_after_ten_unevenful_drops() {
    let mut counter = GiveUp::new();

    for _ in 0..super::MAX_ATTEMPTS {
        assert!(!counter.disconnected());
    }
    assert!(
        counter.disconnected(),
        "the daemon gives up rather than hanging on"
    );
    assert_eq!(counter.attempts(), super::MAX_ATTEMPTS + 1);
}

/// A connection that comes back between drops is one outage, however long it
/// ran.
#[test]
fn reaching_connected_again_resets_the_count() {
    let mut counter = GiveUp::new();

    for _ in 0..super::MAX_ATTEMPTS - 1 {
        assert!(!counter.disconnected());
    }
    counter.connected();
    assert_eq!(counter.attempts(), 0);
    assert!(!counter.disconnected());
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
