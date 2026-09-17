//! The session layer: one agent turn, its views, its record, and everything
//! it needs that is not the chat connection.

/// Fetches what a message carried into the project.
pub mod attachments;
/// Every `!` command, who may run it, and what it says.
pub mod commands;
/// Answers the agent's delegation requests from disk.
pub mod delegating;
/// What a session left on disk, and how much of it.
pub mod disk;
/// Everything a session reports, as one enum.
pub mod event;
/// Reading the project from a thread.
pub mod files;
/// What the host knows about a GitHub checkout.
pub mod github;
/// Names a session and its sandbox.
pub mod ids;
/// Starts, resumes, and ends sessions.
pub mod manager;
/// Resolves what somebody typed to a model the provider serves.
pub mod model;
/// Opens a pull request from what a session changed.
pub mod pr;
/// Which directory a session works in.
pub mod projects;
/// Where a session writes what it will outlive.
pub mod record;
/// Scrubs secrets out of a session event.
pub mod redacted;
/// Remembers threads across restarts.
pub mod registry;
/// The house rules every session is given.
pub mod rules;
/// One session: its turns, its commands, and its ending.
#[allow(
    clippy::module_inception,
    reason = "the session itself lives inside the session layer, as in the original tree"
)]
pub mod session;
/// The append-only record of one session.
pub mod transcript;
/// Delivers a session's output to every attached view.
pub mod views;
