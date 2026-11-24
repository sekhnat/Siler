<script lang="ts">
  import "./lib/theme.svelte";
  import { api } from "./lib/api";
  import AddWindow from "./AddWindow.svelte";
  import CountdownWindow from "./CountdownWindow.svelte";
  import CompleteWindow from "./CompleteWindow.svelte";
  import ProgressWindow from "./ProgressWindow.svelte";
  import MainWindow from "./MainWindow.svelte";
  import TrayPanel from "./TrayPanel.svelte";
  import FatalScreen from "./lib/components/FatalScreen.svelte";

  const route = location.hash.replace(/^#/, "");
  // `progress/42`, `complete/42`: a window about one download.
  const [page, arg] = route.split("/");
  const id = Number(arg);
  // Siler may have failed to start; then the window explains why instead.
  const startup = api.startupError().catch(() => null);
</script>

{#await startup then fatal}
  {#if fatal}
    <FatalScreen message={fatal} />
  {:else if route === "add"}
    <AddWindow />
  {:else if route === "tray"}
    <TrayPanel />
  {:else if route === "countdown"}
    <CountdownWindow />
  {:else if page === "progress" && id}
    <ProgressWindow {id} />
  {:else if page === "complete" && id}
    <CompleteWindow {id} />
  {:else}
    <MainWindow />
  {/if}
{/await}
