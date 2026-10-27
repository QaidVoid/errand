//! Discord login: which Discord account a browser belongs to.
//!
//! Anyone who can reach the interface may read it. Changing anything takes a
//! login, and a logged-in visitor acts as their own account under the same
//! lists the bot applies in the channel, never as the operator.
//!
//! Logins live in memory, so a restart logs everybody out. That is the whole
//! cost of not keeping a signing key on disk.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use serde::Serialize;
use serenity::http::Http;
use serenity::model::id::{GuildId, UserId};

use crate::chat::inbound::is_permitted;
use crate::config::live::LiveConfig;
use crate::config::schema::WebLoginConfig;

/// How long a login lasts before the browser has to log in again.
pub const LOGIN_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

/// How long a browser has to come back from Discord after setting out.
pub const PENDING_TTL_MS: i64 = 10 * 60 * 1_000;

/// The most logins that may be in progress at once.
///
/// Starting one takes nothing but a request, so without a ceiling anybody
/// could fill memory with logins they never mean to finish.
pub const MAX_PENDING: usize = 1_024;

/// The cookie a login is carried in.
pub const LOGIN_COOKIE: &str = "errand_login";

/// The cookie that ties a callback to the browser that set out.
pub const STATE_COOKIE: &str = "errand_login_state";

const DISCORD_API: &str = "https://discord.com/api/v10";
const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";

/// A Discord account, as much of it as the interface shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Visitor {
    /// The account id, which every list in the configuration names.
    pub id: String,
    /// What to call them.
    pub name: String,
    /// Where their avatar is, when they have one.
    pub avatar: Option<String>,
}

/// What a request to Discord came back with.
pub type DiscordCall<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send>>;

/// What logging in asks of Discord.
pub trait Discord: Send + Sync {
    /// The account an authorisation code was granted by.
    fn identify(&self, code: String, redirect_uri: String) -> DiscordCall<Visitor>;

    /// Whether an account is a member of a guild.
    fn is_member(&self, guild_id: String, user_id: String) -> DiscordCall<bool>;
}

/// Why a login did not complete, as the browser is told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginFailure {
    /// The person declined on Discord's page.
    Denied,
    /// The callback did not match a login this browser started, or came too
    /// late.
    Expired,
    /// Discord could not be asked, or answered with something unusable.
    Failed(String),
}

impl LoginFailure {
    /// The word the interface is redirected with.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Failed(_) => "failed",
        }
    }
}

/// A visitor, and whether they may change anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed {
    /// Who they are.
    pub visitor: Visitor,
    /// Whether they may start and prompt sessions.
    pub may_act: bool,
}

struct Active {
    visitor: Visitor,
    member: bool,
    expires_at: i64,
}

/// The clock logins are timed by.
pub type Now = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Every login in progress and every one that completed.
pub struct Logins {
    client_id: String,
    redirect_uri: String,
    secure: bool,
    discord: Arc<dyn Discord>,
    live: LiveConfig,
    now: Now,
    pending: Mutex<HashMap<String, i64>>,
    active: Mutex<HashMap<String, Active>>,
}

impl Logins {
    /// Logins through one Discord application, returning to `base_url`.
    ///
    /// The lists deciding who may act are read from `live` on every request,
    /// so a reloaded allowlist applies to people already logged in.
    pub fn new(
        config: &WebLoginConfig,
        base_url: &str,
        discord: Arc<dyn Discord>,
        live: LiveConfig,
        now: Now,
    ) -> Self {
        let base = base_url.trim_end_matches('/');
        Self {
            client_id: config.client_id.clone(),
            redirect_uri: format!("{base}/auth/callback"),
            secure: base.starts_with("https://"),
            discord,
            live,
            now,
            pending: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
        }
    }

    /// Starts a login. Returns the state to set as a cookie and the address
    /// to send the browser to, or nothing when too many are in progress.
    pub fn begin(&self) -> Option<(String, String)> {
        let state = random_token();
        let now = (self.now)();
        let mut pending = self.pending.lock().expect("the pending lock");
        pending.retain(|_, expires_at| *expires_at > now);
        if pending.len() >= MAX_PENDING {
            return None;
        }
        pending.insert(state.clone(), now + PENDING_TTL_MS);
        drop(pending);

        let url = reqwest::Url::parse_with_params(
            AUTHORIZE_URL,
            [
                ("response_type", "code"),
                ("client_id", self.client_id.as_str()),
                ("scope", "identify"),
                ("prompt", "none"),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("state", state.as_str()),
            ],
        )
        .expect("the authorize address is well formed");
        Some((state, url.to_string()))
    }

    /// Completes a login from Discord's callback. Returns the token to set as
    /// the login cookie.
    ///
    /// The state must be one this daemon handed out, still in time, and the
    /// same one the browser carries in its cookie. Without that last check a
    /// stranger could log somebody in as the stranger.
    pub async fn finish(
        &self,
        query_state: Option<&str>,
        cookie_state: Option<&str>,
        code: Option<&str>,
        error: Option<&str>,
        guild_id: Option<&str>,
    ) -> Result<(String, Visitor), LoginFailure> {
        let issued = query_state
            .and_then(|state| self.pending.lock().expect("the pending lock").remove(state));
        if error.is_some() {
            return Err(LoginFailure::Denied);
        }
        let now = (self.now)();
        let in_time = issued.is_some_and(|expires_at| expires_at > now);
        if !in_time || query_state.is_none() || query_state != cookie_state {
            return Err(LoginFailure::Expired);
        }
        let Some(code) = code.filter(|code| !code.is_empty()) else {
            return Err(LoginFailure::Expired);
        };

        let visitor = self
            .discord
            .identify(code.to_owned(), self.redirect_uri.clone())
            .await
            .map_err(LoginFailure::Failed)?;
        // A wildcard allowlist means anyone in the channel. On the web there
        // is no channel, so the guild stands in for it: without this,
        // "anyone" would mean every Discord account there is.
        let member = match guild_id {
            Some(guild_id) => self
                .discord
                .is_member(guild_id.to_owned(), visitor.id.clone())
                .await
                .map_err(LoginFailure::Failed)?,
            None => false,
        };

        let token = random_token();
        let mut active = self.active.lock().expect("the active lock");
        active.retain(|_, login| login.expires_at > now);
        active.insert(
            token.clone(),
            Active {
                visitor: visitor.clone(),
                member,
                expires_at: now + LOGIN_TTL_MS,
            },
        );
        Ok((token, visitor))
    }

    /// Who a request is from, when it carries a login still in time.
    pub fn signed(&self, headers: &HeaderMap) -> Option<Signed> {
        let token = cookie(headers, LOGIN_COOKIE)?;
        let now = (self.now)();
        let mut active = self.active.lock().expect("the active lock");
        let login = active.get(&token)?;
        if login.expires_at <= now {
            active.remove(&token);
            return None;
        }
        let chat = self.live.get().chat;
        Some(Signed {
            visitor: login.visitor.clone(),
            may_act: login.member && is_permitted(&chat, &login.visitor.id),
        })
    }

    /// Ends the login a request carries, if it carries one.
    pub fn end(&self, headers: &HeaderMap) {
        if let Some(token) = cookie(headers, LOGIN_COOKIE) {
            self.active.lock().expect("the active lock").remove(&token);
        }
    }

    /// The `Set-Cookie` value carrying a login.
    pub fn login_cookie(&self, token: &str) -> String {
        self.cookie_header(LOGIN_COOKIE, token, "/", LOGIN_TTL_MS / 1_000)
    }

    /// The `Set-Cookie` value that removes a login.
    pub fn cleared_login_cookie(&self) -> String {
        self.cookie_header(LOGIN_COOKIE, "", "/", 0)
    }

    /// The `Set-Cookie` value carrying a login in progress.
    pub fn state_cookie(&self, state: &str) -> String {
        self.cookie_header(STATE_COOKIE, state, "/auth", PENDING_TTL_MS / 1_000)
    }

    /// The `Set-Cookie` value that removes a login in progress.
    pub fn cleared_state_cookie(&self) -> String {
        self.cookie_header(STATE_COOKIE, "", "/auth", 0)
    }

    // Lax rather than Strict: the callback is a navigation from Discord, and a
    // Strict cookie would not come with it.
    fn cookie_header(&self, name: &str, value: &str, path: &str, max_age: i64) -> String {
        let secure = if self.secure { "; Secure" } else { "" };
        format!("{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite=Lax{secure}")
    }
}

/// One cookie's value from a request.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty())
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the system generator is available");
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Discord itself: the OAuth exchange over HTTP, and the bot's own client
/// for asking about members.
pub struct DiscordApi {
    client: reqwest::Client,
    client_id: String,
    client_secret: String,
    bot: Arc<Http>,
}

impl DiscordApi {
    /// Discord, as the application in `config` and the bot behind `bot`.
    pub fn new(config: &WebLoginConfig, bot: Arc<Http>) -> Self {
        Self {
            client: reqwest::Client::new(),
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            bot,
        }
    }
}

#[derive(serde::Deserialize)]
struct TokenAnswer {
    access_token: String,
}

#[derive(serde::Deserialize)]
struct UserAnswer {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

impl Discord for DiscordApi {
    fn identify(&self, code: String, redirect_uri: String) -> DiscordCall<Visitor> {
        let client = self.client.clone();
        let client_id = self.client_id.clone();
        let client_secret = self.client_secret.clone();
        Box::pin(async move {
            // The access token is used once, to ask who granted it, and then
            // dropped. Nothing here acts on a visitor's behalf at Discord.
            let token = client
                .post(format!("{DISCORD_API}/oauth2/token"))
                .basic_auth(client_id, Some(client_secret))
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("code", code.as_str()),
                    ("redirect_uri", redirect_uri.as_str()),
                ])
                .send()
                .await
                .map_err(|error| format!("Discord's token endpoint was unreachable: {error}"))?;
            if !token.status().is_success() {
                return Err(format!(
                    "Discord refused the login code with {}; check web.login.clientId, \
                     web.login.clientSecret, and that {redirect_uri} is a registered redirect",
                    token.status()
                ));
            }
            let token: TokenAnswer = token
                .json()
                .await
                .map_err(|error| format!("Discord's token answer was unreadable: {error}"))?;

            let user = client
                .get(format!("{DISCORD_API}/users/@me"))
                .bearer_auth(token.access_token)
                .send()
                .await
                .map_err(|error| format!("Discord's user endpoint was unreachable: {error}"))?;
            if !user.status().is_success() {
                return Err(format!(
                    "Discord would not say who logged in: {}",
                    user.status()
                ));
            }
            let user: UserAnswer = user
                .json()
                .await
                .map_err(|error| format!("Discord's user answer was unreadable: {error}"))?;
            Ok(Visitor {
                avatar: user.avatar.map(|hash| {
                    format!(
                        "https://cdn.discordapp.com/avatars/{}/{hash}.png?size=64",
                        user.id
                    )
                }),
                name: user.global_name.unwrap_or(user.username),
                id: user.id,
            })
        })
    }

    fn is_member(&self, guild_id: String, user_id: String) -> DiscordCall<bool> {
        let bot = Arc::clone(&self.bot);
        Box::pin(async move {
            let (Ok(guild), Ok(user)) = (guild_id.parse::<u64>(), user_id.parse::<u64>()) else {
                return Ok(false);
            };
            if guild == 0 || user == 0 {
                return Ok(false);
            }
            match bot.get_member(GuildId::new(guild), UserId::new(user)).await {
                Ok(_) => Ok(true),
                Err(serenity::Error::Http(error))
                    if error.status_code().map(|status| status.as_u16()) == Some(404) =>
                {
                    Ok(false)
                }
                Err(error) => Err(format!(
                    "Discord would not say who is in the server: {error}"
                )),
            }
        })
    }
}

#[cfg(test)]
mod tests;
