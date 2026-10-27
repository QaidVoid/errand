//! Fetching a song kage's `generate_music` tool could only link to.
//!
//! Under the broker the agent reaches the music server at the broker's
//! address, so a song the server links on its own host looks foreign to
//! kage, which hands back the link instead of the song. The daemon reaches
//! that host directly, so it fetches the song, but only from the host of
//! the provider that carries `music`, without a credential, and without
//! following a redirect, since only the first address was checked.

use std::time::Duration;

use crate::config::schema::AgentConfig;

/// How long fetching one song may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// The largest song read, the cap kage reads a linked song under.
pub const MAX_SONG_BYTES: usize = 16 * 1024 * 1024;

/// Whether `link` is on the host of a provider that carries `music`, the
/// only host the daemon fetches a linked song from.
#[must_use]
pub fn is_music_link(agent: &AgentConfig, link: &str) -> bool {
    agent
        .providers
        .iter()
        .filter(|(_, definition)| definition.get("music").is_some())
        .filter_map(|(name, _)| agent.base_url_of(name))
        .any(|base| link.starts_with(&format!("{}/", origin(base))))
}

/// The scheme and host of `url`, without a path.
fn origin(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |at| at + 3);
    let end = url[after_scheme..]
        .find('/')
        .map_or(url.len(), |at| after_scheme + at);
    &url[..end]
}

/// The song at `link`, fetched without a credential or a redirect, or why
/// it could not be.
///
/// # Errors
///
/// Why the song was not fetched, in words worth posting into a thread.
pub async fn fetch_song(link: String) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;
    let mut answer = client
        .get(link)
        .send()
        .await
        .map_err(|error| format!("it could not be fetched: {error}"))?;
    if !answer.status().is_success() {
        return Err(format!("its server answered {}", answer.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = answer.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len() + chunk.len() > MAX_SONG_BYTES {
            return Err(format!("it is over {MAX_SONG_BYTES} bytes"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;
