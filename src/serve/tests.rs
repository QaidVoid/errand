//! Tests for where the daemon's own answers are said.

use super::answer_in;
use serenity::model::id::ChannelId;

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
