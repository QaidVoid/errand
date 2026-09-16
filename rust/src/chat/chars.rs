//! The enumerated chat character table.
//!
//! This is the complete set of non-ASCII characters the system emits. Each
//! entry names exactly one state. None is decoration, and using one for a
//! state it does not name is a bug.
//!
//! Entries are declared as codepoints rather than as literal glyphs, so this
//! file is itself ASCII and needs no exception from the rule. An editor or a
//! terminal that cannot render an emoji therefore cannot silently corrupt one.

/// One enumerated character: how it is spelled, and the single state it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChatChar {
    /// Codepoints in `U+XXXX` form, in order.
    pub codepoints: &'static [&'static str],
    /// The Unicode name.
    pub name: &'static str,
    /// The one state this character denotes.
    pub meaning: &'static str,
}

/// A reaction placed on the sender's own message, tracking that message's
/// fate. Exactly one is present at a time; an outcome replaces the
/// acknowledgement rather than joining it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionKey {
    /// Accepted, queued or running.
    Accepted,
    /// The turn it started completed.
    Succeeded,
    /// The turn failed, or the message was rejected.
    Failed,
    /// The turn was interrupted.
    Interrupted,
}

impl ReactionKey {
    /// The table entry the reaction is rendered from.
    pub fn entry(self) -> ChatChar {
        match self {
            ReactionKey::Accepted => ChatChar {
                codepoints: &["U+23F3"],
                name: "hourglass not done",
                meaning: "accepted, queued or running",
            },
            ReactionKey::Succeeded => ChatChar {
                codepoints: &["U+2705"],
                name: "white heavy check mark",
                meaning: "the turn it started completed",
            },
            ReactionKey::Failed => ChatChar {
                codepoints: &["U+274C"],
                name: "cross mark",
                meaning: "the turn failed, or the message was rejected",
            },
            ReactionKey::Interrupted => ChatChar {
                codepoints: &["U+23F9", "U+FE0F"],
                name: "stop button",
                meaning: "the turn was interrupted",
            },
        }
    }

    /// Renders the reaction the outcome asks for.
    pub fn glyph(self) -> String {
        glyph(&self.entry())
    }
}

/// The leading glyph of a status line the bot posts in a thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixKey {
    /// A tool call started.
    Tool,
    /// The agent is thinking.
    Thinking,
    /// The agent is asking the user something.
    Question,
    /// Degraded state: shared workspace, backend gap, provider backoff.
    Warning,
    /// A question about one artefact was sent to a cheaper model.
    Delegated,
    /// Connection or session lifecycle changed.
    Connection,
}

pub const ALL_PREFIXES: [PrefixKey; 6] = [
    PrefixKey::Tool,
    PrefixKey::Thinking,
    PrefixKey::Question,
    PrefixKey::Warning,
    PrefixKey::Delegated,
    PrefixKey::Connection,
];

impl PrefixKey {
    /// The table entry the prefix is rendered from.
    pub fn entry(self) -> ChatChar {
        match self {
            PrefixKey::Tool => ChatChar {
                codepoints: &["U+1F527"],
                name: "wrench",
                meaning: "a tool call started",
            },
            PrefixKey::Thinking => ChatChar {
                codepoints: &["U+1F4AD"],
                name: "thought balloon",
                meaning: "the agent is thinking",
            },
            PrefixKey::Question => ChatChar {
                codepoints: &["U+2753"],
                name: "question mark",
                meaning: "the agent is asking the user something",
            },
            PrefixKey::Warning => ChatChar {
                codepoints: &["U+26A0", "U+FE0F"],
                name: "warning sign",
                meaning: "degraded state: shared workspace, backend gap, provider backoff",
            },
            PrefixKey::Delegated => ChatChar {
                codepoints: &["U+1F4E4"],
                name: "outbox tray",
                meaning: "a question about one artefact was sent to a cheaper model",
            },
            PrefixKey::Connection => ChatChar {
                codepoints: &["U+1F50C"],
                name: "electric plug",
                meaning: "connection or session lifecycle changed",
            },
        }
    }

    /// Renders the glyph alone.
    pub fn glyph(self) -> String {
        glyph(&self.entry())
    }

    /// Leads a status line with the glyph for its state.
    pub fn prefixed(self, text: &str) -> String {
        prefixed(self, text)
    }
}

/// Every enumerated character, reactions and prefixes together.
pub fn all_chars() -> Vec<ChatChar> {
    let mut all = Vec::new();
    let reactions = [
        ReactionKey::Accepted,
        ReactionKey::Succeeded,
        ReactionKey::Failed,
        ReactionKey::Interrupted,
    ];
    all.extend(reactions.map(ReactionKey::entry));
    all.extend(ALL_PREFIXES.map(PrefixKey::entry));
    all
}

fn parse_codepoint(codepoint: &str) -> u32 {
    let Some(digits) = codepoint.strip_prefix("U+") else {
        panic!("malformed codepoint {codepoint}; expected U+XXXX");
    };
    assert!(
        !digits.is_empty() && digits.len() <= 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()),
        "malformed codepoint {codepoint}; expected U+XXXX"
    );
    u32::from_str_radix(digits, 16).expect("hex digits")
}

/// Renders an entry to the string the chat service actually receives.
pub fn glyph(entry: &ChatChar) -> String {
    entry
        .codepoints
        .iter()
        .map(|codepoint| char::from_u32(parse_codepoint(codepoint)).expect("a valid codepoint"))
        .collect()
}

/// Renders a reaction by name.
pub fn reaction(key: ReactionKey) -> String {
    key.glyph()
}

/// Prefixes a status line with its enumerated glyph.
pub fn prefixed(key: PrefixKey, text: &str) -> String {
    format!("{} {text}", key.glyph())
}

#[cfg(test)]
mod tests;
