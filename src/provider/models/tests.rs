//! Tests for matching a name to the model it picks out.

use super::{AvailableModel, Matched, match_model};

fn model(provider: &str, id: &str) -> AvailableModel {
    AvailableModel {
        provider: provider.to_owned(),
        id: id.to_owned(),
        default_level: None,
    }
}

/// A bare model id belongs to whichever provider lists it, not the one a
/// session starts on, so a model of another provider is not sent to the
/// default provider's endpoint. The starting provider wins a bare name.
#[test]
fn a_bare_model_id_resolves_to_the_provider_that_serves_it() {
    let available = vec![
        model("zai-coding-cn", "glm-5.3"),
        model("opencode", "free-fast"),
    ];

    let Matched::One(found) = match_model(&available, "free-fast", "zai-coding-cn") else {
        panic!("another provider's model must resolve to that provider");
    };
    assert_eq!(found.provider, "opencode");

    let Matched::One(found) = match_model(&available, "glm-5.3", "zai-coding-cn") else {
        panic!("the starting provider's own model must stay with it");
    };
    assert_eq!(found.provider, "zai-coding-cn");
}

/// A name no provider lists is refused rather than sent on. This is the case
/// that turned a typo into a turn that failed at the provider.
#[test]
fn a_name_nothing_lists_is_refused() {
    let available = vec![model("zai-coding-cn", "glm-5.3")];

    assert_eq!(
        match_model(&available, "no-such-model", "zai-coding-cn"),
        Matched::None
    );
    // Qualified with a provider that does exist but does not serve it.
    assert_eq!(
        match_model(&available, "zai-coding-cn/no-such-model", "zai-coding-cn"),
        Matched::None
    );
    // And a provider that does not exist at all.
    assert_eq!(
        match_model(&available, "no-such-provider/glm-5.3", "zai-coding-cn"),
        Matched::None
    );
}

/// A host that lists no models has said nothing about its models, which is not
/// a statement that any name will do. An empty list must not make every name
/// valid.
#[test]
fn a_host_that_lists_nothing_refuses_every_name() {
    assert_eq!(match_model(&[], "glm-5.3", "zai"), Matched::None);
    assert_eq!(match_model(&[], "zai/glm-5.3", "zai"), Matched::None);
    // Including the configured model, which is not in the list either.
    assert_eq!(match_model(&[], "whatever", "zai"), Matched::None);
}

/// A model id may itself hold a slash, so the leading segment is only read as
/// a provider when the whole name matches one, not merely when the prefix
/// looks like it could.
#[test]
fn a_slash_inside_a_model_id_is_not_a_provider() {
    let available = vec![model("openrouter", "meta/muse-spark-1.3-contributor")];

    let Matched::One(found) = match_model(&available, "meta/muse-spark-1.3-contributor", "zai")
    else {
        panic!("a model id holding a slash must still be found");
    };
    assert_eq!(found.provider, "openrouter");
}

/// A name several providers serve is reported rather than picked from, so the
/// caller can ask which one was meant.
#[test]
fn a_name_two_providers_serve_is_reported_rather_than_guessed() {
    let available = vec![model("alpha", "shared"), model("beta", "shared")];

    assert_eq!(
        match_model(&available, "shared", "gamma"),
        Matched::Several(vec!["alpha/shared".to_owned(), "beta/shared".to_owned()])
    );
    // The starting provider wins a bare name without asking.
    assert_eq!(
        match_model(&available, "shared", "beta"),
        Matched::One(model("beta", "shared"))
    );
    // And naming the provider settles it.
    assert_eq!(
        match_model(&available, "alpha/shared", "beta"),
        Matched::One(model("alpha", "shared"))
    );
}

/// A level on the name is not part of the name, so a model is found by the id
/// with the level taken off.
#[test]
fn a_level_is_not_mistaken_for_part_of_the_id() {
    let available = vec![model("zai", "glm-5.3")];

    // The caller splits the level off first; this asserts the split result
    // matches, so the two halves cannot drift apart.
    let (bare, _level) = crate::session::model::split_level("glm-5.3:max");
    assert_eq!(
        match_model(&available, &bare, "zai"),
        Matched::One(model("zai", "glm-5.3"))
    );
}
