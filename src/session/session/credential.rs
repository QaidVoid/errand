//! The session's GitHub App token: minted before the sandbox starts, kept
//! where the `gh` wrapper reads it, and renewed ahead of its expiry.
//!
//! The mechanics are `crate::session::github::app`. This is the session half:
//! when to mint, where the token goes, and what the thread is told.

use super::{Running, SessionTimer};
use crate::log::{LogValue, fields, now_ms};
use crate::sandbox::paths;
use crate::session::github::app::{
    RENEW_BEFORE_MS, RETRY_MS, SessionToken, TOKEN_FILENAME, mint, renew_in_ms, token_line,
};

impl Running {
    /// Mints the session's token when GitHub is configured with an App, and
    /// says why the session cannot start when it could not be had.
    ///
    /// A failure never falls back to the daemon's own token: that would hand
    /// the session the very credential the App is there to keep from it.
    pub(super) async fn mint_github_token(&mut self) -> bool {
        let Some(app) = self
            .options
            .config
            .github
            .as_ref()
            .and_then(|github| github.app.clone())
        else {
            return true;
        };
        let Some(api) = self.options.github_api.clone() else {
            self.say(
                "could not start this session: there is no way to reach GitHub to mint its token",
            )
            .await;
            return false;
        };
        match mint(&app, &api, now_ms()).await {
            Ok(token) => match self.hold_github_token(token) {
                Ok(()) => true,
                Err(why) => {
                    self.say(&format!("could not start this session: {why}"))
                        .await;
                    false
                }
            },
            Err(why) => {
                self.say(&format!(
                    "could not start this session, since its GitHub token could not be minted: \
                     {why}"
                ))
                .await;
                false
            }
        }
    }

    /// Mints the token again ahead of its expiry, trying again shortly when
    /// that fails, and says so in the thread once a failing streak begins.
    pub(super) async fn renew_github_token(&mut self) {
        if self.ended {
            return;
        }
        let (Some(app), Some(api)) = (
            self.options
                .config
                .github
                .as_ref()
                .and_then(|github| github.app.clone()),
            self.options.github_api.clone(),
        ) else {
            return;
        };
        let failed = match mint(&app, &api, now_ms()).await {
            Ok(token) => self.hold_github_token(token).err(),
            Err(why) => Some(why),
        };
        let Some(why) = failed else {
            self.github_renewal_failing = false;
            return;
        };
        self.log.warn(
            "the session's GitHub token could not be renewed, and is tried again shortly",
            &fields([("detail", LogValue::from(why.as_str()))]),
        );
        if !self.github_renewal_failing {
            self.github_renewal_failing = true;
            self.say(&format!(
                "this session's GitHub token could not be renewed, and it is tried again every \
                 minute. Once it expires, gh and git fail until a renewal succeeds: {why}"
            ))
            .await;
        }
        self.github_timer = Some(self.timers.set_timeout(SessionTimer::GithubToken, RETRY_MS));
    }

    /// Scrubs a new token from everything the session reports, then writes
    /// it where the wrapper reads it and sets its renewal.
    ///
    /// Scrubbed before it is written, so there is no moment the agent can
    /// read it and the thread would show it. Written to a new file and
    /// renamed over the old one, so the wrapper never reads half of one. The
    /// session can write its state directory, so the new file is opened
    /// beneath it without following a link, and the rename replaces whatever
    /// sits at the name rather than writing through it.
    fn hold_github_token(&mut self, token: SessionToken) -> Result<(), String> {
        let now = now_ms();
        self.views.add_secret(token.token.clone());
        // Kept a renewal's length past its expiry, for a clock running ahead
        // of GitHub's.
        for (expired, _) in self.github_scrubbed.extract_if(.., |(_, expires_at)| {
            expires_at.saturating_add(RENEW_BEFORE_MS) <= now
        }) {
            self.views.remove_secret(&expired);
        }
        self.github_scrubbed
            .push((token.token.clone(), token.expires_at_ms));
        let state = &self.options.state_dir;
        let fresh = format!(".{TOKEN_FILENAME}.new");
        let written = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            let mut file =
                paths::open_beneath(state, &fresh, &paths::OpenOptions::truncate_mode(0o600))?;
            file.write_all(token.token.as_bytes())?;
            file.sync_all()?;
            let directory = std::path::Path::new(state);
            std::fs::rename(directory.join(&fresh), directory.join(TOKEN_FILENAME))
        })();
        written.map_err(|error| {
            format!("its GitHub token could not be written to its state directory: {error}")
        })?;
        self.log.info(
            "the session holds a GitHub App token",
            &fields([("token", LogValue::from(token_line(&token, now)))]),
        );
        self.github_timer = Some(
            self.timers
                .set_timeout(SessionTimer::GithubToken, renew_in_ms(&token, now)),
        );
        self.github_token = Some(token);
        Ok(())
    }

    /// What the session's GitHub credential is, for `!status`, or nothing
    /// when GitHub is not configured.
    pub(super) fn github_line(&self) -> Option<String> {
        let github = self.options.config.github.as_ref()?;
        Some(match (&github.app, &self.github_token) {
            (None, _) => "GitHub: this session holds the daemon's own token".to_owned(),
            (Some(_), Some(token)) if token.expires_at_ms <= now_ms() => {
                "GitHub: the App token has expired, and its renewal is tried again every minute"
                    .to_owned()
            }
            (Some(_), Some(token)) if self.github_renewal_failing => format!(
                "{}, but its last renewal failed and is tried again every minute",
                token_line(token, now_ms())
            ),
            (Some(_), Some(token)) => token_line(token, now_ms()),
            (Some(_), None) => "GitHub: no App token is held yet".to_owned(),
        })
    }

    /// Revokes the session's tokens once it has ended and takes the file
    /// away, so a token still good for a while neither works nor is left in
    /// a state directory kept with the thread.
    ///
    /// Revoked rather than left to expire because a resumed session scrubs
    /// only what it came to hold itself, and its conversation may still
    /// carry the old token. A revoked one is worthless wherever it shows.
    pub(super) async fn drop_github_token(&mut self) {
        let state = std::path::Path::new(&self.options.state_dir);
        for name in [TOKEN_FILENAME.to_owned(), format!(".{TOKEN_FILENAME}.new")] {
            if let Err(error) = std::fs::remove_file(state.join(&name))
                && error.kind() != std::io::ErrorKind::NotFound
            {
                self.log.warn(
                    "the ended session's GitHub token file could not be removed",
                    &fields([
                        ("file", LogValue::from(name.as_str())),
                        ("detail", LogValue::from(error.to_string())),
                    ]),
                );
            }
        }
        self.github_token = None;
        let Some(api) = self.options.github_api.clone() else {
            return;
        };
        // Every token still good is revoked, not only the newest: one read
        // before a renewal works until its own expiry otherwise.
        let now = now_ms();
        let held: Vec<String> = self
            .github_scrubbed
            .iter()
            .filter(|(_, expires_at)| *expires_at > now)
            .map(|(token, _)| token.clone())
            .collect();
        for token in held {
            let reply = api(
                "/installation/token".to_owned(),
                crate::session::pr::ApiCall {
                    method: "DELETE".to_owned(),
                    token,
                    body: None,
                },
            )
            .await;
            if reply.status != 204 {
                self.log.warn(
                    "the ended session's GitHub token could not be revoked, and works until it \
                     expires",
                    &fields([("status", LogValue::from(i64::from(reply.status)))]),
                );
            }
        }
    }
}
