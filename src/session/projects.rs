//! The project a session works in.
//!
//! Every session works in a directory of its own under the configured root,
//! named after its id and created on first use. A thread resumed in a project
//! it was given before keeps that one, since its record says where.
//!
//! This decides which directory an agent gets write access to, so the
//! resolved path is checked against the root, because a name can be harmless
//! while the path it lands on is a symlink pointing elsewhere.

use std::path::{Path, PathBuf};

/// The project a session will run in, and its first prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSelection {
    /// Project name, which is also its directory name under the root.
    pub name: String,
    /// Absolute host path. Always inside the configured root.
    pub path: String,
    /// The first prompt.
    pub prompt: String,
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

/// The project a new session works in: a directory of its own under `root`,
/// named after its `token`, without touching the filesystem.
///
/// One of its own, so casual work never lands on top of another session's,
/// or on your own from an hour ago.
pub fn own_project(message: &str, root: &str, token: &str) -> ProjectSelection {
    ProjectSelection {
        name: token.to_owned(),
        path: Path::new(root).join(token).to_string_lossy().into_owned(),
        prompt: message.trim().to_owned(),
    }
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
