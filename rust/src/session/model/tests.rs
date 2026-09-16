//! Tests for model selection, ported from `model_test.ts`.

use std::collections::BTreeMap;

use super::{expand_alias, resolve_model, select_model};

fn aliases(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(name, target)| ((*name).to_owned(), (*target).to_owned()))
        .collect()
}

#[test]
fn a_leading_model_flag_is_read_off_and_the_prompt_keeps_the_rest() {
    assert_eq!(
        select_model("--model meta/muse-spark-1.3-contributor:max build it"),
        super::ModelSelection {
            value: Some("meta/muse-spark-1.3-contributor:max".to_owned()),
            prompt: "build it".to_owned(),
        }
    );
    // Both spellings, because a person writes whichever they are used to.
    assert_eq!(
        select_model("--model=glm-5.3 go"),
        super::ModelSelection {
            value: Some("glm-5.3".to_owned()),
            prompt: "go".to_owned(),
        }
    );
    // A flag with nothing after it selects nothing and stays in the prompt.
    assert_eq!(
        select_model("just do the thing"),
        super::ModelSelection {
            value: None,
            prompt: "just do the thing".to_owned(),
        }
    );
}

/// A prompt that talks about the flag must not be taken as using it.
#[test]
fn a_model_flag_is_only_an_instruction_at_the_very_start() {
    assert_eq!(
        select_model("explain --model to me"),
        super::ModelSelection {
            value: None,
            prompt: "explain --model to me".to_owned(),
        }
    );
}

#[test]
fn a_known_leading_segment_is_the_provider_anything_else_is_the_model() {
    let known = ["zai-coding-cn", "meta"];
    // Named provider, and the model keeps its own slash and thinking level.
    assert_eq!(
        resolve_model("meta/muse-spark-1.3-contributor:max", &known),
        super::ChosenModel {
            provider: Some("meta".to_owned()),
            model: "muse-spark-1.3-contributor:max".to_owned(),
        }
    );
    // A model id that merely contains a slash is not a provider.
    assert_eq!(
        resolve_model("meta/muse-spark-1.3-contributor", &["openrouter"]),
        super::ChosenModel {
            provider: None,
            model: "meta/muse-spark-1.3-contributor".to_owned(),
        }
    );
    // A plain id keeps the configured provider.
    assert_eq!(
        resolve_model("glm-5.3-flash", &known),
        super::ChosenModel {
            provider: None,
            model: "glm-5.3-flash".to_owned(),
        }
    );
}

#[test]
fn a_short_name_stands_for_the_model_it_was_given() {
    let table = aliases(&[("muse", "meta/muse-spark-1.3-contributor")]);
    assert_eq!(
        expand_alias("muse", &table),
        "meta/muse-spark-1.3-contributor"
    );
    // The level is split off before the name is looked up, then put back.
    assert_eq!(
        expand_alias("muse:xhigh", &table),
        "meta/muse-spark-1.3-contributor:xhigh"
    );
    // A name standing for nothing is left exactly as written.
    assert_eq!(expand_alias("glm-5.3-flash", &table), "glm-5.3-flash");
    assert_eq!(expand_alias("mistyped:max", &table), "mistyped:max");
}

#[test]
fn a_level_on_the_name_beats_one_written_into_the_alias() {
    let table = aliases(&[("muse", "meta/muse-spark-1.3-contributor:high")]);
    // The alias carries a default...
    assert_eq!(
        expand_alias("muse", &table),
        "meta/muse-spark-1.3-contributor:high"
    );
    // ...and being more specific replaces it rather than stacking on it.
    assert_eq!(
        expand_alias("muse:xhigh", &table),
        "meta/muse-spark-1.3-contributor:xhigh"
    );
}

/// A colon in a model id must not be read as a thinking level.
#[test]
fn only_a_real_level_is_split_off_the_end() {
    let table = aliases(&[("weird", "provider/model:batch")]);
    assert_eq!(expand_alias("weird", &table), "provider/model:batch");
    assert_eq!(
        expand_alias("weird:max", &table),
        "provider/model:batch:max"
    );
}

/// Naming the model is typed often enough to be worth a short form.
#[test]
fn short_m_is_the_same_flag_as_the_long_one() {
    assert_eq!(
        select_model("-m musecringe:xhigh build it"),
        super::ModelSelection {
            value: Some("musecringe:xhigh".to_owned()),
            prompt: "build it".to_owned(),
        }
    );
    assert_eq!(
        select_model("-m=glm go"),
        super::ModelSelection {
            value: Some("glm".to_owned()),
            prompt: "go".to_owned(),
        }
    );

    // Still only at the very start, and still a whole word: a prompt about a
    // flag, and a word that merely begins with it, are left alone.
    assert_eq!(
        select_model("run it with -m glm"),
        super::ModelSelection {
            value: None,
            prompt: "run it with -m glm".to_owned(),
        }
    );
    assert_eq!(
        select_model("-make the thing"),
        super::ModelSelection {
            value: None,
            prompt: "-make the thing".to_owned(),
        }
    );
    assert_eq!(
        select_model("-m"),
        super::ModelSelection {
            value: None,
            prompt: "-m".to_owned(),
        }
    );
}
