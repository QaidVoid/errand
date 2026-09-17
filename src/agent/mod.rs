//! The agent layer, which drives one `pi` subprocess over its JSONL protocol.

/// Drives one agent process and reports what it says.
pub mod client;
/// A turn's delegated questions, and what they cost it.
pub mod delegate;
/// Reads a delegation request out of what the agent asked for.
pub mod delegation;
/// Splits the agent stream into whole JSONL records.
pub mod framing;
/// The shapes the agent speaks in, and the readers for them.
pub mod protocol;
/// The arguments a launcher is given for one session.
pub mod requests;
