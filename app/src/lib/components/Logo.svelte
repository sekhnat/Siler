<script module lang="ts">
  let next = 0;
</script>

<script lang="ts">
  // The app icon (app/assets/logo.svg): a segmented S, its top part filled like a download in
  // progress. Ids are per instance, so two logos on a page don't share gradients.
  let { size = 28 }: { size?: number } = $props();
  const id = `siler-logo-${next++}`;
</script>

<svg width={size} height={size} viewBox="0 0 512 512" aria-hidden="true">
  <defs>
    <linearGradient id="{id}-body" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#1f2227" />
      <stop offset="1" stop-color="#15171a" />
    </linearGradient>
    <linearGradient id="{id}-accent" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#5cc8e8" />
      <stop offset="1" stop-color="#3a9fc4" />
    </linearGradient>
    <clipPath id="{id}-done">
      <rect x="0" y="0" width="512" height="292" />
    </clipPath>
    <path
      id="{id}-s"
      d="M 342 188 C 342 150 304 128 256 128 C 204 128 172 152 172 188 C 172 232 226 244 256 256 C 292 270 342 282 342 326 C 342 366 306 386 256 386 C 208 386 170 366 170 326"
      fill="none"
      stroke-width="46"
      stroke-dasharray="54 16.3"
    />
  </defs>
  <rect x="16" y="16" width="480" height="480" rx="112" fill="url(#{id}-body)" />
  <rect x="17" y="17" width="478" height="478" rx="111" fill="none" stroke="#2b2f36" stroke-width="2" />
  <use href="#{id}-s" stroke="#2c3037" />
  <g clip-path="url(#{id}-done)">
    <use href="#{id}-s" stroke="url(#{id}-accent)" />
  </g>
  <circle cx="392" cy="120" r="14" fill="url(#{id}-accent)" />
</svg>
