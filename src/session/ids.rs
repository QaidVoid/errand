//! Session identifiers: how one is drawn, and what it reads as.
//!
//! An id is not only a key. It names the session's state directory, it names
//! the sandbox, and where a public interface is configured it is the whole of
//! the address a transcript is served at. That last one makes it a capability
//! rather than a label, which is why it is drawn from the system's random
//! source and not from a pseudo-random generator.

use crate::session::projects::ProjectSelection;

/// Digits an id is written in: lower case, so it survives a case-blind path.
const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Characters of randomness in an id.
///
/// Twelve gives about 62 bits. Collision alone would have been satisfied by
/// half of that, so the length is chosen against guessing instead: where
/// `web.publicUrl` is set the id is the entire address a transcript is served
/// at, nothing authenticates that address, and those addresses get published
/// in pull request descriptions. At twelve, a first collision is some 2.7
/// billion sessions away and an id costs about 15 million years to find at ten
/// thousand guesses a second.
pub const TOKEN_LENGTH: usize = 12;

/// Fresh randomness for one session.
///
/// Values at or above 252 are redrawn rather than folded, because 256 is not a
/// multiple of 36 and folding them would make the first four digits turn up
/// more often than the rest. Skewed digits are the difference between the
/// entropy this claims and the entropy it has.
pub fn session_token(length: usize) -> String {
    let mut out = String::with_capacity(length);
    let mut byte = [0_u8; 1];
    while out.len() < length {
        getrandom::fill(&mut byte).expect("the system random source answers");
        let value = byte[0];
        if value < 252 {
            out.push(ALPHABET[(value % 36) as usize] as char);
        }
    }
    out
}

/// The id a session is known by, which says where it is working.
///
/// A named project is written into the id, so `errand-6a82hff` says which tree
/// the transcript belongs to without anybody having to look it up. An unnamed
/// session is its token alone: its project directory is named after the token
/// already, and `6a82hff-6a82hff` says nothing twice.
///
/// The token is always present and always last, so two sessions in one project
/// stay distinct and nothing has to parse the id to tell them apart.
pub fn session_id(project: &ProjectSelection, token: &str) -> String {
    if project.was_explicit {
        format!("{}-{token}", project.name)
    } else {
        token.to_owned()
    }
}

#[cfg(test)]
mod tests;
