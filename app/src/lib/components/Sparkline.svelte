<script lang="ts">
  let { values, height = 44, color = "var(--accent)" }: { values: number[]; height?: number; color?: string } =
    $props();

  const uid = Math.random().toString(36).slice(2);
  const W = 300;
  const path = $derived.by(() => {
    const max = Math.max(1, ...values);
    const n = values.length;
    if (n < 2) return { line: "", area: "" };
    const pts = values.map((v, i) => [(i / (n - 1)) * W, height - 2 - (v / max) * (height - 6)]);
    const line = pts.map(([x, y], i) => `${i ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
    return { line, area: `${line} L${W},${height} L0,${height} Z` };
  });
</script>

<svg viewBox="0 0 {W} {height}" preserveAspectRatio="none" style:height="{height}px" aria-hidden="true">
  <defs>
    <linearGradient id="spark-{uid}" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color={color} stop-opacity="0.35" />
      <stop offset="1" stop-color={color} stop-opacity="0" />
    </linearGradient>
  </defs>
  <path d={path.area} fill="url(#spark-{uid})" />
  <path d={path.line} fill="none" stroke={color} stroke-width="1.6" vector-effect="non-scaling-stroke" stroke-linejoin="round" />
</svg>

<style>
  svg {
    display: block;
    width: 100%;
  }
</style>
