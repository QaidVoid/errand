/**
 * What to call a session in the list.
 *
 * A session is named by whatever was pasted in to start it, which is
 * very often a bare link. A column of addresses tells a reader nothing, so an
 * address is reduced to the thing it points at and any words in the prompt are
 * shown beside it.
 *
 * Nothing here asks anybody, or any model, for a title. The whole prompt stays
 * on the row for a reader who wants it.
 */

/** How a session reads in the list. */
export interface Named {
  /** The line that identifies it, which is never a bare address. */
  title: string;
  /** What was asked for, when the prompt says more than where to look. */
  detail?: string;
}

/*
 * The scheme is optional because people paste links that have lost it. A line
 * of `//github.com/owner/repo` is an address to everyone except a parser.
 */
const LINK = /^<?((?:https?:)?\/\/[^\s>]+)>?$/;
const GITHUB = /^https?:\/\/(?:www\.)?github\.com\/([^/]+)\/([^/]+)(?:\/([^/]+)\/(\d+))?/;

/**
 * The shortest thing that identifies an address.
 *
 * A GitHub address becomes `owner/repo`, and the number of the pull request or
 * issue when it names one, since that is how people refer to them. Anything
 * else keeps its host and the end of its path, which is the part that says
 * what it is rather than where it lives.
 */
export function shorten(address: string): string {
  const url = address.startsWith("//") ? `https:${address}` : address;
  const repo = GITHUB.exec(url);
  if (repo !== null) {
    const name = `${repo[1]}/${(repo[2] as string).replace(/\.git$/, "")}`;
    const kind = repo[3];
    if (repo[4] === undefined) return name;
    return `${name}${kind === "pull" ? " PR" : kind === "issues" ? " issue" : " "}#${repo[4]}`;
  }

  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return address;
  }
  const path = parsed.pathname.replace(/\/+$/, "");
  const tail = path
    .split("/")
    .filter((part) => part.length > 0)
    .slice(-2)
    .join("/");
  return tail.length === 0 ? parsed.host : `${parsed.host}/${tail}`;
}

/** Whether a line is an address and nothing else. */
function onlyLink(line: string): string | undefined {
  const found = LINK.exec(line.trim());
  return found === null ? undefined : (found[1] as string);
}

/**
 * An address at the start of a line, and whatever was said after it.
 *
 * People paste the link and then type the instruction on the same line as
 * often as on the next one, and a row that leads with the address reads the
 * same either way.
 */
function opensWithLink(line: string): { link: string; rest: string } | undefined {
  const found = /^<?((?:https?:)?\/\/[^\s>]+)>?\s+(\S.*)$/.exec(line.trim());
  if (found === null) return undefined;
  return { link: found[1] as string, rest: (found[2] as string).trim() };
}

/**
 * Reads a name out of an opening prompt.
 *
 * The first line leads, shortened when it is only an address. Whatever comes
 * after it is worth showing only when it has words in it: a second address, or
 * a line of punctuation, says no more than the first did.
 */
export function nameFrom(opening: string | undefined): Named | undefined {
  const lines = (opening ?? "")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
  if (lines.length === 0) return undefined;

  const first = lines[0] as string;
  const alone = onlyLink(first);
  const leading = alone === undefined ? opensWithLink(first) : undefined;
  const link = alone ?? leading?.link;
  if (link === undefined) return { title: first };

  const title = shorten(link);
  // What was said after the address on its own line, or failing that the first
  // line below it that has words rather than another address in it.
  const said = leading?.rest ??
    lines.slice(1).find((line) => onlyLink(line) === undefined && /[a-z]{3}/i.test(line));
  return said === undefined ? { title } : { title, detail: said };
}
