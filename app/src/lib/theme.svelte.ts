export type ThemePref = "system" | "light" | "dark";

const KEY = "kstar.theme";
const media = window.matchMedia("(prefers-color-scheme: dark)");

function read(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    /* storage unavailable */
  }
  return "system";
}

class Theme {
  pref = $state<ThemePref>(read());

  constructor() {
    media.addEventListener("change", () => this.apply());
    this.apply();
  }

  set(p: ThemePref) {
    this.pref = p;
    try {
      localStorage.setItem(KEY, p);
    } catch {
      /* ignore */
    }
    this.apply();
  }

  apply() {
    const dark = this.pref === "dark" || (this.pref === "system" && media.matches);
    document.documentElement.dataset.theme = dark ? "dark" : "light";
  }
}

export const theme = new Theme();
