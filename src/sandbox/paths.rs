//! Whether a path stays inside the directory a session is confined to.
//!
//! One rule, used everywhere a path arrives from outside: from the agent, from
//! a delegation, from a chat message. A second rule written slightly
//! differently is how a containment check ends up being true in one place and
//! false in another.
//!
//! [`within`] answers about the spelling of a path. It cannot answer about a
//! symlink, because a name says nothing about what it points at, and between
//! asking and opening the answer can change. Anything the daemon actually
//! reads or writes on a session's behalf goes through [`open_beneath`], which
//! asks the kernel to enforce containment while it resolves.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use nix::fcntl::{OFlag, OpenHow, ResolveFlag, openat2};
use nix::sys::stat::Mode;

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
    // Compared by component rather than by prefix, so `/srv/projects-old`
    // does not read as being inside `/srv/projects`.
    target
        .starts_with(&base)
        .then(|| target.to_string_lossy().into_owned())
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

/// Opens a path beneath `root`, refusing to leave it and refusing to follow a
/// symlink on the way.
///
/// The daemon runs outside the sandbox and a session can write inside it, so a
/// path the daemon opens on a session's behalf is a path a session can have
/// prepared. Checking the spelling first and opening afterwards leaves a
/// window in which a plain file becomes a link to somewhere else; `openat2`
/// closes it by resolving under the kernel's own rules, once.
///
/// `RESOLVE_BENEATH` refuses to climb out of `root` however the path is
/// written. `RESOLVE_NO_SYMLINKS` refuses every symlink, including one whose
/// target would have been inside: a session with something to say about a file
/// can say it in the file.
///
/// `relative` is resolved against `root`, and an absolute one is read as
/// though it were relative to it, which is the same rule [`within`] applies.
pub fn open_beneath(root: &str, relative: &str, options: &OpenOptions) -> std::io::Result<File> {
    let root_dir = File::open(root)?;
    let wanted = normalize(relative);
    let wanted = wanted.trim_start_matches('/');
    let wanted = if wanted.is_empty() { "." } else { wanted };

    let mut how = OpenHow::new()
        .flags(OFlag::from_bits_truncate(options.flags))
        .resolve(ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_SYMLINKS);
    if let Some(mode) = options.mode {
        how = how.mode(mode);
    }

    match openat2(&root_dir, wanted, how) {
        Ok(opened) => Ok(File::from(opened)),
        Err(errno) => Err(std::io::Error::from_raw_os_error(errno as i32)),
    }
}

/// How [`open_beneath`] should open the file.
///
/// A narrow stand-in for [`std::fs::OpenOptions`], which cannot describe an
/// `openat2` call. Only what the daemon actually asks for is here.
#[derive(Debug, Clone, Copy)]
pub struct OpenOptions {
    flags: i32,
    mode: Option<Mode>,
}

impl OpenOptions {
    /// Opens an existing file for reading.
    pub fn read() -> Self {
        Self {
            flags: OFlag::O_RDONLY.bits(),
            mode: None,
        }
    }

    /// Opens for writing, creating when absent and emptying what is there.
    pub fn truncate() -> Self {
        Self {
            flags: (OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC).bits(),
            mode: Some(Mode::from_bits_truncate(0o600)),
        }
    }

    /// Creates a file, failing when anything is already at the name.
    ///
    /// `O_EXCL` refuses a name that is already taken, including one taken by
    /// a link, so the caller never has to ask first and then hope.
    pub fn create_new() -> Self {
        Self {
            flags: (OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL).bits(),
            mode: Some(Mode::from_bits_truncate(0o600)),
        }
    }
}

/// Reads a file beneath `root` as text, following no symlink to get there.
pub fn read_beneath(root: &str, relative: &str) -> std::io::Result<String> {
    let mut file = open_beneath(root, relative, &OpenOptions::read())?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// Empties a file beneath `root`, following no symlink to get there.
pub fn truncate_beneath(root: &str, relative: &str) -> std::io::Result<()> {
    open_beneath(root, relative, &OpenOptions::truncate()).map(|_| ())
}

#[cfg(test)]
mod tests;
