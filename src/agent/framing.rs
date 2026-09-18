//! Line-feed-only record framing for the agent protocol.
//!
//! The agent's protocol uses the line feed as its only record delimiter.
//! Framing therefore happens on bytes, splitting on 0x0A and decoding each
//! record afterwards. Splitting decoded text instead would be wrong: U+2028
//! and U+2029 are legal inside JSON strings, and every general purpose line
//! reader treats them as newlines. That bug only shows up when an agent
//! message happens to contain one, so it is designed out rather than tested
//! for.

const LINE_FEED: u8 = 0x0a;
const CARRIAGE_RETURN: u8 = 0x0d;

/// Raised when a record grows past the configured ceiling without terminating.
#[derive(Debug, thiserror::Error)]
#[error("agent sent a record longer than {limit} bytes without a line feed")]
pub struct RecordTooLargeError {
    /// The ceiling that was exceeded.
    pub limit: usize,
}

/// Accumulates bytes and yields complete records.
///
/// Feeding the same bytes in any chunking yields the same records, so a record
/// split across reads is reassembled rather than lost.
pub struct LineFramer {
    buffer: Vec<u8>,
    max_record_bytes: usize,
}

impl Default for LineFramer {
    fn default() -> Self {
        Self::new(8 * 1024 * 1024)
    }
}

impl LineFramer {
    /// A framer with `max_record_bytes` as its ceiling on one unterminated
    /// record. A stream that exceeds it is a protocol violation, not something
    /// to buffer forever.
    pub fn new(max_record_bytes: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_record_bytes,
        }
    }

    /// Feeds a chunk and returns every record it completed.
    ///
    /// Returns [`RecordTooLargeError`] when the pending record exceeds the
    /// ceiling.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, RecordTooLargeError> {
        let mut combined = std::mem::take(&mut self.buffer);
        combined.extend_from_slice(chunk);

        let mut records = Vec::new();
        let mut start = 0_usize;

        for (index, byte) in combined.iter().enumerate() {
            if *byte != LINE_FEED {
                continue;
            }
            let mut end = index;
            if end > start && combined[end - 1] == CARRIAGE_RETURN {
                end -= 1;
            }
            records.push(String::from_utf8_lossy(&combined[start..end]).into_owned());
            start = index + 1;
        }

        self.buffer = combined.split_off(start);
        if self.buffer.len() > self.max_record_bytes {
            return Err(RecordTooLargeError {
                limit: self.max_record_bytes,
            });
        }
        Ok(records)
    }

    /// Bytes held for a record that has not terminated yet.
    #[allow(
        dead_code,
        reason = "read by this module's tests, which assert on state the daemon never asks for"
    )]
    pub fn pending_bytes(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests;
