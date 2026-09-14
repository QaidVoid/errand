import { join } from "@std/path";
import { assertEquals, assertStringIncludes } from "@std/assert";
import { houseRulesText, RULES_HEADING, rulesBlock } from "./rules.ts";

Deno.test("a file with rules in it becomes a block under the heading", () => {
  const block = rulesBlock("Always run the suite before committing.");

  assertStringIncludes(block ?? "", RULES_HEADING);
  assertStringIncludes(block ?? "", "Always run the suite before committing.");
});

Deno.test("an empty file is no rules rather than a heading over nothing", () => {
  // An operator who empties the file is saying there are no rules. A heading
  // with nothing under it reads to the agent as an omission it should ask about.
  assertEquals(rulesBlock(""), undefined);
  assertEquals(rulesBlock("   \n\n\t\n"), undefined);
});

Deno.test("the block says where the rules came from, so a conflict is reportable", () => {
  // The agent has the project's own instructions too. Without knowing which is
  // which it cannot say that the two disagree, it can only silently pick one.
  const block = rulesBlock("Never force push.") ?? "";

  assertStringIncludes(block, "every");
  assertStringIncludes(block, "disagree");
});

Deno.test("surrounding blank lines in the file do not survive into the block", () => {
  const padded = rulesBlock("\n\n  Keep commits small.  \n\n") ?? "";

  assertStringIncludes(padded, "Keep commits small.");
  assertEquals(padded.includes("\n\n\n"), false);
});

Deno.test("the rules are readable back for scrubbing, and absent when there are none", async () => {
  const root = await Deno.makeTempDir({ prefix: "errand-rules-" });
  try {
    const path = join(root, "AGENTS.md");
    Deno.writeTextFileSync(path, "  Prefer jj over git.\nNo em-dashes.  \n");
    assertEquals(houseRulesText(path), "Prefer jj over git.\nNo em-dashes.");

    // Nothing configured, an unreadable path, and an emptied file are all
    // "no rules" rather than a failure.
    assertEquals(houseRulesText(undefined), undefined);
    assertEquals(houseRulesText(join(root, "absent.md")), undefined);
    Deno.writeTextFileSync(path, "   \n\n");
    assertEquals(houseRulesText(path), undefined);
  } finally {
    await Deno.remove(root, { recursive: true });
  }
});
