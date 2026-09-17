use super::check_bind_address;

/// Reaching the interface is the authorisation, so the address is the control.
#[test]
fn loopback_is_allowed_by_name_or_by_address() {
    for host in ["127.0.0.1", "localhost", "::1", "[::1]", "127.0.0.53"] {
        assert!(check_bind_address(host).allowed, "{host}");
    }
}

#[test]
fn a_private_address_is_allowed_which_is_how_a_phone_reaches_it() {
    for host in [
        "10.0.0.5",
        "192.168.1.20",
        "172.16.4.1",
        "172.31.255.254",
        "169.254.1.1",
    ] {
        assert!(check_bind_address(host).allowed, "{host}");
    }
}

/// A tailnet lives in the shared address space, and that is the intended use.
#[test]
fn a_tailnet_address_is_allowed_deliberately() {
    assert!(check_bind_address("100.64.0.1").allowed);
    assert!(check_bind_address("100.127.255.255").allowed);
}

#[test]
fn a_private_ipv6_address_is_allowed() {
    for host in ["fd00::1", "[fd12:3456::1]", "fe80::1"] {
        assert!(check_bind_address(host).allowed, "{host}");
    }
}

/// A warning in a log is not a control, so this is a refusal.
#[test]
fn a_wildcard_bind_is_refused_and_says_why() {
    for host in ["0.0.0.0", "::", "[::]", "*"] {
        let verdict = check_bind_address(host);
        assert!(!verdict.allowed, "{host}");
        assert!(
            verdict.reason.contains("every interface"),
            "{}",
            verdict.reason
        );
    }
}

#[test]
fn a_public_address_is_refused_and_says_the_interface_has_no_login() {
    for host in ["8.8.8.8", "172.32.0.1", "100.128.0.1", "2606:4700::1111"] {
        let verdict = check_bind_address(host);
        assert!(!verdict.allowed, "{host}");
        assert!(verdict.reason.contains("no login"), "{}", verdict.reason);
    }
}

/// Resolving here would make what is reachable depend on DNS at startup.
#[test]
fn a_name_that_is_not_an_address_is_refused_as_a_name() {
    let verdict = check_bind_address("errand.example.com");

    assert!(!verdict.allowed);
    assert!(verdict.reason.contains("is a name, not an address"));
}

#[test]
fn nothing_at_all_is_refused_rather_than_treated_as_a_default() {
    assert!(!check_bind_address("   ").allowed);
    assert!(!check_bind_address("").allowed);
}

/// An octet out of range is not an address, so it must not read as private.
#[test]
fn something_that_only_looks_like_an_address_is_not_one() {
    for host in ["10.0.0.256", "10.0.0", "10.0.0.1.5", "010.0.0.1x"] {
        assert!(!check_bind_address(host).allowed, "{host}");
    }
}
