//! The session layer: one agent turn, its views, its record, and everything
//! it needs that is not the chat connection.

pub mod attachments;
pub mod commands;
pub mod disk;
pub mod event;
pub mod files;
pub mod github;
pub mod ids;
pub mod model;
pub mod pr;
pub mod projects;
pub mod record;
pub mod redacted;
pub mod registry;
pub mod rules;
pub mod transcript;
pub mod views;
