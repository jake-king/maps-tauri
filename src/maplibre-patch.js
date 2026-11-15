// A patch to MapLibre 6.7.0: draw a loaded parent from the out-of-view cache while a newly needed
// tile decodes, instead of the background.
//
// WHY. In TileManager._updateRetainedTiles, a needed tile without data looks for a parent to draw
// in its place, but it only pulls one from the out-of-view cache when the needed tile
// `wasRequested()`, which is false while its first load is in flight. So a tile that has just
// entered the view (every new tile during a pan) only finds parents that happen to be in view,
// and at high zoom the low-zoom parents never are. Measured on the pan benchmark with the pixel
// check: the background shows in ~30% of frames on a first visit, up to a third of the screen,
// and a bigger cache, more prefetch or a warm-up pass changed nothing.
//
// WHAT. After MapLibre's own pass, for each needed tile still without data, walk up its parents;
// the first one with data (in view, or in the out-of-view cache) is retained, which draws it
// scaled up. Taking a tile from the cache is MapLibre's own _addTile, so no new request is made.
//
// RISK. This overrides a private method. It is applied only to the exact version it was written
// against and otherwise does nothing (and says so).

const PATCHED_FOR = "6.7.0";

export function retainCachedParents(maplibregl, map, sourceId) {
  const version = maplibregl.getVersion();
  if (version !== PATCHED_FOR) {
    return `not applied: MapLibre ${version}, patch written for ${PATCHED_FOR}`;
  }
  const tm = map.style?.tileManagers?.[sourceId];
  const proto = tm && Object.getPrototypeOf(tm);
  if (!proto || typeof proto._updateRetainedTiles !== "function" || !tm._outOfViewCache) {
    return "not applied: TileManager internals not found";
  }
  if (proto.__cachedParentsPatch) return "already applied";
  const original = proto._updateRetainedTiles;
  proto._updateRetainedTiles = function (idealTileIDs, zoom) {
    const retain = original.call(this, idealTileIDs, zoom);
    const minZ = Math.max(zoom - 10, this._source.minzoom); // MapLibre's own maxUnderzooming
    for (const id of idealTileIDs) {
      if (this._inViewTiles.getTileById(id.key)?.hasData()) continue;
      for (let z = id.overscaledZ - 1; z >= minZ; --z) {
        const pid = id.scaledTo(z);
        if (this._inViewTiles.getTileById(pid.key)?.hasData()) {
          retain[pid.key] = pid;
          break;
        }
        if (this._outOfViewCache.get(pid)?.hasData()) {
          this._addTile(pid); // moves it out of the cache into view; no request
          retain[pid.key] = pid;
          break;
        }
      }
    }
    return retain;
  };
  proto.__cachedParentsPatch = true;
  return "applied";
}
