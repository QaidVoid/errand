//! Tests for the reference page generator.

use super::{
    characters_page, commands_page, config_schema, pages, read_schema, required_fields,
    stale_pages, write_pages,
};
use crate::chat::chars::all_chars;
use crate::session::commands::COMMANDS;

/// The real schema, as the generator reads it.
fn schema() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/config/schema.rs"),
    )
    .expect("the schema is in the tree")
}

#[test]
fn the_schema_is_read_into_every_documented_section() {
    let sections = read_schema(&schema());

    for (name, _) in super::KEYS {
        assert!(
            sections.iter().any(|section| section.name == name),
            "{name} is missing from the scan"
        );
    }
}

#[test]
fn every_command_appears_in_the_command_page() {
    let page = commands_page();

    for (name, meta) in COMMANDS {
        let spelled = meta
            .argument
            .map_or_else(|| name.to_string(), |argument| format!("{name} {argument}"));
        assert!(
            page.contains(&format!("| `{spelled}` |")),
            "{spelled} is missing"
        );
    }
    for title in [
        "The session",
        "Who takes part",
        "The project",
        "You and the host",
    ] {
        assert!(page.contains(&format!("## {title}")), "{title} is missing");
    }
}

#[test]
fn every_character_appears_in_the_characters_page() {
    let page = characters_page();

    for character in all_chars() {
        assert!(
            page.contains(character.name),
            "{} is missing",
            character.name
        );
    }
    assert!(page.contains("In total"), "the total is not stated");
}

#[test]
fn the_configuration_page_names_every_field_the_schema_has() {
    let sections = read_schema(&schema());
    let page = super::configuration_page(&sections, &required_fields());

    let count = page.matches("| `").count();
    assert!(count > 40, "the page carries only {count} fields");
    assert!(page.contains("`channelId`"), "the chat channel is missing");
    assert!(
        page.contains("`requireFullEnforcement`"),
        "enforcement is missing"
    );
}

#[test]
fn the_schema_for_editors_is_valid_json_naming_the_top_level() {
    let sections = read_schema(&schema());
    let schema = config_schema(&sections);

    let parsed: serde_json::Value = serde_json::from_str(&schema).expect("the schema is JSON");
    assert_eq!(parsed["title"], "errand configuration");
    assert!(parsed["properties"]["chat"].is_object(), "chat is missing");
    assert!(
        parsed["properties"]["$schema"].is_object(),
        "the schema pointer is missing"
    );
}

/// A tree whose pages match its code has nothing to report.
#[test]
fn the_check_passes_on_a_tree_whose_pages_match() {
    let root = mirrored_tree();

    assert!(stale_pages(root.path()).is_empty(), "nothing is stale");
}

/// A deliberately stale page is named rather than silently accepted.
#[test]
fn the_check_names_a_deliberately_stale_page() {
    let root = mirrored_tree();
    std::fs::write(
        root.path().join("docs/reference/characters.md"),
        "a page somebody edited by hand",
    )
    .expect("the page is rewritten");

    let stale = stale_pages(root.path());

    assert_eq!(stale, ["docs/reference/characters.md"]);
}

/// Bringing pages back in step writes exactly the stale ones.
#[test]
fn writing_pages_repairs_exactly_the_stale_ones() {
    let root = mirrored_tree();
    std::fs::write(root.path().join("docs/reference/characters.md"), "stale")
        .expect("the page is rewritten");

    assert_eq!(write_pages(root.path()), ["docs/reference/characters.md"]);
    assert!(stale_pages(root.path()).is_empty());
}

#[test]
fn the_committed_pages_match_the_code_this_generator_reads() {
    let root = super::repo_root();

    assert_eq!(stale_pages(&root), Vec::<String>::new());
}

/// Brings the committed pages back in step, for whoever edited the schema or
/// the tables. Not part of the ordinary run: an opt-in, so a test run never
/// writes into the tree.
#[test]
#[ignore = "writes the committed pages; run with --ignored when the code changed"]
fn regenerate_the_reference_pages() {
    let root = super::repo_root();

    write_pages(&root);
    assert_eq!(stale_pages(&root), Vec::<String>::new());
}

/// A tree laid out like the repository, with pages this generator produced.
fn mirrored_tree() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temp directory");
    let schema_path = root.path().join("src/config/schema.rs");
    std::fs::create_dir_all(schema_path.parent().unwrap()).expect("the schema directory");
    std::fs::write(&schema_path, schema()).expect("the schema is copied");

    for (path, wanted) in pages(&schema()) {
        let full = root.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).expect("the page directory");
        std::fs::write(full, wanted).expect("the page is written");
    }
    root
}
