//! The operator's standing instructions, given to every session.
//!
//! A project says how to work on that project, in its own `AGENTS.md`, and the
//! agent reads that by itself. This is the other half: what the person running
//! the daemon wants of every session, whatever repository it is working in and
//! whether or not that repository says anything at all.
//!
//! The text is carried in the block already appended to the agent's system
//! prompt rather than written into the agent's configuration directory. That
//! directory's name belongs to the agent and is overridable, so writing there
//! would be a guess that fails silently; the appended block is a path errand
//! passes itself and can be sure of.

/// Heading the rules are given under, so the agent can tell them apart.
pub const RULES_HEADING: &str = "## House rules";

/// The rules as they reach the agent, or none when there are none.
///
/// Whitespace-only counts as none. A file that has been emptied is an operator
/// saying there are no rules, and a heading over nothing reads as an omission.
pub fn rules_block(contents: &str) -> Option<String> {
    let text = contents.trim();
    if text.is_empty() {
        return None;
    }
    Some(
        [
            RULES_HEADING,
            "",
            "These come from the person running this daemon and apply to every",
            "session. Where they and a project's own instructions disagree, say so",
            "rather than choosing one in silence.",
            "",
            text,
            "",
            "",
        ]
        .join("\n"),
    )
}

/// The rules as written, so they can be scrubbed back out of what is reported.
///
/// The block is handed to the agent as a file, because that is how a system
/// prompt is appended, and that file necessarily sits where the agent can read
/// it. So a session that reads its own prompt back, out of curiosity or by
/// listing its state directory, would otherwise put the operator's rules into
/// the channel and the transcript. They are instructions rather than secrets,
/// but they are the operator's and not the thread's, and the same reporting
/// path is already wrapped for scrubbing.
///
/// Read fresh rather than held, so editing the file reaches the next session
/// the same way the rules themselves do.
///
/// Returns nothing when there are no rules, which is not a failure: a scrub
/// list with nothing in it simply scrubs nothing.
pub fn house_rules_text(path: Option<&str>) -> Option<String> {
    let text = std::fs::read_to_string(path?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests;
