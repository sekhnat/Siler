<script lang="ts">
  import "./lib/theme.svelte";
  import { api } from "./lib/api";
  import AddWindow from "./AddWindow.svelte";
  import MainWindow from "./MainWindow.svelte";
  import FatalScreen from "./lib/components/FatalScreen.svelte";

  const route = location.hash.replace(/^#/, "");
  // K-Star may have failed to start; then the window explains why instead.
  const startup = api.startupError().catch(() => null);
</script>

{#await startup then fatal}
  {#if fatal}
    <FatalScreen message={fatal} />
  {:else if route === "add"}
    <AddWindow />
  {:else}
    <MainWindow />
  {/if}
{/await}
