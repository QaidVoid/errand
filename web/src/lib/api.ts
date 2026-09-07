/**
 * The daemon's HTTP surface.
 *
 * Every call is a plain fetch and every stream is server-sent events, so there
 * is no client library to keep in step with the server.
 */

import type {
  Entry,
  FileContents,
  InterfaceInfo,
  SessionSummary,
  State,
  TreeNode,
  Usage,
} from "./types.ts";

async function json<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api${path}`, init);
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error ?? `${response.status}`);
  }
  return (await response.json()) as T;
}

function send(path: string, body: unknown): Promise<{ id?: string }> {
  return json(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

export const api = {
  describe: () => json<InterfaceInfo>("/interface"),

  sessions: () => json<SessionSummary[]>("/sessions"),

  /** What a stopped session said. A live one is streamed instead. */
  transcript: (session: string) =>
    json<{ entries: Entry[]; dropped: number; usage?: Usage }>(`/sessions/${session}/transcript`),

  tree: (session: string, path: string) =>
    json<TreeNode[]>(`/sessions/${session}/tree?path=${encodeURIComponent(path)}`),

  file: (session: string, path: string) =>
    json<FileContents>(`/sessions/${session}/file?path=${encodeURIComponent(path)}`),

  downloadUrl: (session: string, path: string) =>
    `/api/sessions/${session}/download?path=${encodeURIComponent(path)}`,

  prompt: (session: string, text: string) => send(`/sessions/${session}/send`, { text }),

  start: (project: string, prompt: string) => send("/sessions", { project, prompt }),
};

/** What a subscriber is told, as the stream reports it. */
export interface StreamHandlers {
  /** Everything that follows is the session's record from its beginning. */
  reset: () => void;
  entry: (entry: Entry) => void;
  state: (state: State) => void;
  /** Whether the stream is currently connected, for showing a dropped one. */
  connected: (connected: boolean) => void;
}

/**
 * Streams a session until the returned function is called.
 *
 * A dropped stream is reconnected by the browser and the daemon replays the
 * session from the start, so the reset event is what keeps a reconnect from
 * showing everything twice. There is no recovery logic beyond honouring it.
 */
export function streamSession(id: string, handlers: StreamHandlers): () => void {
  const source = new EventSource(`/api/sessions/${id}/stream`);

  const parse = <T>(event: Event): T => JSON.parse((event as MessageEvent<string>).data) as T;

  source.addEventListener("reset", () => {
    handlers.connected(true);
    handlers.reset();
  });
  source.addEventListener("entry", (event) => handlers.entry(parse<Entry>(event)));
  source.addEventListener("state", (event) => handlers.state(parse<State>(event)));
  source.addEventListener("error", () => handlers.connected(false));

  return () => source.close();
}
