import { assertEquals, assertStringIncludes } from "@std/assert";
import type { GithubConfig } from "../config/schema.ts";
import { AGENT_BIN, STATE_PATH } from "../sandbox/backend.ts";
import {
  attributionFooter,
  ghShimContents,
  gitConfigContents,
  gitIdentityEnv,
  REQUEST_FILENAME,
  reviewInstructions,
  threadLink,
  transcriptLink,
} from "./github.ts";

const GITHUB: GithubConfig = {
  token: "ghp-value",
  userName: "errand-bot",
  userEmail: "bot@example.com",
};

Deno.test("the git config commits as the bot and gets its token from gh", () => {
  const config = gitConfigContents(GITHUB);

  assertStringIncludes(config, "name = errand-bot");
  assertStringIncludes(config, "email = bot@example.com");
  assertStringIncludes(config, "helper = !gh auth git-credential");
});

/** A second copy of the token on disk buys nothing and can be read. */
Deno.test("the token is never written into the git config", () => {
  assertEquals(gitConfigContents(GITHUB).includes(GITHUB.token), false);
});

/**
 * The config file alone was not enough: an agent passing its own name at
 * commit time authored as itself, and the environment beats the file.
 */
Deno.test("the identity is in the environment as well as in the file", () => {
  assertEquals(gitIdentityEnv(GITHUB), {
    GIT_AUTHOR_NAME: "errand-bot",
    GIT_AUTHOR_EMAIL: "bot@example.com",
    GIT_COMMITTER_NAME: "errand-bot",
    GIT_COMMITTER_EMAIL: "bot@example.com",
  });
});

Deno.test("the gh wrapper refuses to open a pull request and says what to do", () => {
  const shim = ghShimContents();

  assertStringIncludes(shim, '[ "$1" = "pr" ] && [ "$2" = "create" ]');
  assertStringIncludes(shim, `${STATE_PATH}/${REQUEST_FILENAME}`);
  assertStringIncludes(shim, "exit 1");
});

/** Anything else has to reach the real program, or the session loses `gh`. */
Deno.test("the wrapper hands every other command to the real gh", () => {
  const shim = ghShimContents();

  assertStringIncludes(shim, 'exec "$dir/gh" "$@"');
  assertStringIncludes(shim, `[ "$dir" != "${AGENT_BIN}" ]`);
  assertStringIncludes(shim, "#!/bin/sh");
});

/**
 * The account that matches a chat name on GitHub belongs to somebody who
 * asked for nothing. This has notified the wrong person once already.
 */
Deno.test("the footer names who asked and mentions nobody", () => {
  const footer = attributionFooter("amelia", {
    thread: "https://discord.com/channels/1/2",
    transcript: "https://errand.example/?session=abc",
  });

  assertStringIncludes(footer, "Requested by amelia via errand.");
  assertStringIncludes(footer, "https://discord.com/channels/1/2");
  assertEquals(footer.includes("@"), false);
});

Deno.test("a footer with nowhere to point at is still an attribution", () => {
  assertEquals(attributionFooter("amelia", {}), "Requested by amelia via errand.");
});

Deno.test("the links are the addresses a reader can open", () => {
  assertEquals(threadLink("111", "222"), "https://discord.com/channels/111/222");
  assertEquals(
    transcriptLink("https://errand.example/", "s 1"),
    "https://errand.example/?session=s%201",
  );
});

Deno.test("the instructions carry the exact footer for the agent to copy", () => {
  const instructions = reviewInstructions(GITHUB, "amelia", { thread: "https://t/1" });

  assertStringIncludes(instructions, attributionFooter("amelia", { thread: "https://t/1" }));
  assertStringIncludes(instructions, "Open nothing unless you were asked to");
  assertStringIncludes(instructions, "errand-bot <bot@example.com>");
});

/** Announcing a pull request before the daemon opens it has happened. */
Deno.test("the instructions say the request is not the result", () => {
  const instructions = reviewInstructions(GITHUB, "amelia", {});

  assertStringIncludes(instructions, "a request, not the result");
  assertStringIncludes(instructions, "do not say a pull request");
  assertStringIncludes(instructions, "Never run `gh pr create`");
});
