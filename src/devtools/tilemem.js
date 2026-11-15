// Tile CPU memory: the bytes of typed arrays reachable from each loaded tile, grouped by the tile
// field that holds them. MapLibre has no per-tile memory figure, so this walks the objects. It is an
// estimate: it sees ArrayBuffers (where nearly all of a tile's bulk lives), not object overhead,
// and each buffer is counted once even when several tiles share it.

const SKIP = new Set(["context", "gl", "painter", "map", "style", "tileID", "dependencies", "actor", "dispatcher"]);

/**
 * Bytes of distinct ArrayBuffers reachable from `root` within `maxDepth`, skipping SKIP keys and
 * anything in `seen` (shared across calls so a buffer is counted once).
 */
export function arrayBytes(root, seen, maxDepth = 8) {
  let total = 0;
  const stack = [[root, 0]];
  while (stack.length) {
    const [v, depth] = stack.pop();
    if (v === null || typeof v !== "object" || seen.has(v)) continue;
    seen.add(v);
    if (v instanceof ArrayBuffer || (typeof SharedArrayBuffer !== "undefined" && v instanceof SharedArrayBuffer)) {
      total += v.byteLength;
      continue;
    }
    if (ArrayBuffer.isView(v)) {
      stack.push([v.buffer, depth]);
      continue;
    }
    if (depth >= maxDepth) continue;
    if (v instanceof Map || v instanceof Set) {
      for (const x of v.values()) stack.push([x, depth + 1]);
      if (v instanceof Map) for (const k of v.keys()) stack.push([k, depth + 1]);
      continue;
    }
    for (const k of Object.keys(v)) if (!SKIP.has(k)) stack.push([v[k], depth + 1]);
  }
  return total;
}

/** Per-field bytes for a set of tiles: `{ field: bytes }`, plus `tiles`, each buffer once. */
export function tileBytesByField(tiles) {
  const seen = new Set();
  const out = {};
  for (const tile of tiles) {
    for (const k of Object.keys(tile)) {
      if (SKIP.has(k)) continue;
      const n = arrayBytes(tile[k], seen);
      if (n) out[k] = (out[k] ?? 0) + n;
    }
  }
  return out;
}

/** The loaded tiles of a source: in view, and in the out-of-view cache. */
export function sourceTiles(map, sourceId = "protomaps") {
  const tm = map.style?.tileManagers?.[sourceId];
  if (!tm) return { inView: [], cached: [] };
  const inView = tm._inViewTiles.getAllIds().map((id) => tm._inViewTiles.getTileById(id)).filter(Boolean);
  const cached = [];
  for (const list of Object.values(tm._outOfViewCache?.data ?? {})) for (const e of list) cached.push(e.value);
  return { inView, cached };
}
