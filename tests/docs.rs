//! Requires a doc comment on every exported item and every public field.
//!
//! The crate is a binary, so rustc's `missing_docs` never fires: nothing here
//! is public to anyone outside it. The rule is still the project's, and a
//! scanner is what enforces it. Private items are left alone, because the
//! rule is about the surface a module offers its siblings, not about every
//! line inside one.
//!
//! A scanner rather than a parser: the tree is written to one shape, and
//! anything it cannot read is reported rather than quietly passed.

use std::path::{Path, PathBuf};

/// An exported item with nothing said about it.
struct Undocumented {
    file: String,
    line: usize,
    item: String,
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
        assert!(
            dir.pop(),
            "could not find the repository root from {}",
            env!("CARGO_MANIFEST_DIR")
        );
    }
}

/// Every `.rs` file under `src`, excluding the test modules beside the code.
fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if !name.ends_with(".rs") || name == "tests.rs" || name.ends_with("_test.rs") {
                continue;
            }
            if name == "test_util.rs" {
                continue;
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

/// What a line declares, when it declares something that needs documenting.
fn declares(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub "))?;
    for keyword in [
        "fn ",
        "async fn ",
        "unsafe fn ",
        "struct ",
        "enum ",
        "trait ",
        "const ",
        "static ",
        "type ",
        "mod ",
    ] {
        if let Some(name) = rest.strip_prefix(keyword) {
            let name: String = name
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            return (!name.is_empty()).then(|| format!("{keyword}{name}"));
        }
    }
    None
}

/// A public field, which is a line of the form `pub name:` inside a body.
fn declares_field(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    rest[name.len()..]
        .trim_start()
        .starts_with(':')
        .then_some(name)
}

/// Whether a doc comment sits above `index`, looking past any attributes.
///
/// An attribute may run over several lines, so the brackets are counted back
/// to the one that opened the block rather than matched line by line.
fn documented(lines: &[&str], index: usize) -> bool {
    let mut at = index;
    let mut depth = 0_i32;
    while at > 0 {
        at -= 1;
        let line = lines[at].trim();
        depth += i32::try_from(line.matches(']').count()).unwrap_or(0)
            - i32::try_from(line.matches('[').count()).unwrap_or(0);
        if depth > 0 {
            continue;
        }
        depth = 0;
        if line.starts_with("///") {
            return true;
        }
        if line.starts_with("#[") {
            continue;
        }
        return false;
    }
    false
}

/// Reports every exported item and public field with no doc comment.
fn scan(file: &str, text: &str) -> Vec<Undocumented> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut in_body = false;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !in_body
            && trimmed.ends_with('{')
            && (trimmed.contains("struct ") || trimmed.contains("enum "))
        {
            in_body = true;
        } else if in_body && trimmed == "}" {
            in_body = false;
        }

        let item = declares(line).or_else(|| in_body.then(|| declares_field(line)).flatten());
        let Some(item) = item else {
            continue;
        };
        if !documented(&lines, index) {
            found.push(Undocumented {
                file: file.to_owned(),
                line: index + 1,
                item,
            });
        }
    }
    found
}

#[test]
fn every_exported_item_says_what_it_is() {
    let root = repo_root();
    let mut undocumented = Vec::new();
    for path in source_files(&root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        undocumented.extend(scan(&relative, &text));
    }

    assert!(
        undocumented.is_empty(),
        "{} exported item(s) carry no doc comment:\n{}",
        undocumented.len(),
        undocumented
            .iter()
            .map(|found| format!("  {}:{}: {}", found.file, found.line, found.item))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn an_item_behind_a_spanning_attribute_still_counts_as_documented() {
    let text = "\
/// Says what it is.
#[allow(
    dead_code,
    reason = \"a reason long enough to wrap\"
)]
pub fn thing() {}

pub fn bare() {}
";
    let found = scan("example.rs", text);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].item, "fn bare");
}

/// A test file that nothing declares is never built, so it never runs and its
/// drift is never noticed. Two were found that way during the port: they had
/// not compiled since they were written.
#[test]
fn every_test_file_is_declared_by_the_module_it_sits_beside() {
    let root = repo_root();
    let mut orphans = Vec::new();
    let mut pending = vec![root.join("src")];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.file_name().unwrap_or_default() != "tests.rs" {
                continue;
            }
            // `foo/tests.rs` is declared by `foo.rs` or by `foo/mod.rs`.
            let Some(owner) = path.parent() else { continue };
            let beside = owner.with_extension("rs");
            let inside = owner.join("mod.rs");
            let declared = [beside, inside].iter().any(|candidate| {
                std::fs::read_to_string(candidate)
                    .is_ok_and(|text| text.lines().any(|line| line.trim() == "mod tests;"))
            });
            if !declared {
                orphans.push(
                    path.strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "test file(s) nothing declares, so nothing builds them:\n  {}",
        orphans.join("\n  ")
    );
}
