import { assertEquals, assertRejects, assertStringIncludes } from "@std/assert";
import { createLogger } from "../log.ts";
import { AgentClient, type AgentHandlers, type AgentProcess } from "./client.ts";

const encoder = new TextEncoder();

/** A process a test writes records into, standing in for the agent. */
function fakeProcess() {
  let push: (record: unknown) => void = () => {};
  let raw: (text: string) => void = () => {};
  let finish: () => void = () => {};
  const written: string[] = [];
  let exit: (code: number) => void = () => {};

  const stdout = new ReadableStream<Uint8Array>({
    start(controller) {
      push = (record) => controller.enqueue(encoder.encode(`${JSON.stringify(record)}\n`));
      raw = (text) => controller.enqueue(encoder.encode(text));
      finish = () => {
        try {
          controller.close();
        } catch {
          // Already closed by an earlier finish, which tests do freely.
        }
      };
    },
  });
  const stderr = new ReadableStream<Uint8Array>({ start: (controller) => controller.close() });
  const exited = new Promise<number>((resolve) => {
    exit = resolve;
  });

  const process: AgentProcess = {
    stdout,
    stderr,
    exited,
    write(bytes) {
      written.push(new TextDecoder().decode(bytes).trim());
    },
  };

  return {
    process,
    written,
    send: (record: unknown) => push(record),
    /** Bytes exactly as given, so a test can send something malformed. */
    chunk: (text: string) => raw(text),
    close: (code = 0) => {
      finish();
      exit(code);
    },
  };
}

function client(handlers: AgentHandlers = {}, dialogTimeoutMs = 300_000) {
  const fake = fakeProcess();
  const agent = new AgentClient(
    fake.process,
    handlers,
    createLogger({}, () => {}),
    dialogTimeoutMs,
  );
  const running = agent.run();
  return { agent, fake, running };
}

/** Lets the stream reader drain what a test just pushed. */
const settle = (): Promise<void> => new Promise((resolve) => setTimeout(resolve, 5));

Deno.test("readiness is the agent answering, and it carries the context window", async () => {
  const { agent, fake, running } = client();

  const ready = agent.waitUntilReady(1_000);
  await settle();
  const sent = JSON.parse(fake.written[0] ?? "{}");
  assertEquals(sent.type, "get_state");

  fake.send({ type: "response", id: sent.id, data: { model: { contextWindow: 200_000 } } });
  await ready;

  assertEquals(agent.contextWindow, 200_000);
  assertEquals(agent.state, "ready");
  fake.close();
  await running;
});

Deno.test("a turn moves the agent through working and back to ready", async () => {
  const seen: string[] = [];
  const { agent, fake, running } = client({
    onTurnStart: () => seen.push("start"),
    onTurnSettled: (produced) => seen.push(`settled:${produced}`),
  });

  fake.send({ type: "agent_start" });
  await settle();
  assertEquals(agent.state, "working");
  assertEquals(agent.isWorking, true);

  fake.send({
    type: "message_end",
    message: { role: "assistant", content: [{ type: "text", text: "done" }] },
  });
  fake.send({ type: "agent_settled" });
  await settle();

  assertEquals(agent.state, "ready");
  assertEquals(seen, ["start", "settled:true"]);
  fake.close();
  await running;
});

/**
 * The caller has to release what it was holding for a turn that will never
 * settle, so it is told whether one was running.
 */
Deno.test("an exit during a turn says so, and an idle exit does not", async () => {
  const during: boolean[] = [];
  const first = client({ onExit: (_code, duringTurn) => during.push(duringTurn) });
  first.fake.send({ type: "agent_start" });
  await settle();
  first.fake.close(1);
  await first.running;

  const second = client({ onExit: (_code, duringTurn) => during.push(duringTurn) });
  second.fake.close(0);
  await second.running;

  assertEquals(during, [true, false]);
});

Deno.test("the exit is reported once, with the code", async () => {
  const codes: number[] = [];
  const { fake, running } = client({ onExit: (code) => codes.push(code) });

  fake.close(3);
  await running;

  assertEquals(codes, [3]);
});

Deno.test("commands are dropped once the agent has ended", async () => {
  const { agent, fake, running } = client();
  fake.close();
  await running;

  assertEquals(agent.prompt("anything"), false);
  assertEquals(agent.isAlive, false);
});

Deno.test("only the assistant's words are reported, not the prompt echoed back", async () => {
  const said: string[] = [];
  const { fake, running } = client({ onAssistantText: (text) => said.push(text) });

  fake.send({
    type: "message_end",
    message: { role: "user", content: [{ type: "text", text: "the prompt" }] },
  });
  fake.send({
    type: "message_end",
    message: { role: "assistant", content: [{ type: "text", text: "  the answer  " }] },
  });
  await settle();

  assertEquals(said, ["the answer"]);
  fake.close();
  await running;
});

Deno.test("thinking is reported once a turn, and each finished thought once", async () => {
  const seen: string[] = [];
  const { fake, running } = client({
    onThinking: () => seen.push("thinking"),
    onThought: (text) => seen.push(`thought:${text}`),
  });

  fake.send({ type: "agent_start" });
  fake.send({ type: "message_update", assistantMessageEvent: { type: "thinking_start" } });
  fake.send({ type: "message_update", assistantMessageEvent: { type: "thinking_start" } });
  fake.send({
    type: "message_update",
    assistantMessageEvent: { type: "thinking_end", content: "weighed it" },
  });
  await settle();

  assertEquals(seen, ["thinking", "thought:weighed it"]);
  fake.close();
  await running;
});

Deno.test("tool calls report their target and whether they failed", async () => {
  const seen: string[] = [];
  const { fake, running } = client({
    onToolStart: (id, name, target) => seen.push(`start:${id}:${name}:${target}`),
    onToolEnd: (id, _name, failed, output) => seen.push(`end:${id}:${failed}:${output}`),
  });

  fake.send({
    type: "tool_execution_start",
    toolCallId: "t1",
    toolName: "bash",
    args: { command: "ls -la" },
  });
  fake.send({
    type: "tool_execution_end",
    toolCallId: "t1",
    toolName: "bash",
    isError: true,
    result: { content: [{ type: "text", text: "no such file" }] },
  });
  await settle();

  assertEquals(seen, ["start:t1:bash:ls -la", "end:t1:true:no such file"]);
  fake.close();
  await running;
});

Deno.test("usage is reported when a turn ends", async () => {
  const models: string[] = [];
  const { fake, running } = client({ onUsage: (usage) => models.push(usage.model ?? "") });

  fake.send({
    type: "turn_end",
    message: { model: "cheap-model", usage: { input: 10, output: 2, totalTokens: 12 } },
  });
  await settle();

  assertEquals(models, ["cheap-model"]);
  fake.close();
  await running;
});

Deno.test("a select dialog is answered by number or by name", async () => {
  const { agent, fake, running } = client();
  fake.send({
    type: "extension_ui_request",
    id: "d-1",
    method: "select",
    title: "Which branch?",
    options: ["main", "dev"],
  });
  await settle();

  assertEquals(agent.pendingDialog?.title, "Which branch?");
  assertEquals(agent.answerDialog("d-1", "2"), "accepted");
  assertEquals(JSON.parse(fake.written.at(-1) ?? "{}").value, "dev");

  fake.send({
    type: "extension_ui_request",
    id: "d-2",
    method: "select",
    title: "Again?",
    options: ["main", "dev"],
  });
  await settle();
  assertEquals(agent.answerDialog("d-2", "MAIN"), "accepted");
  assertEquals(JSON.parse(fake.written.at(-1) ?? "{}").value, "main");

  fake.close();
  await running;
});

Deno.test("a reply that answers nothing leaves the dialog standing", async () => {
  const { agent, fake, running } = client();
  fake.send({ type: "extension_ui_request", id: "d-1", method: "confirm", title: "Proceed?" });
  await settle();

  assertEquals(agent.answerDialog("d-1", "maybe later"), "unrecognized");
  assertEquals(agent.pendingDialog?.id, "d-1");
  assertEquals(agent.answerDialog("d-9", "yes"), "unknown");
  assertEquals(agent.answerDialog("d-1", "yes"), "accepted");
  assertEquals(JSON.parse(fake.written.at(-1) ?? "{}").confirmed, true);

  fake.close();
  await running;
});

/** A chat thread cannot serve an editor, so it is cancelled rather than hung on. */
Deno.test("an editor request is cancelled immediately", async () => {
  const unsupported: string[] = [];
  const { agent, fake, running } = client({ onUnsupportedDialog: (m) => unsupported.push(m) });

  fake.send({ type: "extension_ui_request", id: "d-1", method: "editor", title: "Edit" });
  await settle();

  assertEquals(unsupported, ["editor"]);
  assertEquals(agent.pendingDialog, undefined);
  assertEquals(JSON.parse(fake.written.at(-1) ?? "{}").cancelled, true);

  fake.close();
  await running;
});

Deno.test("a dialog nobody answers is cancelled, and the caller told", async () => {
  const timedOut: string[] = [];
  const { agent, fake, running } = client({ onDialogTimeout: (r) => timedOut.push(r.id) }, 10);

  fake.send({ type: "extension_ui_request", id: "d-1", method: "confirm", title: "Proceed?" });
  await settle();
  await new Promise((resolve) => setTimeout(resolve, 30));

  assertEquals(timedOut, ["d-1"]);
  assertEquals(agent.pendingDialog, undefined);

  fake.close();
  await running;
});

Deno.test("an informational request is neither answered nor reported", async () => {
  const seen: string[] = [];
  const { agent, fake, running } = client({ onDialog: (r) => seen.push(r.id) });

  fake.send({ type: "extension_ui_request", id: "n-1", method: "notify", title: "hello" });
  await settle();

  assertEquals(seen, []);
  assertEquals(agent.pendingDialog, undefined);
  assertEquals(fake.written.length, 0);

  fake.close();
  await running;
});

Deno.test("a command the agent refuses is reported, id or no id", async () => {
  const rejected: string[] = [];
  const { fake, running } = client({
    onCommandRejected: (command, detail) => rejected.push(`${command}:${detail}`),
  });

  fake.send({ type: "response", command: "prompt", success: false, error: "no api key" });
  await settle();

  assertEquals(rejected, ["prompt:no api key"]);
  fake.close();
  await running;
});

Deno.test("a request resolves on its answer and rejects on its deadline", async () => {
  const { agent, fake, running } = client();

  const answered = agent.request({ type: "compact" }, 1_000);
  await settle();
  const id = JSON.parse(fake.written.at(-1) ?? "{}").id;
  fake.send({ type: "response", id, data: { freed: 1_200 } });
  assertEquals(((await answered) as { data: { freed: number } }).data.freed, 1_200);

  await assertRejects(
    () => agent.request({ type: "compact" }, 10),
    Error,
    "did not answer",
  );

  fake.close();
  await running;
});

Deno.test("a request outstanding when the agent dies is failed, not left hanging", async () => {
  const { agent, fake, running } = client();
  const pending = agent.request({ type: "compact" }, 60_000);
  await settle();

  fake.close(1);
  await running;

  await assertRejects(() => pending, Error, "exited with code 1");
});

Deno.test("an unparseable line is skipped rather than ending the session", async () => {
  const said: string[] = [];
  const { agent, fake, running } = client({ onAssistantText: (t) => said.push(t) });

  fake.chunk("this is not json at all\n");
  fake.chunk('{"type":"message_end",\n');
  fake.send({
    type: "message_end",
    message: { role: "assistant", content: [{ type: "text", text: "after" }] },
  });
  await settle();

  assertEquals(said, ["after"]);
  assertEquals(agent.isAlive, true);
  fake.close();
  await running;
});

Deno.test("a record that never terminates ends the session loudly", async () => {
  const violations: string[] = [];
  const fake = fakeProcess();
  const agent = new AgentClient(
    fake.process,
    { onProtocolViolation: (detail) => violations.push(detail) },
    createLogger({}, () => {}),
    300_000,
    64,
  );
  const running = agent.run();

  // No line feed, so the framer holds it and it grows past what it will hold.
  fake.chunk("x".repeat(200));
  await settle();

  assertEquals(violations.length, 1);
  assertStringIncludes(violations[0] ?? "", "without a line feed");
  assertEquals(agent.isAlive, false);

  fake.close();
  await running;
});
