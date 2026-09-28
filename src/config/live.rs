//! The configuration the daemon is running on, which may be replaced while
//! it runs.
//!
//! Reading the file and holding what it said are separate. The file is read
//! once at startup, where a configuration that cannot be parsed is a refusal
//! to start, and again whenever it changes, where the same failure is not:
//! a daemon that stops serving because an edit was caught mid-keystroke
//! would be worse than one that carries on with what it already had.
//!
//! So a rejected reload leaves the running configuration exactly as it was,
//! and says why in the log. Nothing is applied until the whole file has been
//! read and validated, which is what makes a half-written file harmless.

use std::sync::{Arc, RwLock};

use crate::config::schema::{Config, ConfigError};

/// What a reload attempt did.
#[derive(Debug, Clone, PartialEq)]
pub enum Reloaded {
    /// The file was read and says what the daemon is already running on.
    Unchanged,
    /// A different configuration is now in force.
    Changed,
    /// The file could not be used, and what the daemon is running on is
    /// untouched. Carries every reason it was refused.
    Rejected(Vec<String>),
}

/// The configuration in force, shared rather than copied into each holder, so
/// that one replace reaches all of them.
///
/// Shaped like the provider catalog, which is replaced the same way when the
/// models behind it are asked for again. A holder reads the whole
/// configuration and works from that copy, so a session keeps what it was
/// launched with and the next one picks up the new file.
#[derive(Clone)]
pub struct LiveConfig {
    inner: Arc<RwLock<Config>>,
}

impl LiveConfig {
    /// A configuration in force, which is the one given.
    pub fn new(config: Config) -> Self {
        Self {
            inner: Arc::new(RwLock::new(config)),
        }
    }

    /// The configuration in force, as a copy for a holder to work from.
    ///
    /// Copying out rather than lending the lock, so a caller may hold what it
    /// read across an await without a reader blocking the reload.
    pub fn get(&self) -> Config {
        self.inner.read().expect("the configuration lock").clone()
    }

    /// Puts a configuration in force, and says whether it was different.
    ///
    /// A file that resolves to what is already in force is not a change, and
    /// is reported as such so a save with no edit in it does not read as a
    /// reload.
    pub fn replace(&self, incoming: Config) -> bool {
        let mut held = self.inner.write().expect("the configuration lock");
        if *held == incoming {
            return false;
        }
        *held = incoming;
        true
    }

    /// Applies whatever a read of the file produced, refusing nothing.
    ///
    /// A [`ConfigError`] is a reason to carry on with what is in force rather
    /// than a reason to stop: the daemon is serving under a configuration
    /// that was valid when it was read, and an edit that does not parse has
    /// not made that any less true. Every reason is returned, so the log can
    /// say what to fix.
    pub fn reload(&self, incoming: Result<Config, ConfigError>) -> Reloaded {
        match incoming {
            Err(error) => Reloaded::Rejected(error.problems),
            Ok(incoming) => {
                if self.replace(incoming) {
                    Reloaded::Changed
                } else {
                    Reloaded::Unchanged
                }
            }
        }
    }
}

impl std::fmt::Debug for LiveConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The configuration holds credentials, so a debug rendering of it
        // would put them in whatever read this.
        f.debug_struct("LiveConfig").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
