//! What a session says to whichever surface is showing it.
//!
//! The TypeScript `port.ts` declares a wide interface of calls. In this port
//! it collapses into one `SessionEvent` enum (group 8); the types here are the
//! ones other modules already consume.

/// One delegated question and what came of it.
///
/// A session's model may ask a cheaper one about a single artefact. Where the
/// session shows one line and an interface shows the question, the answer, and
/// what it saved.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Delegated {
    /// What the session's model wanted to know.
    pub question: String,
    /// The model that answered, or none when none was asked.
    pub model: Option<String>,
    /// What it was shown, for attributing the answer.
    pub describes: Option<String>,
    /// What it said, absent when it was refused.
    pub answer: Option<String>,
    /// Why nothing was asked, absent when something was.
    pub refused: Option<String>,
    /// Tokens the delegated model was charged, when the provider said.
    pub tokens: Option<i64>,
    /// Characters kept out of the session's own context by asking.
    pub kept_out: Option<usize>,
}
