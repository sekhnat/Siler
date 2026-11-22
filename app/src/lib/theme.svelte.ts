// Light or dark (this computer's choice, kept here) and the skin (kept in the settings):
// both become CSS variables on the page, in every window.
import { listen } from "@tauri-apps/api/event";
import { api, type Skin, type ToolbarStyle } from "./api";

export type ThemePref = "system" | "light" | "dark";

const KEY = "siler.theme";
/** The last skin used, so a new window doesn't open in the default colours first. */
const SKIN_KEY = "siler.skin";
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

interface Look {
  skin: Skin;
  /** Empty for what the skin says. */
  toolbar: ToolbarStyle | "";
}

function cachedLook(): Look | null {
  try {
    const v = JSON.parse(localStorage.getItem(SKIN_KEY) ?? "null");
    return v?.skin?.light && v?.skin?.dark ? v : null;
  } catch {
    return null;
  }
}

class Theme {
  pref = $state<ThemePref>(read());
  skins = $state<Skin[]>([]);
  dark = $state(false);
  private look: Look | null = cachedLook();
  /** Shown while choosing in Settings, in that window only. */
  private trying: Look | null = null;
  private paints = 0;

  constructor() {
    media.addEventListener("change", () => this.apply());
    // Every window follows a change made in another.
    window.addEventListener("storage", (e) => {
      if (e.key !== KEY) return;
      this.pref = read();
      this.apply();
    });
    this.apply();
    void this.load();
    listen("skin-changed", () => void this.load()).catch(() => {});
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

  /** Reads the skins and the chosen one. */
  async load() {
    try {
      const [skins, settings] = await Promise.all([api.skins(), api.getSettings()]);
      this.skins = skins;
      const skin = skins.find((s) => s.id === settings.ui.skin) ?? skins[0];
      if (!skin) return;
      this.look = { skin, toolbar: settings.ui.toolbar_style };
      try {
        localStorage.setItem(SKIN_KEY, JSON.stringify(this.look));
      } catch {
        /* ignore */
      }
      this.apply();
    } catch {
      /* Siler couldn't start: the built-in colours will do */
    }
  }

  preview(id: string, toolbar: ToolbarStyle | "") {
    const skin = this.skins.find((s) => s.id === id);
    this.trying = skin ? { skin, toolbar } : null;
    this.apply();
  }

  endPreview() {
    this.trying = null;
    this.apply();
  }

  apply() {
    const dark = this.pref === "dark" || (this.pref === "system" && media.matches);
    this.dark = dark;
    const root = document.documentElement;
    root.dataset.theme = dark ? "dark" : "light";
    const look = this.trying ?? this.look;
    if (look) {
      for (const [key, value] of Object.entries(dark ? look.skin.dark : look.skin.light)) root.style.setProperty(`--${key}`, value);
      root.style.setProperty("--r", `${look.skin.radius}px`);
      root.dataset.toolbar = look.toolbar || look.skin.toolbar;
    }
    // Tells the tray panel that everything is repainted.
    root.dataset.paint = String(++this.paints);
  }
}

export const theme = new Theme();
