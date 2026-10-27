//! The web interface: which addresses it may bind to, what it serves, and how
//! a connected browser becomes another view of a session.

/// Which addresses the interface may bind to.
pub mod address;
/// Discord login, and who a browser acts as.
pub mod login;
/// The routes the interface serves.
pub mod server;
/// A connected browser as another view of a session.
pub mod view;
