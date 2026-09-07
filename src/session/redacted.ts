/**
 * Scrubs the provider credential out of everything a session reports.
 *
 * The agent authenticates with a key held in its environment, so the key is
 * reachable by the agent by necessity. Anything the agent then prints is
 * forwarded: a tool that dumps the environment, a library that puts a request
 * header in an error, a config file it decides to read out. That output goes
 * to a chat channel, to the web interface, and to the transcript on disk, and
 * a credential posted into a channel has left the machine for good.
 *
 * Wrapping the fan-out rather than each view is what makes this hold: every
 * surface is behind it, including the transcript, so there is no path that
 * reports something the others scrubbed.
 *
 * It replaces the value verbatim. An agent that splits or re-encodes the key
 * defeats it, which is why this is damage control on an unavoidable exposure
 * rather than a boundary.
 */

import { redactText } from "../config/redact.ts";
import type { ThreadPort } from "./port.ts";

/** Wraps a port so no secret value reaches whatever is behind it. */
export function redacting(port: ThreadPort, secrets: readonly string[]): ThreadPort {
  if (secrets.length === 0) return port;
  const clean = (text: string): string => redactText(text, secrets);

  return {
    post: (text) => port.post(clean(text)),
    postNotice: (text, level) => port.postNotice(clean(text), level),
    postReply: (text, command) => port.postReply(clean(text), clean(command)),
    noteToolResult: (result) => port.noteToolResult({ ...result, output: clean(result.output) }),
    beginTurn: (turn) => port.beginTurn(turn),
    noteThinking: (text) => port.noteThinking(clean(text)),
    notePrompt: (author, text) => port.notePrompt(author, clean(text)),
    noteAside: (author, text) => port.noteAside(author, clean(text)),
    appendActivity: (line, tool) =>
      port.appendActivity(
        clean(line),
        tool === undefined
          ? undefined
          : { ...tool, target: tool.target === undefined ? undefined : clean(tool.target) },
      ),
    postDiff: (path, added, removed, body, call) =>
      port.postDiff(
        clean(path),
        added,
        removed,
        clean(body),
        call === undefined ? undefined : clean(call),
      ),
    setWaiting: (text) => port.setWaiting(text === null ? null : clean(text)),
    setReaction: (messageId, outcome) => port.setReaction(messageId, outcome),
    setUsage: (usage) => port.setUsage(usage),
    setBusy: (busy) => port.setBusy(busy),
    upload: (name, bytes, caption) => port.upload(clean(name), bytes, clean(caption)),
    close: (reason) => port.close(reason),
  };
}
