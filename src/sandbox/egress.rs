//! What a brokered session is handed to reach its egress broker: the
//! environment its tools read, the routes its agent is told, and the broker
//! itself, the same under either backend. Only where the session reaches the
//! broker differs, so each function takes that address.

use std::collections::BTreeMap;

use crate::log::{Logger, fields};
use crate::sandbox::agent_config::BrokeredProvider;
use crate::sandbox::backend::SandboxLaunchError;
use crate::sandbox::broker::grants::GrantStore;
use crate::sandbox::broker::{BrokerTemplate, SessionBroker};

/// The variables that point a session's tools at its broker. Each carries the
/// session's proxy credential.
pub const PROXY_VARIABLES: [&str; 4] = ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"];

/// Path the broker answers as the provider on, under its own address.
pub const PROVIDER_PREFIX: &str = "/provider";

/// The path the broker answers one provider on.
pub fn provider_prefix(provider: &str) -> String {
    format!("{PROVIDER_PREFIX}/{provider}")
}

/// What a session's agent is told a provider's base URL is, for a broker it
/// reaches at `host`.
pub fn provider_broker_url(host: &str, port: u16, provider: &str) -> String {
    format!("http://{host}:{port}{}", provider_prefix(provider))
}

/// The session broker's route for each provider it stands in front of, as a
/// session reaching it at `host` is told.
pub fn brokered_providers(
    host: &str,
    broker: Option<&SessionBroker>,
) -> BTreeMap<String, BrokeredProvider> {
    let Some(broker) = broker else {
        return BTreeMap::new();
    };
    broker
        .nonces
        .keys()
        .map(|name| {
            let through = BrokeredProvider {
                base_url: provider_broker_url(host, broker.port, name),
            };
            (name.clone(), through)
        })
        .collect()
}

/// The launch's environment, its provider keys swapped for this session's
/// nonces when it has a broker.
pub fn session_env(
    env: &BTreeMap<String, String>,
    broker: Option<&SessionBroker>,
) -> BTreeMap<String, String> {
    match broker {
        Some(broker) => brokered_env(env, &broker.credential_names, &broker.nonces),
        None => env.clone(),
    }
}

/// Adds the proxy variables that point a brokered session's tools at its
/// broker, reached at `host`.
///
/// Lower and upper case, since programs read one or the other. The broker
/// answers as the provider on its own address, so that one is reached
/// directly rather than tunnelled through itself.
pub fn add_proxy_env(env: &mut BTreeMap<String, String>, host: &str, broker: &SessionBroker) {
    let url = egress_proxy_url(host, broker.port, &broker.proxy_secret);
    for name in PROXY_VARIABLES {
        env.insert(name.to_owned(), url.clone());
    }
    env.insert("NO_PROXY".to_owned(), host.to_owned());
    env.insert("no_proxy".to_owned(), host.to_owned());
    // The agent runs on Node, whose built-in fetch ignores the proxy
    // variables unless this is set. Without it a session bypasses the broker,
    // reaches nothing, and stalls on the provider.
    env.insert("NODE_USE_ENV_PROXY".to_owned(), "1".to_owned());
}

/// Starts a session's own egress broker from `template`, when egress is
/// brokered, admitting what `grants` holds, and says where the session
/// reaches it.
pub async fn start_session_broker(
    template: Option<&BrokerTemplate>,
    session_id: &str,
    grants: Option<std::sync::Arc<GrantStore>>,
    reached_at: &str,
    log: &Logger,
) -> Result<Option<SessionBroker>, SandboxLaunchError> {
    let Some(template) = template else {
        return Ok(None);
    };
    let broker = template.start(session_id, grants).await.map_err(|error| {
        SandboxLaunchError(format!(
            "the session's egress broker could not be started: {error}"
        ))
    })?;
    log.info(
        "egress is brokered for this session",
        &fields([
            ("session", session_id.into()),
            ("via", format!("{reached_at}:{}", broker.port).into()),
        ]),
    );
    Ok(Some(broker))
}

/// A session's environment with every provider key taken out.
///
/// The session is handed the key of the provider it starts on, whichever that
/// is. Under a broker that variable, named in `credential_names` by provider,
/// carries the session's nonce for the provider instead, and a provider the
/// broker has no route to loses the variable altogether, so no real key
/// crosses into the sandbox.
pub fn brokered_env(
    env: &BTreeMap<String, String>,
    credential_names: &BTreeMap<String, String>,
    nonces: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = env.clone();
    for (provider, name) in credential_names {
        match nonces.get(provider) {
            Some(nonce) => env.insert(name.clone(), nonce.clone()),
            None => env.remove(name),
        };
    }
    env
}

/// The authenticated proxy URL a brokered session's tools use, for a broker
/// it reaches at `host`.
pub fn egress_proxy_url(host: &str, port: u16, secret: &str) -> String {
    format!("http://session:{secret}@{host}:{port}")
}
