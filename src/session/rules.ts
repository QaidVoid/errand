/**
 * The operator's standing instructions, given to every session.
 *
 * A project says how to work on that project, in its own `AGENTS.md`, and the
 * agent reads that by itself. This is the other half: what the person running
 * the daemon wants of every session, whatever repository it is working in and
 * whether or not that repository says anything at all.
 *
 * The text is carried in the block already appended to the agent's system
 * prompt rather than written into the agent's configuration directory. That
 * directory's name belongs to the agent and is overridable, so writing there
 * would be a guess that fails silently; the appended block is a path errand
 * passes itself and can be sure of.
 */

/** Heading the rules are given under, so the agent can tell them apart. */
export const RULES_HEADING = "## House rules";

/**
 * The rules as they reach the agent, or undefined when there are none.
 *
 * Whitespace-only counts as none. A file that has been emptied is an operator
 * saying there are no rules, and a heading over nothing reads as an omission.
 */
export function rulesBlock(contents: string): string | undefined {
  const text = contents.trim();
  if (text.length === 0) return undefined;
  return [
    RULES_HEADING,
    "",
    "These come from the person running this daemon and apply to every",
    "session. Where they and a project's own instructions disagree, say so",
    "rather than choosing one in silence.",
    "",
    text,
    "",
    "",
  ].join("\n");
}
