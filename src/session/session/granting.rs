//! `!grant` and `!revoke`: the thread's half of what a session's
//! broker may reach beyond the allowlist.
//!
//! The mechanics are `crate::sandbox::broker::grants`. This is who asked,
//! and what the thread is told.

use super::{IncomingMessage, Running};
use crate::config::schema::EgressMode;
use crate::log::now_ms;
use crate::sandbox::broker::grants::{Grant, Lasting, parse_grant};
use crate::sandbox::broker::host_allowed;
use crate::session::event::ReactionOutcome;

impl Running {
    /// Grants a host to this session's broker, as `!grant` was written.
    pub(super) async fn grant_host(&mut self, rest: &str, message: &IncomingMessage) {
        let (host, lasting) = match parse_grant(rest) {
            Ok(parsed) => parsed,
            Err(why) => {
                self.say(&why).await;
                return;
            }
        };
        let Some(grants) = self.grants_in_force().await else {
            return;
        };
        if host_allowed(&host, &self.options.config.sandbox.egress.allow) {
            self.say(&format!(
                "`{host}` is on the egress allowlist already, so it needs no grant"
            ))
            .await;
            return;
        }
        let (granted, unsaved) = grants.grant(&host, lasting, &message.author_id);
        self.react(&message.id, ReactionOutcome::Accepted).await;
        let mut said = format!("granted `{host}` {}", lasting_phrase(lasting));
        if let Some(why) = unsaved {
            said = format!(
                "{said}. It is in force, but a resume of this thread will not have it: {why}"
            );
        }
        self.say(&said).await;
        self.log.info(
            "a host was granted",
            &crate::log::fields([
                ("host", granted.host.as_str().into()),
                ("grant", granted.id.as_str().into()),
                ("by", granted.granted_by.as_str().into()),
            ]),
        );
    }

    /// Drops every grant for a host, as `!revoke` was written.
    pub(super) async fn revoke_host(&mut self, rest: &str, message: &IncomingMessage) {
        let Some(host) = rest.split_whitespace().next() else {
            self.say("say which host, as `!revoke api.example.com`")
                .await;
            return;
        };
        let Some(grants) = self.grants_in_force().await else {
            return;
        };
        let (gone, unsaved) = grants.revoke(host);
        // A wildcard granted beside it still lets the host through, and the
        // owner is told so rather than that it is no longer granted.
        let still = grants.covering(host).map(|grant| {
            format!(
                "`{host}` is still let through by `{}` `{}`. `!revoke {}` stops that",
                grant.id, grant.host, grant.host
            )
        });
        if gone == 0 {
            self.say(&still.unwrap_or_else(|| format!("nothing was granted for `{host}`")))
                .await;
            return;
        }
        self.react(&message.id, ReactionOutcome::Accepted).await;
        let mut said = still.unwrap_or_else(|| {
            format!(
                "`{host}` is no longer granted, and a connection already open to it stays open \
                 until it ends"
            )
        });
        if let Some(why) = unsaved {
            said = format!("{said}. A resume of this thread would grant it again: {why}");
        }
        self.say(&said).await;
    }

    /// Lists what is granted, as `!grant` with no host asks.
    pub(super) async fn list_grants(&mut self) {
        let Some(grants) = self.grants_in_force().await else {
            return;
        };
        let held = grants.list();
        if held.is_empty() {
            self.say("nothing is granted to this session beyond the egress allowlist")
                .await;
            return;
        }
        let now = now_ms();
        let lines = held
            .iter()
            .map(|grant| grant_line(grant, now))
            .collect::<Vec<_>>()
            .join("\n");
        self.say(&format!("granted to this session:\n{lines}"))
            .await;
    }

    /// The grants of a session whose egress is brokered, or nothing, having
    /// said why there is nothing to grant.
    async fn grants_in_force(
        &mut self,
    ) -> Option<std::sync::Arc<crate::sandbox::broker::grants::GrantStore>> {
        if self.options.config.sandbox.egress.mode != EgressMode::Proxy {
            self.say("egress is open for this session, so there is nothing to grant")
                .await;
            return None;
        }
        let grants = self.grants.clone();
        if grants.is_none() {
            self.say("this session's broker has not started yet").await;
        }
        grants
    }
}

/// How long a grant lasts, as the thread is told.
fn lasting_phrase(lasting: Lasting) -> String {
    match lasting {
        Lasting::Once => "for its next connection".to_owned(),
        Lasting::For(ms) => format!("for {}", minutes(ms)),
        Lasting::Always => "for the rest of this session".to_owned(),
    }
}

/// One grant as `!grant` lists it.
fn grant_line(grant: &Grant, now: i64) -> String {
    let lasting = match (grant.once, grant.expires_at) {
        (true, _) => "for its next connection".to_owned(),
        (false, Some(at)) => format!("for {} more", minutes(at.saturating_sub(now))),
        (false, None) => "for the rest of this session".to_owned(),
    };
    format!(
        "`{}` `{}`, {lasting}, by <@{}>",
        grant.id, grant.host, grant.granted_by
    )
}

/// A length of time to the minute, rounded up so a grant with seconds left
/// is never said to have none.
fn minutes(ms: i64) -> String {
    let minutes = (ms.max(0) + 59_999) / 60_000;
    match minutes {
        1 => "1 minute".to_owned(),
        n if n < 120 => format!("{n} minutes"),
        n => format!("{} hours", n / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::{grant_line, minutes};
    use crate::sandbox::broker::grants::Grant;

    /// A grant's remaining time is said to the minute, never as none while
    /// any is left.
    #[test]
    fn a_grant_is_listed_with_what_is_left_of_it() {
        assert_eq!(minutes(1), "1 minute");
        assert_eq!(minutes(600_000), "10 minutes");
        assert_eq!(minutes(3 * 3_600_000), "3 hours");
        let grant = Grant {
            id: "g2".to_owned(),
            created_at: 0,
            expires_at: Some(600_000),
            once: false,
            host: "api.example.com".to_owned(),
            granted_by: "u-1".to_owned(),
        };
        assert_eq!(
            grant_line(&grant, 30_000),
            "`g2` `api.example.com`, for 10 minutes more, by <@u-1>"
        );
    }
}
