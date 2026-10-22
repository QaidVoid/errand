//! Reading and showing the project from a thread.
//!
//! The containment rule is `crate::sandbox::paths`. This is the session half:
//! resolving what somebody typed, deciding what is small enough to show,
//! reporting an edit as a diff rather than as a name, and posting an image the
//! agent made as an upload rather than as a path.

use std::fmt::Write as _;

use super::{IncomingMessage, MAX_DIFFABLE_BYTES, MAX_IMAGES_PER_TURN, MAX_INLINE, Running};
use crate::agent::protocol::ProducedImage;
use crate::chat::diff::file_diff;
use crate::chat::render::{
    bytes as byte_count, connection_line, directory_listing, file_view, widen,
};
use crate::log::{LogValue, fields};
use crate::provider::images::{endpoint_with_effort, image_extension};
use crate::sandbox::paths;
use crate::session::commands::image_args;
use crate::session::event::{ReactionOutcome, SessionEvent};
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
    ///
    /// Opened beneath the project, the same rule `read_path` answers `!cat`
    /// with. This used to be the one read that went by name: a link planted
    /// where the agent writes would have had the daemon read whatever it
    /// pointed at and post it to the channel.
    pub(super) async fn upload_file(&mut self, request: &str) {
        use std::io::Read;

        let wanted = request.trim();
        if wanted.is_empty() {
            self.say("say which file, as `!file <path>`").await;
            return;
        }

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
        let mut file = match paths::open_beneath(&root, &display, &paths::OpenOptions::read()) {
            Ok(file) => file,
            Err(error) => {
                self.say(&format!("could not read `{wanted}`: {error}"))
                    .await;
                return;
            }
        };
        // Measured on the file that was opened, so what is checked and what is
        // sent are the same however the tree changes underneath.
        let meta = match file.metadata() {
            Ok(meta) => meta,
            Err(error) => {
                self.say(&format!("could not read `{wanted}`: {error}"))
                    .await;
                return;
            }
        };
        if !meta.is_file() {
            self.say(&format!("`{wanted}` is not a file")).await;
            return;
        }
        let limit = self.options.config.output.max_upload_bytes;
        if meta.len() > limit {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let kilobytes = (meta.len() as f64 / 1024.0).round() as u64;
            self.say(&format!(
                "`{wanted}` is {kilobytes} KB, larger than the {limit} byte upload limit"
            ))
            .await;
            return;
        }
        let mut bytes = Vec::new();
        if let Err(error) = file.read_to_end(&mut bytes) {
            self.say(&format!("could not read `{wanted}`: {error}"))
                .await;
            return;
        }
        let name = host.rsplit('/').next().unwrap_or("file").to_owned();
        self.views
            .send(SessionEvent::Upload {
                name,
                bytes,
                caption: format!("`{}` {} bytes", display, meta.len()),
            })
            .await;
    }

    /// Draws a picture with the image model the configuration names.
    ///
    /// One request to `POST {baseUrl}/images/generations`, answered as bytes,
    /// which are posted to the thread as an upload. Deliberately not a turn:
    /// sending the prompt to a chat model is what used to reach
    /// `/chat/completions` and come back as a link in prose instead of a
    /// picture.
    ///
    /// The model is `agent.imageGen` unless the command names one, read as
    /// `!model` reads a name, so an alias and a `provider/id` both work. The
    /// effort is a level, asked for on the command or carried by the name, and
    /// one asked for replaces the other.
    pub(super) async fn generate_image(&mut self, rest: &str, message: &IncomingMessage) {
        let asked = match image_args(rest) {
            Ok(asked) => asked,
            Err(refused) => {
                self.say(&refused).await;
                self.react(&message.id, ReactionOutcome::Failed).await;
                return;
            }
        };
        if asked.prompt.is_empty() {
            self.say(
                "say what to generate, as `!gen-image [--effort <level>] [<model>:<level>] <prompt>`",
            )
            .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        }
        let configured = self
            .options
            .config
            .agent
            .image_gen
            .as_deref()
            .map(str::trim)
            .filter(|named| !named.is_empty());
        // A model named on the command is the more specific thing to have
        // said, so it beats the configured one.
        let Some(named) = asked.model.or(configured) else {
            self.say("no image model is configured; name one with `agent.imageGen`")
                .await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        };
        let Some(draw_image) = self.options.draw_image.clone() else {
            self.say("this host has no way to draw an image").await;
            self.react(&message.id, ReactionOutcome::Failed).await;
            return;
        };

        // Read from the catalog rather than this session's own copy of the
        // configuration, so a name added since it was launched still works.
        let endpoint = match endpoint_with_effort(
            &self.options.config.agent,
            &self.options.catalog.models(),
            named,
            &self.options.catalog.aliases(),
            asked.effort.as_deref(),
        ) {
            Ok(endpoint) => endpoint,
            Err(refused) => {
                self.say(&refused.0).await;
                self.react(&message.id, ReactionOutcome::Failed).await;
                return;
            }
        };
        let prompt = asked.prompt.to_owned();

        self.react(&message.id, ReactionOutcome::Accepted).await;
        self.say(&connection_line(&format!(
            "drawing this on `{}`",
            endpoint.model
        )))
        .await;

        let drawn = match draw_image(endpoint.clone(), prompt).await {
            Ok(drawn) => drawn,
            Err(refused) => {
                self.say(&refused).await;
                self.react(&message.id, ReactionOutcome::Failed).await;
                return;
            }
        };
        let limit = self.options.config.output.max_upload_bytes;
        let picture = match drawn.into_picture(&endpoint.model, limit) {
            Ok(picture) => picture,
            Err(refused) => {
                self.say(&refused.0).await;
                self.react(&message.id, ReactionOutcome::Failed).await;
                return;
            }
        };

        self.log.info(
            "posting a picture the command drew",
            &fields([
                ("model", LogValue::from(endpoint.model.as_str())),
                ("bytes", LogValue::from(picture.bytes.len())),
            ]),
        );
        self.views
            .send(SessionEvent::Upload {
                name: picture.name,
                bytes: picture.bytes,
                caption: picture.caption,
            })
            .await;
    }

    /// Posts an image the agent made as an upload, captioned with its size,
    /// where the tool saved it when it did, and what its server said about
    /// it. One that cannot be posted is reported in the thread with the
    /// reason instead, so somebody told that an image was made is not left
    /// waiting for it. Either way it counts toward the turn's cap.
    pub(super) async fn post_image(&mut self, image: &ProducedImage) {
        if self.turn_images >= MAX_IMAGES_PER_TURN {
            if self.turn_images == MAX_IMAGES_PER_TURN {
                self.say(&format!(
                    "the agent made more than {MAX_IMAGES_PER_TURN} images this turn, so the \
                     rest were not posted"
                ))
                .await;
            }
            self.turn_images += 1;
            return;
        }
        self.turn_images += 1;
        let saved = image
            .path
            .as_deref()
            .filter(|path| self.host_path(path).is_some())
            .map(|path| self.display_path(path));
        let size = u64::try_from(image.bytes.len()).unwrap_or(u64::MAX);
        let limit = self.options.config.output.max_upload_bytes;
        let refusal = if size > limit {
            Some(format!(
                "the agent made a {} image, over the {} upload limit, so it was not posted",
                byte_count(widen(size)),
                byte_count(widen(limit))
            ))
        } else if image_extension(&image.bytes).is_none() {
            Some(
                "the agent made an image that is not a PNG, JPEG, GIF or WebP, so it was not \
                 posted"
                    .to_owned(),
            )
        } else {
            None
        };
        if let Some(refusal) = refusal {
            self.log.warn(
                "an image the agent made was not posted",
                &fields([("reason", LogValue::from(refusal.as_str()))]),
            );
            self.say(&refusal).await;
            return;
        }
        let name = saved
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .filter(|name| !name.is_empty())
            .map_or_else(
                || format!("image.{}", image_extension(&image.bytes).unwrap_or("png")),
                str::to_owned,
            );
        self.log.info(
            "posting an image the agent made",
            &fields([("name", LogValue::from(name.as_str()))]),
        );
        let caption = image_caption(saved.as_deref(), widen(size), image.note.as_deref());
        self.views
            .send(SessionEvent::Upload {
                name,
                bytes: image.bytes.clone(),
                caption,
            })
            .await;
    }
}

/// The longest part of an image server's note shown in a caption.
const MAX_NOTE_CHARS: usize = 300;

/// The caption an image the agent made is posted with: where the tool saved
/// it when it did, its size, and in brackets what its server said, such as a
/// fallback to another model, so a reader sees it beside the picture.
///
/// The note is the server's free text: it is kept to printable ASCII on one
/// line and cut short, with `...` where it was cut, so it cannot carry
/// control characters or a wall of text into the thread.
fn image_caption(saved: Option<&str>, size: f64, note: Option<&str>) -> String {
    let mut caption = match saved {
        Some(path) => format!("generated `{path}` {}", byte_count(size)),
        None => format!("generated an image, {}", byte_count(size)),
    };
    let printable: String = note
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_graphic() { c } else { ' ' })
        .collect();
    let whole = printable.split_whitespace().collect::<Vec<_>>().join(" ");
    let note = if whole.len() > MAX_NOTE_CHARS {
        format!("{}...", whole[..MAX_NOTE_CHARS].trim_end())
    } else {
        whole
    };
    if !note.is_empty() {
        let _ = write!(caption, " [{note}]");
    }
    caption
}
