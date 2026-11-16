import test from "node:test";
import assert from "node:assert/strict";
import { lut, codeToDbz, hazardLabel, age, DBZ_COLORS } from "../src/rust-renderer/weather.js";

test("codes map back to dBZ as wx::radar writes them", () => {
  assert.equal(codeToDbz(0), null);
  assert.equal(codeToDbz(1), 5);
  assert.equal(codeToDbz(61), 35);
  assert.equal(codeToDbz(255), 132);
});

test("the colour table: nothing for code 0, the NWS band for each code, weak echoes fainter", () => {
  const t = lut();
  assert.equal(t.length, 256 * 4);
  assert.deepEqual([...t.subarray(0, 4)], [0, 0, 0, 0]);
  const rgba = (code) => [...t.subarray(code * 4, code * 4 + 4)];
  const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  assert.deepEqual(rgba(1).slice(0, 3), hex(DBZ_COLORS[0][1])); // 5 dBZ
  assert.deepEqual(rgba(61).slice(0, 3), hex("#fdf802")); // 35 dBZ: yellow
  assert.deepEqual(rgba(60).slice(0, 3), hex("#008e00")); // 34.5 dBZ: still the 30-35 band
  assert.ok(rgba(1)[3] < rgba(61)[3]);
});

test("hazard labels", () => {
  assert.equal(hazardLabel({ hazard: "TURB-HI", severity: "MOD", base: "180", top: "400" }), "TURB MOD 180-400");
  assert.equal(hazardLabel({ hazard: "ICE", severity: "MOD", base: "FZL", top: "230" }), "ICE MOD FZL-230");
  assert.equal(hazardLabel({ hazard: "FZLVL", level: "160" }), "FZL 160");
  assert.equal(hazardLabel({ hazard: "NEW-THING" }), "NEW-THING");
});

test("ages", () => {
  const t = Date.UTC(2026, 10, 16, 20, 4) / 1000;
  assert.equal(age(t, t + 180), "20:04Z, 3 min old");
  assert.equal(age(t, t + 3 * 3600), "20:04Z, 3 h old");
  assert.equal(age(t, t - 5400), "20:04Z, in 1.5 h");
  assert.equal(age(null, t), "none");
});
