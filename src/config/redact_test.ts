import { assertEquals, assertStringIncludes } from "@std/assert";
import { loadConfig } from "./load.ts";
import { validateConfig } from "./validate.ts";
import { redactConfig, REDACTION, redactText, secretValues } from "./redact.ts";
import { SECRET_PATHS } from "./schema.ts";

const TOKEN = "chat-token-2f8a41cc";
const CREDENTIAL = "sk-live-9f3c7a11d4e6";
const GITHUB_TOKEN = "ghp-4b1d90ff7ac2";

const config = loadConfig(
  "/anywhere",
  () =>
    JSON.stringify({
      chat: { token: TOKEN, channelId: "c", allowedUserIds: ["u"] },
      agent: { provider: "anthropic", credentialName: "ANTHROPIC_API_KEY", credential: CREDENTIAL },
      github: { token: GITHUB_TOKEN, userName: "errand-bot", userEmail: "bot@example.com" },
      projectRoot: "/tmp/errand/projects",
      stateDir: "/tmp/errand/state",
    }),
);

Deno.test("the configuration can be logged with every secret field blanked", () => {
  const shown = JSON.stringify(redactConfig(config));

  assertEquals(shown.includes(TOKEN), false);
  assertEquals(shown.includes(CREDENTIAL), false);
  assertEquals(shown.includes(GITHUB_TOKEN), false);
  assertEquals((shown.match(/\[redacted\]/g) ?? []).length, SECRET_PATHS.length);
});

/** Blanking a copy: the running daemon still needs the real values. */
Deno.test("redacting for the log does not disturb the configuration", () => {
  redactConfig(config);

  assertEquals(config.chat.token, TOKEN);
});

Deno.test("what a running session must scrub is every secret it holds", () => {
  assertEquals(secretValues(config).sort(), [TOKEN, CREDENTIAL, GITHUB_TOKEN].sort());
});

/** A section that is not configured has no secret, and gains no field. */
Deno.test("a secret in a section that was omitted is simply not there", () => {
  const withoutGithub = { ...config, github: undefined };

  assertEquals(secretValues(withoutGithub), [TOKEN, CREDENTIAL]);
  assertEquals(redactConfig(withoutGithub).github, undefined);
});

Deno.test("a secret is scrubbed wherever it appears in free text", () => {
  const text = `Authorization: Bearer ${CREDENTIAL}\nretrying with ${CREDENTIAL}`;

  const scrubbed = redactText(text, [CREDENTIAL]);

  assertEquals(scrubbed.includes(CREDENTIAL), false);
  assertEquals((scrubbed.match(/\[redacted\]/g) ?? []).length, 2);
  assertEquals(scrubbed.startsWith("Authorization: Bearer "), true);
});

/**
 * A configuration can hold a short value in a secret field. Replacing it
 * everywhere would rewrite ordinary prose without hiding anything worth
 * hiding, so it is left alone in text and blanked structurally instead.
 */
Deno.test("a value too short to be a credential is not scrubbed from prose", () => {
  assertEquals(redactText("the cat sat on the mat", ["cat"]), "the cat sat on the mat");
  assertEquals(secretValues({ ...config, chat: { ...config.chat, token: "short" } }), [
    CREDENTIAL,
    GITHUB_TOKEN,
  ]);
});

Deno.test("text holding no secret is returned as it was", () => {
  assertEquals(redactText("nothing to see", [CREDENTIAL]), "nothing to see");
  assertEquals(REDACTION, "[redacted]");
});

/** A second provider's key is as much a secret as the first one's. */
Deno.test("a defined provider's credential is scrubbed too", () => {
  const config = validateConfig({
    chat: { token: "chat-token-value", channelId: "c", allowedUserIds: ["u"] },
    agent: {
      provider: "zai",
      credentialName: "K",
      credential: "the-default-key",
      providers: { meta: { baseUrl: "https://api.meta.example/v1", credential: "the-meta-key" } },
    },
    projectRoot: "/tmp/p",
    stateDir: "/tmp/s",
  });

  const secrets = secretValues(config);
  assertEquals(secrets.includes("the-meta-key"), true);
  assertEquals(secrets.includes("the-default-key"), true);
  assertStringIncludes(redactText("key=the-meta-key here", secrets), REDACTION);
});
