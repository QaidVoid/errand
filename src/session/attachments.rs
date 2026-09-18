//! Files somebody attached to a message, taken into a session's project.
//!
//! An attachment is bytes from whoever can post in the channel, so it is
//! written where the daemon says and never where the sender says. The name is
//! derived from what was sent and then resolved by the same containment rule
//! that confines the agent, so a name aiming outside the project cannot get
//! there.
//!
//! They land under a directory of their own rather than the project root,
//! because a file handed over is an input rather than the work: an attachment
//! called README.md must not sit on top of the project's own.

use std::future::Future;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::sandbox::paths;

/// The directory attachments are written to, relative to the project.
pub const ATTACHMENTS_DIR: &str = "attachments";

/// A file as it arrived from the chat service, before anything was done.
#[derive(Debug, Clone, PartialEq)]
pub struct RawAttachment {
    /// The service's own id for the file.
    pub id: String,
    /// The name the sender's file had. Never used as a path without checking.
    pub name: String,
    /// Where to fetch it from, which the service signs and expires.
    pub url: String,
    /// How large the service says it is, in bytes.
    pub size: u64,
    /// What the service believes it is, when it says.
    pub content_type: Option<String>,
}

/// A file that was taken, and where it went.
#[derive(Debug, Clone, PartialEq)]
pub struct Taken {
    /// The path within the project, which is what the agent is told.
    pub path: String,
    /// The file's contents, held so an image can also be shown to a model.
    pub bytes: Vec<u8>,
    /// What the service believed it was, when it said.
    pub content_type: Option<String>,
}

/// A file that was not taken, and why, in words worth posting.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    /// The name the sender's file had.
    pub name: String,
    /// Why it was not taken, in words worth posting.
    pub reason: String,
}

/// What came of a message's attachments.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Outcome {
    /// Files that reached the project, in the order they arrived.
    pub taken: Vec<Taken>,
    /// Files that did not, each with its reason.
    pub refused: Vec<Refused>,
}

/// Limits on what may arrive.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Largest single file that may be taken.
    pub max_bytes: u64,
    /// How many files one message may carry.
    pub max_count: usize,
}

/// Reduces a sender's filename to one path segment.
///
/// Everything that could make it a path is removed rather than escaped,
/// because a name is a label here and never a location.
fn safe_name(name: &str) -> String {
    let bare = name
        .replace(['/', '\\'], "_")
        .trim_start_matches('.')
        .trim()
        .to_owned();
    let cleaned: String = bare
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if cleaned.is_empty() {
        "attachment".to_owned()
    } else {
        cleaned
    }
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// A path that is free, by adding a number rather than replacing what is
/// there.
///
/// An attachment silently overwriting a file would be the worst outcome of
/// somebody being helpful.
fn free_path(directory: &Path, name: &str) -> PathBuf {
    let dot = name.rfind('.');
    let extension = match dot {
        Some(at) if at > 0 => &name[at..],
        _ => "",
    };
    let stem = &name[..name.len() - extension.len()];
    let mut attempt = directory.join(name);
    let mut next = 2;
    while exists(&attempt) {
        attempt = directory.join(format!("{stem}-{next}{extension}"));
        next += 1;
    }
    attempt
}

/// Whether a file is one the model could be asked to look at.
pub fn is_image(content_type: Option<&str>, name: &str) -> bool {
    if content_type.is_some_and(|kind| kind.starts_with("image/")) {
        return true;
    }
    let lower = name.to_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp"]
        .iter()
        .any(|extension| lower.ends_with(extension))
}

/// Takes what was attached into the project.
///
/// Refusals are collected rather than thrown, so one oversized file does not
/// lose the message it came with.
pub async fn receive<F, Fut>(
    attachments: &[RawAttachment],
    project_path: &str,
    limits: Limits,
    fetch_file: F,
) -> Outcome
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>>>,
{
    let mut taken = Vec::new();
    let mut refused = Vec::new();
    let too_big = format!("larger than the {} byte limit", limits.max_bytes);

    for (index, file) in attachments.iter().enumerate() {
        if index >= limits.max_count {
            refused.push(Refused {
                name: file.name.clone(),
                reason: format!("more than {} file(s) on one message", limits.max_count),
            });
            continue;
        }

        if file.size > limits.max_bytes {
            refused.push(Refused {
                name: file.name.clone(),
                reason: too_big.clone(),
            });
            continue;
        }

        let bytes = match fetch_file(file.url.clone()).await {
            Ok(bytes) => bytes,
            Err(error) => {
                refused.push(Refused {
                    name: file.name.clone(),
                    reason: format!("could not be fetched: {error}"),
                });
                continue;
            }
        };

        // Checked again against what actually arrived, because the size the
        // service reported is a claim until the bytes are in hand.
        if bytes.len() as u64 > limits.max_bytes {
            refused.push(Refused {
                name: file.name.clone(),
                reason: too_big.clone(),
            });
            continue;
        }

        let directory = paths::host_path_under(project_path, project_path, ATTACHMENTS_DIR);
        let Some(directory) = directory else {
            refused.push(Refused {
                name: file.name.clone(),
                reason: "the project has nowhere to put it".to_owned(),
            });
            continue;
        };

        let saved = std::fs::create_dir_all(&directory).and_then(|()| {
            let target = free_path(Path::new(&directory), &safe_name(&file.name));
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&target)
                .and_then(|mut file| {
                    use std::io::Write;
                    file.write_all(&bytes)
                })
                .map(|()| target)
        });
        match saved {
            Ok(target) => {
                let name = target.display().to_string().split_off(directory.len() + 1);
                taken.push(Taken {
                    path: format!("{ATTACHMENTS_DIR}/{name}"),
                    bytes,
                    content_type: file.content_type.clone(),
                });
            }
            Err(error) => refused.push(Refused {
                name: file.name.clone(),
                reason: format!("could not be saved: {error}"),
            }),
        }
    }

    Outcome { taken, refused }
}

#[cfg(test)]
mod tests;
