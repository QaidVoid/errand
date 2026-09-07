/** The theme: this project's own, rather than the one every site has. */
import type { Theme } from "vitepress";
import Layout from "./Layout.vue";
import "./style.css";

export default { Layout } satisfies Theme;
