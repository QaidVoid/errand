//! The chat layer: how agent output becomes messages, and the character table
//! that governs everything the daemon says.

pub mod chars;
pub mod commands;
pub mod diff;
pub mod gateway;
pub mod inbound;
pub mod outbox;
pub mod render;
pub mod threads;
