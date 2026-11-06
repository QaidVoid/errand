import type { Theme } from "vitepress";
import DefaultTheme from "vitepress/theme";

import "@fontsource-variable/space-grotesk";
import "@fontsource-variable/geist";
import "@fontsource-variable/geist-mono";
import "./style.css";

import Home from "./components/Home.vue";

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component("Home", Home);
  },
} satisfies Theme;
