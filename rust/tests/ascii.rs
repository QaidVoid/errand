//! Rejects non-ASCII characters in tracked text files.
//!
//! The port of `scripts/check_ascii.ts`, asking version control what is
//! tracked exactly as `scripts/tracked.ts` does: a checkout accumulates
//! directories that belong to whatever tools somebody runs in it, and a file
//! nobody committed is not the project's. No file is exempt. Chat output may
//! carry emoji, but the table that enumerates them declares codepoints rather
//! than glyphs, so even that file is ASCII. Everything stays greppable in a
//! terminal with no font coverage.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Extensions that hold bytes nobody text-checks, so they are not decoded.
const BINARY: [&str; 9] = [
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".woff", ".woff2",
];

/// Where an offending character sits in a tracked file.
struct Offence {
    file: String,
    line: usize,
    column: usize,
    character: char,
}

/// The nearest directory version control answers for, walking up from the
/// crate, so the test finds the repository root however cargo was invoked.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        for marker in [".jj", ".git"] {
            if dir.join(marker).exists() {
                return dir;
            }
        }
        if !dir.pop() {
            panic!(
                "could not find the repository root from {}",
                env!("CARGO_MANIFEST_DIR")
            );
        }
    }
}

/// Asks one tool for the tracked files, or nothing when it is not installed.
///
/// A tool that is absent is not a failure: the checkout this runs in decides
/// which one is there, and continuous integration has only git.
fn listed_by(root: &Path, program: &str, args: &[&str]) -> Option<Vec<String>> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

/// Every tracked file, by path relative to the repository root.
fn tracked_files(root: &Path) -> Vec<String> {
    listed_by(root, "jj", &["file", "list"])
        .or_else(|| listed_by(root, "git", &["ls-files"]))
        .unwrap_or_else(|| panic!("could not list tracked files; neither jj nor git answered"))
}

/// Collects the characters of one file that are not ASCII, with where they sit.
fn scan(file: &str, text: &str) -> Vec<Offence> {
    let mut offences = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        for (column, character) in line.chars().enumerate() {
            if !character.is_ascii() {
                offences.push(Offence {
                    file: file.to_owned(),
                    line: index + 1,
                    column: column + 1,
                    character,
                });
            }
        }
    }
    offences
}

#[test]
fn tracked_tree_is_ascii() {
    let root = repo_root();
    let files = tracked_files(&root);
    let mut offences = Vec::new();
    let mut checked = 0;

    for file in files {
        if BINARY.iter().any(|extension| file.ends_with(extension)) {
            continue;
        }
        let bytes = match std::fs::read(root.join(&file)) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        // A file that is not UTF-8 is skipped, as the text read it replaces
        // refused to decode it rather than checking half of it.
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        checked += 1;
        offences.extend(scan(&file, &text));
    }

    if offences.is_empty() {
        println!("ascii check passed across {checked} tracked files");
        return;
    }
    let report: String = offences
        .iter()
        .map(|offence| {
            format!(
                "{}:{}:{} non-ascii U+{:x}",
                offence.file, offence.line, offence.column, offence.character as u32
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    panic!(
        "{report}\n{} non-ascii character(s) in tracked files",
        offences.len()
    );
}

#[test]
fn scan_reports_the_first_offence_in_a_line() {
    let offences = scan("a.txt", "ok\nbad \u{e9} here");
    assert_eq!(offences.len(), 1);
    assert_eq!(offences[0].file, "a.txt");
    assert_eq!(offences[0].line, 2);
    assert_eq!(offences[0].column, 5);
    assert_eq!(offences[0].character, '\u{e9}');
}
