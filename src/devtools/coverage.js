// Which parts of the view have nothing drawn: the honest "gray box" measure.
//
// "Tiles still loading" overstates it: while a detailed tile decodes, MapLibre draws a loaded
// parent (blurry) or loaded children in its place. A needed tile is UNCOVERED only when no
// drawable tile overlaps it at all: not itself, not an ancestor, not a descendant. A descendant
// covers only part of its parent, so this undercounts partly covered tiles, never overcounts.

/** Does drawable tile `d` overlap needed tile `t`? Both `{ z, x, y, wrap }` in canonical coords. */
export function overlaps(t, d) {
  if (t.wrap !== d.wrap) return false;
  if (d.z <= t.z) {
    const s = t.z - d.z;
    return t.x >> s === d.x && t.y >> s === d.y;
  }
  const s = d.z - t.z;
  return d.x >> s === t.x && d.y >> s === t.y;
}

/** How many of the `needed` tiles no `drawable` tile overlaps. */
export function uncoveredCount(needed, drawable) {
  let n = 0;
  for (const t of needed) if (!drawable.some((d) => overlaps(t, d))) n += 1;
  return n;
}

const canon = (id) => ({ z: id.canonical.z, x: id.canonical.x, y: id.canonical.y, wrap: id.wrap });

/**
 * `{ needed, uncovered }` for a source in a live map, or null if MapLibre's internals moved. Uses
 * the public coveringTiles() for what the view needs and the tile manager's renderable set for
 * what can be drawn this frame.
 */
export function measureCoverage(map, sourceId = "protomaps") {
  const tm = map.style?.tileManagers?.[sourceId];
  const src = map.getSource(sourceId);
  if (!tm || !src || typeof tm.getRenderableIds !== "function") return null;
  const needed = map
    .coveringTiles({ tileSize: src.tileSize ?? 512, minzoom: src.minzoom, maxzoom: src.maxzoom, roundZoom: false })
    .map(canon);
  const drawable = tm
    .getRenderableIds()
    .map((key) => tm.getTileByID(key))
    .filter(Boolean)
    .map((tile) => canon(tile.tileID));
  return { needed: needed.length, uncovered: uncoveredCount(needed, drawable) };
}
