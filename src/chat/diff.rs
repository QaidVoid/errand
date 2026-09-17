//! Renders what an edit changed, as a unified diff.
//!
//! A diff rather than the file: it shows intent instead of contents, it is a
//! fraction of the size, and it does not push a whole file into a chat every
//! time one line moves. The whole file is still available on request.
//!
//! Implemented here rather than by shelling out to `diff`, because a
//! subprocess per edit is a lot of machinery for something this small, and
//! this keeps the output format ours to bound.

/// Lines of context kept either side of a change.
pub const CONTEXT_LINES: usize = 2;

/// Most diff lines posted before the rest is summarised.
pub const MAX_DIFF_LINES: usize = 60;

/// Longest single line shown before it is cut, since a minified file is one line.
const MAX_LINE_LENGTH: usize = 200;

/// A change to one file, ready to post.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    /// True when there is nothing to show.
    pub empty: bool,
    /// Lines added across the whole file.
    pub added: usize,
    /// Lines removed across the whole file.
    pub removed: usize,
    /// The rendered diff body, already bounded.
    pub body: String,
}

/// Longest common subsequence of two line arrays, as a table of match lengths.
///
/// Quadratic, which is fine for a source file and is bounded by the caller
/// refusing to diff anything large.
fn lcs_table(a: &[&str], b: &[&str]) -> Vec<u32> {
    let width = b.len() + 1;
    let mut table = vec![0_u32; (a.len() + 1) * width];

    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i * width + j] = if a[i] == b[j] {
                table[(i + 1) * width + j + 1] + 1
            } else {
                table[(i + 1) * width + j].max(table[i * width + j + 1])
            };
        }
    }
    table
}

/// One line of a diff: kept, added, or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Kept,
    Added,
    Removed,
}

impl OpKind {
    fn mark(self) -> char {
        match self {
            OpKind::Kept => ' ',
            OpKind::Added => '+',
            OpKind::Removed => '-',
        }
    }
}

fn operations<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(OpKind, &'a str)> {
    let width = b.len() + 1;
    let table = lcs_table(a, b);
    let mut ops = Vec::new();

    let mut i = 0;
    let mut j = 0;
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            ops.push((OpKind::Kept, a[i]));
            i += 1;
            j += 1;
        } else if table[(i + 1) * width + j] >= table[i * width + j + 1] {
            ops.push((OpKind::Removed, a[i]));
            i += 1;
        } else {
            ops.push((OpKind::Added, b[j]));
            j += 1;
        }
    }
    for rest in &a[i..] {
        ops.push((OpKind::Removed, rest));
    }
    for rest in &b[j..] {
        ops.push((OpKind::Added, rest));
    }

    ops
}

/// Length in code points, which is what a reader sees.
fn point_len(text: &str) -> usize {
    text.chars().count()
}

fn clip(text: &str) -> String {
    if point_len(text) > MAX_LINE_LENGTH {
        let mut cut: String = text.chars().take(MAX_LINE_LENGTH).collect();
        cut.push_str(" ...");
        cut
    } else {
        text.to_owned()
    }
}

/// Builds a bounded unified diff between two versions of a file.
///
/// Unchanged regions are dropped apart from a little context, so a one line
/// change in a thousand line file reads as a one line change.
pub fn file_diff(before: &str, after: &str) -> FileDiff {
    if before == after {
        return FileDiff {
            empty: true,
            added: 0,
            removed: 0,
            body: String::new(),
        };
    }

    let a: Vec<&str> = before.split('\n').collect();
    let b: Vec<&str> = after.split('\n').collect();
    let ops = operations(&a, &b);
    let added = ops
        .iter()
        .filter(|(kind, _)| *kind == OpKind::Added)
        .count();
    let removed = ops
        .iter()
        .filter(|(kind, _)| *kind == OpKind::Removed)
        .count();

    let mut keep = vec![false; ops.len()];
    for (index, (kind, _)) in ops.iter().enumerate() {
        if *kind == OpKind::Kept {
            continue;
        }
        let from = index.saturating_sub(CONTEXT_LINES);
        let to = (index + CONTEXT_LINES).min(ops.len() - 1);
        for kept in &mut keep[from..=to] {
            *kept = true;
        }
    }

    let mut lines: Vec<String> = Vec::new();
    let mut truncated = 0_usize;
    let mut previous_kept: Option<usize> = None;

    for (index, kept) in keep.iter().enumerate() {
        if !kept {
            continue;
        }
        if lines.len() >= MAX_DIFF_LINES {
            truncated += 1;
            continue;
        }
        if let Some(previous) = previous_kept
            && index > previous + 1
        {
            lines.push("@@".to_owned());
        }
        let (kind, text) = ops[index];
        lines.push(format!("{}{}", kind.mark(), clip(text)));
        previous_kept = Some(index);
    }

    if truncated > 0 {
        lines.push(format!("@@ {truncated} further line(s) not shown"));
    }

    FileDiff {
        empty: false,
        added,
        removed,
        body: lines.join("\n"),
    }
}

/// The message posted for a change to one file.
pub fn render_diff(path: &str, diff: &FileDiff) -> String {
    format!(
        "`{path}` +{} -{}\n```diff\n{}\n```",
        diff.added, diff.removed, diff.body
    )
}

#[cfg(test)]
mod tests;
