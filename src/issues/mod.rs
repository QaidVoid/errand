//! GitHub issues and pull requests as a place work is asked for and answered.
//!
//! Beside the chat channel, not instead of it. A mention of the bot's account
//! on an issue or pull request, or an assignment to it, starts a session the
//! way a message in the channel does, with the issue as its thread: later
//! comments continue it, and each turn is answered with one comment.

/// Asks GitHub what was said to the bot, and turns it into messages.
pub mod poll;
/// Sends each thread to the surface it lives on.
pub mod route;
/// Names an issue or pull request as a thread a session lives in.
pub mod thread;
/// An issue as a thread: made, found again, and written to.
pub mod view;
