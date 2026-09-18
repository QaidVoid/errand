//! The provider modules: what the host knows about models, what is left of a
//! metered window, and the one-question calls made beside a session.

/// One question put to a model beside a session.
pub mod ask;
/// Reads a metered window from a gateway that reports one.
pub mod gateway;
/// The host's model store: which models exist and what they cost.
pub mod models;
/// Holds a window answer so every session does not ask again.
pub mod usage;
/// Describes an image for a session whose model cannot see one.
pub mod vision;
/// Reads a metered window from z.ai's own endpoint.
pub mod zai;
