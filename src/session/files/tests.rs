//! Tests for file reading, ported from `files_test.ts`.

use super::{language_for, looks_binary, read_directory, read_file_for_display};
use tempfile::TempDir;

fn with_project() -> TempDir {
    TempDir::with_prefix("errand-files-").expect("a temporary directory")
}

fn path(root: &TempDir, name: &str) -> String {
    root.path().join(name).to_string_lossy().into_owned()
}

/// The project root the readers resolve beneath.
fn root_of(root: &TempDir) -> String {
    root.path().to_string_lossy().into_owned()
}

#[test]
fn a_directory_lists_directories_first_then_by_name() {
    let root = with_project();
    std::fs::create_dir(path(&root, "src")).expect("created");
    std::fs::create_dir(path(&root, "docs")).expect("created");
    std::fs::write(path(&root, "readme.md"), "hello").expect("written");
    std::fs::write(path(&root, "deno.json"), "{}").expect("written");

    let entries = read_directory(&root_of(&root), "").expect("listed");

    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["docs", "src", "deno.json", "readme.md"]
    );
    assert!(entries[0].directory);
    assert_eq!(entries[3].size, 5);
}

/// A surface asks for a path again, so the path it was given has to work.
#[test]
fn each_entry_carries_the_path_to_ask_for_it_by() {
    let root = with_project();
    std::fs::create_dir(path(&root, "src")).expect("created");
    std::fs::write(path(&root, "src/main.ts"), "").expect("written");

    let root_path = root.path().to_string_lossy().into_owned();
    assert_eq!(
        read_directory(&root_path, "").expect("listed")[0].path,
        "src"
    );
    assert_eq!(
        read_directory(&root_of(&root), "src").expect("listed")[0].path,
        "src/main.ts"
    );
}

#[test]
fn an_empty_directory_lists_nothing_rather_than_failing() {
    let root = with_project();

    assert_eq!(
        read_directory(&root_of(&root), "").expect("listed"),
        Vec::new()
    );
}

#[test]
fn a_text_file_is_read_with_the_language_to_show_it_in() {
    let root = with_project();
    std::fs::write(path(&root, "main.ts"), "const x = 1;\n").expect("written");

    let contents =
        read_file_for_display(&root_of(&root), "main.ts", super::MAX_INLINE_BYTES).expect("read");

    assert_eq!(contents.text, "const x = 1;\n");
    assert_eq!(contents.language, "ts");
    assert!(!contents.binary);
    assert!(!contents.truncated);
    assert_eq!(contents.size, 13);
}

/// Pasting an image into a thread as mojibake helps nobody.
#[test]
fn a_file_with_a_nul_byte_is_reported_as_binary_not_as_text() {
    let root = with_project();
    std::fs::write(path(&root, "logo.png"), [0x89_u8, 0x50, 0, 0x1a]).expect("written");

    let contents =
        read_file_for_display(&root_of(&root), "logo.png", super::MAX_INLINE_BYTES).expect("read");

    assert!(contents.binary);
    assert_eq!(contents.text, "");
}

#[test]
fn a_long_file_is_cut_and_says_the_whole_size_it_was_cut_from() {
    let root = with_project();
    std::fs::write(path(&root, "big.txt"), "x".repeat(5_000)).expect("written");

    let contents = read_file_for_display(&root_of(&root), "big.txt", 100).expect("read");

    assert!(contents.truncated);
    assert_eq!(contents.text.chars().count(), 100);
    assert_eq!(contents.size, 5_000);
}

/// A limit counted in bytes can land inside a character. Decoding the half of
/// it that was read would end the shown text with a replacement mark.
#[test]
fn a_cut_landing_inside_a_character_does_not_show_half_of_one() {
    let root = with_project();
    std::fs::write(path(&root, "wide.txt"), "ab\u{1F50C}cd").expect("written");

    let contents = read_file_for_display(&root_of(&root), "wide.txt", 4).expect("read");

    assert_eq!(contents.text, "ab");
    assert!(!contents.text.contains('\u{FFFD}'));
}

#[test]
fn a_directory_asked_for_as_a_file_says_so_rather_than_reading_it() {
    let root = with_project();
    std::fs::create_dir(path(&root, "src")).expect("created");

    let error = read_file_for_display(&root_of(&root), "src", super::MAX_INLINE_BYTES)
        .expect_err("a directory is not a file");
    assert_eq!(error.to_string(), "src is not a file");
}

#[test]
fn an_extension_picks_the_language_and_an_unknown_one_picks_none() {
    assert_eq!(language_for("src/main.ts"), "ts");
    assert_eq!(language_for("Makefile"), "");
    assert_eq!(language_for("notes.MD"), "md");
}

#[test]
fn only_a_nul_in_the_first_block_makes_content_binary() {
    assert!(!looks_binary(b"plain text"));
    assert!(looks_binary(&[0_u8]));
    let mut late = vec![0x61_u8; 9_000];
    late[8_500] = 0;
    assert!(!looks_binary(&late));
}
