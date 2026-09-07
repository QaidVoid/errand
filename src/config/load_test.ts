import { assertEquals, assertStringIncludes, assertThrows } from "@std/assert";
import { configPath, loadConfig } from "./load.ts";
import { ConfigError } from "./schema.ts";

const VALID = JSON.stringify({
  chat: { token: "t", channelId: "c", allowedUserIds: ["u"] },
  agent: { provider: "anthropic", credentialName: "ANTHROPIC_API_KEY", credential: "k" },
  projectRoot: "/tmp/errand/projects",
  stateDir: "/tmp/errand/state",
});

Deno.test("the path comes from the environment, or falls back", () => {
  assertEquals(configPath({}), "config.json");
  assertEquals(configPath({ ERRAND_CONFIG: "  " }), "config.json");
  assertEquals(configPath({ ERRAND_CONFIG: "/etc/errand.json" }), "/etc/errand.json");
});

Deno.test("a valid file loads", () => {
  const config = loadConfig("/anywhere", () => VALID);
  assertEquals(config.chat.channelId, "c");
});

/** Three different failures, so three different things to do about them. */
Deno.test("a missing file says where it looked and what to do", () => {
  const error = assertThrows(
    () =>
      loadConfig("/etc/errand.json", () => {
        throw new Deno.errors.NotFound("nope");
      }),
    ConfigError,
  ) as ConfigError;

  assertStringIncludes(error.problems[0] ?? "", "/etc/errand.json");
  assertStringIncludes(error.problems[0] ?? "", "ERRAND_CONFIG");
});

Deno.test("a file that is not JSON is not reported as a field problem", () => {
  const error = assertThrows(() => loadConfig("/c.json", () => "{ nope"), ConfigError);
  assertStringIncludes(String(error), "not valid JSON");
});

Deno.test("a file that parses but says something impossible lists every reason", () => {
  const error = assertThrows(() => loadConfig("/c.json", () => "{}"), ConfigError) as ConfigError;
  assertEquals(error.problems.length > 3, true);
});
