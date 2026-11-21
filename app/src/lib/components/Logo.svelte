<script lang="ts">
  let { size = 28 }: { size?: number } = $props();
</script>

<svg width={size} height={size} viewBox="0 0 1024 1024" aria-hidden="true">
  <defs>
    <linearGradient id="siler-bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#7c5cff" />
      <stop offset="0.55" stop-color="#5b6cff" />
      <stop offset="1" stop-color="#22d3ee" />
    </linearGradient>
  </defs>
  <rect x="32" y="32" width="960" height="960" rx="224" fill="url(#siler-bg)" />
  <path
    d="M512 150 C 538 352, 586 398, 790 424 C 586 450, 538 496, 512 698 C 486 496, 438 450, 234 424 C 438 398, 486 352, 512 150 Z"
    fill="#fff"
  />
  <path
    d="M760 168 C 768 226, 782 240, 840 248 C 782 256, 768 270, 760 328 C 752 270, 738 256, 680 248 C 738 240, 752 226, 760 168 Z"
    fill="#fff"
    opacity="0.75"
  />
  <rect x="322" y="782" width="380" height="64" rx="32" fill="#fff" opacity="0.92" />
</svg>
