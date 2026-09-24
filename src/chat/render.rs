//! Splits and formats agent output into messages the chat service will take.
//!
//! Splitting is by code point, never by byte, so a multi-byte character is
//! never cut in half. A split landing inside a fenced code block closes the
//! fence in the earlier message and reopens it with the same language in the
//! next, so every message posted is independently well formed.

use serde_json::Value;

use crate::agent::protocol::DialogMethod;
use crate::agent::protocol::DialogRequest;
use crate::chat::chars::{PrefixKey, prefixed};
use crate::provider::discover::Outcome;
use crate::provider::usage::{Quota, is_spent};
use crate::session::event::Delegated;
use crate::session::files::Entry;
use crate::session::files::{FileContents, MAX_INLINE_BYTES};

/// The service's per-message character limit.
pub const MESSAGE_LIMIT: usize = 2000;

/// The service's thread name limit.
pub const THREAD_NAME_LIMIT: usize = 100;

/// The fence marker.
const FENCE: &str = "```";

/// The cost of closing one at the end of a chunk.
const CLOSING_COST: usize = FENCE.len() + 1;

/// Room left for a reopened fence when pre-splitting an over-long line.
const FENCE_HEADROOM: usize = 16;

/// Most entries listed in a thread before the rest is summarised.
pub const MAX_LISTED_ENTRIES: usize = 200;

/// Splits a string into pieces of at most `size` code points.
fn split_by_code_points(text: &str, size: usize) -> Vec<String> {
    let points: Vec<char> = text.chars().collect();
    if points.len() <= size {
        return vec![text.to_owned()];
    }
    points
        .chunks(size)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Length in code points, which is what the limit actually counts.
fn length(text: &str) -> usize {
    text.chars().count()
}

/// Returns the fence language when the line opens or closes a fenced block,
/// or nothing when it is ordinary text.
fn fence_language(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with(FENCE) {
        return None;
    }
    Some(trimmed[FENCE.len()..].trim().to_owned())
}

/// Splits text into messages that each fit the limit, preferring line
/// boundaries and repairing any fence the split lands inside.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    if length(text) <= limit {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![text.to_owned()]
        };
    }

    let mut lines = Vec::new();
    for line in text.split('\n') {
        lines.extend(split_by_code_points(line, limit - FENCE_HEADROOM));
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_length = 0;
    let mut open_language: Option<String> = None;

    for line in &lines {
        let reserve = if open_language.is_some() {
            CLOSING_COST
        } else {
            0
        };
        let cost = if current.is_empty() {
            length(line)
        } else {
            length(line) + 1
        };
        if current_length + cost + reserve > limit {
            flush(
                &mut chunks,
                &mut current,
                &mut current_length,
                open_language.as_ref(),
            );
        }

        let first_in_chunk = current.is_empty();
        current.push(line.clone());
        current_length += if first_in_chunk {
            length(line)
        } else {
            length(line) + 1
        };

        if let Some(language) = fence_language(line) {
            open_language = if open_language.is_none() {
                Some(language)
            } else {
                None
            };
        }
    }

    if !current.is_empty() {
        chunks.push(current.join("\n"));
    }

    chunks
        .into_iter()
        .filter(|chunk| !chunk.is_empty())
        .collect()
}

/// Closes the chunk under construction, and reopens its fence in the next one
/// when a split landed inside the block.
fn flush(
    chunks: &mut Vec<String>,
    current: &mut Vec<String>,
    current_length: &mut usize,
    open_language: Option<&String>,
) {
    if current.is_empty() {
        return;
    }
    let mut body = current.join("\n");
    if open_language.is_some() {
        body.push('\n');
        body.push_str(FENCE);
    }
    chunks.push(body);
    current.clear();
    *current_length = 0;
    if let Some(language) = open_language {
        let reopened = format!("{FENCE}{language}");
        *current_length = length(&reopened);
        current.push(reopened);
    }
}

/// Derives a thread name from the first prompt and the project it runs in,
/// truncated to the limit without splitting a character.
pub fn thread_name(project: &str, prompt: &str) -> String {
    let first_line = prompt
        .split('\n')
        .find(|line| !line.trim().is_empty())
        .unwrap_or("session");
    let collapsed = collapse_spaces(first_line.trim());
    let prefix = format!("{project}: ");
    let room = THREAD_NAME_LIMIT - length(&prefix);
    let mut body: String = collapsed.chars().take(room).collect();
    while body.ends_with(|c: char| c.is_whitespace()) {
        body.pop();
    }
    if body.is_empty() {
        "session".clone_into(&mut body);
    }
    format!("{prefix}{body}")
}

/// Any run of whitespace becomes one space.
fn collapse_spaces(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut previous_was_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            if !previous_was_space {
                collapsed.push(' ');
            }
            previous_was_space = true;
        } else {
            collapsed.push(character);
            previous_was_space = false;
        }
    }
    collapsed.trim_end().to_owned()
}

/// Truncates tool output and says so, rather than posting a silent prefix.
pub fn truncate(text: &str, max: usize) -> String {
    let points: Vec<char> = text.chars().collect();
    if points.len() <= max {
        return text.to_owned();
    }
    let kept: String = points[..max].iter().collect();
    format!(
        "{kept}\n[truncated, {} more characters]",
        points.len() - max
    )
}

/// The line announcing that a tool call started.
///
/// The command is shown whole. Its tail is often the part that says what it
/// was for, so shortening throws away the half worth reading. Only the
/// service's own message limit ever cuts anything, and that is handled where
/// blocks are sent.
///
/// Whitespace is flattened so a multi-line command stays one entry in a block,
/// and the target is wrapped in inline code, which stops the client turning
/// any URL in it into a link.
pub fn tool_line(tool_name: &str, target: Option<&str>) -> String {
    let Some(target) = target else {
        return prefixed(PrefixKey::Tool, &format!("`{tool_name}`"));
    };
    if target.trim().is_empty() {
        return prefixed(PrefixKey::Tool, &format!("`{tool_name}`"));
    }

    let flattened = collapse_spaces(target.trim());
    // Backticks inside the target would end the inline code span early.
    let flattened = flattened.replace('`', "'");
    prefixed(PrefixKey::Tool, &format!("`{tool_name}` `{flattened}`"))
}

/// What a session has cost, in one short line.
///
/// Cached input is reported as a share of everything sent, because that is the
/// number worth watching: it is what keeps a long session affordable.
pub fn usage_summary(usage: &Usage) -> String {
    let sent = usage.input + usage.cache_read;
    #[expect(clippy::cast_possible_truncation)]
    let cached = if sent == 0.0 {
        0
    } else {
        ((usage.cache_read / sent) * 100.0).round() as i64
    };

    let mut parts = vec![
        format!("{} tokens", tokens(usage.total_tokens)),
        format!("{cached}% cached"),
    ];

    // What the conversation is carrying now, as opposed to what it has spent
    // in total. The share is the useful part: it says how much room is left.
    if usage.context_tokens > 0.0 {
        parts.push(if usage.context_window <= 0.0 {
            format!("{} context", tokens(usage.context_tokens))
        } else {
            #[expect(clippy::cast_possible_truncation)]
            let share = ((usage.context_tokens / usage.context_window) * 100.0).round() as i64;
            format!(
                "{}/{} context ({share}%)",
                tokens(usage.context_tokens),
                tokens(usage.context_window),
            )
        });
    }
    if usage.cost > 0.0 {
        parts.push(format!("${:.4}", usage.cost));
    }
    parts.join(", ")
}

/// Which model answered, what the turn took, and what it spent.
///
/// Grouped rather than run together: which model, how long it took, and what
/// it cost are separate questions, and a single comma separated run makes the
/// reader count commas to find the one they wanted. The model leads because a
/// session can be switched from under a reader, and a turn that reads oddly
/// is worth attributing before it is worth timing.
pub fn turn_summary(model: Option<&str>, timing: &str, usage: Option<&Usage>) -> String {
    let mut groups = Vec::new();
    if let Some(model) = model.filter(|model| !model.is_empty()) {
        groups.push(format!("`{model}`"));
    }
    if !timing.is_empty() {
        groups.push(timing.to_owned());
    }
    if let Some(usage) = usage {
        let spent = usage_summary(usage);
        if !spent.is_empty() {
            groups.push(spent);
        }
    }
    groups.join(" | ")
}

/// What a session has spent, as the renderer reads it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Usage {
    /// Input tokens the turn charged uncached.
    pub input: f64,
    /// Input tokens served from the provider's cache.
    pub cache_read: f64,
    /// Everything the turn charged.
    pub total_tokens: f64,
    /// What the turn cost, when the provider prices it.
    pub cost: f64,
    /// What the conversation is carrying now.
    pub context_tokens: f64,
    /// How much the model holds before it is compacted.
    pub context_window: f64,
}

/// Suffixes, smallest first, each a thousand times the one before.
const MAGNITUDES: [(f64, &str); 3] = [(1_000_000_000.0, "B"), (1_000_000.0, "M"), (1_000.0, "k")];

/// A count as a person would read it.
///
/// Carries up rather than growing a long number: a million tokens reads as
/// `1.0M`, not `1000.0k`. Below a thousand it is left exactly as it is, since
/// rounding a small count loses the only detail it had.
pub fn tokens(count: f64) -> String {
    let size = count.abs();
    for (index, (magnitude, suffix)) in MAGNITUDES.iter().enumerate() {
        if size < *magnitude {
            continue;
        }

        let scaled = count / magnitude;
        // One decimal until three digits, then none: 12.3M, but 123M.
        let digits = usize::from(scaled.abs() < 100.0);

        #[expect(
            clippy::uninlined_format_args,
            reason = "rounding can push a value into the next magnitude: 999,999 would read as 1000k, which is a magnitude out. Carry it up instead"
        )]
        let rounded = format!("{scaled:.digits$}", digits = digits);
        if rounded
            .trim_start_matches('-')
            .parse::<f64>()
            .is_ok_and(|value| value.abs() >= 1000.0)
            && index > 0
        {
            let (bigger, bigger_suffix) = MAGNITUDES[index - 1];
            return format!("{:.1}{}", count / bigger, bigger_suffix);
        }
        #[expect(clippy::uninlined_format_args)]
        return format!("{scaled:.digits$}{suffix}", digits = digits);
    }
    format!("{count}")
}

/// A moment, rendered so the client counts down to it in the reader's own zone.
///
/// `<t:seconds:R>` is resolved by the client, so one message reads correctly
/// for everyone and keeps reading correctly as the wait shortens. Writing the
/// time out here would be wrong for anyone in another zone and stale a minute
/// later.
pub fn when_relative(epoch_ms: i64) -> String {
    format!("<t:{}:R>", epoch_ms.div_euclid(1000))
}

/// The same moment in plain text, for a surface that renders no markup.
///
/// A bot's status is one such surface: `<t:seconds:R>` arrives there verbatim
/// and reads as punctuation. This is coarse on purpose. A status is refreshed
/// on an interval rather than per second, so a minute is the smallest unit
/// that is still true by the time anybody reads it.
pub fn when_relative_plain(epoch_ms: i64, now: i64) -> String {
    #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let minutes = ((epoch_ms - now) as f64 / 60_000.0).ceil() as i64;
    if minutes <= 0 {
        return "now".to_owned();
    }
    if minutes < 60 {
        return format!("in {minutes}m");
    }
    let hours = minutes / 60;
    let rest = minutes % 60;
    if rest == 0 {
        format!("in {hours}h")
    } else {
        format!("in {hours}h {rest}m")
    }
}

/// `!usage` as a markdown-style table, in a code block so the columns line
/// up: chat does not render a markdown table, only a fixed-width font.
///
/// Each row is a provider and what it said of its window, or nothing where it
/// did not answer. What is left is shown rather than what is used, because
/// that is the number somebody is deciding on. A code block shows `<t:...>`
/// verbatim, so a reset is written out twice instead: how long until it, as
/// of `now`, and the moment itself in UTC. ASCII throughout, so it renders the
/// same in any font.
pub fn usage_table(rows: &[(String, Option<Quota>)], now: i64) -> String {
    let dash = || "-".to_owned();
    let mut table = vec![[
        "provider".to_owned(),
        "left".to_owned(),
        "resets in".to_owned(),
        "at (UTC)".to_owned(),
    ]];
    for (provider, quota) in rows {
        let (left, resets, at) = match quota {
            None => ("unknown".to_owned(), dash(), dash()),
            Some(quota) => {
                let left = left_of(quota);
                match quota.resets_at {
                    None => (left, dash(), dash()),
                    Some(at) => (left, span_until(at, now), utc_minute(at)),
                }
            }
        };
        table.push([provider.clone(), left, resets, at]);
    }

    let widths: Vec<usize> = (0..4)
        .map(|column| table.iter().map(|row| row[column].len()).max().unwrap_or(0))
        .collect();
    let line = |cells: &[String]| {
        let cells: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:<width$}"))
            .collect();
        format!("| {} |", cells.join(" | "))
    };
    let rule = widths
        .iter()
        .map(|width| "-".repeat(width + 2))
        .collect::<Vec<_>>()
        .join("|");
    let mut lines = vec![line(&table[0]), format!("|{rule}|")];
    lines.extend(table[1..].iter().map(|row| line(row)));
    format!("```\n{}\n```", lines.join("\n"))
}

/// What is left of a window, or that it is spent.
fn left_of(quota: &Quota) -> String {
    if is_spent(quota) {
        "spent".to_owned()
    } else {
        format!("{}%", (100.0 - quota.percentage).round().clamp(0.0, 100.0))
    }
}

/// How long until a moment, in days, hours, and minutes as it grows.
fn span_until(epoch_ms: i64, now: i64) -> String {
    let minutes = ((epoch_ms - now).max(0) + 59_999) / 60_000;
    let (days, hours, minutes) = (minutes / 1_440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

/// What `!models refresh` found, one line per provider asked.
pub fn models_refreshed(outcomes: &[Outcome]) -> String {
    if outcomes.is_empty() {
        return "no provider is set to be asked for its models; set `discover` on one".to_owned();
    }
    outcomes
        .iter()
        .map(|(provider, outcome)| match outcome {
            Ok(1) => format!("`{provider}`: 1 model"),
            Ok(count) => format!("`{provider}`: {count} models"),
            Err(why) => format!("`{provider}`: kept the models it names, since {why}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A moment as `YYYY-MM-DD HH:MM` in UTC.
fn utc_minute(epoch_ms: i64) -> String {
    jiff::Timestamp::from_millisecond(epoch_ms).map_or_else(
        |_| "-".to_owned(),
        |at| at.strftime("%Y-%m-%d %H:%M").to_string(),
    )
}

/// How long a turn took, and how much of it was spent before it said
/// anything.
///
/// The wait before the first token is what a person in a thread actually
/// feels, and it is not the same number as how long the whole turn took: a
/// turn that answers at once and then runs tools for a minute reads very
/// differently from one that sits silent for a minute and then answers.
pub fn turn_timing(first_output_ms: Option<i64>, wall_ms: i64) -> String {
    let wall = duration(wall_ms.max(0));
    match first_output_ms.filter(|first| *first >= 0) {
        Some(first) => format!("{wall} ({} to first word)", duration(first)),
        None => wall,
    }
}

/// A span, in the largest unit that still says something useful.
///
/// Sub-second is where the interesting end of a first-token wait lives, and
/// hours is where a long agent turn lives, so neither end is rounded away.
pub fn duration(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a turn is far below f64's exact range"
    )]
    let seconds = ms as f64 / 1_000.0;
    if seconds < 60.0 {
        return format!("{seconds:.1}s");
    }
    let whole = ms / 1_000;
    let (minutes, seconds) = (whole / 60, whole % 60);
    if minutes < 60 {
        return format!("{minutes}m{seconds:02}s");
    }
    format!("{}h{:02}m", minutes / 60, minutes % 60)
}

/// A line saying something went wrong but the session carries on.
pub fn warning_line(text: &str) -> String {
    prefixed(PrefixKey::Warning, text)
}

/// A connection or session lifecycle line.
pub fn connection_line(text: &str) -> String {
    prefixed(PrefixKey::Connection, text)
}

/// A question the agent is asking the user.
pub fn question_line(text: &str) -> String {
    prefixed(PrefixKey::Question, text)
}

/// A bracketed ASCII marker, for states with no enumerated glyph. A queue
/// position is a number, not a state, so no emoji spells it.
pub fn marker(state: &str) -> String {
    format!("[{state}]")
}

/// What a compaction achieved, or that it did not say.
///
/// The counts are the point: a compaction that freed nothing looks exactly
/// like one that freed half the window unless the numbers are shown.
pub fn compaction_line(answer: &Value) -> String {
    if answer.get("success") == Some(&Value::Bool(false)) {
        let detail = match answer.get("error").and_then(Value::as_str) {
            Some(error) => error.to_owned(),
            None => "the agent refused".to_owned(),
        };
        return connection_line(&format!("compaction did not run: {detail}"));
    }

    let data = answer.get("data").unwrap_or(&Value::Null);
    let before = data.get("tokensBefore").and_then(Value::as_f64);
    let after = data.get("estimatedTokensAfter").and_then(Value::as_f64);

    match (before, after) {
        (Some(before), Some(after)) => connection_line(&format!(
            "compacted the conversation, about {} tokens down to {}",
            tokens(before),
            tokens(after)
        )),
        _ => connection_line("compacted the conversation"),
    }
}

/// A dialog rendered for a thread, numbering options so a reply can pick one.
///
/// The reply is free text from a person, so what an answer may look like is
/// spelled out rather than assumed: a thread has no buttons to press.
pub fn dialog_lines(request: &DialogRequest) -> String {
    let mut lines = vec![request.title.clone()];
    if let Some(message) = &request.message {
        lines.push(message.clone());
    }

    match (request.method, &request.options) {
        (DialogMethod::Select, Some(options)) => {
            for (index, option) in options.iter().enumerate() {
                lines.push(format!("{}. {option}", index + 1));
            }
            lines.push("reply with a number or the option text".to_owned());
        }
        (DialogMethod::Confirm, _) => {
            lines.push("reply yes or no".to_owned());
        }
        _ => lines.push("reply with your answer".to_owned()),
    }

    lines.join("\n")
}

/// A delegation, as one line in a conversation.
///
/// Says which model was asked and about what, so a reader can tell that part
/// of a turn was answered by something other than the session's own model.
/// What it said goes to the agent rather than here: it is working material,
/// and a thread that showed every delegated answer in full would bury the
/// conversation it belongs to.
pub fn delegation_line(delegated: &Delegated) -> String {
    if let Some(refused) = &delegated.refused {
        return prefixed(
            PrefixKey::Warning,
            &format!("a delegated question was not asked: {refused}"),
        );
    }

    let saved = match delegated.kept_out {
        None | Some(0) => String::new(),
        Some(kept_out) => format!(
            ", keeping {} out of this conversation",
            bytes(widen(kept_out as u64))
        ),
    };
    prefixed(
        PrefixKey::Delegated,
        &format!(
            "asked {} about {}{saved}",
            delegated.model.as_deref().unwrap_or(""),
            delegated.describes.as_deref().unwrap_or("")
        ),
    )
}

/// Widens a whole count for the renderer, where a byte count beyond what an
/// f64 carries is beyond any real file.
#[expect(clippy::cast_precision_loss)]
fn widen(count: u64) -> f64 {
    count as f64
}

/// A byte count, in the units a person reading a thread would use.
///
/// Decimal units, because that is what a disk quota and a file manager both
/// report, and a session's budget is written the same way.
pub fn bytes(count: f64) -> String {
    if count < 1000.0 {
        return format!("{count} B");
    }
    let units = ["kB", "MB", "GB", "TB"];
    let mut value = count / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

/// A directory as one fenced block, sizes aligned in a column.
///
/// A fence is shown in a monospaced font, which is the only way the sizes line
/// up for every reader.
pub fn directory_listing(entries: &[Entry], display_path: &str) -> String {
    if entries.is_empty() {
        return format!("`{display_path}/` is empty");
    }

    let shown = &entries[..entries.len().min(MAX_LISTED_ENTRIES)];
    let rows: Vec<(String, String)> = shown
        .iter()
        .map(|entry| {
            (
                if entry.directory {
                    format!("{}/", entry.name)
                } else {
                    entry.name.clone()
                },
                if entry.directory {
                    String::new()
                } else {
                    bytes(widen(entry.size))
                },
            )
        })
        .collect();
    let width = rows
        .iter()
        .map(|(_, size)| size.chars().count())
        .max()
        .unwrap_or(0);
    let body = rows
        .iter()
        .map(|(name, size)| format!("{size:>width$}  {name}"))
        .collect::<Vec<_>>()
        .join("\n");
    let more = if entries.len() > shown.len() {
        format!("\n... {} more", entries.len() - shown.len())
    } else {
        String::new()
    };

    format!(
        "`{display_path}/` {} entries\n```\n{body}{more}\n```",
        entries.len()
    )
}

/// A file as a fenced block, with a note when it was cut or is not text.
pub fn file_view(contents: &FileContents) -> String {
    let size = bytes(widen(contents.size));
    if contents.binary {
        return format!(
            "`{}` is binary, {size}. Use `!file` to download it.",
            contents.path
        );
    }

    let cut = if contents.truncated {
        format!(
            "\n... cut at {} of {size}, use `!file` for all of it",
            bytes(widen(MAX_INLINE_BYTES))
        )
    } else {
        String::new()
    };

    format!(
        "`{path}` {size}\n```{language}\n{text}{cut}\n```",
        path = contents.path,
        size = size,
        language = contents.language,
        text = contents.text,
        cut = cut,
    )
}

#[cfg(test)]
mod tests;
