//! The chat layer: how agent output becomes messages, and the character table
//! that governs everything the daemon says.

/// Every glyph the daemon may use, enumerated by codepoint.
pub mod chars;
/// The slash commands, and their registration with the service.
pub mod commands;
/// Renders a patch small enough for a message.
pub mod diff;
/// The connection to the chat service, and what it reports.
pub mod gateway;
/// Decides whether an arriving message is for this daemon.
pub mod inbound;
/// One ordered, buffered work queue per thread.
pub mod outbox;
/// Turns what a session produces into message text.
pub mod render;
/// A thread as a session writes to it.
pub mod threads;
