//! Splits and formats agent output into messages the chat service will take.
//!
//! Splitting counts what the service counts, UTF-16 units, and never cuts a
//! character in half. A split landing inside a fenced code block closes the
//! fence in the earlier message and reopens it with the same language in the
//! next, so every message posted is independently well formed.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use crate::agent::protocol::DialogMethod;
use crate::agent::protocol::DialogRequest;
use crate::chat::chars::{PrefixKey, prefixed};
use crate::provider::discover::Outcome;
use crate::provider::models::AvailableModel;
use crate::provider::usage::{Quota, is_spent};
use crate::session::event::Card;
use crate::session::event::CardColor;
use crate::session::event::CardField;
use crate::session::event::Delegated;
use crate::session::files::Entry;
use crate::session::files::{FileContents, MAX_INLINE_BYTES};
use crate::session::model::split_level;

/// Makes free text inert inside captions by removing mention and span syntax.
pub fn inert(text: &str) -> String {
    text.chars()
        .map(|character| {
            if matches!(character, '<' | '@' | '`') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// The longest stretch of an image server's free text a caption shows.
pub const MAX_SERVER_TEXT_CHARS: usize = 300;

/// An image server's free text, ready to sit inside a caption's code span:
/// made inert, kept to printable ASCII on one line, and cut short with `...`
/// where it was cut. Inside the span no markdown renders, so the text cannot
/// format the thread or carry a masked link. Empty when nothing printable is
/// left.
pub fn server_text(text: &str) -> String {
    let printable: String = inert(text)
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { ' ' })
        .collect();
    let whole = printable.split_whitespace().collect::<Vec<_>>().join(" ");
    if whole.len() > MAX_SERVER_TEXT_CHARS {
        format!("{}...", whole[..MAX_SERVER_TEXT_CHARS].trim_end())
    } else {
        whole
    }
}

/// The longest stretch of a person's prompt a caption quotes back.
pub const MAX_QUOTED_CHARS: usize = 600;

/// A person's text as a block quote, one `> ` per line, cut short with `...`
/// past [`MAX_QUOTED_CHARS`].
///
/// Markdown and mention syntax is escaped rather than removed, so the quote
/// reads as it was written, yet cannot ping anyone, format the message, or
/// carry a masked link under the bot's name.
pub fn quoted(text: &str) -> String {
    let text = text.trim();
    let mut kept: String = text.chars().take(MAX_QUOTED_CHARS).collect();
    if kept.len() < text.len() {
        kept = format!("{}...", kept.trim_end());
    }
    kept.lines()
        .map(|line| {
            let mut out = String::from("> ");
            for character in line.chars() {
                if "\\*_~`|<>#[]()-".contains(character) {
                    out.push('\\');
                }
                out.push(character);
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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

/// Splits a string into pieces of at most `size` UTF-16 units, never between
/// the two halves of a character the limit counts as two.
fn split_by_utf16(text: &str, size: usize) -> Vec<String> {
    if length(text) <= size {
        return vec![text.to_owned()];
    }
    let mut pieces: Vec<String> = Vec::new();
    let mut piece = String::new();
    let mut piece_length = 0;
    for character in text.chars() {
        let units = character.len_utf16();
        if piece_length + units > size {
            pieces.push(std::mem::take(&mut piece));
            piece_length = 0;
        }
        piece.push(character);
        piece_length += units;
    }
    pieces.push(piece);
    pieces
}

/// Length in UTF-16 units, which is what the limit actually counts.
fn length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// The first `limit` UTF-16 units of the text, ending on a whole character.
fn take_units(text: &str, limit: usize) -> String {
    let mut taken = String::new();
    let mut units = 0;
    for character in text.chars() {
        let next = units + character.len_utf16();
        if next > limit {
            break;
        }
        units = next;
        taken.push(character);
    }
    taken
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
        lines.extend(split_by_utf16(line, limit - FENCE_HEADROOM));
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

/// Derives a thread name from the first prompt, truncated to the limit
/// without splitting a character.
///
/// The project is not named in it: every session works in a directory of
/// its own, named after its id, which would say nothing to whoever reads the
/// channel.
pub fn thread_name(prompt: &str) -> String {
    let first_line = prompt
        .split('\n')
        .find(|line| !line.trim().is_empty())
        .unwrap_or("session");
    let collapsed = collapse_spaces(first_line.trim());
    let mut body: String = take_units(&collapsed, THREAD_NAME_LIMIT);
    while body.ends_with(|c: char| c.is_whitespace()) {
        body.pop();
    }
    if body.is_empty() {
        "session".clone_into(&mut body);
    }
    body
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
/// number worth watching: it is what keeps a long session affordable. Counts
/// the agent did not report are left out rather than shown as zero, since a
/// zero reads as a real figure.
pub fn usage_summary(usage: &Usage) -> String {
    let mut parts = Vec::new();
    if usage.total_tokens > 0.0 {
        parts.push(format!("{} tokens", tokens(usage.total_tokens)));
    }
    let sent = usage.input + usage.cache_read + usage.cache_write;
    if sent > 0.0 {
        #[expect(clippy::cast_possible_truncation)]
        let cached = ((usage.cache_read / sent) * 100.0).round() as i64;
        parts.push(format!("{cached}% cached"));
    }

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
    if let Some(rate) = usage.output_rate {
        parts.push(format!("{rate:.0} tok/s"));
    }
    parts.join(", ")
}

/// Output tokens per second over `elapsed_ms`, or nothing when either is
/// missing.
///
/// Measured from the prompt to the turn's end, so the wait for the first
/// token and time spent running tools count too, and a tool heavy turn reads
/// slower than the model streams.
pub fn output_rate(output: u64, elapsed_ms: i64) -> Option<f64> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a turn's token count and length are far below f64's exact range"
    )]
    (output > 0 && elapsed_ms > 0).then(|| output as f64 * 1_000.0 / elapsed_ms as f64)
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
    /// Input tokens written to the provider's cache.
    pub cache_write: f64,
    /// Everything the session charged.
    pub total_tokens: f64,
    /// What the session cost, when the provider prices it.
    pub cost: f64,
    /// What the conversation is carrying now.
    pub context_tokens: f64,
    /// How much the model holds before it is compacted.
    pub context_window: f64,
    /// The turn's output tokens per second, when that can be told.
    pub output_rate: Option<f64>,
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

/// The most labeled cells an embed may carry, as the service limits it.
const EMBED_FIELD_LIMIT: usize = 25;

/// The most text an embed holds all together, as the service limits it.
const EMBED_WEIGHT_LIMIT: usize = 6_000;

/// The most body an embed carries, as the service limits it.
const EMBED_DESCRIPTION_LIMIT: usize = 4_096;

/// What `!usage` found, as a card.
///
/// A provider is its field: the name on its own row, and under it one line
/// with what is left and, when the provider named a moment, the service's
/// own timestamp, which counts down in place. A provider that named no
/// reset says only what is left: a dash would repeat the line above it
/// without saying anything new.
///
/// Past the card's own limit of cells the windows fall back to the table in
/// the body. A reading of nothing says so, rather than drawing an empty
/// card.
///
/// The color carries the verdict: amber when a provider has spent its
/// window, green otherwise. A provider that did not answer is not a
/// warning; it says unknown and the card stays calm.
pub fn usage_card(rows: &[(String, Option<Quota>)], now: i64) -> Card {
    let spent = rows
        .iter()
        .any(|(_, quota)| quota.as_ref().is_some_and(is_spent));
    let description = if rows.len() > EMBED_FIELD_LIMIT {
        usage_table(rows, now)
    } else if rows.is_empty() {
        "no provider reports a usage window".to_owned()
    } else {
        String::new()
    };
    Card {
        color: if spent {
            CardColor::Warning
        } else {
            CardColor::Ready
        },
        title: "usage windows".to_owned(),
        description,
        fields: rows
            .iter()
            .take(EMBED_FIELD_LIMIT)
            .map(|(provider, quota)| CardField {
                name: provider.clone(),
                value: window_line(quota.as_ref()),
            })
            .collect(),
        footer: None,
    }
}

/// What one provider says of its window, as its cell's text.
fn window_line(quota: Option<&Quota>) -> String {
    let Some(quota) = quota else {
        return "unknown".to_owned();
    };
    let left = if is_spent(quota) {
        "**spent**".to_owned()
    } else {
        format!("**{}** left", left_of(quota))
    };
    match quota.resets_at {
        None => left,
        // The service's own seconds, which the client counts down live, on
        // the same line as what is left, so a provider is one row of facts
        // under its name.
        Some(at) => format!("{left} \u{b7} resets <t:{}:R>", at / 1000),
    }
}

/// A markdown table's one row, cut into its cells.
///
/// The line has to be fenced by pipes on both ends, which is how every
/// writer of a table writes one; the cells are trimmed of the padding a
/// writer puts around them.
fn cells_of(line: &str) -> Option<Vec<String>> {
    let inside = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    Some(inside.split('|').map(|cell| cell.trim().to_owned()).collect())
}

/// Whether a row is the rule between a table's head and its body: dashes
/// on every cell, with an alignment colon allowed at either end, which is
/// part of the syntax and not a word to draw.
fn is_rule(cells: &[String]) -> bool {
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let dashes = cell.trim_matches(':');
            !dashes.is_empty() && dashes.chars().all(|ch| ch == '-')
        })
}

/// Reads the whole text as one markdown table: a head, a rule under it, and
/// rows as wide as the head.
fn parse_table(text: &str) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let mut lines = text.lines();
    let header = cells_of(lines.next()?)?;
    let rule = cells_of(lines.next()?)?;
    if header.len() < 2 || rule.len() != header.len() || !is_rule(&rule) {
        return None;
    }
    let mut rows = Vec::new();
    for line in lines {
        let row = cells_of(line)?;
        if row.len() != header.len() {
            return None;
        }
        rows.push(row);
    }
    Some((header, rows))
}

/// A table drawn whole, its columns padded so every row lines up under the
/// fixed-width font of a fence.
fn aligned_table(header: &[String], rows: &[Vec<String>]) -> String {
    let mut table = vec![header.to_vec()];
    table.extend(rows.iter().cloned());
    let mut widths = vec![0; header.len()];
    for row in &table {
        for (cell, width) in row.iter().zip(&mut widths) {
            *width = (*width).max(cell.chars().count());
        }
    }
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
    lines.join("\n")
}

/// A markdown table, drawn as a card.
///
/// A table of two columns is a name and its value, so it becomes labeled
/// cells: nothing to misalign, and the same layout the usage card draws. A
/// wider table has columns a reader maps values back to, which stacked
/// cells lose, so it is drawn whole as an aligned block, which the fence's
/// fixed-width font keeps lined up. Anything else (a ragged or unheaded
/// table, more rows than the card has cells for, more text than an embed
/// holds) is not drawn at all: the caller keeps the words exactly as the
/// writer wrote them.
pub fn table_card(title: &str, text: &str) -> Option<Card> {
    let (header, rows) = parse_table(text.trim())?;
    if header.len() == 2 {
        if rows.len() > EMBED_FIELD_LIMIT {
            return None;
        }
        let head = format!("**{}**", header.join(" \u{b7} "));
        let mut weight = head.encode_utf16().count();
        let mut fields = Vec::with_capacity(rows.len());
        for row in rows {
            let (name, value) = (row[0].clone(), row[1].clone());
            // A cell with nothing to head its field with would draw a
            // label over a value nobody can place.
            if name.is_empty() {
                return None;
            }
            weight += name.encode_utf16().count() + value.encode_utf16().count();
            fields.push(CardField { name, value });
        }
        if weight > EMBED_WEIGHT_LIMIT {
            return None;
        }
        return Some(Card {
            color: CardColor::Ready,
            title: title.to_owned(),
            description: head,
            fields,
            footer: None,
        });
    }
    let description = format!("```\n{}\n```", aligned_table(&header, &rows));
    if description.encode_utf16().count() > EMBED_DESCRIPTION_LIMIT {
        return None;
    }
    Some(Card {
        color: CardColor::Ready,
        title: title.to_owned(),
        description,
        fields: Vec::new(),
        footer: None,
    })
}

/// Splits an answer at its markdown tables, each table drawn as a card and
/// the words around it kept as they were written.
///
/// Every table outside a code fence that can be drawn becomes a piece: the
/// words written before it travel with the card as the message's content,
/// when they fit one message, and words written after follow as their own
/// message, because a card draws under the words it travels with and
/// lifting a followed table would turn the answer around. A table that
/// cannot be drawn stays in the words; an answer with no table comes back
/// as it went in, so a surface without cards posts exactly what it would
/// have without any of this.
pub fn split_tables(text: &str) -> Vec<(String, Option<Card>)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut pieces: Vec<(String, Option<Card>)> = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    let mut fenced = false;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            words.push(line);
            at += 1;
            continue;
        }
        if !fenced && line.trim_start().starts_with('|') {
            let mut end = at;
            while end < lines.len() && lines[end].trim_start().starts_with('|') {
                end += 1;
            }
            if let Some(card) = table_card("", &lines[at..end].join("\n")) {
                let intro = words.join("\n").trim().to_owned();
                if intro.encode_utf16().count() > MESSAGE_LIMIT {
                    // Words too long for one message travel on their own,
                    // where the ordinary split handles their length.
                    pieces.push((intro, None));
                    pieces.push((String::new(), Some(card)));
                } else {
                    pieces.push((intro, Some(card)));
                }
                words.clear();
                at = end;
                continue;
            }
        }
        words.push(line);
        at += 1;
    }
    if pieces.is_empty() {
        return vec![(text.to_owned(), None)];
    }
    let rest = words.join("\n").trim().to_owned();
    if !rest.is_empty() {
        pieces.push((rest, None));
    }
    pieces
}

/// A short name is shown beside the model it stands for.
///
/// The first line is empty, so the intro the caller writes stands apart from
/// the first header. Shared because a session lists from its own model and
/// the daemon lists from the catalog, and the two must not drift apart.
pub fn listed_models(
    available: &[AvailableModel],
    current_provider: &str,
    current_model: &str,
    aliases: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut ordered: Vec<&AvailableModel> = available.iter().collect();
    // Stable, so the order a provider lists its models in survives.
    ordered.sort_by_key(|model| (model.provider != current_provider, &model.provider));

    let mut served: BTreeMap<&str, usize> = BTreeMap::new();
    for model in available {
        *served.entry(model.id.as_str()).or_default() += 1;
    }

    let mut lines = vec![String::new()];
    let mut group: Option<&str> = None;
    for model in ordered {
        if group != Some(model.provider.as_str()) {
            if group.is_some() {
                lines.push(String::new());
            }
            group = Some(model.provider.as_str());
            lines.push(format!("**{}**", model.provider));
        }

        let mut name = if served[model.id.as_str()] > 1 {
            model.qualified()
        } else {
            model.id.clone()
        };
        if let Some(level) = &model.default_level {
            name.push_str(level);
        }
        let mut line = if model.provider == current_provider && model.id == current_model {
            format!("- **`{name}`**")
        } else {
            format!("- `{name}`")
        };
        for short in short_names(aliases, model) {
            line.push(' ');
            line.push_str(&short);
        }
        lines.push(line);
    }
    lines
}

/// The short names that stand for one model, in the order they were written.
///
/// A short name spelled the same as the model teaches nobody anything, so it
/// is left out rather than shown beside the name it repeats.
pub fn short_names(aliases: &BTreeMap<String, String>, model: &AvailableModel) -> Vec<String> {
    let qualified = model.qualified();
    aliases
        .iter()
        .filter(|(short, target)| {
            let bare = split_level(target).0;
            (bare == qualified || bare == model.id) && **short != model.id
        })
        .map(|(short, _)| format!("`{short}`"))
        .collect()
}

/// A short name spelled the same as the model teaches nobody anything, so it
/// is left out rather than shown beside the name it repeats.
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
pub(crate) fn widen(count: u64) -> f64 {
    count as f64
}

/// The caption an image or clip the agent made is posted with: where the
/// tool saved it when it did, its size, and in brackets what its server
/// said, such as a fallback to another model, so a reader sees it beside the
/// media. `noun` is what was made, "an image" or "a clip".
///
/// The note is the server's free text, so it goes through [`server_text`]
/// and sits in a code span. The path is made inert, so neither can ping
/// anyone or close the code span.
pub fn media_caption(saved: Option<&str>, size: f64, note: Option<&str>, noun: &str) -> String {
    let mut caption = match saved {
        Some(path) => format!("generated `{}` {}", inert(path), bytes(size)),
        None => format!("generated {noun}, {}", bytes(size)),
    };
    let note = server_text(note.unwrap_or_default());
    if !note.is_empty() {
        let _ = write!(caption, " [`{note}`]");
    }
    caption
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
