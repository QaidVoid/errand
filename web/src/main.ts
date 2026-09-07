import { mount } from "svelte";
import App from "./App.svelte";
import { restore } from "./lib/theme.ts";
import "./styles/app.css";

// Before mounting, so the first paint is in the chosen theme rather than
// flashing the default one and correcting itself.
restore();

const target = document.querySelector("#app");
if (target === null) throw new Error("no mount point");

export default mount(App, { target });
