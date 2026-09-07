/**
 * The local web interface.
 *
 * Serves the built assets and a small API over them. It holds no state of its
 * own: sessions, their output, and their history all live in the session
 * layer, and a browser is just another view attached to them.
 *
 * There is no authentication, deliberately. The bind address is the access
 * control, and it is checked before anything is served.
 */

import { join, normalize } from "@std/path";
import type { WebConfig } from "../config/schema.ts";
import type { Logger } from "../log.ts";
import { hostPathUnder } from "../sandbox/paths.ts";
import { readDirectory, readFileForDisplay } from "../session/files.ts";
import type { SessionManager } from "../session/manager.ts";
import type { SessionUsage } from "../session/port.ts";
import { Transcript, TRANSCRIPT_FILENAME } from "../session/transcript.ts";
import type { Recorded } from "../session/views.ts";
import { checkBindAddress } from "./address.ts";
import { type NameLookup, WebView, type WireEntry, withoutMentions } from "./view.ts";

/**
 * The identity a request from the interface acts under.
 *
 * Reaching the interface already means being on the operator's own network, so
 * a request carries operator authority. Giving it a name rather than borrowing
 * somebody's account id keeps that visible in a session's own records.
 */
export const WEB_ACTOR = "web-interface";

/** Raised when the interface cannot be served, with what to do about it. */
export class WebInterfaceError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WebInterfaceError";
  }
}

/** Content types for what the build produces. */
const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".svg": "image/svg+xml",
  ".json": "application/json; charset=utf-8",
  ".woff2": "font/woff2",
  ".map": "application/json; charset=utf-8",
};

function contentType(path: string): string {
  return TYPES[path.slice(path.lastIndexOf("."))] ?? "application/octet-stream";
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8" },
  });
}

function exists(path: string): boolean {
  try {
    Deno.statSync(path);
    return true;
  } catch {
    return false;
  }
}

/**
 * A session's name, with mentions rewritten as they are everywhere else.
 *
 * An opening prompt is a message, so it can carry the same markup any other
 * message can, and a list of `<@1523363748427993218>` names nothing.
 */
function named(opening: string | undefined, names?: NameLookup): string | undefined {
  if (opening === undefined) return undefined;
  const shown = withoutMentions(opening, names);
  return shown.length > 0 ? shown : undefined;
}

/** What the interface lists for each session. */
export interface SessionSummary {
  id: string;
  project: string;
  owner: string;
  busy: boolean;
  ended: boolean;
  /**
   * Whether a sandbox is running for it.
   *
   * A session that is not live is not finished: sending to it starts it again
   * with its history intact, which is how a thread begun in chat is picked up
   * in a browser.
   */
  live: boolean;
  /** What the session was first asked to do, when that was recorded. */
  opening: string | undefined;
  /** The chat thread showing the same session, for linking back to it. */
  threadId: string | undefined;
  startedAt: number;
  /** When it last did or was told anything. */
  lastActiveAt: number;
}

/** The interface, bound to one address. */
export class WebServer {
  private server: Deno.HttpServer | null = null;
  private readonly started = new Map<string, number>();
  /**
   * What each session was first asked, by session.
   *
   * Cached because the list is polled and an opening never changes, so reading
   * it again would mean opening a file per session per poll.
   */
  private readonly openings = new Map<string, string>();

  constructor(
    private readonly config: WebConfig,
    private readonly sessions: SessionManager,
    private readonly assets: string,
    private readonly log: Logger,
    /** The guild the served channel is in, so a session can link to its thread. */
    private readonly guildId?: string | undefined,
    /** Resolves an account id to a name, so mentions read as people. */
    private readonly names?: NameLookup | undefined,
  ) {}

  /** The address it is listening on, once started. */
  get url(): string {
    return `http://${this.config.host}:${this.config.port}`;
  }

  /**
   * Starts listening.
   *
   * @throws WebInterfaceError when the address is not private, or the
   *   interface has not been built. Neither is worth serving something broken
   *   over.
   */
  start(): void {
    const verdict = checkBindAddress(this.config.host);
    if (!verdict.allowed) {
      throw new WebInterfaceError(`refusing to serve the interface: ${verdict.reason}`);
    }

    if (!exists(join(this.assets, "index.html"))) {
      throw new WebInterfaceError(
        `the interface is not built. Run \`deno task build:web\` to produce ${this.assets}.`,
      );
    }

    this.server = Deno.serve({
      hostname: this.config.host,
      port: this.config.port,
      onListen: () => {
        this.log.info("web interface listening", {
          url: this.url,
          observer: this.config.observer,
        });
      },
    }, (request) => this.route(request));
  }

  /** Stops listening. */
  async stop(): Promise<void> {
    await this.server?.shutdown();
    this.server = null;
  }

  private route(request: Request): Promise<Response> | Response {
    const url = new URL(request.url);
    const path = url.pathname;

    if (path === "/api/interface") return this.describe();
    if (path === "/api/sessions" && request.method === "GET") return this.listSessions();
    if (path === "/api/sessions" && request.method === "POST") return this.startSession(request);

    const send = /^\/api\/sessions\/([^/]+)\/send$/.exec(path);
    if (send !== null && request.method === "POST") return this.send(send[1] as string, request);

    const stream = /^\/api\/sessions\/([^/]+)\/stream$/.exec(path);
    if (stream !== null) return this.streamSession(stream[1] as string);

    const transcript = /^\/api\/sessions\/([^/]+)\/transcript$/.exec(path);
    if (transcript !== null) return this.transcript(transcript[1] as string);

    const tree = /^\/api\/sessions\/([^/]+)\/tree$/.exec(path);
    if (tree !== null) return this.tree(tree[1] as string, url.searchParams.get("path") ?? "");

    const file = /^\/api\/sessions\/([^/]+)\/file$/.exec(path);
    if (file !== null) return this.file(file[1] as string, url.searchParams.get("path") ?? "");

    const download = /^\/api\/sessions\/([^/]+)\/download$/.exec(path);
    if (download !== null) {
      return this.download(download[1] as string, url.searchParams.get("path") ?? "");
    }

    if (path.startsWith("/api/")) return json({ error: "no such route" }, 404);

    return this.serveAsset(path);
  }

  /**
   * Refuses anything that would change something, when configured to observe.
   *
   * @returns the refusal, or undefined when the request may proceed.
   */
  private refuseIfObserving(): Response | undefined {
    if (!this.config.observer) return undefined;
    return json({ error: "the interface is an observer and cannot change anything" }, 403);
  }

  private async body(request: Request): Promise<Record<string, unknown> | null> {
    try {
      const parsed: unknown = await request.json();
      if (typeof parsed !== "object" || parsed === null) return null;
      return parsed as Record<string, unknown>;
    } catch {
      return null;
    }
  }

  /**
   * What the interface is allowed to do, so it can show only what works.
   *
   * An observer that still drew a composer would be offering something every
   * attempt at which is refused.
   */
  private describe(): Response {
    return json({ observer: this.config.observer, guildId: this.guildId ?? null });
  }

  private async startSession(request: Request): Promise<Response> {
    const refused = this.refuseIfObserving();
    if (refused !== undefined) return refused;

    const body = await this.body(request);
    const prompt = typeof body?.prompt === "string" ? body.prompt.trim() : "";
    const project = typeof body?.project === "string" ? body.project.trim() : "";
    if (prompt.length === 0) return json({ error: "a prompt is required" }, 400);

    const outcome = await this.sessions.startDetached({
      project,
      prompt,
      ownerId: WEB_ACTOR,
      ownerName: "the interface",
    });

    return outcome.status === "started"
      ? json({ id: outcome.session.id })
      : json({ error: outcome.reason }, 409);
  }

  private async send(id: string, request: Request): Promise<Response> {
    const refused = this.refuseIfObserving();
    if (refused !== undefined) return refused;

    const body = await this.body(request);
    const text = typeof body?.text === "string" ? body.text.trim() : "";
    if (text.length === 0) return json({ error: "nothing to send" }, 400);

    // The same path a message in a thread takes, so prompts queue and commands
    // apply exactly as they do there.
    const delivered = await this.sessions.deliverToSession(id, {
      id: `web-${Date.now()}`,
      authorId: WEB_ACTOR,
      authorName: "the interface",
      content: text,
    });

    return delivered
      ? json({ accepted: true, detail: "sent" })
      : json({ accepted: false, detail: "no such live session" }, 404);
  }

  private listSessions(): Response {
    const live: SessionSummary[] = this.sessions.sessions.map((session) => {
      if (!this.started.has(session.id)) this.started.set(session.id, Date.now());
      return {
        id: session.id,
        project: session.project.name,
        owner: session.ownerId,
        busy: session.isBusy,
        ended: session.isEnded,
        live: true,
        opening: named(session.opening, this.names),
        threadId: this.sessions.threadIdFor(session.id),
        lastActiveAt: session.lastActiveAt,
        startedAt: this.started.get(session.id) ?? Date.now(),
      };
    });

    // Threads whose sandbox has gone are still listed, because the agent's
    // history outlives it and sending to one picks the conversation back up.
    const resumable: SessionSummary[] = this.sessions.resumable().map((record) => ({
      id: record.sessionId,
      project: record.projectName,
      owner: record.ownerId,
      busy: false,
      ended: false,
      live: false,
      opening: named(this.openingOf(record.sessionId, record.stateDir), this.names),
      threadId: record.threadId,
      startedAt: record.updatedAt,
      lastActiveAt: record.updatedAt,
    }));

    return json([...live, ...resumable]);
  }

  /**
   * What a session said, for one that is not running.
   *
   * A live session is streamed instead, which replays from memory. This reads
   * the same output back from where it was written down.
   */
  private transcript(id: string): Response {
    const record = this.sessions.resumable().find((candidate) => candidate.sessionId === id);
    if (record === undefined) {
      // A live session has its history on its stream, so there is nothing to
      // read back for it.
      if (this.sessions.forSession(id) !== undefined) return json({ entries: [], dropped: 0 });
      return json({ error: "no such session" }, 404);
    }

    const stored = new Transcript(join(record.stateDir, TRANSCRIPT_FILENAME), this.log).read();

    // The turn is carried on each entry rather than announced between them,
    // because a stored transcript is read in one piece rather than streamed.
    const entries: WireEntry[] = [];
    let usage: SessionUsage | undefined;
    for (const held of stored.entries) {
      // Usage is state rather than an item in the conversation, so only the
      // latest is reported, through the same field a live session uses. A
      // stopped session has no other way to say what it cost.
      if (held.entry.call === "usage") {
        usage = held.entry.usage;
        continue;
      }
      entries.push(this.wire(held.entry, held.at, held.turn));
    }

    // A session recorded before turns were kept has none to report, which the
    // interface shows ungrouped rather than guessing at.
    const grouped = stored.entries.some((held) => held.turn !== undefined);

    return json({
      entries,
      dropped: stored.dropped,
      grouped,
      ...(usage === undefined ? {} : { usage }),
    });
  }

  /** One recorded entry as the browser reads it. */
  private wire(
    entry: Exclude<Recorded, { call: "usage" }>,
    at: number,
    turn: number | undefined,
  ): WireEntry {
    const held = turn === undefined ? {} : { turn };
    const clean = (text: string): string => withoutMentions(text, this.names);

    switch (entry.call) {
      case "post":
        return { ...held, kind: "message", text: clean(entry.text), at };
      case "prompt":
        return { ...held, kind: "prompt", author: entry.author, text: clean(entry.text), at };
      case "aside":
        return { ...held, kind: "aside", author: entry.author, text: clean(entry.text), at };
      case "notice":
        return { ...held, kind: "notice", text: clean(entry.text), level: entry.level, at };
      case "thinking":
        return { ...held, kind: "thinking", text: entry.text, at };
      case "reply":
        return { ...held, kind: "reply", text: clean(entry.text), command: entry.command, at };
      case "toolResult":
        return { ...held, kind: "toolResult", result: entry.result, at };
      case "activity":
        return {
          ...held,
          kind: "activity",
          line: entry.line,
          at,
          ...(entry.tool === undefined ? {} : { tool: entry.tool }),
        };
      case "delegation":
        return { ...held, kind: "delegation", delegated: entry.delegated, at };
      case "diff":
        return {
          ...held,
          kind: "diff",
          path: entry.path,
          added: entry.added,
          removed: entry.removed,
          body: entry.body,
          at,
          ...(entry.cause === undefined ? {} : { cause: entry.cause }),
        };
      default:
        return { ...held, kind: "file", name: entry.name, size: entry.size, at };
    }
  }

  /** What a stopped session was asked, read from its transcript once. */
  private openingOf(sessionId: string, stateDir: string): string | undefined {
    const known = this.openings.get(sessionId);
    if (known !== undefined) return known;

    const found = new Transcript(join(stateDir, TRANSCRIPT_FILENAME), this.log).opening();
    if (found !== undefined && found.length > 0) this.openings.set(sessionId, found);
    return found;
  }

  private async streamSession(id: string): Promise<Response> {
    const view = new WebView(this.names);
    const detach = await this.sessions.attachView(id, view);

    if (detach === undefined) {
      view.stop();
      return json({ error: "no such live session" }, 404);
    }

    // The browser closing cancels the stream, which is the only signal that a
    // view has gone. Detaching then keeps the fan out from growing forever.
    void (async () => {
      while (!view.isClosed) {
        await new Promise((resolve) => setTimeout(resolve, 1_000));
      }
      detach();
    })();

    return new Response(view.body, {
      headers: {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        connection: "keep-alive",
      },
    });
  }

  /**
   * Resolves a project-relative path for a session.
   *
   * Goes through the session so the containment is the one the sandbox
   * applies, rather than a second implementation that could be looser.
   */
  private locate(id: string, requested: string): { host: string; relative: string } | Response {
    const relative = requested.trim().length === 0 ? "." : requested.trim();
    const session = this.sessions.forSession(id);

    // A session that has stopped keeps its project, so its files stay readable
    // without having to start a sandbox just to look at them.
    const record = session === undefined
      ? this.sessions.resumable().find((candidate) => candidate.sessionId === id)
      : undefined;

    if (session === undefined && record === undefined) {
      return json({ error: "no such session" }, 404);
    }

    const host = session !== undefined
      ? session.resolveInProject(relative)
      : record === undefined
      ? undefined
      : hostPathUnder(record.projectPath, record.projectPath, relative);

    if (host === undefined) return json({ error: "outside this session's project" }, 403);
    return { host, relative: relative === "." ? "" : relative };
  }

  private tree(id: string, requested: string): Response {
    const located = this.locate(id, requested);
    if (located instanceof Response) return located;

    try {
      if (!Deno.statSync(located.host).isDirectory) return json({ error: "not a directory" }, 400);
      return json(readDirectory(located.host, located.relative));
    } catch (error) {
      return json({ error: String(error) }, 404);
    }
  }

  private file(id: string, requested: string): Response {
    const located = this.locate(id, requested);
    if (located instanceof Response) return located;

    try {
      if (Deno.statSync(located.host).isDirectory) return json({ error: "is a directory" }, 400);
      return json(readFileForDisplay(located.host, located.relative));
    } catch (error) {
      return json({ error: String(error) }, 404);
    }
  }

  private async download(id: string, requested: string): Promise<Response> {
    const located = this.locate(id, requested);
    if (located instanceof Response) return located;

    let file: Deno.FsFile;
    try {
      file = await Deno.open(located.host, { read: true });
    } catch {
      return json({ error: "no such file" }, 404);
    }

    const name = located.relative.split("/").pop() ?? "file";
    return new Response(file.readable, {
      headers: {
        "content-type": "application/octet-stream",
        "content-disposition": `attachment; filename="${name.replace(/"/g, "")}"`,
      },
    });
  }

  private async serveAsset(path: string): Promise<Response> {
    const wanted = path === "/" ? "/index.html" : path;

    // Normalised and then checked, so a traversal in the request cannot reach
    // outside the built assets.
    const resolved = join(this.assets, normalize(wanted));
    if (!resolved.startsWith(this.assets)) return new Response("not found", { status: 404 });

    try {
      const file = await Deno.open(resolved, { read: true });
      return new Response(file.readable, {
        headers: { "content-type": contentType(resolved) },
      });
    } catch {
      // Anything unrecognised is the interface itself, so a reload of a deep
      // path still lands on the application rather than on a blank 404.
      const index = await Deno.open(join(this.assets, "index.html"), { read: true });
      return new Response(index.readable, {
        headers: { "content-type": TYPES[".html"] as string },
      });
    }
  }
}
