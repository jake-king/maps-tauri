#!/usr/bin/env node
// Differential test for crates/vtile: every tile in the archive decoded twice, by two independent
// pipelines, and compared layer by layer.
//   Rust:       crates/vtile (hand-rolled PMTiles reader + MVT decoder), via its `summary` example
//   JavaScript: pmtiles.js reading the file + @mapbox/vector-tile (MapLibre 6.7.0's own decoder)
// Usage: node tools/mvt_diff.mjs [data/basemap.pmtiles]
import { execFileSync } from "node:child_process";
import { openSync, readSync } from "node:fs";
import { PMTiles } from "pmtiles";
import { VectorTile } from "@mapbox/vector-tile";
import { PbfReader as Pbf } from "pbf";

const path = process.argv[2] ?? "data/basemap.pmtiles";

class NodeFileSource {
  constructor(p) { this.fd = openSync(p, "r"); this.p = p; }
  getKey() { return this.p; }
  async getBytes(offset, length) {
    const buf = Buffer.alloc(length);
    readSync(this.fd, buf, 0, length, offset);
    return { data: buf.buffer.slice(buf.byteOffset, buf.byteOffset + length) };
  }
}

const rust = execFileSync("cargo", ["run", "-q", "--release", "-p", "vtile", "--example", "summary", path],
  { encoding: "utf8", maxBuffer: 1 << 28 }).split("\n").filter(Boolean).map((l) => JSON.parse(l));

const archive = new PMTiles(new NodeFileSource(path));
const typeIndex = (t) => Math.min(t, 3);
let tiles = 0, layersCompared = 0;
const bad = [];
for (const r of rust) {
  const [z, x, y] = r.tile.split("/").map(Number);
  const res = await archive.getZxy(z, x, y);
  if (!res) { bad.push(`${r.tile}: rust has it, pmtiles.js does not`); continue; }
  const vt = new VectorTile(new Pbf(new Uint8Array(res.data)));
  const names = Object.keys(vt.layers).sort();
  const rnames = Object.keys(r.layers).sort();
  if (names.join() !== rnames.join()) bad.push(`${r.tile}: layers js=${names} rust=${rnames}`);
  for (const name of names) {
    const l = vt.layers[name];
    const s = { features: l.length, types: [0, 0, 0, 0], rings: 0, vertices: 0, checksum: 0n, extent: l.extent, kinds: {} };
    for (let i = 0; i < l.length; i++) {
      const f = l.feature(i);
      s.types[typeIndex(f.type)] += 1;
      const g = f.loadGeometry();
      s.rings += g.length;
      for (const ring of g) {
        s.vertices += ring.length;
        for (const p of ring) s.checksum += BigInt(p.x) + 3n * BigInt(p.y);
      }
      const k = f.properties.kind ?? "";
      const key = typeof k === "string" ? k : "";
      s.kinds[key] = (s.kinds[key] ?? 0) + 1;
    }
    const rl = r.layers[name];
    if (!rl) continue;
    layersCompared += 1;
    for (const field of ["features", "rings", "vertices", "extent"]) {
      if (s[field] !== rl[field]) bad.push(`${r.tile} ${name}.${field}: js=${s[field]} rust=${rl[field]}`);
    }
    if (s.checksum !== BigInt(rl.checksum)) bad.push(`${r.tile} ${name}.checksum: js=${s.checksum} rust=${rl.checksum}`);
    if (s.types.join() !== rl.types.join()) bad.push(`${r.tile} ${name}.types: js=${s.types} rust=${rl.types}`);
    const sk = JSON.stringify(Object.entries(s.kinds).sort()), rk = JSON.stringify(Object.entries(rl.kinds).sort());
    if (sk !== rk) bad.push(`${r.tile} ${name}.kinds differ`);
  }
  tiles += 1;
}
console.log(`${tiles} tiles, ${layersCompared} layers compared: ${bad.length ? bad.length + " DISAGREE" : "identical"}`);
for (const b of bad.slice(0, 10)) console.log("  " + b);
process.exit(bad.length ? 1 : 0);
