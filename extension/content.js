// Alt + click on a link lets the browser handle that download itself (K-Star steps aside).
"use strict";

document.addEventListener(
  "click",
  (e) => {
    if (!e.altKey) return;
    const a = e.target instanceof Element ? e.target.closest("a[href]") : null;
    if (a) browser.runtime.sendMessage({ type: "bypass" }).catch(() => {});
  },
  { capture: true, passive: true },
);
