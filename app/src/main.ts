import "@fontsource-variable/inter";
import "./app.css";
import { mount } from "svelte";
import App from "./App.svelte";

// Keep the webview feeling like a native app.
window.addEventListener("contextmenu", (e) => {
  if (!(e.target as HTMLElement).closest("input, textarea, .selectable")) e.preventDefault();
});

mount(App, { target: document.getElementById("app")! });
