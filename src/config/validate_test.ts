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

Deno.test("a daemon with no interface configured serves none", () => {
  assertEquals(validateConfig(valid()).web, undefined);
});

Deno.test("an interface takes its address, port and role", () => {
  const web = validateConfig(valid({
    web: { host: "100.64.0.2", port: 9000, observer: true, publicUrl: "https://errand.example" },
  })).web;

  assertEquals(web?.host, "100.64.0.2");
  assertEquals(web?.port, 9000);
  assertEquals(web?.observer, true);
  assertEquals(web?.publicUrl, "https://errand.example");
});

Deno.test("an interface that says only that it exists gets the defaults", () => {
  const web = validateConfig(valid({ web: {} })).web;

  assertEquals(web?.host, DEFAULTS.web.host);
  assertEquals(web?.port, DEFAULTS.web.port);
  assertEquals(web?.observer, false);
  assertEquals(web?.publicUrl, undefined);
});

Deno.test("an interface port that is not a port is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ web: { port: "8080" } })).join("\n"),
    "web.port must be a number greater than zero",
  );
});

Deno.test("no extra grant is the same as no policyExtra section", () => {
  assertEquals(validateConfig(valid()).sandbox.policyExtra, undefined);
});

Deno.test("extra grants are taken as absolute paths", () => {
  const extra = validateConfig(valid({
    sandbox: {
      policyExtra: {
        read: ["/opt/toolchains"],
        write: ["/srv/output"],
        execute: ["/opt/toolchains/bin"],
      },
    },
  })).sandbox.policyExtra;

  assertEquals(extra?.read, ["/opt/toolchains"]);
  assertEquals(extra?.write, ["/srv/output"]);
  assertEquals(extra?.execute, ["/opt/toolchains/bin"]);
});

/** There is no working directory to resolve one against after the pivot. */
Deno.test("a relative path in a grant is refused, not resolved", () => {
  const problems = problemsOf(valid({
    sandbox: { policyExtra: { read: ["./shared", "/fine"] } },
  })).join("\n");

  assertStringIncludes(problems, "must be an absolute path");
});

/** A section that grants nothing is a mistake worth naming. */
Deno.test("an empty grant is refused rather than silently doing nothing", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { policyExtra: {} } })).join("\n"),
    "grants nothing",
  );
});

Deno.test("a misspelled grant list is refused like any other setting", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { policyExtra: { reed: ["/opt"] } } })).join("\n"),
    "sandbox.policyExtra.reed is not a setting",
  );
});

Deno.test("no environment section is the same as no variables", () => {
  assertEquals(validateConfig(valid()).sandbox.env, undefined);
});

Deno.test("variables named in configuration are read", () => {
  const env = validateConfig(valid({
    sandbox: { env: { CARGO_HOME: "/var/cache/cargo", RUSTUP_HOME: "/opt/rustup" } },
  })).sandbox.env;

  assertEquals(env, { CARGO_HOME: "/var/cache/cargo", RUSTUP_HOME: "/opt/rustup" });
});

/** The policy sets both, to paths it places. */
Deno.test("a variable the policy sets itself is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { env: { HOME: "/somewhere" } } })).join("\n"),
    "must not set HOME",
  );
});

/** Shadowing it would authenticate the agent with whatever was set here. */
Deno.test("the variable carrying the credential is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { env: { ANTHROPIC_API_KEY: "not-the-real-one" } } })).join("\n"),
    "carries the provider credential",
  );
});

Deno.test("a name no shell would accept is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { env: { "CARGO HOME": "/var/cache" } } })).join("\n"),
    "is not a variable name",
  );
});

Deno.test("a value that is not a string is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { env: { CARGO_HOME: 7 } } })).join("\n"),
    "must be a string",
  );
});

Deno.test("an empty environment is refused rather than silently doing nothing", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { env: {} } })).join("\n"),
    "names nothing",
  );
});

Deno.test("directories added to the path are taken as absolute paths", () => {
  assertEquals(
    validateConfig(valid({ sandbox: { pathExtra: ["/opt/toolchains/bin"] } })).sandbox.pathExtra,
    ["/opt/toolchains/bin"],
  );
});

Deno.test("a relative directory on the path is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { pathExtra: ["bin"] } })).join("\n"),
    "must be an absolute path",
  );
});

Deno.test("the egress ports default to https alone", () => {
  assertEquals(validateConfig(valid()).sandbox.egressPorts, [443]);
});

Deno.test("configured egress ports are read in order", () => {
  assertEquals(
    validateConfig(valid({ sandbox: { egressPorts: [80, 443] } })).sandbox.egressPorts,
    [80, 443],
  );
});

Deno.test("a port outside the socket range is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { egressPorts: [80, 70000] } })).join("\n"),
    "not a port between 1 and 65535",
  );
});

Deno.test("an empty egress list is refused rather than silencing the network", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { egressPorts: [] } })).join("\n"),
    "names no port",
  );
});

Deno.test("the host address is shown to a session by default", () => {
  assertEquals(validateConfig(valid()).sandbox.hideHostAddress, false);
});

Deno.test("hiding the host address is read as a flag", () => {
  assertEquals(
    validateConfig(valid({ sandbox: { hideHostAddress: true } })).sandbox.hideHostAddress,
    true,
  );
});

Deno.test("an absolute rules path is kept, and absent stays absent", () => {
  const withRules = validateConfig(valid({
    agent: {
      provider: "anthropic",
      credentialName: "ANTHROPIC_API_KEY",
      credential: "secret-value",
      rulesPath: "/etc/errand/AGENTS.md",
    },
  }));
  assertEquals(withRules.agent.rulesPath, "/etc/errand/AGENTS.md");

  assertEquals(validateConfig(valid()).agent.rulesPath, undefined);
});

Deno.test("a relative rules path is refused rather than resolved against the daemon's cwd", () => {
  // Resolving it would make the same configuration name a different file
  // depending on where the daemon happened to be started from.
  assertStringIncludes(
    problemsOf(valid({
      agent: {
        provider: "anthropic",
        credentialName: "ANTHROPIC_API_KEY",
        credential: "secret-value",
        rulesPath: "AGENTS.md",
      },
    })).join("\n"),
    "agent.rulesPath must be an absolute path, got AGENTS.md",
  );
});

Deno.test("a rules path that is not a path at all is refused", () => {
  assertStringIncludes(
    problemsOf(valid({
      agent: {
        provider: "anthropic",
        credentialName: "ANTHROPIC_API_KEY",
        credential: "secret-value",
        rulesPath: "",
      },
    })).join("\n"),
    "agent.rulesPath must be an absolute path",
  );
});

/**
 * Brokered by default: it is what keeps the provider credential out of a
 * sandbox, and a lone `*` restricts no host, so what it costs is the audit and
 * the port, not reach.
 */
Deno.test("egress defaults to a brokered pass-through", () => {
  const config = validateConfig(valid());
  assertEquals(config.sandbox.egress.mode, "proxy");
  assertEquals(config.sandbox.egress.allow, ["*"]);
});

Deno.test("proxy mode with a hostname allowlist is accepted and lower-cased", () => {
  const config = validateConfig(valid({
    sandbox: { egress: { mode: "proxy", allow: ["GitHub.com", "*.githubusercontent.com"] } },
  }));
  assertEquals(config.sandbox.egress.mode, "proxy");
  assertEquals(config.sandbox.egress.allow, ["github.com", "*.githubusercontent.com"]);
});

Deno.test("a lone * catch-all is accepted as an allowlist entry", () => {
  const config = validateConfig(valid({
    sandbox: { egress: { mode: "proxy", allow: ["*"] } },
  }));
  assertEquals(config.sandbox.egress.allow, ["*"]);
});

Deno.test("an unknown egress mode is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { egress: { mode: "wideopen" } } })).join("\n"),
    "sandbox.egress.mode must be one of open, proxy",
  );
});

Deno.test("a malformed allowlist entry is refused rather than handed to the broker", () => {
  // A broker told to permit "http://x" or "x/y" either permits nothing or more
  // than was meant, so a non-hostname is a configuration error.
  assertStringIncludes(
    problemsOf(valid({ sandbox: { egress: { allow: ["http://evil.com/x"] } } })).join("\n"),
    "is not a hostname",
  );
});

Deno.test("an unknown key under egress is refused", () => {
  assertStringIncludes(
    problemsOf(valid({ sandbox: { egress: { mode: "proxy", allowlist: [] } } })).join("\n"),
    "sandbox.egress",
  );
});

Deno.test("provider definitions are passed through, and their shape is checked", () => {
  const config = validateConfig(valid({
    agent: {
      provider: "meta",
      credentialName: "META_API_KEY",
      credential: "k",
      providers: {
        meta: {
          baseUrl: "https://api.meta.example/v1",
          api: "openai-completions",
          models: [{ id: "muse-spark-1.3-contributor", reasoning: true }],
        },
      },
    },
  }));
  // Handed to the agent as written, so a field this does not know is kept.
  const meta = config.agent.providers.meta as Record<string, unknown>;
  assertEquals(meta.api, "openai-completions");
  assertEquals((meta.models as unknown[]).length, 1);
});

Deno.test("a provider definition that is not an object is refused", () => {
  const error = assertThrows(
    () =>
      validateConfig(valid({
        agent: {
          provider: "p",
          credentialName: "K",
          credential: "k",
          providers: { meta: "https://api.meta.example/v1" },
        },
      })),
    ConfigError,
  ) as ConfigError;
  assertStringIncludes(error.problems.join("\n"), "agent.providers.meta");
});
