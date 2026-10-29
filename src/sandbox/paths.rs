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

/// Copies the directory at `relative` beneath `root` to `destination`,
/// following no link to reach it or anything in it.
///
/// For a tree the session can change while it is read. Every entry is opened
/// through the descriptor of the directory holding it, with links refused by
/// the kernel, and what it is is asked of the open descriptor, so nothing can
/// be swapped in between a look and an open. A link, a pipe, a socket or a
/// device is left out rather than followed or read, which can only leave the
/// copy short of something, never holding what was elsewhere, and so is an
/// entry that goes while the copy is made. A tree nested deeper than `depth`,
/// or holding more than `max_bytes`, is refused: the session can grow it as
/// fast as it is read, and sparse files cost it nothing.
pub fn copy_tree_beneath(
    root: &str,
    relative: &str,
    destination: &Path,
    depth: usize,
    max_bytes: u64,
) -> std::io::Result<()> {
    let top = open_beneath(root, relative, &OpenOptions::directory())?;
    let mut budget = max_bytes;
    copy_directory(&top, destination, depth, &mut budget)
}

fn copy_directory(
    directory: &File,
    destination: &Path,
    depth: usize,
    budget: &mut u64,
) -> std::io::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(pinned_path(directory))? {
        let name = entry?.file_name();
        let how = OpenHow::new()
            .flags(OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC)
            .resolve(ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_SYMLINKS);
        let opened = match openat2(directory, name.as_os_str(), how) {
            Ok(opened) => File::from(opened),
            // A link, a socket, and an entry that went since it was listed.
            Err(
                nix::errno::Errno::ELOOP | nix::errno::Errno::ENXIO | nix::errno::Errno::ENOENT,
            ) => {
                continue;
            }
            Err(errno) => return Err(std::io::Error::from_raw_os_error(errno as i32)),
        };
        let kind = opened.metadata()?.file_type();
        let target = destination.join(&name);
        if kind.is_dir() {
            if depth == 0 {
                return Err(std::io::Error::other(
                    "the tree nests deeper than a copy is made of",
                ));
            }
            copy_directory(&opened, &target, depth - 1, budget)?;
        } else if kind.is_file() {
            let mut copy = File::create_new(&target)?;
            let copied = std::io::copy(&mut (&opened).take(budget.saturating_add(1)), &mut copy)?;
            if copied > *budget {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    "the tree holds more than a copy is made of",
                ));
            }
            *budget -= copied;
        }
    }
    Ok(())
}

/// Whether this host's kernel enforces containment during path resolution.
///
/// `openat2` arrived in Linux 5.6. Without it every read the daemon makes on a
/// session's behalf fails, which would show up as a session that cannot read
/// its own project and notes that are never harvested. Asked once at startup
/// so the answer is a refusal to start rather than a puzzle.
pub fn containment_is_enforced() -> bool {
    !matches!(
        open_beneath("/", ".", &OpenOptions::read()),
        Err(error) if error.raw_os_error() == Some(nix::errno::Errno::ENOSYS as i32)
    )
}

/// Names an already-open file by its descriptor, so it can be reached again
/// without resolving its path a second time.
///
/// The descriptor is pinned to one file for as long as it is held, whatever
/// happens to the name it was opened under. Reading the name again is what
/// this exists to avoid.
pub fn pinned_path(file: &File) -> String {
    use std::os::fd::AsRawFd;
    format!("/proc/self/fd/{}", file.as_raw_fd())
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
    /// Opens an existing file for reading, without waiting.
    ///
    /// `O_NONBLOCK` changes nothing for a file or a directory. A pipe the
    /// agent planted at the name opens at once, instead of blocking until
    /// something opens it to write: a caller that checks for a file refuses
    /// it, and one that reads it whole gets nothing or an error rather than
    /// waiting.
    pub fn read() -> Self {
        Self {
            flags: (OFlag::O_RDONLY | OFlag::O_NONBLOCK).bits(),
            mode: None,
        }
    }

    /// Opens an existing directory, to read what it holds.
    pub fn directory() -> Self {
        Self {
            flags: (OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC).bits(),
            mode: None,
        }
    }

    /// Opens for writing, creating when absent and emptying what is there.
    ///
    /// Without waiting, as [`read`] does: a pipe the agent planted at the name
    /// fails to open at once when nothing reads it, rather than holding the
    /// daemon until something does.
    ///
    /// [`read`]: OpenOptions::read
    pub fn truncate() -> Self {
        Self::truncate_mode(0o600)
    }

    /// Opens for writing as [`truncate`] does, with the given permissions.
    ///
    /// [`truncate`]: OpenOptions::truncate
    pub fn truncate_mode(mode: u32) -> Self {
        Self {
            flags: (OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC | OFlag::O_NONBLOCK).bits(),
            mode: Some(Mode::from_bits_truncate(mode)),
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

/// The most a daemon read of a session-written file takes in, for the
/// readers with no tighter bound of their own.
///
/// Notes, requests and the policy are a few kilobytes. The agent can grow any
/// of them, so a read with no bound is one way to make the daemon hold a file
/// the size of the disk.
pub const MAX_READ_BYTES: u64 = 1024 * 1024;

/// Reads a file beneath `root` as text, following no symlink to get there,
/// and refuses one longer than `limit` bytes with `FileTooLarge`, a kind
/// of its own so a caller can tell it from text that is not UTF-8.
///
/// Read through the bound rather than measured first, since the file can grow
/// between a measurement and the read.
pub fn read_beneath(root: &str, relative: &str, limit: u64) -> std::io::Result<String> {
    let file = open_beneath(root, relative, &OpenOptions::read())?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    // Measured before the text is checked, so a file cut by the bound in the
    // middle of a character is still refused for its length.
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            format!("{relative} is longer than the {limit} byte read limit"),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Empties a file beneath `root`, following no symlink to get there.
pub fn truncate_beneath(root: &str, relative: &str) -> std::io::Result<()> {
    open_beneath(root, relative, &OpenOptions::truncate()).map(|_| ())
}

/// Writes `bytes` to a file beneath `root`, replacing what was there.
///
/// The daemon writes into trees a session can also write, so the name is
/// resolved by the kernel rather than trusted: a link planted at it redirects
/// nothing.
pub fn write_beneath(root: &str, relative: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = open_beneath(root, relative, &OpenOptions::truncate())?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests;
