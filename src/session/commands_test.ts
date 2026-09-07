import { assertEquals, assertStringIncludes } from "@std/assert";
import {
  answerWithoutSession,
  asksForPullRequest,
  type CommandAccess,
  COMMANDS,
  helpText,
  isAddressedToBot,
  isAside,
  isCommand,
  mayRun,
  parseUserId,
} from "./commands.ts";

const OWNER = { isOwner: true, isGuest: false };
const GUEST = { isOwner: false, isGuest: true };
const STRANGER = { isOwner: false, isGuest: false };

Deno.test("a command is named by its whole first word", () => {
  assertEquals(isCommand("!status"), true);
  assertEquals(isCommand("  !ls src  "), true);
  assertEquals(isCommand("!statusreport"), false);
  assertEquals(isCommand("please !status"), false);
  assertEquals(isCommand("write a !status handler"), false);
});

/** An unknown `!` word belongs to another bot, and is not worth a model call. */
Deno.test("anything starting with a bang is addressed to a bot, known or not", () => {
  assertEquals(isAddressedToBot("!somebodyelses thing"), true);
  assertEquals(isAddressedToBot("  !status"), true);
  assertEquals(isAddressedToBot("what does ! mean"), false);
});

Deno.test("an aside is marked three times so it cannot be typed by accident", () => {
  assertEquals(isAside("!!! ignore this"), true);
  assertEquals(isAside("!! nearly"), false);
  assertEquals(isAside("!status"), false);
});

/** An aside must not be answered as a command, whatever follows the marks. */
Deno.test("an aside is never a command", () => {
  assertEquals(isCommand("!!! !stop"), false);
});

Deno.test("only the owner may steer or end the session", () => {
  for (const standing of [GUEST, STRANGER]) {
    assertEquals(mayRun("owner", standing), false);
  }
  assertEquals(mayRun("owner", OWNER), true);
});

Deno.test("an invited guest may read the project, a stranger may not", () => {
  assertEquals(mayRun("guest", GUEST), true);
  assertEquals(mayRun("guest", OWNER), true);
  assertEquals(mayRun("guest", STRANGER), false);
});

Deno.test("what is harmless is open to anyone permitted to be here", () => {
  assertEquals(mayRun("anyone", STRANGER), true);
});

/** Powering off the machine is not a session's to grant, not even the owner's. */
Deno.test("no session role turns the host off", () => {
  for (const standing of [OWNER, GUEST, STRANGER]) {
    assertEquals(mayRun("host", standing), false);
  }
});

/** A command added later must state who may run it, not inherit "anyone". */
Deno.test("every command declares its access and its group", () => {
  const levels: CommandAccess[] = ["anyone", "guest", "owner", "host"];
  for (const [name, meta] of Object.entries(COMMANDS)) {
    assertEquals(name.startsWith("!"), true, name);
    assertEquals(levels.includes(meta.access), true, name);
    assertEquals(meta.summary.length > 0, true, name);
  }
});

Deno.test("an account is named by a mention or by its bare id", () => {
  assertEquals(parseUserId("<@123456789>"), "123456789");
  assertEquals(parseUserId(" <@!123456789> "), "123456789");
  assertEquals(parseUserId("123456789"), "123456789");
});

/** Guessing at an id means `!deny` withdrawing somebody who was never named. */
Deno.test("anything that is not an id is refused rather than guessed at", () => {
  for (const text of ["", "amelia", "<@abc>", "12", "<@123456789> and friends"]) {
    assertEquals(parseUserId(text), undefined, text);
  }
});

Deno.test("asking for a pull request is recognised however it is spelled", () => {
  for (
    const text of ["open a PR", "raise a pull request", "send pull-requests", "a merge request"]
  ) {
    assertEquals(asksForPullRequest(text), true, text);
  }
});

/** The gate only has to catch the ask; it must not fire on ordinary work. */
Deno.test("ordinary work is not read as asking to publish", () => {
  for (const text of ["prepare the branch", "review the diff", "push to my fork"]) {
    assertEquals(asksForPullRequest(text), false, text);
  }
});

Deno.test("help is answered without starting a session, and nothing else is", () => {
  const answer = answerWithoutSession("!help");
  assertStringIncludes(answer ?? "", "!stop");
  assertStringIncludes(answer ?? "", "!ls");
  assertEquals(answerWithoutSession("!ls src"), undefined);
  assertEquals(answerWithoutSession("write me a parser"), undefined);
});

Deno.test("help groups the commands and says which are not open to all", () => {
  const text = helpText();

  assertStringIncludes(text, "THE SESSION");
  assertStringIncludes(text, "WHO TAKES PART");
  assertStringIncludes(text, "!steer <instruction>");
  assertStringIncludes(text, "(owner)");
  assertEquals((text.match(/```/g) ?? []).length, 2);
});

/** A summary that ran past its column would break the alignment for every row. */
Deno.test("the command column is one width for every line", () => {
  const body = helpText().split("```")[1] ?? "";
  const summaryAt = body.split("\n")
    .filter((line) => line.startsWith("  !"))
    .map((line) => /^ {2}\S+(?: \S+)? +/.exec(line)?.[0].length);

  assertEquals(new Set(summaryAt).size, 1);
});
