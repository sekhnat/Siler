<script lang="ts">
  import type { Category } from "../api";
  import { categoryMeta } from "../categories";

  let { category, size = 38 }: { category: Category; size?: number } = $props();
  const meta = $derived(categoryMeta(category));
</script>

<div class="tile" style:--c={meta.color} style:width="{size}px" style:height="{size}px">
  <meta.icon size={Math.round(size * 0.47)} strokeWidth={1.9} />
</div>

<style>
  .tile {
    flex: none;
    display: grid;
    place-items: center;
    border-radius: 28%;
    color: var(--c);
    background: linear-gradient(145deg, color-mix(in srgb, var(--c) 22%, transparent), color-mix(in srgb, var(--c) 9%, transparent));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--c) 20%, transparent);
  }
</style>
