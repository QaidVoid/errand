//! Resolves a project from the first message of a session.
//!
//! A message may name a project as a `name:` prefix. The name becomes a
//! directory under the configured root, created on first use. Without a prefix
//! a session works in a directory of its own.
//!
//! This decides which directory an agent gets write access to, so a name that
//! is not a plain single path segment is rejected outright rather than
//! sanitised. Resolution is then checked against the root a second time,
//! because a name can be harmless while the path it lands on is a symlink
//! pointing elsewhere.

use std::path::{Path, PathBuf};

/// A valid project name: one path segment, no separators, no leading dot.
///
/// Excluding `.` as a first character is what makes `.` and `..`
/// unrepresentable, and the character class excludes both separators so a name
/// can never span segments.
pub fn is_valid_project_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    match bytes.first() {
        Some(first) if first.is_ascii_alphanumeric() || *first == b'_' => {}
        _ => return false,
    }
    bytes.len() <= 64
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The project a session will run in, and the prompt with any prefix removed.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSelection {
    /// Project name, which is also its directory name under the root.
    pub name: String,
    /// Absolute host path. Always inside the configured root.
    pub path: String,
    /// The first prompt, with a recognised project prefix stripped.
    pub prompt: String,
    /// Whether the prefix named the project explicitly.
    pub was_explicit: bool,
}

/// Raised when a resolved project path would land outside the root.
#[derive(Debug, thiserror::Error)]
#[error("project {name} resolves to {path}, which is outside the project root {root}")]
pub struct ProjectEscapeError {
    /// The project that was asked for.
    pub name: String,
    /// Where it actually resolved to.
    pub path: String,
    /// The root it had to stay inside.
    pub root: String,
}

/// Matches a leading `name:` on the first line, before any validation.
///
/// The part after the colon is what keeps a URL out. `https://example.com`
/// opens with something that is a perfectly valid project name followed by a
/// colon, so without it a message that begins with a link selects a project
/// called `https`, and every later message beginning with a link is refused
/// because that project already has a live session. A colon followed
/// immediately by two slashes is a scheme, never a name. `note: //TODO` still
/// selects `note`, because there the slashes are not what follows the colon.
fn prefix_match(message: &str) -> Option<(String, usize)> {
    let mut matched = 0;
    let mut end = 0;
    for character in message.chars() {
        if matched >= 64 || character.is_whitespace() || matches!(character, ':' | '/' | '\\') {
            break;
        }
        matched += 1;
        end += character.len_utf8();
    }
    if matched == 0 || !message[end..].starts_with(':') {
        return None;
    }
    let after_colon = end + 1;
    if message[after_colon..].starts_with("//") {
        return None;
    }
    let whitespace = message[after_colon..]
        .find(|character: char| !character.is_whitespace())
        .unwrap_or(message.len() - after_colon);
    Some((message[..end].to_owned(), after_colon + whitespace))
}

/// Chooses the project for a session, without touching the filesystem.
///
/// A named project is a place to come back to: the same name reaches the same
/// directory across sessions. An unnamed one gets a directory of its own, so
/// casual work never lands on top of another session's, or your own from an
/// hour ago.
///
/// A prefix that is not a valid project name is left as ordinary prompt text,
/// so a malformed name can never redirect a session rather than merely failing
/// to select one.
pub fn select_project(message: &str, root: &str, fallback_name: &str) -> ProjectSelection {
    if let Some((candidate, prefix_len)) = prefix_match(message)
        && is_valid_project_name(&candidate)
    {
        return ProjectSelection {
            path: join(root, &candidate),
            name: candidate.clone(),
            prompt: message[prefix_len..].trim().to_owned(),
            was_explicit: true,
        };
    }

    ProjectSelection {
        name: fallback_name.to_owned(),
        path: join(root, fallback_name),
        prompt: message.trim().to_owned(),
        was_explicit: false,
    }
}

fn join(root: &str, name: &str) -> String {
    Path::new(root).join(name).to_string_lossy().into_owned()
}

/// Creates the project directory if it does not exist and confirms it really
/// is inside the root.
///
/// The containment check runs after resolution, not before: the name can be a
/// clean single segment while the directory it names is a symlink out of the
/// root, and only the resolved path reveals that.
///
/// Returns [`ProjectEscapeError`] when the resolved path is outside the root.
pub fn ensure_project_directory(
    selection: &ProjectSelection,
    root: &str,
) -> Result<(), ProjectEscapeError> {
    std::fs::create_dir_all(&selection.path).map_err(|_| ProjectEscapeError {
        name: selection.name.clone(),
        path: selection.path.clone(),
        root: root.to_owned(),
    })?;

    let real_root = std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    let real_path =
        std::fs::canonicalize(&selection.path).unwrap_or_else(|_| PathBuf::from(&selection.path));
    let real_root = real_root.to_string_lossy().into_owned();
    let real_path = real_path.to_string_lossy().into_owned();

    if real_path != real_root && !real_path.starts_with(&format!("{real_root}/")) {
        return Err(ProjectEscapeError {
            name: selection.name.clone(),
            path: real_path,
            root: real_root,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
