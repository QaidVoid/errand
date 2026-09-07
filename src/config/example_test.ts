import { assertEquals } from "@std/assert";
import { validateConfig } from "./validate.ts";
import { DEFAULTS } from "./schema.ts";

/**
 * The file somebody copies to start from.
 *
 * An example that no longer validates is worse than none: it is copied, it
 * fails, and the first thing this daemon ever did was refuse. Checked here so
 * that renaming a field breaks the suite rather than somebody's first run.
 */
Deno.test("the example configuration is one the daemon accepts", () => {
  const example = JSON.parse(Deno.readTextFileSync("config.example.json"));

  const config = validateConfig(example);

  assertEquals(config.chat.channelId.length > 0, true);
  assertEquals(config.agent.provider, "zai-coding-cn");
  assertEquals(config.agent.delegate?.model, "glm-5.3-flash");
  assertEquals((config.github?.userName ?? "").length > 0, true);
  assertEquals(config.web?.port, DEFAULTS.web.port);
});

/** Every section the example shows should be one the daemon knows. */
Deno.test("the example names no setting the daemon would refuse", () => {
  const example = JSON.parse(Deno.readTextFileSync("config.example.json"));

  // Validation refuses an unknown key outright, so reaching here is the check.
  validateConfig(example);
  assertEquals(Object.keys(example).includes("discord"), false);
});

/**
 * What an editor checks against, and what the daemon checks against.
 *
 * Both are generated from the same interfaces, so this asserts they agree on
 * the sections rather than restating either.
 */
Deno.test("the schema offers every section the daemon reads", () => {
  const schema = JSON.parse(Deno.readTextFileSync("config.schema.json"));
  const example = JSON.parse(Deno.readTextFileSync("config.example.json"));

  const offered = Object.keys(schema.properties);
  for (const key of Object.keys(example)) {
    assertEquals(offered.includes(key), true, `${key} is not in the schema`);
  }
  assertEquals(schema.additionalProperties, false);
  assertEquals(offered.includes("$schema"), true);
});

/** A file that names its schema must still be one the daemon accepts. */
Deno.test("naming the schema is not a setting the daemon refuses", () => {
  const example = JSON.parse(Deno.readTextFileSync("config.example.json"));

  assertEquals(typeof example.$schema, "string");
  validateConfig(example);
});
