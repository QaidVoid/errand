//! errand runs a coding agent from a chat channel, in a sandbox it cannot
//! escape.
//!
//! The Rust daemon is ported into this crate module by module, mirroring
//! `src/` one to one. Until the cutover, the TypeScript daemon under `src/`
//! is the implementation and this crate carries the ported modules and the
//! suite that judges them.

// The port lands each module with its tests before anything calls it, so the
// tree holds unusable exports until the daemon is wired (group 10).
#[allow(dead_code)]
mod config;

#[cfg(test)]
mod test_util;

fn main() {}
