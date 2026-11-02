//! Names an issue or pull request as a thread a session lives in.
//!
//! A chat thread is named by the service's own id. An issue is not in that
//! service, so its thread is named `<forge>:<owner>/<repo>#<number>`, which
//! no chat id can ever be, and so where a thread lives is read off its name.
//! The forge is in the name because a daemon may serve one forge now and
//! another later, and the two must not read as each other's threads.

use crate::config::schema::ForgeKind;
use crate::forge;

/// Every prefix a thread or account may have been named with, past and
/// present, so a name from an earlier configuration is still read as what
/// it is rather than as a chat id that looks like one.
pub const PREFIXES: [&str; 2] = ["github:", "forgejo:"];

/// The thread an issue or pull request is.
pub fn thread_id(repository: &str, number: u64, kind: ForgeKind) -> String {
    format!("{}{repository}#{number}", forge::thread_prefix(kind))
}

/// Whether a thread lives on a forge rather than in the chat service.
pub fn is_forge_thread(thread_id: &str) -> bool {
    PREFIXES.iter().any(|prefix| thread_id.starts_with(prefix))
}

/// The repository, as `owner/repo`, and the number a thread names.
///
/// Either forge's spelling is accepted, since the links written down last
/// run outlive a change of forge, and an old name still names one issue.
pub fn issue_of(thread_id: &str) -> Option<(&str, u64)> {
    let prefix = PREFIXES
        .iter()
        .find(|prefix| thread_id.starts_with(**prefix))?;
    let (repository, number) = thread_id[prefix.len()..].rsplit_once('#')?;
    let (owner, name) = repository.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((repository, number.parse().ok()?))
}

/// How a forge account is told apart from a chat account.
pub fn account_id(login: &str, kind: ForgeKind) -> String {
    format!("{}{}", forge::thread_prefix(kind), login.to_lowercase())
}

/// Writes every forge account named the way the chat names an account,
/// `<@github:login>` or `<@forgejo:login>`, as the bare login, for a surface
/// that cannot mention it. Without the `@`, since there it would read as a
/// mention that failed.
pub fn logins_plainly(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((start, prefix)) = PREFIXES
        .iter()
        .filter_map(|prefix| {
            let marker = format!("<@{prefix}");
            rest.find(&marker).map(|at| (at, prefix))
        })
        .min_by_key(|(at, _)| *at)
    {
        let marker_len = prefix.len() + 2;
        let after = &rest[start + marker_len..];
        let Some(end) = after.find('>') else {
            break;
        };
        plain.push_str(&rest[..start]);
        plain.push_str(&after[..end]);
        rest = &after[end + 1..];
    }
    plain.push_str(rest);
    plain
}

#[cfg(test)]
mod tests;
