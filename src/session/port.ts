/**
 * What a session says, and the shape anything showing it must implement.
 *
 * Kept apart from the session itself so that a surface which renders a session
 * depends on this alone. A chat thread, a stored transcript, and a browser are
 * all the same interface with different ideas of what is worth showing.
 *
 * Every method is about what happened, not about how to draw it. Where a
 * surface needs the parts rather than a rendered line, both are passed, so
 * neither has to unpick the other's text.
 */

/** What a session has cost so far. */
export interface SessionUsage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  totalTokens: number;
  cost: number;
  /** Tokens in the most recent request, which is the context it carries. */
  contextTokens: number;
  /**
   * How much context the model holds, when the agent has said.
   *
   * Without it `contextTokens` is a number with nothing to measure it against.
   */
  contextWindow?: number | undefined;
  /** Turns taken so far. */
  turns: number;
  /** The model that answered, when the agent named it. */
  model?: string | undefined;
}

/** What kind of thing the daemon is reporting about itself. */
export type NoticeLevel = "started" | "warning" | "done" | "ended";

/** What a tool was doing, for a surface that renders it itself. */
export interface ToolActivity {
  /** The agent's own identifier for the call, so a result can find it. */
  id?: string | undefined;
  name: string;
  target?: string | undefined;
  failed?: boolean;
}

/** What a tool produced. */
export interface ToolResult {
  id: string;
  name: string;
  failed: boolean;
  /** Already truncated to the configured limit. */
  output: string;
}

/**
 * What came of asking a cheaper model about one artefact.
 *
 * Carried as its parts rather than as a rendered line, because a thread shows
 * one line and an interface shows the question, the answer, and what it saved.
 */
export interface Delegated {
  /** What the session's model wanted to know. */
  question: string;
  /** The model that answered, or undefined when none was asked. */
  model?: string | undefined;
  /** What it was shown, for attributing the answer. */
  describes?: string | undefined;
  /** What it said, absent when it was refused. */
  answer?: string | undefined;
  /** Why nothing was asked, absent when something was. */
  refused?: string | undefined;
  /** Tokens the delegated model was charged, when the provider said. */
  tokens?: number | undefined;
  /** Characters kept out of the session's own context by asking. */
  keptOut?: number | undefined;
}

/** The four states a sender's message can end in. */
export type ReactionOutcome = "accepted" | "succeeded" | "failed" | "interrupted";

/** Why a session ended, for the final message and the log. */
export type EndReason =
  | "stopped"
  /**
   * Ended because the agent would not answer an interruption.
   *
   * Apart from a deliberate stop, because it is not one: the thread is left
   * open and its record kept, so what was being worked on can be picked up
   * again rather than being lost to a turn that would not let go.
   */
  | "unresponsive"
  | "idle"
  | "crashed"
  | "resource limit"
  | "startup failed"
  | "shutdown"
  | "thread archived"
  | "protocol violation";

/** One surface showing one session. */
export interface ThreadPort {
  /** Posts a message, splitting it if needed. */
  post(text: string): Promise<void>;
  /**
   * Reports something the daemon did, rather than something the agent said.
   *
   * A thread has one voice and shows these as ordinary messages. Anywhere else
   * they are the frame around the conversation rather than part of it, so they
   * are marked as what they are instead of arriving as the agent talking.
   */
  postNotice(text: string, level: NoticeLevel): Promise<void>;
  /**
   * Answers a command.
   *
   * A reply belongs to the command that asked for it rather than to the
   * conversation, so a surface can attach it to that instead of showing it as
   * the agent having spoken.
   */
  postReply(text: string, command: string): Promise<void>;
  /**
   * Reports that a cheaper model was asked about something.
   *
   * Shown wherever the conversation is read, so a delegated answer can never
   * be mistaken for the session model's own words.
   */
  noteDelegation(delegated: Delegated): void;
  /**
   * Reports what a tool produced, once it has finished.
   *
   * Separate from the call itself, so a surface can attach the result to it
   * rather than showing the two as unrelated events.
   */
  noteToolResult(result: ToolResult): void;
  /**
   * Opens a turn, which everything reported after it belongs to.
   *
   * A thread shows a conversation in order and has no use for the boundary. A
   * surface that groups what it shows needs to be told where one is.
   */
  beginTurn(turn: number): void;
  /**
   * Notes what the agent reasoned before answering.
   *
   * A thread does not show this: it is long, and a conversation is not the
   * place for it. A surface that can fold it away shows it.
   */
  noteThinking(text: string): void;
  /**
   * Notes that somebody asked for something.
   *
   * A chat thread already holds the message that started a turn, so it does
   * nothing with this. Every other surface has no such copy, and without it
   * would show the agent talking to itself.
   */
  notePrompt(author: string, text: string, id?: string): Promise<void>;
  /**
   * Notes something said to the people in the thread, not to the agent.
   *
   * A thread already holds the message and nobody there can mistake it for the
   * agent having been told. Anywhere else it has to be marked, or reading a
   * session back would show the agent being told something it never heard.
   */
  noteAside(author: string, text: string, id?: string): Promise<void>;
  /**
   * Adds a line of tool activity, extending the current block when there is
   * one.
   *
   * The rendered line is what a thread shows. The parts are passed alongside
   * it so another surface can lay them out itself rather than unpicking text.
   */
  appendActivity(line: string, tool?: ToolActivity): Promise<void>;
  /**
   * Reports what an edit changed.
   *
   * Passed as its parts rather than as rendered text, so a thread can render a
   * fenced block and an interface a real diff, from the same report.
   */
  postDiff(
    path: string,
    added: number,
    removed: number,
    body: string,
    call?: string,
  ): Promise<void>;
  /** Creates or updates the single message reporting queue position. */
  setWaiting(text: string | null): Promise<void>;
  /** Sets the outcome reaction on a message, replacing any earlier one. */
  setReaction(messageId: string, outcome: ReactionOutcome): Promise<void>;
  /**
   * Reports what the session has cost so far.
   *
   * State rather than history: only the current total is meaningful, so a
   * surface shows the latest instead of every step towards it.
   */
  setUsage(usage: SessionUsage): void;
  /**
   * Marks the thread as having work in progress, so a client can show that the
   * agent is working for as long as it actually is.
   */
  setBusy(busy: boolean): void;
  /** Uploads a file so it can be read or downloaded. */
  upload(name: string, bytes: Uint8Array, caption: string): Promise<void>;
  /**
   * Reports that the session has ended, and why.
   *
   * The reason decides what a chat thread does with itself. Somebody typing
   * `!stop` is finished with it, so it is archived. Anything else, an idle
   * timeout or a crash or a restart, leaves it open: those sessions can be
   * resumed, and a thread archived out of the sidebar is one its own author
   * has to go hunting for.
   */
  close(reason: EndReason): Promise<void>;
}
