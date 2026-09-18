//! Reading and showing the project from a thread.
//!
//! The containment rule is `crate::sandbox::paths`. This is the session half:
//! resolving what somebody typed, deciding what is small enough to show, and
//! reporting an edit as a diff rather than as a name.

use super::{MAX_DIFFABLE_BYTES, MAX_INLINE, MAX_UPLOAD_BYTES, Running};
use crate::chat::diff::file_diff;
use crate::chat::render::{directory_listing, file_view};
use crate::log::{LogValue, fields};
use crate::sandbox::paths;
use crate::session::event::SessionEvent;
use crate::session::files::{NotAFileError, read_directory, read_file_for_display};

impl Running {
    /// Records a file's contents before an edit, so the change can be shown.
    /// Reads a project file, refusing a link however the path is spelled.
    ///
    /// The agent writes this tree, so a file the daemon is about to read can
    /// have become a link to somewhere else since it was last looked at.
    fn read_contained(&self, agent_path: &str) -> std::io::Result<String> {
        let display = self.display_path(agent_path);
        paths::read_beneath(&self.options.project.path, &display)
    }

    pub(super) fn snapshot(&mut self, agent_path: &str) {
        let Some(host) = self.host_path(agent_path) else {
            return;
        };
        match std::fs::metadata(&host) {
            Ok(meta) if meta.is_file() && meta.len() <= MAX_DIFFABLE_BYTES => {
                if let Ok(text) = self.read_contained(agent_path) {
                    self.remember_edit(agent_path, text);
                }
            }
            // A file that does not exist yet is an empty one for diffing
            // purposes.
            _ => self.remember_edit(agent_path, String::new()),
        }
    }

    pub(super) fn remember_edit(&mut self, agent_path: &str, before: String) {
        if let Some(existing) = self
            .pending_edits
            .iter_mut()
            .find(|(path, _)| path == agent_path)
        {
            existing.1 = before;
        } else {
            self.pending_edits.push((agent_path.to_owned(), before));
        }
    }

    /// Posts what an edit changed, once the tool has finished.
    pub(super) async fn report_edit(&mut self, tool_name: &str, call: &str) {
        if !self.options.config.output.post_diffs {
            self.pending_edits.clear();
            return;
        }

        let edits = std::mem::take(&mut self.pending_edits);
        for (agent_path, before) in edits {
            let Some(host) = self.host_path(&agent_path) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&host) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_DIFFABLE_BYTES {
                continue;
            }
            let Ok(after) = self.read_contained(&agent_path) else {
                continue;
            };

            let diff = file_diff(&before, &after);
            if diff.empty {
                continue;
            }
            self.log.info(
                "posting an edit",
                &fields([
                    ("tool", LogValue::from(tool_name)),
                    ("path", LogValue::from(agent_path.as_str())),
                ]),
            );
            self.views
                .send(SessionEvent::Diff {
                    path: self.display_path(&agent_path),
                    added: diff.added as u64,
                    removed: diff.removed as u64,
                    body: diff.body,
                    cause: Some(call.to_owned()),
                })
                .await;
        }
    }

    pub(super) fn host_path(&self, agent_path: &str) -> Option<String> {
        self.sandbox
            .as_ref()
            .and_then(|sandbox| (sandbox.to_host_path)(agent_path))
    }

    /// A path as a reader would recognise it, relative to the project.
    pub(super) fn display_path(&self, agent_path: &str) -> String {
        let Some(host) = self.host_path(agent_path) else {
            return agent_path.to_owned();
        };
        let root = &self.options.project.path;
        match host.strip_prefix(root) {
            Some(rest) => rest.trim_start_matches('/').to_owned(),
            None => host,
        }
    }

    /// Lists a directory or shows a file, without involving the agent.
    pub(super) async fn read_path(&mut self, request: &str) {
        let wanted = request.trim();
        let wanted = if wanted.is_empty() { "." } else { wanted };
        let root = self.options.project.path.clone();
        let Some(host) = self.host_path(wanted) else {
            self.say(&format!("`{wanted}` is not inside this session's project"))
                .await;
            return;
        };

        let display = {
            let shown = self.display_path(wanted);
            if shown.is_empty() {
                ".".to_owned()
            } else {
                shown
            }
        };
        let result = std::fs::metadata(&host).map(|meta| meta.is_dir());
        match result {
            Ok(true) => {
                // A directory is listed whichever was asked for: `!cat` on
                // one is a mistake worth answering rather than an error worth
                // reporting.
                match read_directory(&root, &display) {
                    Ok(entries) => {
                        self.say(&directory_listing(&entries, &display)).await;
                    }
                    Err(error) => {
                        self.say(&format!("could not read `{wanted}`: {error}"))
                            .await;
                    }
                }
            }
            Ok(false) => match read_file_for_display(&root, &display, MAX_INLINE) {
                Ok(contents) => {
                    let shown = file_view(&contents);
                    self.say(&shown).await;
                }
                Err(NotAFileError(path)) => {
                    self.say(&format!("could not read `{wanted}`: {path} is not a file"))
                        .await;
                }
            },
            Err(error) => {
                self.say(&format!("could not read `{wanted}`: {error}"))
                    .await;
            }
        }
    }

    /// Uploads a file from the project on request.
    pub(super) async fn upload_file(&mut self, request: &str) {
        let wanted = request.trim();
        if wanted.is_empty() {
            self.say("say which file, as `!file <path>`").await;
            return;
        }

        let Some(host) = self.host_path(wanted) else {
            self.say(&format!("`{wanted}` is not inside this session's project"))
                .await;
            return;
        };

        match std::fs::metadata(&host) {
            Ok(meta) if !meta.is_file() => {
                self.say(&format!("`{wanted}` is not a file")).await;
            }
            Ok(meta) if meta.len() > MAX_UPLOAD_BYTES => {
                #[expect(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss
                )]
                let kilobytes = (meta.len() as f64 / 1024.0).round() as u64;
                self.say(&format!(
                    "`{wanted}` is {kilobytes} KB, larger than the upload limit"
                ))
                .await;
            }
            Ok(meta) => match std::fs::read(&host) {
                Ok(bytes) => {
                    let name = host.rsplit('/').next().unwrap_or("file").to_owned();
                    self.views
                        .send(SessionEvent::Upload {
                            name,
                            bytes,
                            caption: format!(
                                "`{}` {} bytes",
                                self.display_path(wanted),
                                meta.len()
                            ),
                        })
                        .await;
                }
                Err(error) => {
                    self.say(&format!("could not read `{wanted}`: {error}"))
                        .await;
                }
            },
            Err(error) => {
                self.say(&format!("could not read `{wanted}`: {error}"))
                    .await;
            }
        }
    }
}
