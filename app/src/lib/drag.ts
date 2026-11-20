import { Channel, invoke } from "@tauri-apps/api/core";
import type { Download } from "./api";
import { categoryMeta } from "./categories";

/** A finished download whose file is there to be dragged into another app. */
export function draggable(d: Download): boolean {
  return d.status === "completed" && !d.file_missing && !!d.dir && !!d.filename;
}

function pathOf(d: Download): string {
  return `${d.dir!.replace(/\/+$/, "")}/${d.filename}`;
}

/** The picture under the pointer: a tile in the category colour with the file extension. */
function dragImage(d: Download, count: number): string {
  const px = 64 * Math.min(2, Math.max(1, Math.round(window.devicePixelRatio || 1)));
  const s = px / 64;
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = px;
  const ctx = canvas.getContext("2d")!;
  ctx.scale(s, s);
  ctx.fillStyle = categoryMeta(d.category).color;
  ctx.beginPath();
  ctx.roundRect(6, 6, 52, 52, 14);
  ctx.fill();
  const ext = (d.filename?.split(".").pop() ?? "").slice(0, 4).toUpperCase() || "FILE";
  ctx.fillStyle = "#ffffff";
  ctx.font = `700 ${ext.length > 3 ? 13 : 16}px Inter, sans-serif`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(ext, 32, 33);
  if (count > 1) {
    ctx.fillStyle = "#121521";
    ctx.beginPath();
    ctx.arc(52, 12, 11, 0, Math.PI * 2);
    ctx.fill();
    ctx.fillStyle = "#ffffff";
    ctx.font = "700 11px Inter, sans-serif";
    ctx.fillText(count > 99 ? "99+" : String(count), 52, 12.5);
  }
  return canvas.toDataURL("image/png");
}

/** Starts a native drag of the files, so they can be dropped into a file manager or app. */
export async function startFileDrag(items: Download[]): Promise<void> {
  const files = items.filter(draggable);
  if (!files.length) return;
  const onEvent = new Channel<unknown>();
  await invoke("plugin:drag|start_drag", {
    item: files.map(pathOf),
    image: dragImage(files[0], files.length),
    options: { mode: "copy" },
    onEvent,
  });
}
