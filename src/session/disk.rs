//! How much a session has written.
//!
//! Measured rather than enforced: no backend caps what a process tree writes
//! in aggregate without a sized filesystem under it, so the daemon watches
//! instead and stops a session that passes its budget.

use std::path::PathBuf;

/// Bytes held under a directory, following no symlink.
///
/// A link is counted as the link rather than as what it points at, so a
/// session cannot appear to hold a hundred gigabytes by linking to one, and
/// cannot hide what it wrote by linking out of its own directory either.
///
/// Returns None when the directory is not there.
pub fn tree_bytes(root: &str) -> Option<u64> {
    if std::fs::symlink_metadata(root).is_err() {
        return None;
    }

    let mut total = 0;
    let mut pending = vec![PathBuf::from(root)];

    while let Some(directory) = pending.pop() {
        // The whole walk of one directory is guarded, not only the call that
        // opens it. A session removing its own work while it is being measured
        // is ordinary, and it must not throw out of a measurement.
        let Ok(entries) = std::fs::read_dir(&directory) else {
            // The directory went while it was being read. What was counted
            // before it went still counts.
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => pending.push(path),
                _ => {
                    // Counted as the link, for a link. Gone between listing
                    // and measuring, for a file that went: a running session
                    // does that, and the rest still counts.
                    if let Ok(meta) = std::fs::symlink_metadata(&path) {
                        total += meta.len();
                    }
                }
            }
        }
    }
    Some(total)
}

/// What a check of a session's disk use concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Under,
    Close,
    Over,
}

impl Verdict {
    /// The verdict as it is written down.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Under => "under",
            Verdict::Close => "close",
            Verdict::Over => "over",
        }
    }
}

/// The share of the budget at which a session is warned rather than stopped.
pub const WARN_AT: f64 = 0.8;

/// Where a session's use sits against its budget.
pub fn verdict(used: u64, budget: u64) -> Verdict {
    if budget == 0 {
        return Verdict::Under;
    }
    if used >= budget {
        return Verdict::Over;
    }
    #[allow(clippy::cast_precision_loss)] // budgets are far below f64's exact range
    let close = {
        let used = used as f64;
        let budget = budget as f64;
        used >= budget * WARN_AT
    };
    if close {
        Verdict::Close
    } else {
        Verdict::Under
    }
}

/// Fastest a session's use is measured, however quickly it is growing.
pub const MIN_CHECK_MS: u64 = 1_000;

/// How long to wait before measuring again, from how fast the session is
/// writing now.
///
/// A fixed interval decides the overshoot. Measured every 30 seconds, a
/// session writing a gigabyte a second is 20 GB past a 5 GB budget before
/// anything notices, which is what a real session did. Aiming at half the time
/// the current rate needs to reach the budget keeps the check ahead of the
/// writing, while an idle session settles back to the configured interval
/// rather than walking the tree every second for nothing.
pub fn next_check_ms(
    written: u64,
    last_written: u64,
    budget: u64,
    since_last: u64,
    slowest: u64,
) -> u64 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "byte counts and their ratios live far below f64's exact range; only the ratio matters here, not the last bits of a petabyte-scale count"
    )]
    let grew = written as f64 - last_written as f64;
    if grew <= 0.0 || since_last == 0 {
        return slowest;
    }

    #[allow(clippy::cast_precision_loss)]
    let projected = {
        let per_ms = grew / since_last as f64;
        let remaining = budget.saturating_sub(written) as f64;
        (remaining / per_ms) * 0.5
    };

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "float to integer casts saturate, so a projection beyond every u64 lands on the slowest interval rather than wrapping"
    )]
    let rounded = projected.round() as u64;
    MIN_CHECK_MS.max(slowest.min(rounded))
}

#[cfg(test)]
#[path = "disk/tests.rs"]
mod tests;
