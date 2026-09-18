//! Which addresses the interface may bind to.
//!
//! The interface has no login. Reaching it is the authorisation, so the
//! address it listens on is the entire access control and is checked rather
//! than trusted. A public bind is refused at startup instead of warned about,
//! because a warning in a log is not a control.
//!
//! The tailnet range is allowed deliberately: reaching a machine over a
//! private overlay network is the intended way to use this from a phone.

/// Why an address was allowed or refused, phrased for a startup failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressVerdict {
    /// Whether the interface may bind here.
    pub allowed: bool,
    /// Why, in words an operator can act on.
    pub reason: String,
}

const LOOPBACK_NAMES: [&str; 4] = ["localhost", "127.0.0.1", "::1", "[::1]"];

/// Any address on this machine, which is what makes a bind public.
const WILDCARDS: [&str; 4] = ["0.0.0.0", "::", "[::]", "*"];

fn parse_ipv4(host: &str) -> Option<[u8; 4]> {
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return None;
    }

    let mut octets = [0u8; 4];
    for (slot, part) in octets.iter_mut().zip(&parts) {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        // The range check above makes the truncation impossible.
        let value: u32 = part.parse().ok()?;
        if value > 255 {
            return None;
        }
        #[expect(clippy::cast_possible_truncation)]
        {
            *slot = value as u8;
        }
    }
    Some(octets)
}

fn is_private_ipv4(octets: [u8; 4]) -> bool {
    let [a, b, ..] = octets;
    if a == 127 {
        return true; // loopback
    }
    if a == 10 {
        return true; // RFC 1918
    }
    if a == 192 && b == 168 {
        return true; // RFC 1918
    }
    if a == 172 && (16..=31).contains(&b) {
        return true; // RFC 1918
    }
    if a == 169 && b == 254 {
        return true; // link local
    }
    // RFC 6598 shared address space, which is where a tailnet lives.
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    false
}

fn is_private_ipv6(host: &str) -> bool {
    let bare = host.trim_matches(|c| c == '[' || c == ']').to_lowercase();
    if bare == "::1" {
        return true;
    }
    if bare.starts_with("fe80:") {
        return true; // link local
    }
    // Unique local addresses, fc00::/7.
    bare.starts_with("fc") || bare.starts_with("fd")
}

/// Decides whether the interface may listen on an address.
///
/// Returns why not, when it may not, so the refusal can say something useful.
pub fn check_bind_address(host: &str) -> AddressVerdict {
    let trimmed = host.trim().to_lowercase();

    if trimmed.is_empty() {
        return AddressVerdict {
            allowed: false,
            reason: "no address was given".to_owned(),
        };
    }

    if WILDCARDS.contains(&trimmed.as_str()) {
        return AddressVerdict {
            allowed: false,
            reason: format!(
                "{host} listens on every interface, including public ones. Bind to a loopback or private address instead."
            ),
        };
    }

    if LOOPBACK_NAMES.contains(&trimmed.as_str()) {
        return AddressVerdict {
            allowed: true,
            reason: "loopback".to_owned(),
        };
    }

    if let Some(octets) = parse_ipv4(&trimmed) {
        return if is_private_ipv4(octets) {
            AddressVerdict {
                allowed: true,
                reason: "private address".to_owned(),
            }
        } else {
            AddressVerdict {
                allowed: false,
                reason: format!(
                    "{host} is a public address, and the interface has no login. Bind to a loopback, private, or tailnet address instead."
                ),
            }
        };
    }

    if trimmed.contains(':') {
        return if is_private_ipv6(&trimmed) {
            AddressVerdict {
                allowed: true,
                reason: "private address".to_owned(),
            }
        } else {
            AddressVerdict {
                allowed: false,
                reason: format!(
                    "{host} is a public address, and the interface has no login. Bind to a loopback, private, or tailnet address instead."
                ),
            }
        };
    }

    // A hostname could resolve anywhere, and resolving it here would mean the
    // check depended on DNS at the moment of startup.
    AddressVerdict {
        allowed: false,
        reason: format!(
            "{host} is a name, not an address. Give the address to bind to, so what is reachable does not depend on what a name resolves to."
        ),
    }
}

#[cfg(test)]
#[path = "address/tests.rs"]
mod tests;
