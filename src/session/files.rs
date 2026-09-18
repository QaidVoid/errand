//! Reading files in a session's project, without involving the agent.
//!
//! Looking at a directory should not cost a model call or wait behind the
//! admission queue. These run in the daemon and answer immediately.
//!
//! Nothing here executes a shell. Each operation is a filesystem call on a
//! path the caller has already confined to the project, so there is no command
//! for a crafted argument to inject into. Reading is structured only: what a
//! thread or an interface makes of it is rendering, and lives with the
//! rendering.

use std::path::Path;

/// Longest file read inline before it is cut. Whole files go by upload.
pub const MAX_INLINE_BYTES: u64 = 12 * 1024;

/// The fence language for a path, or an empty string when unknown.
pub fn language_for(path: &str) -> String {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("ts") => "ts",
        Some("tsx") => "tsx",
        Some("js") => "js",
        Some("jsx") => "jsx",
        Some("json") => "json",
        Some("md") => "md",
        Some("sh" | "bash") => "bash",
        Some("fish") => "fish",
        Some("py") => "python",
        Some("rs") => "rust",
        Some("go") => "go",
        Some("c" | "h") => "c",
        Some("cpp") => "cpp",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("sql") => "sql",
        Some("html") => "html",
        Some("css") => "css",
        _ => "",
    }
    .to_owned()
}

/// Whether content looks binary.
///
/// A NUL byte in the first block is the same heuristic `grep` uses, and it is
/// what stops an image being pasted into a thread as mojibake.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8_000)].contains(&0)
}

/// One entry in a directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The name within its directory.
    pub name: String,
    /// Path relative to the project, so a surface can ask for it again.
    pub path: String,
    /// Whether it is a directory.
    pub directory: bool,
    /// Zero for a directory, and for a file whose size could not be read.
    pub size: u64,
}

/// Lists a directory, directories first and then by name.
///
/// One order for every surface: a thread and an interface listing the same
/// directory differently is a bug report waiting to happen.
pub fn read_directory(host_path: &str, relative: &str) -> std::io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for found in std::fs::read_dir(host_path)? {
        let found = found?;
        let file_type = found.file_type()?;
        let mut size = 0;
        if file_type.is_file() {
            size = found.metadata().map_or(0, |meta| meta.len());
        }
        let name = found.file_name().to_string_lossy().into_owned();
        let path = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        entries.push(Entry {
            name,
            path,
            directory: file_type.is_dir(),
            size,
        });
    }

    entries.sort_by(|left, right| match (left.directory, right.directory) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => left.name.cmp(&right.name),
    });
    Ok(entries)
}

/// A file's contents, or a statement that it is not text.
#[derive(Debug, Clone, PartialEq)]
pub struct FileContents {
    /// The path as the asker named it.
    pub path: String,
    /// The whole file's size, in bytes.
    pub size: u64,
    /// Whether the content looks binary.
    pub binary: bool,
    /// Whether the file continues past what was read.
    pub truncated: bool,
    /// What was read, decoded, or empty for a binary file.
    pub text: String,
    /// The language to read it as, or empty when the extension is not one this
    /// knows. Reported so a surface highlights what the daemon named rather
    /// than deriving a second mapping or guessing from the contents.
    pub language: String,
}

/// Raised when a path names something that cannot be shown as a file.
#[derive(Debug, thiserror::Error)]
#[error("{0} is not a file")]
pub struct NotAFileError(pub String);

/// Reads a file for display, refusing to render something that is not text.
///
/// Only the first `limit` bytes are read: showing a large file inline is not
/// useful, and reading all of it to throw most away costs the whole file.
///
/// Returns [`NotAFileError`] when the path is a directory or a device.
pub fn read_file_for_display(
    host_path: &str,
    relative: &str,
    limit: u64,
) -> Result<FileContents, NotAFileError> {
    let meta = std::fs::metadata(host_path).map_err(|_| NotAFileError(relative.to_owned()))?;
    if !meta.is_file() {
        return Err(NotAFileError(relative.to_owned()));
    }

    let raw = read_head(host_path, limit);
    let truncated = meta.len() > limit;

    if looks_binary(&raw) {
        return Ok(FileContents {
            path: relative.to_owned(),
            size: meta.len(),
            binary: true,
            truncated,
            text: String::new(),
            language: String::new(),
        });
    }

    Ok(FileContents {
        path: relative.to_owned(),
        size: meta.len(),
        binary: false,
        truncated,
        text: decode_whole(&raw, truncated),
        language: language_for(relative),
    })
}

fn read_head(host_path: &str, limit: u64) -> Vec<u8> {
    use std::io::Read;

    let Ok(mut file) = std::fs::File::open(host_path) else {
        return Vec::new();
    };
    #[expect(clippy::cast_possible_truncation)]
    let mut buffer = vec![0_u8; limit.min(usize::MAX as u64) as usize];
    let mut read = 0;
    while read < buffer.len() {
        match file.read(&mut buffer[read..]) {
            Ok(0) | Err(_) => break,
            Ok(count) => read += count,
        }
    }
    buffer.truncate(read);
    buffer
}

/// Decodes bytes, dropping a character the cut landed in the middle of.
///
/// A limit counted in bytes can fall inside a multi-byte character, which
/// decodes to a replacement mark. Showing one at the very end of a file that
/// was cut anyway is noise, so it goes.
fn decode_whole(bytes: &[u8], truncated: bool) -> String {
    let text = String::from_utf8_lossy(bytes).into_owned();
    if truncated && text.ends_with('\u{FFFD}') {
        text[..text.len() - '\u{FFFD}'.len_utf8()].to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests;
