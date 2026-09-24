//! Names an issue or pull request as a thread a session lives in.
//!
//! A chat thread is named by the service's own id. An issue is not in that
//! service, so its thread is named `github:<owner>/<repo>#<number>`, which no
//! chat id can ever be, and so where a thread lives is read off its name.

/// What starts the name of every thread on GitHub.
const PREFIX: &str = "github:";

/// The thread an issue or pull request is.
pub fn thread_id(repository: &str, number: u64) -> String {
    format!("{PREFIX}{repository}#{number}")
}

/// Whether a thread lives on GitHub rather than in the chat service.
pub fn is_github_thread(thread_id: &str) -> bool {
    thread_id.starts_with(PREFIX)
}

/// The repository, as `owner/repo`, and the number a thread names.
pub fn issue_of(thread_id: &str) -> Option<(&str, u64)> {
    let (repository, number) = thread_id.strip_prefix(PREFIX)?.rsplit_once('#')?;
    let (owner, name) = repository.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((repository, number.parse().ok()?))
}

/// How a GitHub account is told apart from a chat account.
pub fn account_id(login: &str) -> String {
    format!("{PREFIX}{}", login.to_lowercase())
}

/// Writes every GitHub account named the way the chat names an account,
/// `<@github:login>`, as `@login`, for a surface that cannot mention it.
pub fn logins_plainly(text: &str) -> String {
    let marker = format!("<@{PREFIX}");
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&marker) {
        let after = &rest[start + marker.len()..];
        let Some(end) = after.find('>') else {
            break;
        };
        plain.push_str(&rest[..start]);
        plain.push('@');
        plain.push_str(&after[..end]);
        rest = &after[end + 1..];
    }
    plain.push_str(rest);
    plain
}

#[cfg(test)]
mod tests;
