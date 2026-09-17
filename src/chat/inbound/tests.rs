//! Inbound routing tests, ported from `inbound_test.ts`.

use super::{
    DeletionDecision, InboundDecision, RawMessage, classify, classify_deletion, is_blocked,
    is_permitted, without_bot_mention,
};
use crate::config::schema::ChatConfig;

const CHANNEL: &str = "served-channel";

fn config() -> ChatConfig {
    ChatConfig {
        token: "bot-token".to_owned(),
        channel_id: CHANNEL.to_owned(),
        allowed_user_ids: vec!["u-1".to_owned(), "u-2".to_owned()],
        blocked_user_ids: Vec::new(),
        operator_user_ids: Vec::new(),
        start_on_mention: false,
    }
}

fn message() -> RawMessage {
    RawMessage {
        id: "m-1".to_owned(),
        author_id: "u-1".to_owned(),
        author_name: Some("somebody".to_owned()),
        author_is_bot: false,
        channel_id: CHANNEL.to_owned(),
        parent_channel_id: None,
        content: "do the thing".to_owned(),
        attachments: Vec::new(),
    }
}

#[test]
fn a_message_in_the_served_channel_starts_a_session() {
    assert_eq!(
        classify(&message(), &config(), None),
        InboundDecision::Start
    );
}

#[test]
fn a_reply_in_a_thread_of_the_served_channel_goes_to_that_thread() {
    let mut sent = message();
    sent.channel_id = "t-9".to_owned();
    sent.parent_channel_id = Some(CHANNEL.to_owned());

    assert_eq!(
        classify(&sent, &config(), None),
        InboundDecision::Thread {
            thread_id: "t-9".to_owned()
        }
    );
}

#[test]
fn anywhere_else_is_ignored_including_a_direct_message() {
    let other = |channel: &str, parent: Option<&str>| {
        let mut sent = message();
        sent.channel_id = channel.to_owned();
        sent.parent_channel_id = parent.map(str::to_owned);
        sent
    };

    assert!(matches!(
        classify(&other("other", None), &config(), None),
        InboundDecision::Ignore { .. }
    ));
    assert!(matches!(
        classify(&other("t-9", Some("other-channel")), &config(), None),
        InboundDecision::Ignore { .. }
    ));
    assert!(matches!(
        classify(&other("dm-1", None), &config(), None),
        InboundDecision::Ignore { .. }
    ));
}

#[test]
fn a_bot_is_ignored_including_this_one() {
    let mut sent = message();
    sent.author_is_bot = true;
    assert!(matches!(
        classify(&sent, &config(), None),
        InboundDecision::Ignore { .. }
    ));
}

#[test]
fn somebody_not_on_the_allowlist_is_ignored() {
    let mut sent = message();
    sent.author_id = "stranger".to_owned();
    assert!(matches!(
        classify(&sent, &config(), None),
        InboundDecision::Ignore { .. }
    ));
}

/// Naming it would describe the allowlist to the person it excludes.
#[test]
fn no_refusal_names_an_account_or_a_list() {
    let mut sent = message();
    sent.author_id = "stranger".to_owned();
    let InboundDecision::Ignore { reason } = classify(&sent, &config(), None) else {
        panic!("expected a refusal");
    };

    assert!(!reason.contains("stranger"));
    assert!(!reason.contains("u-1"));
    assert!(!reason.contains("u-2"));
}

#[test]
fn the_wildcard_admits_anyone_who_can_post_there() {
    let mut open = config();
    open.allowed_user_ids = vec!["*".to_owned()];

    assert_eq!(classify(&message(), &open, None), InboundDecision::Start);
    let mut sent = message();
    sent.author_id = "somebody-else".to_owned();
    assert!(matches!(
        classify(&sent, &open, None),
        InboundDecision::Start
    ));
}

/// The reason the list exists: an open channel has no other way to exclude
/// one person without closing it to everybody.
#[test]
fn a_blocked_account_is_refused_even_when_the_channel_is_open() {
    let mut open = config();
    open.allowed_user_ids = vec!["*".to_owned()];
    open.blocked_user_ids = vec!["troll".to_owned()];

    let mut troll = message();
    troll.author_id = "troll".to_owned();
    assert!(matches!(
        classify(&troll, &open, None),
        InboundDecision::Ignore { .. }
    ));
    let mut anybody = message();
    anybody.author_id = "anybody".to_owned();
    assert!(matches!(
        classify(&anybody, &open, None),
        InboundDecision::Start
    ));
}

#[test]
fn blocking_beats_the_allowlist_and_any_role() {
    let mut both = config();
    both.allowed_user_ids = vec!["u-1".to_owned(), "troll".to_owned()];
    both.operator_user_ids = vec!["troll".to_owned()];
    both.blocked_user_ids = vec!["troll".to_owned()];

    assert!(is_blocked(&both, "troll"));
    assert!(!is_permitted(&both, "troll"));
    let mut troll = message();
    troll.author_id = "troll".to_owned();
    assert!(matches!(
        classify(&troll, &both, None),
        InboundDecision::Ignore { .. }
    ));
    let mut known = message();
    known.author_id = "u-1".to_owned();
    assert!(matches!(
        classify(&known, &both, None),
        InboundDecision::Start
    ));
}

/// A message carrying only a file still says something: that a file arrived.
#[test]
fn an_attachment_with_no_words_is_still_a_message() {
    let mut attached = message();
    attached.content = "   ".to_owned();
    attached.attachments = vec![super::RawAttachment {
        id: "a-1".to_owned(),
        name: "shot.png".to_owned(),
        url: "https://x/1".to_owned(),
        size: 10,
        content_type: Some("image/png".to_owned()),
    }];

    assert!(matches!(
        classify(&attached, &config(), None),
        InboundDecision::Start
    ));
    let mut quiet = message();
    quiet.content = "  ".to_owned();
    assert!(matches!(
        classify(&quiet, &config(), None),
        InboundDecision::Ignore { .. }
    ));
}

/// A channel is often also somewhere people talk. Requiring the bot to be
/// named lets them, and only a message addressed to it opens a sandbox.
#[test]
fn with_the_setting_on_only_a_message_naming_the_bot_starts_one() {
    let mut config = config();
    config.start_on_mention = true;

    let mut plain = message();
    plain.content = "what did you all think?".to_owned();
    assert!(matches!(
        classify(&plain, &config, Some("bot-1")),
        InboundDecision::Ignore { .. }
    ));

    let mut named = message();
    named.content = "<@bot-1> demo: fix the parser".to_owned();
    assert!(matches!(
        classify(&named, &config, Some("bot-1")),
        InboundDecision::Start
    ));

    let mut banged = message();
    banged.content = "look at this <@!bot-1>".to_owned();
    assert!(matches!(
        classify(&banged, &config, Some("bot-1")),
        InboundDecision::Start
    ));
}

/// Naming a different bot in the channel is not naming this one.
#[test]
fn somebody_elses_bot_is_not_this_one() {
    let mut config = config();
    config.start_on_mention = true;

    let mut sent = message();
    sent.content = "<@other> do a thing".to_owned();
    assert!(matches!(
        classify(&sent, &config, Some("bot-1")),
        InboundDecision::Ignore { .. }
    ));
}

/// Inside a thread the session is the conversation, so nothing is required.
#[test]
fn a_thread_reply_never_has_to_name_the_bot() {
    let mut config = config();
    config.start_on_mention = true;
    let mut reply = message();
    reply.content = "carry on".to_owned();
    reply.channel_id = "thread-1".to_owned();
    reply.parent_channel_id = Some(CHANNEL.to_owned());

    assert!(matches!(
        classify(&reply, &config, Some("bot-1")),
        InboundDecision::Thread { .. }
    ));
}

#[test]
fn with_the_setting_off_any_message_starts_one_as_before() {
    let mut sent = message();
    sent.content = "demo: go".to_owned();
    assert!(matches!(
        classify(&sent, &config(), Some("bot-1")),
        InboundDecision::Start
    ));
    assert!(matches!(
        classify(&sent, &config(), None),
        InboundDecision::Start
    ));
}

/// Refusing everything is better than starting work nobody addressed.
#[test]
fn a_bot_that_does_not_know_its_own_name_starts_nothing() {
    let mut config = config();
    config.start_on_mention = true;
    let mut sent = message();
    sent.content = "<@bot-1> go".to_owned();

    let InboundDecision::Ignore { reason } = classify(&sent, &config, None) else {
        panic!("expected a refusal");
    };
    assert!(reason.contains("its own name"));
}

#[test]
fn the_mention_that_summoned_it_is_not_part_of_what_was_asked() {
    assert_eq!(
        without_bot_mention("<@bot-1> demo: fix the parser", "bot-1"),
        "demo: fix the parser"
    );
    assert_eq!(
        without_bot_mention("hey <@!bot-1> look at <@bot-1> this", "bot-1"),
        "hey look at this"
    );
    assert_eq!(
        without_bot_mention("nothing to remove", "bot-1"),
        "nothing to remove"
    );
}

/// A message beginning with `!` is already addressed to a bot. Asking for a
/// mention as well makes `!usage` and `!help` vanish in the channel, which is
/// where they are most useful.
#[test]
fn a_command_reaches_the_daemon_without_naming_the_bot() {
    let mut config = config();
    config.start_on_mention = true;

    for content in ["!help", "!usage", "!shutdown", "  !status"] {
        let mut sent = message();
        sent.content = content.to_owned();
        assert!(matches!(
            classify(&sent, &config, Some("bot-1")),
            InboundDecision::Start
        ));
    }
}

/// Including one this daemon does not answer, which it then leaves alone.
#[test]
fn somebody_elses_command_is_let_through_and_ignored_later() {
    let mut config = config();
    config.start_on_mention = true;
    let mut sent = message();
    sent.content = "!somebodyelses".to_owned();

    assert!(matches!(
        classify(&sent, &config, Some("bot-1")),
        InboundDecision::Start
    ));
}

/// An aside in the channel is people talking, and starts nothing either way.
#[test]
fn an_aside_is_let_through_and_starts_nothing() {
    let mut config = config();
    config.start_on_mention = true;
    let mut sent = message();
    sent.content = "!!! anyone around?".to_owned();

    assert!(matches!(
        classify(&sent, &config, Some("bot-1")),
        InboundDecision::Start
    ));
}

/// A thread has an owner who decides who takes part, and the session refuses
/// anyone they have not invited. A bot is not a special case of that, so
/// being automated is no longer a reason to drop a message the owner allowed.
#[test]
fn a_bot_is_let_into_a_thread_and_left_out_of_the_channel() {
    // On the operator's allowlist, as any participant must be: `!allow` is
    // the owner's grant on top of that, not a way around it.
    let bot_message = |author_id: &str, channel: &str, parent: Option<&str>| {
        let mut sent = message();
        sent.author_is_bot = true;
        sent.author_id = author_id.to_owned();
        sent.channel_id = channel.to_owned();
        sent.parent_channel_id = parent.map(str::to_owned);
        sent
    };

    // In a thread: routed, and the session decides with its guest list.
    assert_eq!(
        classify(
            &bot_message("u-2", "thread-1", Some(CHANNEL)),
            &config(),
            Some("me")
        ),
        InboundDecision::Thread {
            thread_id: "thread-1".to_owned()
        }
    );

    // In the channel: still ignored, so nothing automated starts a session.
    assert!(matches!(
        classify(&bot_message("u-2", CHANNEL, None), &config(), Some("me")),
        InboundDecision::Ignore { .. }
    ));

    // And never its own, wherever it is said.
    assert_eq!(
        classify(
            &bot_message("me", "thread-1", Some(CHANNEL)),
            &config(),
            Some("me")
        ),
        InboundDecision::Ignore {
            reason: "its own message"
        }
    );

    // A bot nobody allowed is still refused, the same as a person.
    assert!(matches!(
        classify(
            &bot_message("stranger", "t", Some(CHANNEL)),
            &config(),
            Some("me")
        ),
        InboundDecision::Ignore { .. }
    ));
}

/// A deletion older than the cache arrives with no author and no text.
/// Routing has to work from where it was said, or this feature would only
/// ever cover messages recent enough to still be cached.
#[test]
fn a_deletion_is_routed_without_an_author_or_any_text() {
    let deletion = |channel: &str, parent: Option<&str>| super::RawDeletion {
        id: "m-9".to_owned(),
        channel_id: channel.to_owned(),
        parent_channel_id: parent.map(str::to_owned),
    };

    // In a served thread.
    assert_eq!(
        classify_deletion(&deletion("thread-1", Some(CHANNEL)), &config()),
        DeletionDecision::Withdraw {
            message_id: "m-9".to_owned(),
            thread_id: Some("thread-1".to_owned()),
        }
    );

    // In the served channel itself, which belongs to no thread.
    assert_eq!(
        classify_deletion(&deletion(CHANNEL, None), &config()),
        DeletionDecision::Withdraw {
            message_id: "m-9".to_owned(),
            thread_id: None,
        }
    );

    // Anywhere else is not ours, the same as for a message.
    assert!(matches!(
        classify_deletion(&deletion("other", None), &config()),
        DeletionDecision::Ignore { .. }
    ));
    assert!(matches!(
        classify_deletion(&deletion("t", Some("other")), &config()),
        DeletionDecision::Ignore { .. }
    ));
}
