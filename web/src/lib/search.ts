/**
 * Finding something in a session, and showing where it was found.
 *
 * Two things a reader needs, and one of them is easy to forget: which
 * exchanges hold the word, and where in them it is. Filtering to matching
 * exchanges without marking the word leaves somebody scrolling a wall of
 * output looking for it by eye, which is what searching was meant to avoid.
 *
 * Marking is done with ranges rather than by wrapping the text in elements.
 * Wrapping means splitting text nodes that the framework created and still
 * holds references to, and it then updates nodes that are no longer in the
 * page: entries disappear as soon as anything else on the page changes. A
 * highlight over a range leaves the document exactly as it was.
 */

/** The name the stylesheet knows this highlight by. */
const HIGHLIGHT = "errand-search";

/** What a search is looking for, reduced to what comparing needs. */
export function needleOf(search: string): string {
  return search.trim().toLowerCase();
}

/** Whether some text holds the needle. */
export function matches(text: string, needle: string): boolean {
  return needle.length > 0 && text.toLowerCase().includes(needle);
}

/** How many times the needle occurs in some text. */
export function countIn(text: string, needle: string): number {
  if (needle.length === 0) return 0;
  const lowered = text.toLowerCase();
  let found = 0;
  let at = lowered.indexOf(needle);
  while (at !== -1) {
    found += 1;
    at = lowered.indexOf(needle, at + needle.length);
  }
  return found;
}

/** Whether this browser can highlight without altering the document. */
function supported(): boolean {
  return typeof CSS !== "undefined" && "highlights" in CSS && typeof Highlight === "function";
}

/** Ranges by the element they were found in, so one can be redone alone. */
const held = new Map<HTMLElement, Range[]>();

function republish(): void {
  if (!supported()) return;
  const all = [...held.values()].flat();
  if (all.length === 0) {
    CSS.highlights.delete(HIGHLIGHT);
    return;
  }
  CSS.highlights.set(HIGHLIGHT, new Highlight(...all));
}

/** Elements whose text is not the session's own words. */
const SKIP = new Set(["SCRIPT", "STYLE"]);

/** Every occurrence of the needle inside an element, as ranges. */
function rangesIn(root: HTMLElement, needle: string): Range[] {
  if (needle.length === 0) return [];

  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (node) =>
      node.parentElement !== null && SKIP.has(node.parentElement.tagName)
        ? NodeFilter.FILTER_REJECT
        : NodeFilter.FILTER_ACCEPT,
  });

  const found: Range[] = [];
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    const text = (node.textContent ?? "").toLowerCase();
    let at = text.indexOf(needle);
    while (at !== -1) {
      const range = new Range();
      range.setStart(node, at);
      range.setEnd(node, at + needle.length);
      found.push(range);
      at = text.indexOf(needle, at + needle.length);
    }
  }
  return found;
}

/** What the marking action is told: the term, and when to look again. */
export interface Marking {
  needle: string;
  /**
   * Changes when the element's contents change.
   *
   * A range points at a text node, so ranges taken before an exchange gained
   * a line are ranges into the old contents. This is how the action knows to
   * take them again, since it cannot see the page changing by itself.
   */
  revision: number;
}

/**
 * A Svelte action that keeps an element's matches highlighted.
 *
 * Applied where the text is drawn rather than where it is held, because the
 * rendered form is the only place a match in markdown, a tool line, and a diff
 * can all be found the same way.
 */
export function marking(node: HTMLElement, marking: Marking): {
  update(next: Marking): void;
  destroy(): void;
} {
  const apply = (next: Marking): void => {
    const ranges = rangesIn(node, next.needle);
    if (ranges.length === 0) held.delete(node);
    else held.set(node, ranges);
    republish();
  };

  apply(marking);
  return {
    update: apply,
    destroy(): void {
      held.delete(node);
      republish();
    },
  };
}
