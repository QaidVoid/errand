/**
 * What the interface knows about each kind of tool, to lay out a call.
 *
 * Read from the tool's name and from the text it returned, since that text is
 * all the agent reports. A tool this does not recognise is still shown, as
 * its output, so nothing here can hide a call.
 */

/** What a tool does to the project, which decides its colour. */
export type ToolKind = "writes" | "runs" | "reads" | "web" | "agents" | "media" | "asks" | "other";

/** Which kind a tool is, by its name. */
export function toolKind(name: string | undefined): ToolKind {
  if (name === undefined) return "other";
  const lowered = name.toLowerCase();
  if (lowered === "ask_user_question") return "asks";
  if (/^(agent|swarm)$/.test(lowered)) return "agents";
  if (/^(generate|edit)_(image|speech|music)$/.test(lowered)) return "media";
  if (/(fetch|http|web|url|search)/.test(lowered) && !/grep/.test(lowered)) return "web";
  if (/(edit|write|create|apply|patch)/.test(lowered)) return "writes";
  if (/(bash|shell|run|exec|command)/.test(lowered)) return "runs";
  if (/(read|cat|view|open|grep|find|glob|ls|list)/.test(lowered)) return "reads";
  return "other";
}

/**
 * A glyph per kind of tool, so a run of calls can be scanned by shape.
 *
 * ASCII only, like everything else in this repository.
 */
export function glyph(name: string | undefined): string {
  const lowered = name?.toLowerCase() ?? "";
  if (/(think|plan|todo|task)/.test(lowered)) return "~";
  switch (toolKind(name)) {
    case "asks":
      return "?";
    case "agents":
      return "&";
    case "media":
      return "%";
    case "web":
      return "@";
    case "writes":
      return "+";
    case "runs":
      return "$";
    case "reads":
      return /(read|cat|view|open)/.test(lowered) ? ">" : "*";
    default:
      return "-";
  }
}

/** One item of a todo list, as the tool reports the list back. */
export interface Todo {
  state: "done" | "doing" | "open";
  text: string;
}

/** The items of a todo list's report, in order, or none when it lists none. */
export function todos(output: string): Todo[] {
  const found: Todo[] = [];
  for (const line of output.split("\n")) {
    const item = /^\s*\[([ x~])\]\s+(.*)$/.exec(line);
    if (item === null) continue;
    const mark = item[1];
    found.push({
      state: mark === "x" ? "done" : mark === "~" ? "doing" : "open",
      text: item[2] ?? "",
    });
  }
  return found;
}

/** One agent a call started, as its report opens. */
export interface AgentRun {
  name: string;
  state: string;
  model?: string | undefined;
  tools?: number | undefined;
  ms?: number | undefined;
  /** What it reported, without the tags around it. */
  report: string;
}

/** Each agent an `agent` or `swarm` call ran, with what it reported. */
export function agentRuns(output: string): AgentRun[] {
  const runs: AgentRun[] = [];
  const opening = /<agent\s([^>]*)>/g;
  const starts = [...output.matchAll(opening)];
  starts.forEach((start, index) => {
    const attributes = new Map(
      [...(start[1] ?? "").matchAll(/(\w+)="([^"]*)"/g)].map((pair) => [pair[1] ?? "", pair[2] ?? ""]),
    );
    const from = (start.index ?? 0) + start[0].length;
    const to = starts[index + 1]?.index ?? output.length;
    const report = output
      .slice(from, to)
      .replace(/<\/?(agent|swarm)\b[^>]*>/g, "")
      .trim();
    const number = (key: string) => {
      const value = Number(attributes.get(key));
      return Number.isFinite(value) && attributes.has(key) ? value : undefined;
    };
    runs.push({
      name: attributes.get("name") ?? "agent",
      state: attributes.get("state") ?? "",
      model: attributes.get("model"),
      tools: number("tools"),
      ms: number("run_ms"),
      report,
    });
  });
  return runs;
}

/** A length of time as a reader says it. */
export function duration(ms: number): string {
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  return minutes < 60 ? `${minutes}m` : `${Math.floor(minutes / 60)}h${minutes % 60}m`;
}

/** The longest one line account of a call before it is cut. */
const SUMMARY_CHARS = 60;

/**
 * A one line account of how a call went, so a folded call still says
 * something: what a tool that reports in a known shape reported, or else the
 * first line of its output, or its size.
 */
export function summarize(name: string | undefined, output: string, failed: boolean): string {
  if (output.length === 0) return failed ? "failed" : "";
  const lowered = name?.toLowerCase() ?? "";

  if (lowered === "todo_list") {
    const items = todos(output);
    if (items.length > 0) {
      const done = items.filter((item) => item.state === "done").length;
      return `${done}/${items.length} done`;
    }
  }
  if (lowered === "ask_user_question") {
    if (/declined/i.test(output)) return "declined";
    const answered = output.split("\n").find((line) => line.trim().startsWith("- "));
    const answer = answered?.slice(answered.lastIndexOf(": ") + 2).trim();
    if (answer !== undefined && answer.length > 0) return cut(`answered: ${answer}`);
  }
  if (lowered === "agent") {
    const run = agentRuns(output)[0];
    if (run !== undefined) {
      const parts = [run.name, run.state];
      if (run.tools !== undefined) parts.push(`${run.tools} tools`);
      if (run.ms !== undefined) parts.push(duration(run.ms));
      return parts.filter((part) => part.length > 0).join(", ");
    }
  }
  if (lowered === "swarm") {
    const tally = output.split("\n")[0]?.trim() ?? "";
    if (/^completed:/.test(tally)) return cut(tally);
  }
  if (lowered === "web_search") {
    const results = output.split("\n").filter((line) => /^\d+\.\s/.test(line)).length;
    if (results > 0) return `${results} ${results === 1 ? "result" : "results"}`;
  }

  const lines = output.split("\n");
  const first = (lines[0] ?? "").trim();
  if (lines.length === 1) {
    // A long line usually opens with the sentence that says what happened.
    const sentence = first.split(/\.\s/)[0] ?? first;
    return cut(first.length > SUMMARY_CHARS && sentence.length < first.length ? sentence : first);
  }
  return `${lines.length} lines`;
}

function cut(text: string): string {
  return text.length <= SUMMARY_CHARS ? text : `${text.slice(0, SUMMARY_CHARS - 3).trimEnd()}...`;
}
