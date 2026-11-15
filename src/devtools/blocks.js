// The overlay's "bytes per block" rows, as guile-2d's profiler shows them: the GPU ledger's live
// allocations, and the loaded tiles' CPU arrays by the field that holds them.

import { sourceTiles, tileBytesByField } from "./tilemem.js";

const FIELD_LABELS = {
  latestRawTileData: "raw vector data",
  latestFeatureIndex: "feature index",
  buckets: "buckets (CPU)",
  collisionBoxArray: "collision boxes",
  glyphAtlasImage: "glyph atlas (CPU)",
  imageAtlasImage: "icon atlas (CPU)",
  imageAtlas: "icon atlas (CPU)",
};

/** `[{ label, bytes, note }]`, largest tile fields first. */
export function blockRows(map, gpu) {
  const g = gpu.snapshot();
  const { inView, cached } = sourceTiles(map);
  const byField = tileBytesByField([...inView, ...cached]);
  const merged = {};
  for (const [k, n] of Object.entries(byField)) {
    const label = FIELD_LABELS[k] ?? k;
    merged[label] = (merged[label] ?? 0) + n;
  }
  const tileRows = Object.entries(merged)
    .sort((a, b) => b[1] - a[1])
    .map(([label, bytes]) => ({ label: `  ${label}`, bytes, note: "" }));
  const tileTotal = tileRows.reduce((a, r) => a + r.bytes, 0);
  return [
    { label: "GPU buffers", bytes: g.buffers.bytes, note: `${g.buffers.count}` },
    { label: "GPU textures", bytes: g.textures.bytes, note: `${g.textures.count}` },
    { label: "GPU renderbuffers", bytes: g.renderbuffers.bytes, note: `${g.renderbuffers.count}` },
    { label: "GPU shader programs", bytes: 0, note: `${g.programs} linked (driver memory, not counted)` },
    { label: "tiles, CPU arrays", bytes: tileTotal, note: `${inView.length}+${cached.length}` },
    ...tileRows,
  ];
}
