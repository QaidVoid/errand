import { assertEquals } from "@std/assert";
import {
  asDialogRequest,
  isFireAndForget,
  messageRole,
  messageText,
  startsThinking,
  thinkingEnded,
  toolTarget,
  usageOf,
} from "./protocol.ts";

Deno.test("usage is read off a record, counting cache reads apart from input", () => {
  const usage = usageOf({
    message: {
      model: "glm-5.3",
      usage: { input: 900, output: 120, cacheRead: 8_000, cacheWrite: 0, totalTokens: 9_020 },
    },
  });

  assertEquals(usage?.input, 900);
  assertEquals(usage?.cacheRead, 8_000);
  assertEquals(usage?.model, "glm-5.3");
});

Deno.test("a record with no usage reports none rather than zeroes", () => {
  assertEquals(usageOf({ type: "agent_settled" }), undefined);
});

Deno.test("a dialog the agent waits on is recognised, with its options", () => {
  const request = asDialogRequest({
    type: "extension_ui_request",
    id: "d-1",
    method: "select",
    title: "Which branch?",
    options: ["main", "dev", 7],
  });

  assertEquals(request?.method, "select");
  assertEquals(request?.title, "Which branch?");
  assertEquals(request?.options, ["main", "dev"]);
});

/** Answering one of these would be answering a question nobody asked. */
Deno.test("an informational request is not a dialog", () => {
  assertEquals(isFireAndForget("notify"), true);
  assertEquals(isFireAndForget("select"), false);
  assertEquals(
    asDialogRequest({ type: "extension_ui_request", id: "n-1", method: "notify" }),
    null,
  );
});

Deno.test("a record that is not a dialog request is not mistaken for one", () => {
  assertEquals(asDialogRequest({ type: "agent_settled" }), null);
  assertEquals(asDialogRequest({ type: "extension_ui_request", method: "select" }), null);
});

Deno.test("a tool call is named by the argument a reader would recognise", () => {
  assertEquals(toolTarget({ command: "ls -la" }), "ls -la");
  assertEquals(toolTarget({ file_path: "/workspace/main.ts" }), "/workspace/main.ts");
  assertEquals(toolTarget({ pattern: "TODO" }), "TODO");
  assertEquals(toolTarget({ unrelated: "x" }), undefined);
  assertEquals(toolTarget({ command: "   " }), undefined);
  assertEquals(toolTarget("not an object"), undefined);
});

Deno.test("the text of a message is its text parts, in order", () => {
  const text = messageText({
    content: [
      { type: "text", text: "first " },
      { type: "tool_use", id: "t1" },
      { type: "text", text: "second" },
    ],
  });

  assertEquals(text, "first second");
  assertEquals(messageText({ content: [] }), "");
  assertEquals(messageText(null), "");
});

Deno.test("thinking is read from the event, which arrives before any message", () => {
  assertEquals(startsThinking({ assistantMessageEvent: { type: "thinking_start" } }), true);
  assertEquals(startsThinking({ assistantMessageEvent: { type: "text_delta" } }), false);
  assertEquals(startsThinking({ type: "agent_settled" }), false);

  assertEquals(
    thinkingEnded({ assistantMessageEvent: { type: "thinking_end", content: "weighed it" } }),
    "weighed it",
  );
  assertEquals(thinkingEnded({ assistantMessageEvent: { type: "thinking_start" } }), undefined);
});

Deno.test("the role of a message is read when it has one", () => {
  assertEquals(messageRole({ role: "assistant" }), "assistant");
  assertEquals(messageRole({}), undefined);
  assertEquals(messageRole(undefined), undefined);
});
