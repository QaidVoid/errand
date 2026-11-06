import { defineConfig } from "vitepress";

const repo = "https://github.com/QaidVoid/errand";

/**
 * The documentation site.
 *
 * The pages under `reference/` are generated from the code, and `cargo test`
 * fails when they drift. Everything else is written by hand.
 */
export default defineConfig({
  title: "errand",
  description: "Run a coding agent from a chat channel, in a sandbox it cannot escape.",
  lang: "en-GB",
  cleanUrls: true,
  lastUpdated: true,

  head: [
    ["link", { rel: "icon", type: "image/svg+xml", href: "/favicon.svg" }],
    ["meta", { name: "theme-color", content: "#0e1218" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:title", content: "errand" }],
    [
      "meta",
      {
        property: "og:description",
        content: "Run a coding agent from a chat channel, in a sandbox it cannot escape.",
      },
    ],
  ],

  markdown: {
    theme: { light: "github-dark", dark: "github-dark" },
  },

  themeConfig: {
    logo: "/favicon.svg",

    nav: [
      { text: "Guide", link: "/guide/introduction", activeMatch: "/guide/" },
      { text: "Operations", link: "/operations/sandboxing", activeMatch: "/operations/" },
      { text: "Reference", link: "/reference/configuration", activeMatch: "/reference/" },
    ],

    sidebar: [
      {
        text: "Guide",
        items: [
          { text: "Introduction", link: "/guide/introduction" },
          { text: "Quick start", link: "/guide/quick-start" },
          { text: "Working in a thread", link: "/guide/threads" },
          { text: "Models", link: "/guide/models" },
          { text: "Delegation", link: "/guide/delegation" },
          { text: "Instructions", link: "/guide/instructions" },
          { text: "Memory", link: "/guide/memory" },
          { text: "GitHub and Forgejo", link: "/guide/forges" },
          { text: "Images", link: "/guide/images" },
          { text: "Audio", link: "/guide/audio" },
          { text: "Web interface", link: "/guide/interface" },
        ],
      },
      {
        text: "Operations",
        items: [
          { text: "Sandboxing", link: "/operations/sandboxing" },
          { text: "Network egress", link: "/operations/egress" },
          { text: "The Incus backend", link: "/operations/incus" },
          { text: "Running as a service", link: "/operations/service" },
          { text: "Configuration and data", link: "/operations/configuration" },
        ],
      },
      {
        text: "Reference",
        items: [
          { text: "Configuration", link: "/reference/configuration" },
          { text: "Commands", link: "/reference/commands" },
          { text: "Characters", link: "/reference/characters" },
        ],
      },
    ],

    socialLinks: [{ icon: "github", link: repo }],

    editLink: {
      pattern: `${repo}/edit/main/docs/:path`,
      text: "Edit this page",
    },

    search: { provider: "local" },

    outline: { level: [2, 3], label: "On this page" },

    footer: {
      message: "Released under the MIT or Apache 2.0 license, at your option.",
    },
  },
});
