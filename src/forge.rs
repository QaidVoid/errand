//! What differs between the forges the daemon can work with.
//!
//! GitHub and Forgejo expose the same idea under slightly different
//! addresses, headers and names: an API under its own host or its own
//! prefix, a bearer token or a token header, and a thread prefix that keeps
//! the two apart in chat. Every one of those differences is asked of here,
//! so no caller ends up branching on which forge it is talking to.

use crate::config::schema::{ForgeConfig, ForgeKind};

/// Where one forge keeps the REST API the daemon calls.
///
/// GitHub serves its API from a host of its own; a Forgejo instance mounts
/// the API under its own address, one `/api/v1` deep.
pub fn api_base(forge: &ForgeConfig) -> String {
    match forge.kind {
        ForgeKind::Github => "https://api.github.com".to_owned(),
        ForgeKind::Forgejo => format!("{}/api/v1", instance_of(forge).trim_end_matches('/')),
    }
}

/// Where one forge keeps the pages a person reads, and clones from.
///
/// GitHub keeps both at github.com; a Forgejo instance keeps both at its own
/// address. This is the base an issue's web address and a repository's clone
/// address are built on.
pub fn site_base(forge: &ForgeConfig) -> String {
    match forge.kind {
        ForgeKind::Github => "https://github.com".to_owned(),
        ForgeKind::Forgejo => instance_of(forge).trim_end_matches('/').to_owned(),
    }
}

/// The value of the credential header one forge asks for.
///
/// GitHub takes a bearer token; Forgejo asks the same header to say the word
/// `token` instead. `token` is whatever the call presents: the daemon's own,
/// or one minted for a session or a single request.
pub fn auth_header(kind: ForgeKind, token: &str) -> String {
    match kind {
        ForgeKind::Github => format!("Bearer {token}"),
        ForgeKind::Forgejo => format!("token {token}"),
    }
}

/// Adds the response format and API version headers required by a forge.
pub fn api_headers(kind: ForgeKind, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match kind {
        ForgeKind::Github => request
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28"),
        ForgeKind::Forgejo => request.header("Accept", "application/json"),
    }
}

/// Whether a byte can occur within a login on this forge.
pub fn login_character(kind: ForgeKind, byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || byte == b'-'
        || (kind == ForgeKind::Forgejo && matches!(byte, b'_' | b'.'))
}

/// Whether a login has the length and characters supported for this forge.
///
/// This is a permissive filter, not a complete account identifier validator.
/// Dotted Forgejo names are accepted because an instance can enable them.
pub fn valid_login(kind: ForgeKind, login: &str) -> bool {
    let maximum = match kind {
        ForgeKind::Github => 39,
        ForgeKind::Forgejo => 40,
    };
    (1..=maximum).contains(&login.len()) && login.bytes().all(|byte| login_character(kind, byte))
}

/// The prefix naming one forge's threads and accounts in chat, such as
/// `github:` in `github:owner/repo#7`. Two forges served at different times
/// must not read as each other's threads.
pub fn thread_prefix(kind: ForgeKind) -> &'static str {
    match kind {
        ForgeKind::Github => "github:",
        ForgeKind::Forgejo => "forgejo:",
    }
}

/// What the forge is called where an operator reads about it.
pub fn display(kind: ForgeKind) -> &'static str {
    match kind {
        ForgeKind::Github => "GitHub",
        ForgeKind::Forgejo => "Forgejo",
    }
}

/// The instance's address, which every Forgejo forge was validated to carry.
fn instance_of(forge: &ForgeConfig) -> &str {
    forge
        .url
        .as_deref()
        .expect("validated: a Forgejo forge carries its instance's address")
}

#[cfg(test)]
mod tests {
    use super::{ForgeKind, api_headers};

    #[test]
    fn api_headers_follow_the_forges_protocol() {
        let client = reqwest::Client::new();
        let github = api_headers(ForgeKind::Github, client.get("https://example.com"))
            .build()
            .unwrap();
        assert_eq!(github.headers()["accept"], "application/vnd.github+json");
        assert_eq!(github.headers()["x-github-api-version"], "2022-11-28");

        let forgejo = api_headers(ForgeKind::Forgejo, client.get("https://example.com"))
            .build()
            .unwrap();
        assert_eq!(forgejo.headers()["accept"], "application/json");
        assert!(!forgejo.headers().contains_key("x-github-api-version"));
    }
}
