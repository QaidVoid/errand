import { assertEquals, assertStringIncludes } from "@std/assert";
import type { ChatConfig } from "../config/schema.ts";
import { classify, isBlocked, isPermitted, type RawMessage, withoutBotMention } from "./inbound.ts";

const CHANNEL = "served-channel";

const CONFIG: ChatConfig = {
  token: "bot-token",
  channelId: CHANNEL,
  allowedUserIds: ["u-1", "u-2"],
  blockedUserIds: [],
  operatorUserIds: [],
  startOnMention: false,
};

function message(overrides: Partial<RawMessage> = {}): RawMessage {
  return {
    id: "m-1",
    authorId: "u-1",
    authorName: "somebody",
    authorIsBot: false,
    channelId: CHANNEL,
    parentChannelId: undefined,
    content: "do the thing",
    attachments: [],
    ...overrides,
  };
}

Deno.test("a message in the served channel starts a session", () => {
  assertEquals(classify(message(), CONFIG), { kind: "start" });
});

Deno.test("a reply in a thread of the served channel goes to that thread", () => {
  assertEquals(
    classify(message({ channelId: "t-9", parentChannelId: CHANNEL }), CONFIG),
    { kind: "thread", threadId: "t-9" },
  );
});

Deno.test("anywhere else is ignored, including a direct message", () => {
  assertEquals(classify(message({ channelId: "other" }), CONFIG).kind, "ignore");
  assertEquals(
    classify(message({ channelId: "t-9", parentChannelId: "other-channel" }), CONFIG).kind,
    "ignore",
  );
  assertEquals(
    classify(message({ channelId: "dm-1", parentChannelId: undefined }), CONFIG).kind,
    "ignore",
  );
});

Deno.test("a bot is ignored, including this one", () => {
  assertEquals(classify(message({ authorIsBot: true }), CONFIG).kind, "ignore");
});

Deno.test("somebody not on the allowlist is ignored", () => {
  assertEquals(classify(message({ authorId: "stranger" }), CONFIG).kind, "ignore");
});

/** Naming it would describe the allowlist to the person it excludes. */
Deno.test("no refusal names an account or a list", () => {
  const refused = classify(message({ authorId: "stranger" }), CONFIG);
  if (refused.kind !== "ignore") throw new Error("expected a refusal");

  assertEquals(refused.reason.includes("stranger"), false);
  assertEquals(refused.reason.includes("u-1"), false);
  assertEquals(refused.reason.includes("u-2"), false);
});

Deno.test("the wildcard admits anyone who can post there", () => {
  const open = { ...CONFIG, allowedUserIds: ["*"] };

  assertEquals(classify(message({ authorId: "anybody" }), open), { kind: "start" });
  assertEquals(classify(message({ authorId: "somebody-else" }), open).kind, "start");
});

/**
 * The reason the list exists: an open channel has no other way to exclude one
 * person without closing it to everybody.
 */
Deno.test("a blocked account is refused even when the channel is open", () => {
  const open = { ...CONFIG, allowedUserIds: ["*"], blockedUserIds: ["troll"] };

  assertEquals(classify(message({ authorId: "troll" }), open).kind, "ignore");
  assertEquals(classify(message({ authorId: "anybody" }), open).kind, "start");
});

Deno.test("blocking beats the allowlist and any role", () => {
  const both = {
    ...CONFIG,
    allowedUserIds: ["u-1", "troll"],
    operatorUserIds: ["troll"],
    blockedUserIds: ["troll"],
  };

  assertEquals(isBlocked(both, "troll"), true);
  assertEquals(isPermitted(both, "troll"), false);
  assertEquals(classify(message({ authorId: "troll" }), both).kind, "ignore");
  assertEquals(classify(message({ authorId: "u-1" }), both).kind, "start");
});

/** A message carrying only a file still says something: that a file arrived. */
Deno.test("an attachment with no words is still a message", () => {
  const attached = message({
    content: "   ",
    attachments: [{
      id: "a-1",
      name: "shot.png",
      url: "https://x/1",
      size: 10,
      contentType: "image/png",
    }],
  });

  assertEquals(classify(attached, CONFIG).kind, "start");
  assertEquals(classify(message({ content: "  " }), CONFIG).kind, "ignore");
});

/**
 * A channel is often also somewhere people talk. Requiring the bot to be
 * named lets them, and only a message addressed to it opens a sandbox.
 */
Deno.test("with the setting on, only a message naming the bot starts one", () => {
  const config = { ...CONFIG, startOnMention: true };

  assertEquals(
    classify(message({ content: "what did you all think?" }), config, "bot-1").kind,
    "ignore",
  );
  assertEquals(
    classify(message({ content: "<@bot-1> demo: fix the parser" }), config, "bot-1").kind,
    "start",
  );
  assertEquals(
    classify(message({ content: "look at this <@!bot-1>" }), config, "bot-1").kind,
    "start",
  );
});

/** Naming a different bot in the channel is not naming this one. */
Deno.test("somebody else's bot is not this one", () => {
  const config = { ...CONFIG, startOnMention: true };

  assertEquals(
    classify(message({ content: "<@other> do a thing" }), config, "bot-1").kind,
    "ignore",
  );
});

/** Inside a thread the session is the conversation, so nothing is required. */
Deno.test("a thread reply never has to name the bot", () => {
  const config = { ...CONFIG, startOnMention: true };
  const reply = message({
    content: "carry on",
    channelId: "thread-1",
    parentChannelId: CHANNEL,
  });

  assertEquals(classify(reply, config, "bot-1").kind, "thread");
});

Deno.test("with the setting off, any message starts one as before", () => {
  assertEquals(classify(message({ content: "demo: go" }), CONFIG, "bot-1").kind, "start");
  assertEquals(classify(message({ content: "demo: go" }), CONFIG).kind, "start");
});

/** Refusing everything is better than starting work nobody addressed. */
Deno.test("a bot that does not know its own name starts nothing", () => {
  const config = { ...CONFIG, startOnMention: true };

  const decision = classify(message({ content: "<@bot-1> go" }), config);

  assertEquals(decision.kind, "ignore");
  assertStringIncludes(decision.kind === "ignore" ? decision.reason : "", "its own name");
});

Deno.test("the mention that summoned it is not part of what was asked", () => {
  assertEquals(withoutBotMention("<@bot-1> demo: fix the parser", "bot-1"), "demo: fix the parser");
  assertEquals(
    withoutBotMention("hey <@!bot-1> look at <@bot-1> this", "bot-1"),
    "hey look at this",
  );
  assertEquals(withoutBotMention("nothing to remove", "bot-1"), "nothing to remove");
});

/**
 * A message beginning with `!` is already addressed to a bot. Asking for a
 * mention as well makes `!usage` and `!help` vanish in the channel, which is
 * where they are most useful.
 */
Deno.test("a command reaches the daemon without naming the bot", () => {
  const config = { ...CONFIG, startOnMention: true };

  for (const content of ["!help", "!usage", "!shutdown", "  !status"]) {
    assertEquals(classify(message({ content }), config, "bot-1").kind, "start", content);
  }
});

/** Including one this daemon does not answer, which it then leaves alone. */
Deno.test("somebody else's command is let through and ignored later", () => {
  const config = { ...CONFIG, startOnMention: true };

  assertEquals(classify(message({ content: "!somebodyelses" }), config, "bot-1").kind, "start");
});

/** An aside in the channel is people talking, and starts nothing either way. */
Deno.test("an aside is let through and starts nothing", () => {
  const config = { ...CONFIG, startOnMention: true };

  assertEquals(classify(message({ content: "!!! anyone around?" }), config, "bot-1").kind, "start");
});
