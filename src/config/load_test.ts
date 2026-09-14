import { assertEquals, assertStringIncludes, assertThrows } from "@std/assert";
import { configCandidates, configPath, loadConfig } from "./load.ts";
import { ConfigError } from "./schema.ts";

const VALID = JSON.stringify({
  chat: { token: "t", channelId: "c", allowedUserIds: ["u"] },
  agent: { provider: "anthropic", credentialName: "ANTHROPIC_API_KEY", credential: "k" },
  projectRoot: "/tmp/errand/projects",
  stateDir: "/tmp/errand/state",
});

/** Naming the file outright wins, so nothing has to be guessed at. */
Deno.test("the environment names the file, and nothing else is consulted", () => {
  assertEquals(configPath({ ERRAND_CONFIG: "/etc/errand.json" }, () => false), "/etc/errand.json");
  assertEquals(configCandidates({ ERRAND_CONFIG: "/somewhere.json" }), ["/somewhere.json"]);
});

Deno.test("a blank variable is not a path, so the search runs", () => {
  const looked = configCandidates({ ERRAND_CONFIG: "  ", HOME: "/home/amelia" });

  assertEquals(looked[0], "/home/amelia/.config/errand/config.json");
});

/**
 * A person's own configuration comes first, so running the daemon by hand on a
 * host that also serves one does not pick up the service's token.
 */
Deno.test("it looks in the account's config directory, then the system's", () => {
  assertEquals(configCandidates({ HOME: "/home/amelia" }), [
    "/home/amelia/.config/errand/config.json",
    "/etc/errand/config.json",
    "config.json",
  ]);
});

Deno.test("a chosen config root is honoured", () => {
  assertEquals(
    configCandidates({ HOME: "/home/amelia", XDG_CONFIG_HOME: "/home/amelia/cfg" })[0],
    "/home/amelia/cfg/errand/config.json",
  );
});

Deno.test("the first one that is actually there is the one used", () => {
  const env = { HOME: "/home/amelia" };

  assertEquals(
    configPath(env, (path) => path === "/etc/errand/config.json"),
    "/etc/errand/config.json",
  );
  assertEquals(configPath(env, (path) => path === "config.json"), "config.json");
  // With none of them there, the failure names the place most likely meant.
  assertEquals(configPath(env, () => false), "/home/amelia/.config/errand/config.json");
});

Deno.test("a valid file loads", () => {
  const config = loadConfig("/anywhere", () => VALID);
  assertEquals(config.chat.channelId, "c");
});

/** Three different failures, so three different things to do about them. */
Deno.test("a missing file says where it looked and what to do", () => {
  const error = assertThrows(
    () =>
      loadConfig(
        "/home/amelia/.config/errand/config.json",
        () => {
          throw new Deno.errors.NotFound("nope");
        },
        { HOME: "/home/amelia" },
      ),
    ConfigError,
  ) as ConfigError;

  const said = error.problems.join("\n");
  assertStringIncludes(said, "/home/amelia/.config/errand/config.json");
  assertStringIncludes(said, "/etc/errand/config.json");
  assertStringIncludes(said, "ERRAND_CONFIG");
});

Deno.test("a file that is not JSON is not reported as a field problem", () => {
  const error = assertThrows(() => loadConfig("/c.json", () => "{ nope"), ConfigError);
  assertStringIncludes(String(error), "not valid JSON");
});

Deno.test("a file that parses but says something impossible lists every reason", () => {
  const error = assertThrows(() => loadConfig("/c.json", () => "{}"), ConfigError) as ConfigError;
  assertEquals(error.problems.length > 3, true);
});

Deno.test("house rules beside the configuration are picked up without being named", () => {
  const config = loadConfig(
    "/etc/errand/config.json",
    () => VALID,
    {},
    (path) => path === "/etc/errand/AGENTS.md",
  );

  assertEquals(config.agent.rulesPath, "/etc/errand/AGENTS.md");
});

Deno.test("the default follows the configuration that was loaded, not a fixed path", () => {
  // Somebody with a file in their home directory and another in /etc gets the
  // one belonging to the configuration in force.
  const config = loadConfig(
    "/home/a/.config/errand/config.json",
    () => VALID,
    {},
    (path) => path.startsWith("/home/a/"),
  );

  assertEquals(config.agent.rulesPath, "/home/a/.config/errand/AGENTS.md");
});

Deno.test("a named rules path wins over the file beside the configuration", () => {
  const named = JSON.stringify({
    ...JSON.parse(VALID),
    agent: {
      ...JSON.parse(VALID).agent,
      rulesPath: "/somewhere/else/RULES.md",
    },
  });

  const config = loadConfig(
    "/etc/errand/config.json",
    () => named,
    {},
    () => true,
  );

  assertEquals(config.agent.rulesPath, "/somewhere/else/RULES.md");
});

Deno.test("no file beside the configuration means no rules, and is not a refusal", () => {
  // A default nobody asked for must not be able to stop the daemon.
  const config = loadConfig(
    "/etc/errand/config.json",
    () => VALID,
    {},
    () => false,
  );

  assertEquals(config.agent.rulesPath, undefined);
});
