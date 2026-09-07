/** Shapes the daemon sends, matching src/web/view.ts and src/web/server.ts. */

/** What a tool was doing, so the interface can lay it out itself. */
export interface ToolActivity {
  /** The agent's identifier for the call, so its result can find it. */
  id?: string;
  name: string;
  target?: string;
  failed?: boolean;
}

/** What a tool produced, already truncated by the daemon. */
export interface ToolResult {
  id: string;
  name: string;
  failed: boolean;
  output: string;
}

/** What kind of thing the daemon is reporting about itself. */
export type NoticeLevel = "started" | "warning" | "done" | "ended";

/** What a session has cost so far. */
export interface Usage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  totalTokens: number;
  cost: number;
  contextTokens: number;
  /** How much the model can hold, when the agent has said. */
  contextWindow?: number;
  turns: number;
  model?: string;
}

/** One thing a session reported, as it arrives. */
export type Entry =
  & { turn?: number }
  & (
    | { kind: "message"; text: string; at: number }
    | { kind: "prompt"; author: string; text: string; at: number }
    | { kind: "aside"; author: string; text: string; at: number }
    | { kind: "activity"; line: string; tool?: ToolActivity; at: number }
    | { kind: "notice"; text: string; level: NoticeLevel; at: number }
    | { kind: "thinking"; text: string; at: number }
    | { kind: "reply"; text: string; command: string; at: number }
    | { kind: "toolResult"; result: ToolResult; at: number }
    | { kind: "file"; name: string; size: number; at: number }
    | {
      kind: "diff";
      path: string;
      added: number;
      removed: number;
      body: string;
      /** The tool call that made the change, absent in an older recording. */
      cause?: string;
      at: number;
    }
  );

/** A change to how a session should be shown. Every field is optional. */
export interface State {
  busy?: boolean;
  waiting?: string | null;
  ended?: boolean;
  usage?: Usage;
}

/** A session as the interface lists it. */
export interface SessionSummary {
  id: string;
  project: string;
  owner: string;
  busy: boolean;
  ended: boolean;
  /** Whether a sandbox is running. A session that is not live can be resumed. */
  live: boolean;
  /** What it was first asked to do, when that was recorded. */
  opening?: string;
  /** The chat thread showing the same session, when there is one. */
  threadId?: string;
  startedAt: number;
  /** When it last did or was told anything. */
  lastActiveAt: number;
}

/** What the interface may do, as the daemon reports it. */
export interface InterfaceInfo {
  /** True when nothing may be changed from here. */
  observer: boolean;
  /** The guild the channel is in, for linking to a thread. */
  guildId: string | null;
}

/** A node in a project's file tree. */
export interface TreeNode {
  name: string;
  path: string;
  directory: boolean;
  size: number;
}

/** A file as the viewer receives it, already truncated by the daemon. */
export interface FileContents {
  path: string;
  size: number;
  binary: boolean;
  truncated: boolean;
  text: string;
  /** What to read it as. Empty when the daemon does not know the extension. */
  language?: string;
}
