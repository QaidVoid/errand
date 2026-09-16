//! What a provider says is left to spend, whoever the provider is.
//!
//! A metered provider refuses work once its window is spent, and without
//! asking first the refusal arrives as a failed turn: the thread has already
//! opened, the sandbox has already started, and the person is told something
//! went wrong rather than when to come back.
//!
//! Each provider answers this in its own shape and at its own endpoint, so the
//! reading is per provider and lives beside it. What is shared is the window
//! itself, how long an answer is worth holding, and what a spent one is
//! called.

use serde_json::Value;

/// What the provider says about the window a prompt would be charged to.
#[derive(Debug, Clone, PartialEq)]
pub struct Quota {
    /// How much of the window is spent, 0 to 100.
    pub percentage: f64,
    /// When the window rolls over, in epoch milliseconds, where that is known.
    ///
    /// A window nothing has been charged to yet has nothing scheduled to
    /// reset, and a provider that says so is answering rather than failing.
    /// Treating the missing time as no answer would clear the status exactly
    /// when the window is emptiest.
    pub resets_at: Option<i64>,
}

/// True when the window is spent and a prompt would be refused.
pub fn is_spent(quota: &Quota) -> bool {
    quota.percentage >= 100.0
}

/// How long an unspent answer is reused before the provider is asked again.
pub const QUOTA_TTL_MS: i64 = 60_000;

/// What a provider answers with, reduced to what a reading consumes.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    /// The HTTP status the provider answered with.
    pub status: u16,
    /// The parsed JSON body, or nothing when it was not JSON.
    pub body: Option<Value>,
}

impl HttpResponse {
    /// Whether the provider answered with success.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// What went wrong when a provider could not be reached at all.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FetchError(pub String);

/// What the fetcher is asked for.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// Headers the request carries.
    pub headers: Vec<(String, String)>,
    /// How long the answer may take before it is abandoned.
    pub timeout_ms: u64,
}

/// Fetches a URL. Injected so tests need no network.
///
/// An unreachable provider is an `Err`; the readers treat that as no answer.
pub trait Fetch: Send + Sync {
    /// Performs the request, or says why it could not.
    fn fetch(
        &self,
        url: String,
        request: HttpRequest,
    ) -> impl std::future::Future<Output = Result<HttpResponse, FetchError>> + Send;
}

/// Holds the last answer so the provider is not asked once per message.
///
/// A spent window is not asked about again until it rolls over, because the
/// answer cannot change before then. An unspent one is asked about on a short
/// interval, since the only way it changes is by being used.
///
/// The reading is given rather than built here: the gate is about when to ask,
/// and every provider answers differently.
pub struct QuotaGate<R> {
    read: R,
    now: Box<dyn Fn() -> i64 + Send + Sync>,
    held: Option<Quota>,
    held_at: i64,
}

impl<R, F> QuotaGate<R>
where
    R: Fn() -> F,
    F: Future<Output = Option<Quota>>,
{
    /// Holds answers from `read`, with the clock that decides how long they
    /// are worth holding.
    pub fn new(read: R, now: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        Self {
            read,
            now: Box::new(now),
            held: None,
            held_at: 0,
        }
    }

    /// What the window looks like, or nothing when that cannot be established.
    ///
    /// `None` means carry on. It is returned for an unreachable provider as
    /// well as for an unrecognised answer, and both must leave work running.
    pub async fn current(&mut self) -> Option<Quota> {
        let at = (self.now)();
        if let Some(held) = &self.held {
            // A spent window cannot change before it rolls over, so it is held
            // until then. Without a time to wait for, it is held like any
            // other.
            let until = held.resets_at;
            if is_spent(held) && until.is_some_and(|until| at < until) {
                return self.held.clone();
            }
            if !is_spent(held) && at - self.held_at < QUOTA_TTL_MS {
                return self.held.clone();
            }
        }

        let fresh = (self.read)().await?;
        self.held = Some(fresh.clone());
        self.held_at = at;
        Some(fresh)
    }

    /// Forgets what was held, so the next question reaches the provider.
    pub fn forget(&mut self) {
        self.held = None;
        self.held_at = 0;
    }
}

/// One provider whose window this host can ask about.
pub struct UsageSource<R> {
    /// The provider name, as the configuration and `--model` call it.
    pub provider: String,
    /// The gate that holds the window's answer.
    pub gate: QuotaGate<R>,
}

/// What a thread is told when a window is spent.
pub fn spent_message(provider: &str, relative: Option<&str>) -> String {
    let back = relative.map_or(String::new(), |relative| format!("; it resets {relative}"));
    format!("{provider}'s usage window is spent, so this cannot run yet{back}")
}

/// What is said when a provider was asked and did not answer usefully.
pub const UNKNOWN_QUOTA: &str = "the model provider did not say what is left of the usage window";

/// What the window looks like, in a line somebody asked for on purpose.
///
/// Says what is left rather than what is spent. "58% left" is the number
/// somebody is deciding on, where "42% used" has to be subtracted first.
pub fn quota_message(provider: &str, quota: &Quota, relative: Option<&str>) -> String {
    let left = (100.0 - quota.percentage).round().max(0.0);
    let state = if is_spent(quota) {
        format!("{provider}'s usage window is spent")
    } else {
        format!("{left}% of {provider}'s usage window is left")
    };
    match relative {
        None => state,
        Some(relative) => format!("{state}, and it resets {relative}"),
    }
}

/// Longest a status may be before the service refuses it.
///
/// The status is one line under the bot's name, and a refusal is silent: the
/// name simply carries nothing. So it is trimmed here rather than sent and
/// hoped for.
pub const STATUS_LIMIT: usize = 128;

/// One provider's window, ready to render.
#[derive(Debug, Clone)]
pub struct Window {
    /// The provider the window belongs to.
    pub provider: String,
    /// What the provider said is left.
    pub quota: Quota,
    /// When it resets, already rendered, or nothing where that is not known.
    pub relative: Option<String>,
}

/// The windows as a bot status, which has far less room than a message.
///
/// One provider reads as a sentence, because there is room for one and naming
/// it says nothing a glance at the configuration would not. Several are named,
/// because then which one has room is the whole question, and they are cut
/// down to the percentage each has left. A spent one says when it is back,
/// since that is the only thing left to know about it.
///
/// Providers that could not be read are simply absent: a host asking two of
/// them should not lose the answer it has because the other did not come.
/// When none can be read there is nothing to say, and the caller clears the
/// status rather than leaving a stale number under the bot's name.
///
/// Returns nothing when there is nothing worth showing.
pub fn usage_status(windows: &[Window]) -> Option<String> {
    let first = windows.first()?;
    if windows.len() == 1 {
        let left = (100.0 - first.quota.percentage).round().max(0.0);
        if is_spent(&first.quota) {
            return Some(match &first.relative {
                None => "usage spent".to_owned(),
                Some(relative) => format!("usage spent, back {relative}"),
            });
        }
        return Some(match &first.relative {
            None => format!("{left}% usage left"),
            Some(relative) => format!("{left}% usage left, resets {relative}"),
        });
    }

    let mut parts: Vec<String> = Vec::new();
    for window in windows {
        let left = (100.0 - window.quota.percentage).round().max(0.0);
        let segment = if is_spent(&window.quota) {
            match &window.relative {
                None => format!("{} spent", window.provider),
                Some(relative) => format!("{} spent, back {relative}", window.provider),
            }
        } else {
            format!("{} {left}%", window.provider)
        };
        // Dropped rather than truncated: half a provider's name reads as a
        // different provider.
        let candidate = parts
            .iter()
            .chain(std::iter::once(&segment))
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ");
        if candidate.len() > STATUS_LIMIT {
            break;
        }
        parts.push(segment);
    }
    (!parts.is_empty()).then(|| parts.join(" | "))
}

#[cfg(test)]
mod tests;
