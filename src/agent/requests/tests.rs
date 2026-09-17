//! Tests for the delegate command and its instructions, alongside
//! `delegate_test.ts`'s shape assertions.

use super::{DELEGATE_COMMAND, DELEGATE_DIR, delegate_command_contents, delegate_instructions};

#[test]
fn the_command_waits_a_little_longer_than_the_daemons_deadline() {
    let contents = delegate_command_contents(5_000);
    // Five seconds is fifty tenths, plus the one-second grace.
    assert!(contents.contains("-lt 150"), "{}", contents);
}

#[test]
fn the_command_writes_a_request_and_reads_the_answer_beside_it() {
    let contents = delegate_command_contents(1_000);

    assert!(contents.starts_with("#!/bin/sh"));
    assert!(contents.contains("dir=/state/delegations"));
    assert!(contents.contains("${dir}/${id}.request"));
    assert!(contents.contains("${dir}/${id}.answer"));
    assert!(contents.contains("${dir}/${id}.refused"));
    // The answer is renamed into place, so half of one is never read.
    assert!(contents.contains("${dir}/${id}.writing"));
}

#[test]
fn the_instructions_name_the_command_its_budget_and_its_limits() {
    let instructions = delegate_instructions("cheap-model", 8);

    assert!(instructions.contains("`delegate` asks cheap-model"));
    assert!(instructions.contains("You may ask 8 times per turn"));
    assert!(instructions.contains("--file src/parse.ts"));
}

/// The command is the only spelling on the agent's PATH.
#[test]
fn the_command_name_is_the_one_the_instructions_teach() {
    assert_eq!(DELEGATE_COMMAND, "delegate");
    assert_eq!(DELEGATE_DIR, "delegations");
}
