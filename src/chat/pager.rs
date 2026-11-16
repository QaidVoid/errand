//! Cards turned a page at a time: which page each posted card shows, and the
//! buttons that turn it.
//!
//! A paged card is held here by the message that shows it, for an hour, so
//! a click finds its pages again. A click after that, or after a restart,
//! finds nothing, and the clicker is told to ask again.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serenity::builder::{CreateActionRow, CreateButton};
use serenity::model::application::ButtonStyle;

use crate::session::event::Card;

/// What a page button's id starts with, so a click is known for one.
pub const BUTTON_PREFIX: &str = "errand-page:";

/// How long a posted card stays turnable, in milliseconds.
const KEEP_MS: i64 = 60 * 60 * 1000;

/// The most cards held at once. The one held longest goes first.
const MOST_HELD: usize = 256;

/// One posted card and the page it shows.
struct Held {
    card: Card,
    page: usize,
    since: i64,
}

/// The page a turned card now shows.
pub struct Turned {
    /// The card as that page alone shows it.
    pub shown: Card,
    /// Which page it is, from zero.
    pub index: usize,
    /// How many pages the card has.
    pub count: usize,
}

/// Where each posted paged card stands.
pub struct Pager {
    held: Mutex<BTreeMap<u64, Held>>,
    now: fn() -> i64,
}

impl Pager {
    /// An empty pager on the clock `now`, in epoch milliseconds.
    pub fn new(now: fn() -> i64) -> Self {
        Self {
            held: Mutex::new(BTreeMap::new()),
            now,
        }
    }

    /// Remembers `card` as shown by `message_id`, on its first page.
    ///
    /// An ordinary card has nothing to turn, and is not held.
    pub fn hold(&self, message_id: u64, card: &Card) {
        self.hold_at(message_id, card, 0);
    }

    /// Remembers `card` as shown by `message_id`, on page `page`. A card held
    /// again, as one that grows does, starts its hour over.
    pub fn hold_at(&self, message_id: u64, card: &Card, page: usize) {
        if card.page_count() < 2 {
            return;
        }
        let now = (self.now)();
        let mut held = self.held.lock().expect("the pager lock");
        held.retain(|_, entry| now - entry.since < KEEP_MS);
        while held.len() >= MOST_HELD {
            let Some(oldest) = held
                .iter()
                .min_by_key(|(_, entry)| entry.since)
                .map(|(id, _)| *id)
            else {
                break;
            };
            held.remove(&oldest);
        }
        held.insert(
            message_id,
            Held {
                card: card.clone(),
                page: page.min(card.page_count() - 1),
                since: now,
            },
        );
    }

    /// Turns the card `message_id` shows as the button `button_id` says.
    ///
    /// Nothing when the card is no longer held, or the id names no turn.
    pub fn turn(&self, message_id: u64, button_id: &str) -> Option<Turned> {
        let now = (self.now)();
        let mut held = self.held.lock().expect("the pager lock");
        let entry = held.get_mut(&message_id)?;
        if now - entry.since >= KEEP_MS {
            held.remove(&message_id);
            return None;
        }
        let count = entry.card.page_count();
        entry.page = match button_id.strip_prefix(BUTTON_PREFIX)? {
            "prev" => entry.page.saturating_sub(1),
            "next" => (entry.page + 1).min(count - 1),
            _ => return None,
        };
        Some(Turned {
            shown: entry.card.page(entry.page),
            index: entry.page,
            count,
        })
    }
}

/// The buttons under page `index` of `count`, each off where it leads
/// nowhere.
pub fn buttons(index: usize, count: usize) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{BUTTON_PREFIX}prev"))
            .label("prev")
            .style(ButtonStyle::Secondary)
            .disabled(index == 0),
        CreateButton::new(format!("{BUTTON_PREFIX}next"))
            .label("next")
            .style(ButtonStyle::Secondary)
            .disabled(index + 1 >= count),
    ])]
}

#[cfg(test)]
mod tests;
