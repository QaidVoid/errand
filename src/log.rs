//! Structured ASCII-only logging.
//!
//! Chat output may carry emoji; a log line describing it may not. Escaping
//! rather than dropping keeps the line greppable and lossless without making a
//! log file's readability depend on terminal font coverage.

use std::collections::BTreeMap;
use std::sync::Arc;

/// Severity of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Ordinary progress.
    Info,
    /// Something degraded but survivable.
    Warn,
    /// Something failed that a person should look at.
    Error,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

/// The value of one extra field on a line.
#[derive(Debug, Clone, PartialEq)]
pub enum LogValue {
    /// A string value.
    Text(String),
    /// A whole number.
    Number(i64),
    /// A flag.
    Flag(bool),
}

impl From<&str> for LogValue {
    fn from(value: &str) -> Self {
        LogValue::Text(value.to_owned())
    }
}

impl From<String> for LogValue {
    fn from(value: String) -> Self {
        LogValue::Text(value)
    }
}

impl From<i64> for LogValue {
    fn from(value: i64) -> Self {
        LogValue::Number(value)
    }
}

impl From<usize> for LogValue {
    #[expect(clippy::cast_possible_wrap)]
    fn from(value: usize) -> Self {
        LogValue::Number(value as i64)
    }
}

impl From<bool> for LogValue {
    fn from(value: bool) -> Self {
        LogValue::Flag(value)
    }
}

/// Extra key and value pairs appended to a line.
pub type LogFields = BTreeMap<String, LogValue>;

/// Builds fields from pairs, so a call site reads as the line will.
pub fn fields<const N: usize>(pairs: [(&str, LogValue); N]) -> LogFields {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// Where a line goes. Injected so a test reads lines instead of a stream.
pub type Sink = Arc<dyn Fn(LogLevel, &str) + Send + Sync>;

const MAX_ASCII: char = '\u{7f}';

/// Replaces every non-ASCII character with its `\u{XXXX}` escape, so that a
/// line describing an emoji-bearing message is still pure ASCII.
pub fn to_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character > MAX_ASCII {
            out.push_str("\\u{");
            let digits = format!("{:04X}", character as u32);
            out.push_str(&digits);
            out.push('}');
        } else {
            out.push(character);
        }
    }
    out
}

fn format_fields(fields: &LogFields) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = fields
        .iter()
        .map(|(key, value)| {
            let shown = match value {
                LogValue::Text(text) => text.clone(),
                LogValue::Number(number) => format!("{number}"),
                LogValue::Flag(flag) => format!("{flag}"),
            };
            format!("{key}={}", to_ascii(&shown))
        })
        .collect();
    format!(" {}", parts.join(" "))
}

/// Formats one line without writing it, so it can be asserted on.
///
/// The time leads, because the first question asked of a daemon's log is when
/// something happened, and a line without one is only useful while the process
/// that wrote it is still running.
pub fn format_line(level: LogLevel, message: &str, fields: &LogFields, at_ms: i64) -> String {
    let moment = jiff::Timestamp::from_millisecond(at_ms).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let civil = moment.to_zoned(jiff::tz::TimeZone::UTC).datetime();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z [{}] {}{}",
        civil.year(),
        civil.month(),
        civil.day(),
        civil.hour(),
        civil.minute(),
        civil.second(),
        moment.subsec_nanosecond() / 1_000_000,
        level.as_str(),
        to_ascii(message),
        format_fields(fields),
    )
}

/// Writes to the process streams directly.
///
/// Synchronously, so that a line written just before a crash is on disk rather
/// than in a buffer that never flushed.
pub fn stream_sink(level: LogLevel, line: &str) {
    use std::io::Write;

    let line = format!("{line}\n");
    if level == LogLevel::Error {
        let _ = std::io::stderr().write_all(line.as_bytes());
    } else {
        let _ = std::io::stdout().write_all(line.as_bytes());
    }
}

/// A logger, optionally bound to fields it repeats on every line.
#[derive(Clone)]
pub struct Logger {
    base: Arc<LogFields>,
    sink: Sink,
}

impl Logger {
    /// Creates a logger, optionally carrying fields on every line it writes.
    pub fn new(base: LogFields, sink: Sink) -> Self {
        Self {
            base: Arc::new(base),
            sink,
        }
    }

    fn emit(&self, level: LogLevel, message: &str, fields: &LogFields) {
        let mut merged = (*self.base).clone();
        for (key, value) in fields {
            merged.insert(key.clone(), value.clone());
        }
        (self.sink)(level, &format_line(level, message, &merged, now_ms()));
    }

    /// Reports ordinary progress.
    pub fn info(&self, message: &str, fields: &LogFields) {
        self.emit(LogLevel::Info, message, fields);
    }

    /// Reports something degraded but survivable.
    pub fn warn(&self, message: &str, fields: &LogFields) {
        self.emit(LogLevel::Warn, message, fields);
    }

    /// Reports something failed that a person should look at.
    pub fn error(&self, message: &str, fields: &LogFields) {
        self.emit(LogLevel::Error, message, fields);
    }

    /// Returns a logger that adds the given fields to every line.
    pub fn with(&self, fields: LogFields) -> Logger {
        let mut merged = (*self.base).clone();
        for (key, value) in fields {
            merged.insert(key, value);
        }
        Logger {
            base: Arc::new(merged),
            sink: Arc::clone(&self.sink),
        }
    }
}

/// The current time, in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            #[expect(clippy::cast_possible_truncation)]
            let millis = since.as_millis() as i64;
            millis
        })
}

/// The logger the daemon runs on, writing to the process streams.
pub fn logger() -> Logger {
    Logger::new(LogFields::new(), Arc::new(stream_sink))
}

#[cfg(test)]
mod tests;
