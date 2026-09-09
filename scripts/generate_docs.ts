// deno-lint-ignore-file no-console -- a generator reports to a terminal.
/**
 * Writes the reference pages from the code they describe.
 *
 * A configuration reference written by hand is wrong the first time a field
 * changes, and wrong documentation is worse than none: it is believed. The
 * schema, the command table, and the character table all already say what they
 * mean, so the pages are made from them and `deno task check` fails when what
 * is committed no longer matches.
 *
 * @module
 */

import { ALL_CHARS, PREFIXES, REACTIONS } from "../src/chat/chars.ts";
import { COMMANDS } from "../src/session/commands.ts";
import { ConfigError, DEFAULTS } from "../src/config/schema.ts";
import { validateConfig } from "../src/config/validate.ts";

/** One field of one configuration section. */
export interface Field {
  name: string;
  type: string;
  /** What its doc comment says, as one paragraph. */
  says: string;
}

/** One section of the configuration. */
export interface Section {
  name: string;
  says: string;
  fields: Field[];
}

/** Raised when the schema cannot be read, so the pages are never half made. */
export class SchemaUnreadable extends Error {}

/**
 * Reads the interfaces out of the schema.
 *
 * A scanner rather than a parser: the file is written to one shape, every
 * field carries a doc comment, and anything it cannot read is a failure rather
 * than a field quietly left out.
 */
export function readSchema(source: string): Section[] {
  const sections: Section[] = [];
  const lines = source.split("\n");

  let held: string[] = [];
  let inComment = false;
  let section: Section | null = null;

  const paragraph = (comment: string[]): string =>
    comment
      .map((line) => line.replace(/^\s*\/?\*+\/?/, "").replace(/\*\/\s*$/, "").trim())
      .filter((line) => line.length > 0)
      .join(" ")
      // A link in a doc comment is for an editor, not for a page.
      .replace(/\{@link (\w+)\}/g, "`$1`")
      .replace(/\s+/g, " ")
      .trim();

  for (const line of lines) {
    const trimmed = line.trim();

    if (trimmed.startsWith("/*")) {
      held = [trimmed];
      inComment = !trimmed.endsWith("*/");
      continue;
    }
    if (inComment) {
      held.push(trimmed);
      if (trimmed.endsWith("*/")) inComment = false;
      continue;
    }

    // The top-level interface is read too, since `projectRoot` and `stateDir`
    // are required and belong to no section.
    const opening = /^export interface (\w*)Config \{$/.exec(trimmed) ??
      (trimmed === "export interface Config {" ? ["", "Root"] : null);
    if (opening !== null) {
      section = { name: (opening[1] as string) || "Root", says: paragraph(held), fields: [] };
      sections.push(section);
      held = [];
      continue;
    }

    if (trimmed === "}") {
      section = null;
      held = [];
      continue;
    }

    const field = /^(\w+)\??:\s*(.+);$/.exec(trimmed);
    if (field !== null && section !== null) {
      section.fields.push({
        name: field[1] as string,
        type: (field[2] as string).replace(/\s+/g, " "),
        says: paragraph(held),
      });
    }
    held = [];
  }

  if (sections.length === 0) throw new SchemaUnreadable("no interfaces were found in the schema");
  for (const found of sections) {
    if (found.fields.length === 0) {
      throw new SchemaUnreadable(`${found.name} was read with no fields`);
    }
  }
  return sections;
}

/**
 * Which fields the daemon refuses to start without.
 *
 * Asked of the validator rather than decided here: it is the thing that
 * actually refuses, so a field that becomes required says so in the page
 * without anybody remembering to change it. The sections that may be omitted
 * whole are asked about separately, since their fields are only required once
 * the section exists.
 */
export function requiredFields(): Set<string> {
  const named = new Set<string>();

  const collect = (raw: unknown): void => {
    try {
      validateConfig(raw);
    } catch (error) {
      if (!(error instanceof ConfigError)) throw error;
      for (const problem of error.problems) {
        const found = /^([\w.]+) is required/.exec(problem);
        if (found !== null) named.add(found[1] as string);
      }
    }
  };

  collect({});
  collect({ github: {}, agent: { delegate: {} } });
  return named;
}

/** The default for one field, as it would be written in the file. */
function defaultOf(section: string, field: string): string {
  const held = (DEFAULTS as Record<string, Record<string, unknown>>)[section.toLowerCase()];
  const value = held?.[field];
  if (value === undefined) return "";
  return `\`${JSON.stringify(value)}\``;
}

/**
 * Which key each section is written under, in the order they are documented.
 *
 * Ordered here rather than taken from the file, because what a reader needs
 * first is not the order the types happen to be declared in.
 */
const KEYS: [string, string][] = [
  ["Root", "top level"],
  ["Chat", "chat"],
  ["Agent", "agent"],
  ["Delegate", "agent.delegate"],
  ["Github", "github"],
  ["Sandbox", "sandbox"],
  ["PolicyExtra", "sandbox.policyExtra"],
  ["Output", "output"],
  ["Web", "web"],
  ["Shutdown", "shutdown"],
  ["Limits", "limits"],
  ["Timeouts", "timeouts"],
];

/** The configuration reference. */
export function configurationPage(sections: Section[], required: Set<string>): string {
  const out: string[] = [
    "<!-- Generated by scripts/generate_docs.ts. Edit the schema, not this. -->",
    "",
    "# Configuration",
    "",
    "Every field the daemon reads, taken from `src/config/schema.ts`. A key that",
    "is not one of these is a refusal to start rather than a setting quietly",
    "ignored, so a misspelling says so.",
    "",
    "The file is read from `~/.config/errand/config.json`, then",
    "`/etc/errand/config.json`, then `config.json` in the working directory.",
    "`ERRAND_CONFIG` names one outright.",
    "",
    "There is a file to copy in the repository, `config.example.json`, and a",
    "schema beside it. Naming the schema in your own file gives an editor",
    "completion and checking as you type:",
    "",
    "```json",
    '{ "$schema": "https://raw.githubusercontent.com/QaidVoid/errand/main/config.schema.json" }',
    "```",
    "",
  ];

  for (const [name, key] of KEYS) {
    const section = sections.find((found) => found.name === name);
    if (section === undefined) continue;

    // A field whose type is another section is documented as that section.
    const fields = section.fields.filter((field) => !/Config( \| undefined)?$/.test(field.type));
    if (fields.length === 0) continue;

    out.push(`## ${key}`, "");
    if (section.says.length > 0) out.push(section.says, "");
    out.push("| field | type | default | what it does |", "| --- | --- | --- | --- |");

    for (const field of fields) {
      const shown = field.type.replace(/\|/g, "or").replace(/\s+/g, " ");
      const fallback = defaultOf(section.name, field.name);
      const stands = fallback !== ""
        ? fallback
        : required.has(`${key}.${field.name}`) || required.has(field.name)
        ? "required"
        : "none";
      out.push(`| \`${field.name}\` | \`${shown}\` | ${stands} | ${field.says} |`);
    }
    out.push("");
  }

  return `${out.join("\n")}\n`;
}

/** The command reference. */
export function commandsPage(): string {
  const access: Record<string, string> = {
    anyone: "anyone permitted",
    guest: "the owner and whoever they invited",
    owner: "the owner and operators",
    host: "named accounts, answered by the daemon",
  };
  const groups: [string, string][] = [
    ["session", "The session"],
    ["people", "Who takes part"],
    ["project", "The project"],
    ["you", "You and the host"],
  ];

  const out: string[] = [
    "<!-- Generated by scripts/generate_docs.ts. Edit the command table, not this. -->",
    "",
    "# Commands",
    "",
    "Typed in a thread, or picked as a slash command: both run the same code, so",
    "what they do and who may do it cannot drift apart.",
    "",
    "Anything else is a prompt for the agent. A message starting `!!!` is an",
    "aside: everyone in the thread sees it and the agent is never told.",
    "",
  ];

  for (const [group, title] of groups) {
    const inGroup = Object.entries(COMMANDS).filter(([, meta]) => meta.group === group);
    if (inGroup.length === 0) continue;

    out.push(`## ${title}`, "", "| command | who may | what it does |", "| --- | --- | --- |");
    for (const [name, meta] of inGroup) {
      const spelled = meta.argument === undefined ? name : `${name} ${meta.argument}`;
      out.push(`| \`${spelled}\` | ${access[meta.access]} | ${meta.summary} |`);
    }
    out.push("");
  }

  return `${out.join("\n")}\n`;
}

/** The character table, which is the whole set this system may emit. */
export function charactersPage(): string {
  const glyphOf = (entry: { codepoints: readonly string[] }): string =>
    entry.codepoints.map((point) => `\`${point}\``).join(" ");

  const out: string[] = [
    "<!-- Generated by scripts/generate_docs.ts. Edit the table, not this. -->",
    "",
    "# Characters",
    "",
    "Every character outside ASCII this system emits, and the one state each",
    "means. None is decoration, and using one for a state it does not name is a",
    "bug. Everything else, including every log line, is ASCII.",
    "",
    "They are declared as codepoints rather than as glyphs, so the source stays",
    "ASCII and an editor that cannot render one cannot corrupt it.",
    "",
    "## Reactions",
    "",
    "Placed on the sender's own message, tracking that message's fate. Exactly",
    "one is present at a time.",
    "",
    "| codepoints | name | means |",
    "| --- | --- | --- |",
  ];

  for (const entry of Object.values(REACTIONS)) {
    out.push(`| ${glyphOf(entry)} | ${entry.name} | ${entry.meaning} |`);
  }

  out.push("", "## Prefixes", "", "Placed at the start of a line the daemon writes.", "");
  out.push("| codepoints | name | means |", "| --- | --- | --- |");
  for (const entry of Object.values(PREFIXES)) {
    out.push(`| ${glyphOf(entry)} | ${entry.name} | ${entry.meaning} |`);
  }
  out.push("", `In total ${ALL_CHARS.length} characters.`, "");

  return `${out.join("\n")}\n`;
}

/** How a field's type is written for an editor that reads JSON Schema. */
function jsonType(type: string, sections: Section[]): Record<string, unknown> {
  const bare = type.replace(/\s*\|\s*undefined$/, "").trim();

  if (bare === "string") return { type: "string" };
  if (bare === "number") return { type: "number" };
  if (bare === "boolean") return { type: "boolean" };
  if (bare === "string[]") return { type: "array", items: { type: "string" } };
  if (bare === "Record<string, string>") {
    return { type: "object", additionalProperties: { type: "string" } };
  }
  if (bare === "SandboxBackend") return { type: "string", enum: ["podman", "bailey"] };
  if (bare === "NetworkMode") return { type: "string", enum: ["restricted", "none"] };

  const named = /^(\w+)Config$/.exec(bare);
  const section = named === null
    ? undefined
    : sections.find((found) => found.name === (named[1] as string));
  if (section !== undefined) return objectFor(section, sections);

  // Anything this does not recognise is left unconstrained rather than
  // constrained wrongly: an editor refusing a valid file is worse than one
  // that checks a little less.
  return {};
}

/** One section as a JSON Schema object. */
function objectFor(section: Section, sections: Section[]): Record<string, unknown> {
  const properties: Record<string, unknown> = {};
  const required: string[] = [];

  for (const field of section.fields) {
    properties[field.name] = { description: field.says, ...jsonType(field.type, sections) };
    // A field is required when the type says it is always there and there is
    // no default to fall back on.
    if (!field.type.includes("undefined") && defaultOf(section.name, field.name) === "") {
      required.push(field.name);
    }
  }

  return {
    type: "object",
    description: section.says,
    additionalProperties: false,
    properties,
    ...(required.length === 0 ? {} : { required }),
  };
}

/**
 * The configuration file, as a schema an editor can check against.
 *
 * Generated from the same interfaces the daemon validates against, so what an
 * editor accepts and what the daemon accepts cannot drift apart.
 */
export function configSchema(sections: Section[]): string {
  const root = sections.find((found) => found.name === "Root");
  if (root === undefined) throw new SchemaUnreadable("the top-level interface was not found");

  const schema = {
    $schema: "https://json-schema.org/draft/2020-12/schema",
    $id: "https://raw.githubusercontent.com/QaidVoid/errand/main/config.schema.json",
    title: "errand configuration",
    ...objectFor(root, sections),
  } as Record<string, unknown>;

  // The pointer an editor follows to find this file is not a setting.
  const properties = schema.properties as Record<string, unknown>;
  properties.$schema = { type: "string", description: "Where this schema lives." };

  return `${JSON.stringify(schema, null, 2)}\n`;
}

/** What each generated page should hold, by where it lives. */
export function pages(schema: string): Record<string, string> {
  const sections = readSchema(schema);
  return {
    "config.schema.json": configSchema(sections),
    "docs/reference/configuration.md": configurationPage(sections, requiredFields()),
    "docs/reference/commands.md": commandsPage(),
    "docs/reference/characters.md": charactersPage(),
  };
}

if (import.meta.main) {
  const checking = Deno.args.includes("--check");
  const schema = Deno.readTextFileSync("src/config/schema.ts");
  let stale = 0;

  for (const [path, wanted] of Object.entries(pages(schema))) {
    let held: string | undefined;
    try {
      held = Deno.readTextFileSync(path);
    } catch {
      held = undefined;
    }
    if (held === wanted) continue;

    stale += 1;
    if (checking) {
      console.error(`${path} no longer matches the code it is generated from`);
      continue;
    }
    Deno.mkdirSync("docs/reference", { recursive: true });
    Deno.writeTextFileSync(path, wanted);
    console.log(`wrote ${path}`);
  }

  if (checking && stale > 0) {
    console.error("run `deno task docs` to bring them back in step");
    Deno.exit(1);
  }
  if (checking) console.log("the reference pages match the code");
}
