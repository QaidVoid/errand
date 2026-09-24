//! Structured ASCII-only logging.
//!
//! Chat output may carry emoji; a log line describing it may not. Escaping
//! rather than dropping keeps the line greppable and lossless without making a
//! log file's readability depend on terminal font coverage.

use std::collections::BTreeMap;
use std::sync::Arc;

/// Severity of a log line, least verbose first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Something failed that a person should look at.
    Error,
    /// Something degraded but survivable.
    Warn,
    /// Ordinary progress.
    Info,
    /// Each decision the daemon takes, and why. Shown with `-v`.
    Debug,
    /// Each step between decisions. Shown with `-vv`.
    Trace,
    /// What crosses a boundary, verbatim, such as every line exchanged with
    /// the agent. Shown with `-vvv`.
    Wire,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
            LogLevel::Wire => "wire",
        }
    }

    /// The most verbose level shown for a count of `-v` flags.
    pub fn from_verbosity(count: usize) -> Self {
        match count {
            0 => LogLevel::Info,
            1 => LogLevel::Debug,
            2 => LogLevel::Trace,
            _ => LogLevel::Wire,
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

impl From<u64> for LogValue {
    /// A count past `i64::MAX` is not a count anything here produces, and a
    /// log line is not worth failing over, so it is pinned rather than wrapped.
    fn from(value: u64) -> Self {
        LogValue::Number(i64::try_from(value).unwrap_or(i64::MAX))
    }
}

impl From<usize> for LogValue {
    /// A count past `i64::MAX` is not a count anything here produces, and a
    /// log line is not worth failing over, so it is pinned rather than wrapped.
    fn from(value: usize) -> Self {
        LogValue::Number(i64::try_from(value).unwrap_or(i64::MAX))
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
///
/// A control character is escaped the same way, and for a second reason. A
/// record is one line, and a field holding a newline would end it early: the
/// rest would read as a line the daemon wrote, which is a line anybody who can
/// send a chat message could then choose. Escaping keeps a record a record.
pub fn to_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if character > MAX_ASCII || character.is_control() {
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
    max: LogLevel,
}

impl Logger {
    /// Creates a logger that writes info and above, optionally carrying fields
    /// on every line it writes.
    pub fn new(base: LogFields, sink: Sink) -> Self {
        Self {
            base: Arc::new(base),
            sink,
            max: LogLevel::Info,
        }
    }

    /// The same logger, writing everything up to `max`.
    pub fn up_to(mut self, max: LogLevel) -> Self {
        self.max = max;
        self
    }

    /// Whether a line at `level` would be written, so a caller can skip
    /// building one that would not.
    pub fn enabled(&self, level: LogLevel) -> bool {
        level <= self.max
    }

    fn emit(&self, level: LogLevel, message: &str, fields: &LogFields) {
        if !self.enabled(level) {
            return;
        }
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

    /// Reports a decision and why it was taken.
    pub fn debug(&self, message: &str, fields: &LogFields) {
        self.emit(LogLevel::Debug, message, fields);
    }

    /// Reports a step between decisions.
    pub fn trace(&self, message: &str, fields: &LogFields) {
        self.emit(LogLevel::Trace, message, fields);
    }

    /// Reports what crossed a boundary, verbatim.
    pub fn wire(&self, message: &str, fields: &LogFields) {
        self.emit(LogLevel::Wire, message, fields);
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
            max: self.max,
        }
    }
}

/// The current time, in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// The logger the daemon runs on, writing to the process streams up to `max`.
pub fn logger(max: LogLevel) -> Logger {
    Logger::new(LogFields::new(), Arc::new(stream_sink)).up_to(max)
}

#[cfg(test)]
mod tests;
