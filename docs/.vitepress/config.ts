import { defineConfig } from "vitepress";

/**
 * The documentation site.
 *
 * The reference pages under `reference/` are generated from the code by
 * `deno task docs`, and `deno task check` fails when they drift. Everything
 * else is written by hand, because it is judgement rather than a list.
 */
export default defineConfig({
  title: "errand",
  description: "Run a coding agent from a chat channel, in a sandbox it cannot escape.",
  lang: "en-GB",
  cleanUrls: true,
  lastUpdated: false,
  outDir: "../dist/docs",
  head: [["link", { rel: "icon", type: "image/svg+xml", href: "/favicon.svg" }]],

  themeConfig: {
    repo: "https://github.com/QaidVoid/errand",
    sidebar: [
      {
        text: "using it",
        items: [
          { text: "What it is", link: "/" },
          { text: "Getting started", link: "/start" },
          { text: "In a thread", link: "/threads" },
          { text: "Memory", link: "/memory" },
          { text: "Two models", link: "/models" },
        ],
      },
      {
        text: "running it",
        items: [
          { text: "Sandboxing", link: "/sandboxing" },
          { text: "As a service", link: "/service" },
          { text: "The interface", link: "/interface" },
        ],
      },
      {
        text: "reference",
        items: [
          { text: "Configuration", link: "/reference/configuration" },
          { text: "Commands", link: "/reference/commands" },
          { text: "Characters", link: "/reference/characters" },
        ],
      },
    ],
  },
});
