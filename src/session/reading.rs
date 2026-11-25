//! What `!ls`, `!cat`, `!file` and `!pwd` answer.
//!
//! They read the project on the host and never involve the agent, so a
//! thread whose session is not running is answered the same way a live one
//! is, without waking it: no sandbox, no session slot, and nothing a drain
//! refuses.

use std::io::Read;

use crate::chat::render::{directory_listing, file_view, inert};
use crate::sandbox::backend::WORKSPACE_PATH;
use crate::sandbox::paths;
use crate::session::files::{
    MAX_INLINE_BYTES, NotAFileError, read_directory, read_file_for_display,
};

/// The commands answered here.
pub const READ_COMMANDS: [&str; 4] = ["!ls", "!cat", "!file", "!pwd"];

/// What a read command answers with.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadAnswer {
    /// Words for the thread.
    Say(String),
    /// A file for the thread, with the words posted under it.
    Upload {
        /// The file's name.
        name: String,
        /// What it holds.
        bytes: Vec<u8>,
        /// The line posted under it.
        caption: String,
    },
}

/// The project a read command looks at.
pub struct Project<'a> {
    /// The project's name, as the thread knows it.
    pub name: &'a str,
    /// Where it lives on the host.
    pub path: &'a str,
}

/// Answers `word`, one of [`READ_COMMANDS`], with `rest` after it, or
/// nothing for any other command.
pub fn answer(
    word: &str,
    rest: &str,
    project: &Project,
    max_upload_bytes: u64,
) -> Option<ReadAnswer> {
    match word {
        "!ls" | "!cat" => Some(ReadAnswer::Say(list_or_show(project.path, rest))),
        "!file" => Some(upload(project.path, rest, max_upload_bytes)),
        "!pwd" => Some(ReadAnswer::Say(format!(
            "`{}` at `{}`",
            project.name, project.path
        ))),
        _ => None,
    }
}

/// `wanted` on the host, read as the agent would read it, and as a reader
/// would recognise it: relative to the project, or `.` for the project.
fn resolve(root: &str, wanted: &str) -> Option<(String, String)> {
    let host = paths::host_path_under(WORKSPACE_PATH, root, wanted)?;
    let shown = host.strip_prefix(root).map_or_else(
        || host.clone(),
        |rest| rest.trim_start_matches('/').to_owned(),
    );
    let shown = if shown.is_empty() {
        ".".to_owned()
    } else {
        shown
    };
    Some((host, shown))
}

/// Lists a directory or shows a file.
fn list_or_show(root: &str, request: &str) -> String {
    let wanted = request.trim();
    let wanted = if wanted.is_empty() { "." } else { wanted };
    let Some((host, display)) = resolve(root, wanted) else {
        return format!("`{wanted}` is not inside this session's project");
    };
    match std::fs::metadata(&host).map(|meta| meta.is_dir()) {
        // A directory is listed whichever was asked for: `!cat` on one is a
        // mistake worth answering rather than an error worth reporting.
        Ok(true) => match read_directory(root, &display) {
            Ok(entries) => directory_listing(&entries, &display),
            Err(error) => format!("could not read `{wanted}`: {error}"),
        },
        Ok(false) => match read_file_for_display(root, &display, MAX_INLINE_BYTES) {
            Ok(contents) => file_view(&contents),
            Err(NotAFileError(path)) => format!("could not read `{wanted}`: {path} is not a file"),
        },
        Err(error) => format!("could not read `{wanted}`: {error}"),
    }
}

/// A file from the project, for uploading.
///
/// Opened beneath the project, the same rule `!cat` reads by. This used to
/// be the one read that went by name: a link planted where the agent writes
/// would have had the daemon read whatever it pointed at and post it.
fn upload(root: &str, request: &str, limit: u64) -> ReadAnswer {
    let wanted = request.trim();
    if wanted.is_empty() {
        return ReadAnswer::Say("say which file, as `!file <path>`".to_owned());
    }
    let Some((host, display)) = resolve(root, wanted) else {
        return ReadAnswer::Say(format!("`{wanted}` is not inside this session's project"));
    };
    let file = match paths::open_beneath(root, &display, &paths::OpenOptions::read()) {
        Ok(file) => file,
        Err(error) => return ReadAnswer::Say(format!("could not read `{wanted}`: {error}")),
    };
    // Measured on the file that was opened, so what is checked and what is
    // sent are the same however the tree changes underneath.
    let meta = match file.metadata() {
        Ok(meta) => meta,
        Err(error) => return ReadAnswer::Say(format!("could not read `{wanted}`: {error}")),
    };
    if !meta.is_file() {
        return ReadAnswer::Say(format!("`{wanted}` is not a file"));
    }
    if meta.len() > limit {
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let kilobytes = (meta.len() as f64 / 1024.0).round() as u64;
        return ReadAnswer::Say(format!(
            "`{wanted}` is {kilobytes} KB, larger than the {limit} byte upload limit"
        ));
    }
    // Read through the limit, not trusting the measurement above: the agent
    // can grow the file between the two.
    let mut bytes = Vec::new();
    if let Err(error) = (&file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        return ReadAnswer::Say(format!("could not read `{wanted}`: {error}"));
    }
    if bytes.len() as u64 > limit {
        return ReadAnswer::Say(format!(
            "`{wanted}` grew past the {limit} byte upload limit while it was read"
        ));
    }
    let name = host.rsplit('/').next().unwrap_or("file").to_owned();
    let caption = format!("`{}` {} bytes", inert(&display), bytes.len());
    ReadAnswer::Upload {
        name,
        bytes,
        caption,
    }
}

#[cfg(test)]
mod tests;
