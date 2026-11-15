// Save what a map just drew as a PNG (logs/snapshot-<name>.png), so a renderer's output can be
// looked at without the screen. Pixels are read in the map's "render" event, before the browser
// composites the frame, while the drawing buffer is still intact.

import { invoke } from "@tauri-apps/api/core";

/** The fixed view every snapshot uses, so the two renderers can be compared image to image. */
export const SNAPSHOT_VIEW = { center: [-118.4912, 34.0105], zoom: 15 }; // Santa Monica Pier

export async function snapshot(map, name) {
  map.jumpTo(SNAPSHOT_VIEW);
  await new Promise((r) => map.once("idle", r));
  const url = await new Promise((resolve) => {
    map.once("render", () => {
      const gl = map.painter.context.gl;
      const w = gl.drawingBufferWidth, h = gl.drawingBufferHeight;
      const px = new Uint8ClampedArray(w * h * 4);
      gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, px);
      const flipped = new Uint8ClampedArray(px.length); // GL rows run bottom-up
      for (let y = 0; y < h; y++) flipped.set(px.subarray((h - 1 - y) * w * 4, (h - y) * w * 4), y * w * 4);
      const c = document.createElement("canvas");
      c.width = w;
      c.height = h;
      c.getContext("2d").putImageData(new ImageData(flipped, w, h), 0, 0);
      resolve(c.toDataURL("image/png"));
    });
    map.triggerRepaint();
  });
  return invoke("save_snapshot", { name, dataUrl: url });
}
