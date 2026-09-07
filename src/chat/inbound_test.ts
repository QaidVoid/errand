import { assertEquals } from "@std/assert";
import type { ChatConfig } from "../config/schema.ts";
import { classify, isBlocked, isPermitted, type RawMessage } from "./inbound.ts";

const CHANNEL = "served-channel";

const CONFIG: ChatConfig = {
  token: "bot-token",
  channelId: CHANNEL,
  allowedUserIds: ["u-1", "u-2"],
  blockedUserIds: [],
  operatorUserIds: [],
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
