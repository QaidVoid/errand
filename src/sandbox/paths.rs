//! Whether a path stays inside the directory a session is confined to.
//!
//! One rule, used everywhere a path arrives from outside: from the agent, from
//! a delegation, from a chat message. A second rule written slightly
//! differently is how a containment check ends up being true in one place and
//! false in another.

use std::path::{Component, Path, PathBuf};

/// Resolves `path` against `base`, lexically, which is what a containment
/// check compares against.
fn resolve_against(base: &Path, path: &str) -> PathBuf {
    let wanted = Path::new(path);
    let absolute = if wanted.is_absolute() {
        wanted.to_path_buf()
    } else {
        base.join(wanted)
    };
    let mut parts: Vec<Component> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::RootDir) | None => {}
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(_) => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    let mut resolved = PathBuf::new();
    for part in parts {
        resolved.push(part.as_os_str());
    }
    resolved
}

/// Removes `.` and `..` segments, which is what the resolution does before a
/// path is compared against the root.
fn normalize(path: &str) -> String {
    let mut parts: Vec<Component> = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(parts.last(), Some(Component::Normal(_))) {
                    parts.pop();
                } else if !parts.is_empty() || component != Component::RootDir {
                    parts.push(component);
                }
            }
            other => parts.push(other),
        }
    }
    let mut normalized = PathBuf::new();
    for part in parts {
        normalized.push(part.as_os_str());
    }
    normalized.to_string_lossy().into_owned()
}

/// Resolves `wanted` against `root` and returns it only if it stays inside.
///
/// Traversal is removed before the comparison rather than searched for, so a
/// path does not have to be recognised as hostile to be refused. The root
/// itself counts as inside.
///
/// Returns the resolved absolute path, or nothing when it escapes.
pub fn within(root: &str, wanted: &str) -> Option<String> {
    let base = super::resolve_root(root);
    let target = resolve_against(Path::new(&base), &normalize(wanted));
    let target = target.to_string_lossy().into_owned();
    if target == base {
        return Some(target);
    }
    target.starts_with(&format!("{base}/")).then_some(target)
}

/// Translates a path as the agent sees it into a path on the host.
///
/// A leading separator does not mean the host's root. An absolute path that is
/// not already inside the workspace is read as project-relative, so
/// `/etc/passwd` resolves to a file of that name inside the project rather
/// than to the host's. That keeps one rule whether the agent sees host paths
/// or a mount point.
///
/// Returns the host path, or nothing when it would leave the project.
pub fn host_path_under(workspace: &str, project_path: &str, requested: &str) -> Option<String> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return None;
    }

    let stripped = trimmed.strip_prefix(workspace).unwrap_or(trimmed);
    let relative = stripped.trim_start_matches('/');
    let relative = if relative.is_empty() { "." } else { relative };

    within(project_path, relative)
}

#[cfg(test)]
mod tests;
