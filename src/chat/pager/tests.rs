//! Tests for turning paged cards.

use std::sync::atomic::{AtomicI64, Ordering};

use super::{BUTTON_PREFIX, Pager};
use crate::session::event::{Card, CardColor, CardPage};

/// The clock the expiry test moves. The other test reads a still one, so
/// the two never move each other's time.
static NOW: AtomicI64 = AtomicI64::new(0);

fn now() -> i64 {
    NOW.load(Ordering::SeqCst)
}

fn still() -> i64 {
    0
}

fn three_pages() -> Card {
    Card::paged(
        CardColor::Settled,
        "commands".to_owned(),
        ["one", "two", "three"]
            .into_iter()
            .map(|label| CardPage {
                label: label.to_owned(),
                description: format!("all about {label}"),
                fields: Vec::new(),
            })
            .collect(),
        None,
    )
}

fn press(pager: &Pager, message_id: u64, which: &str) -> Option<usize> {
    pager
        .turn(message_id, &format!("{BUTTON_PREFIX}{which}"))
        .map(|turned| turned.index)
}

/// A card turns within its pages and stops at either end rather than
/// wrapping, and each turn shows the page it lands on.
#[test]
fn a_card_turns_within_its_pages_and_stops_at_the_ends() {
    let pager = Pager::new(still);
    pager.hold(7, &three_pages());

    assert_eq!(press(&pager, 7, "prev"), Some(0));
    assert_eq!(press(&pager, 7, "next"), Some(1));
    assert_eq!(press(&pager, 7, "next"), Some(2));
    let last = pager.turn(7, &format!("{BUTTON_PREFIX}next")).unwrap();
    assert_eq!((last.index, last.count), (2, 3));
    assert_eq!(last.shown.description, "all about three");
    assert!(last.shown.footer.unwrap().contains("page 3 of 3: three"));
}

/// Nothing is held for an ordinary card, a card held past an hour is gone,
/// and an id that names no turn turns nothing.
#[test]
fn an_ordinary_or_expired_card_or_a_strange_button_turns_nothing() {
    NOW.store(1_000, Ordering::SeqCst);
    let pager = Pager::new(now);
    let ordinary = Card::paged(
        CardColor::Ready,
        "one page".to_owned(),
        vec![CardPage {
            label: "only".to_owned(),
            description: "all of it".to_owned(),
            fields: Vec::new(),
        }],
        None,
    );
    pager.hold(1, &ordinary);
    pager.hold(2, &three_pages());

    assert_eq!(press(&pager, 1, "next"), None);
    assert!(pager.turn(2, "something-else:next").is_none());
    assert!(pager.turn(2, &format!("{BUTTON_PREFIX}sideways")).is_none());

    NOW.store(1_000 + 60 * 60 * 1000, Ordering::SeqCst);
    assert_eq!(press(&pager, 2, "next"), None);
}
