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
mod admission;
#[allow(dead_code)]
mod agent;
#[allow(dead_code)]
mod chat;
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod log;
#[allow(dead_code)]
mod memory;
#[allow(dead_code)]
mod provider;
#[allow(dead_code)]
mod sandbox;
#[allow(dead_code)]
mod session;

#[cfg(test)]
mod test_util;

fn main() {}
