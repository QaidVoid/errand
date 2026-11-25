//! What `!status` reports that the agent does not keep: when the session
//! started and the last totals it reported.
//!
//! Kept with the session's record after each turn, so a session resumed
//! after a restart reports itself as a whole rather than only since the
//! daemon came back. The agent replays its own totals once it reports again;
//! these cover the time before it has.

use serde::{Deserialize, Serialize};

use super::Running;
use crate::log::{LogValue, fields};
use crate::session::event::SessionUsage;
use crate::session::record::{prepare_record_dir, record_dir};

/// The file beside the session's record that holds what it saved.
const SAVED_FILENAME: &str = "status.json";

/// What one session saved about itself.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    /// When the session first started, in epoch milliseconds.
    started_at: i64,
    /// The totals it last reported.
    usage: SessionUsage,
}

impl Running {
    /// Saves when the session started and what it has spent, for a resume.
    pub(super) fn save_status(&self) {
        let saved = Saved {
            started_at: self.started_at,
            usage: self.usage.clone(),
        };
        let path =
            std::path::Path::new(&prepare_record_dir(&self.options.state_dir)).join(SAVED_FILENAME);
        let written = serde_json::to_string(&saved)
            .map_err(|error| error.to_string())
            .and_then(|text| std::fs::write(&path, text).map_err(|error| error.to_string()));
        if let Err(error) = written {
            self.log.warn(
                "the session's start and totals could not be saved, so a resume reports them \
                 only from then on",
                &fields([("detail", LogValue::from(error))]),
            );
        }
    }

    /// Takes back what an earlier run of this session saved, when it saved
    /// anything. A session from before this was kept has nothing to take.
    pub(super) fn restore_status(&mut self) {
        let path = std::path::Path::new(&record_dir(&self.options.state_dir)).join(SAVED_FILENAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.log.warn(
                    "the session's saved start and totals could not be read, so they count \
                     from now",
                    &fields([("detail", LogValue::from(error.to_string()))]),
                );
                return;
            }
        };
        match serde_json::from_str::<Saved>(&text) {
            Ok(saved) => {
                self.started_at = saved.started_at;
                self.usage = saved.usage;
            }
            Err(error) => self.log.warn(
                "the session's saved start and totals do not read, so they count from now",
                &fields([("detail", LogValue::from(error.to_string()))]),
            ),
        }
    }
}
