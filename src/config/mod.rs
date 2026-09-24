//! The daemon's configuration: its shape and defaults, the reading of the
//! file, and the refusal that turns whatever was written into a [`Config`].
//!
//! The reading and the validating are separate, so that "the file is not
//! there" and "the file says something impossible" are different failures
//! with different messages.

use std::path::{Component, Path, PathBuf};

/// How a provider definition says to ask it for its models.
pub mod discover;
/// Finds the configuration file and reads it.
pub mod load;
/// Removes secrets from anything about to leave the daemon.
pub mod redact;
/// The shape of a resolved configuration, and its defaults.
pub mod schema;
/// Reads a byte size written the way people write one.
pub mod size;
/// Turns whatever was written into a configuration, or refuses.
pub mod validate;

#[cfg(test)]
mod example_test;

/// Whether a path is written from the root, which is what an absolute path
/// means on this platform.
pub(super) fn is_absolute(path: &str) -> bool {
    Path::new(path).is_absolute()
}

/// Resolves a path against the process's working directory, lexically, which
/// is what the resolved configuration holds.
pub(super) fn resolve(path: &str) -> String {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => path.to_path_buf(),
        }
    };
    let mut parts: Vec<Component> = Vec::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                // The root absorbs a `..` beyond it.
                Some(Component::RootDir) | None => {}
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                // A `..` at the start of a relative path is kept, since
                // nothing above it has been named yet.
                Some(_) => parts.push(component),
            },
            other => parts.push(other),
        }
    }
    let mut resolved = PathBuf::new();
    for part in parts {
        resolved.push(part.as_os_str());
    }
    resolved.to_string_lossy().into_owned()
}
