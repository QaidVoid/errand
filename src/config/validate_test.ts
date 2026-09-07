import { assertEquals, assertStringIncludes, assertThrows } from "@std/assert";
import { ConfigError, DEFAULTS } from "./schema.ts";
import { validateConfig } from "./validate.ts";

function valid(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    chat: {
      token: "a.token.value",
      channelId: "111222333444555666",
      allowedUserIds: ["777888999000111222"],
    },
    agent: {
      provider: "anthropic",
      credentialName: "ANTHROPIC_API_KEY",
      credential: "secret-value",
    },
    projectRoot: "/tmp/errand/projects",
    stateDir: "/tmp/errand/state",
    ...overrides,
  };
}

function problemsOf(raw: unknown): string[] {
  const error = assertThrows(() => validateConfig(raw), ConfigError) as ConfigError;
  return [...error.problems];
}

Deno.test("a minimal file resolves, with the documented defaults filled in", () => {
  const config = validateConfig(valid());

  assertEquals(config.chat.channelId, "111222333444555666");
  assertEquals(config.agent.model, undefined);
  assertEquals(config.sandbox.backend, DEFAULTS.sandbox.backend);
  assertEquals(config.limits.maxConcurrentTurns, DEFAULTS.limits.maxConcurrentTurns);
  assertEquals(config.timeouts.idleMs, DEFAULTS.timeouts.idleMs);
  assertEquals(config.chat.blockedUserIds, []);
});

Deno.test("every problem is reported, not only the first", () => {
  const problems = problemsOf({ chat: {}, agent: {} });

  assertEquals(problems.length > 3, true);
  assertStringIncludes(problems.join("\n"), "chat.token");
  assertStringIncludes(problems.join("\n"), "agent.provider");
  assertStringIncludes(problems.join("\n"), "projectRoot");
});

/**
 * A setting that takes no effect is worse than one that is rejected: the
 * daemon then runs without a guarantee somebody believes they configured.
 */
Deno.test("a misspelled setting is refused rather than ignored", () => {
  const problems = problemsOf(valid({ sandbox: { requireFullEnforcment: false } }));
  assertStringIncludes(problems.join("\n"), "sandbox.requireFullEnforcment is not a setting");

  const atRoot = problemsOf(valid({ projectRooot: "/tmp/x" }));
  assertStringIncludes(atRoot.join("\n"), "config.projectRooot is not a setting");
});

Deno.test("an empty allowlist refuses to start rather than admitting everyone", () => {
  const problems = problemsOf(valid({ chat: { token: "t", channelId: "c", allowedUserIds: [] } }));
  assertStringIncludes(problems.join("\n"), "allow-everyone");
});

Deno.test("paths must be absolute, and must not be the same directory", () => {
  assertStringIncludes(
    problemsOf(valid({ projectRoot: "./projects" })).join("\n"),
    "projectRoot must be an absolute path",
  );
  assertStringIncludes(
    problemsOf(valid({ projectRoot: "/tmp/same", stateDir: "/tmp/same" })).join("\n"),
    "must be different directories",
  );
});

Deno.test("sizes and counts are checked, so a typo cannot become a limit", () => {
  const problems = problemsOf(
    valid({ sandbox: { memory: "four gigs", cpus: 0 }, limits: { maxQueueLength: -3 } }),
  );

  assertStringIncludes(problems.join("\n"), "sandbox.memory must be a size");
  assertStringIncludes(problems.join("\n"), "sandbox.cpus must be a number greater than zero");
  assertStringIncludes(problems.join("\n"), "limits.maxQueueLength");
});

Deno.test("a backend that does not exist is named, with the ones that do", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { backend: "docker" } })).join("\n"),
    "sandbox.backend must be one of podman, bailey",
  );
});

Deno.test("anything that is not an object is refused with one clear reason", () => {
  assertEquals(problemsOf([1, 2, 3]), ["the configuration file must contain a JSON object"]);
  assertEquals(problemsOf("nope"), ["the configuration file must contain a JSON object"]);
});

/** Inert under bailey, required under podman, and validated the same either way. */
Deno.test("the container image defaults, and is refused when it is not a name", () => {
  assertEquals(validateConfig(valid()).sandbox.image, DEFAULTS.sandbox.image);
  assertEquals(
    validateConfig(valid({ sandbox: { image: "localhost/mine:v2" } })).sandbox.image,
    "localhost/mine:v2",
  );
  assertStringIncludes(
    problemsOf(valid({ sandbox: { image: 7 } })).join("\n"),
    "sandbox.image must be a non-empty string",
  );
});

Deno.test("a daemon with no GitHub identity is configured, not broken", () => {
  assertEquals(validateConfig(valid()).github, undefined);
});

Deno.test("a GitHub identity is taken whole", () => {
  const github = validateConfig(valid({
    github: { token: "ghp-value", userName: "errand-bot", userEmail: "bot@example.com" },
  })).github;

  assertEquals(github?.userName, "errand-bot");
  assertEquals(github?.token, "ghp-value");
});

/** Pushing as half an identity is worse than not being able to push. */
Deno.test("a GitHub section missing a field is refused, not half-filled", () => {
  const problems = problemsOf(valid({ github: { token: "ghp-value" } })).join("\n");

  assertStringIncludes(problems, "github.userName is required");
  assertStringIncludes(problems, "github.userEmail is required");
});

Deno.test("a misspelled GitHub setting is refused like any other", () => {
  assertStringIncludes(
    problemsOf(valid({
      github: {
        token: "t",
        userName: "n",
        userEmail: "e",
        userNmae: "typo",
      },
    })).join("\n"),
    "github.userNmae is not a setting",
  );
});

Deno.test("what reaches a thread has documented defaults", () => {
  const output = validateConfig(valid()).output;

  assertEquals(output, DEFAULTS.output);
});

Deno.test("what reaches a thread can be turned up or down", () => {
  const output = validateConfig(valid({
    output: { forwardToolOutput: true, postDiffs: false, maxToolOutputChars: 4_000 },
  })).output;

  assertEquals(output.forwardToolOutput, true);
  assertEquals(output.postDiffs, false);
  assertEquals(output.maxToolOutputChars, 4_000);
  assertEquals(output.maxAttachmentsPerMessage, DEFAULTS.output.maxAttachmentsPerMessage);
});

Deno.test("an output limit that is not a number is refused", () => {
  const problems = problemsOf(valid({
    output: { maxAttachmentBytes: "5mb", postDiffs: "yes" },
  })).join("\n");

  assertStringIncludes(problems, "output.maxAttachmentBytes must be a number");
  assertStringIncludes(problems, "output.postDiffs must be true or false");
});

/** Silence about a command that turns the machine off means nobody. */
Deno.test("nobody may power off the host unless somebody is named", () => {
  assertEquals(validateConfig(valid()).shutdown.allowedUserIds, []);
  assertEquals(
    validateConfig(valid({ shutdown: { allowedUserIds: ["777"] } })).shutdown.allowedUserIds,
    ["777"],
  );
});

Deno.test("a shutdown list that is not a list of accounts is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ shutdown: { allowedUserIds: "everyone" } })).join("\n"),
    "shutdown.allowedUserIds must be a list of account ids",
  );
});
