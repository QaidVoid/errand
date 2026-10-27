<script lang="ts">
  /**
   * One line icon, drawn in the current text colour.
   *
   * Drawn rather than taken from a font, so a glyph a system draws as a
   * coloured emoji cannot turn up in the middle of a monochrome toolbar.
   */
  const PATHS = {
    search: "M11 11l3.5 3.5M12.5 7a5.5 5.5 0 1 1-11 0 5.5 5.5 0 0 1 11 0z",
    hash: "M3 6h11M2 10h11M6.5 2.5l-1.5 11M11 2.5l-1.5 11",
    folder: "M1.5 4.5a1 1 0 0 1 1-1h3.6l1.4 1.5h5.5a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1h-10.5a1 1 0 0 1-1-1z",
    help: "M6 6a2 2 0 1 1 2.8 1.8c-.5.3-.8.7-.8 1.2v.5M8 11.8v.2M14.5 8a6.5 6.5 0 1 1-13 0 6.5 6.5 0 0 1 13 0z",
    menu: "M2 4h12M2 8h12M2 12h12",
    palette:
      "M8 1.5a6.5 6.5 0 0 0 0 13c.8 0 1.2-.6 1.2-1.2 0-.9-.7-1.1-.7-1.9 0-.7.6-1.2 1.3-1.2h1.6a3.1 3.1 0 0 0 3.1-3.1C14.5 4 11.6 1.5 8 1.5zM4.8 8.2h.01M6 5h.01M9.6 4.6h.01M11.6 7h.01",
    send: "M8 13V3M3.5 7.5L8 3l4.5 4.5",
    plus: "M8 3v10M3 8h10",
    command: "M3 4.5l3.5 3.5L3 11.5M8.5 11.5h4.5",
    layers: "M8 2l6 3-6 3-6-3 6-3zM2 8l6 3 6-3M2 11l6 3 6-3",
  } as const;

  let { name, size = 16 }: { name: keyof typeof PATHS; size?: number } = $props();
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 16 16"
  fill="none"
  stroke="currentColor"
  stroke-width="1.5"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"><path d={PATHS[name]} /></svg
>
