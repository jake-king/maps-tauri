import test from "node:test";
import assert from "node:assert/strict";
import { lut, codeToDbz, hazardLabel, age, status, stamp, metarStamp, nearestStation, atLevel, flightLevel, zulu, relative, mercToWorld, DBZ_COLORS } from "../src/rust-renderer/weather.js";
import { range, chosen, along, stepFrom } from "../src/rust-renderer/timebar.js";

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

test("the status line says what is loading, from the first click", () => {
  const on = { radar: true, satellite: true, hazards: true };
  const now = Date.UTC(2026, 10, 16, 20, 10) / 1000;
  const sel = { radar: [now - 2400, now - 1800, now - 1200, now - 600, now], satellite: now - 120, hazards: now + 3000 };
  const base = { now, at: now, selection: { radar: [], satellite: null, hazards: null }, radarOk: null, satelliteOk: null, hazardsOk: null, hazardsValid: null, fromDisk: false, errors: [], loading: { busy: true, hazards: false } };
  assert.equal(status(null, on), "starting…");
  assert.equal(status(null, { radar: false, satellite: false, hazards: false }), "");
  assert.equal(status(base, on, {}), "radar: loading… · satellite: loading… · hazards: loading…");
  const some = { ...base, selection: sel, hazardsValid: now + 3000, radarOk: now, satelliteOk: now, hazardsOk: now, loading: { busy: false, hazards: false } };
  assert.equal(status(some, on, { radar: 1, satellite: false }), "radar: loading 1 of 5… · satellite: loading…");
  assert.equal(status(some, on, { radar: 5, satellite: true }), "", "all there: nothing to say");
  assert.equal(status({ ...some, fromDisk: true, errors: ["x"] }, on, { radar: 5, satellite: true }), "offline: last fetched · 1 fetch errors");
  // the future: no radar or satellite exists, and that is not "loading"
  const ahead = { ...some, at: now + 6 * 3600, selection: { radar: [], satellite: null, hazards: now + 5 * 3600 } };
  assert.equal(status(ahead, on, { radar: 0, satellite: null }), "");
});

test("the valid-time stamp says what each layer shows and when it is from", () => {
  const now = Date.UTC(2026, 10, 16, 20, 10) / 1000;
  const on = { radar: true, satellite: true, hazards: true };
  const s = { now, at: now - 3 * 3600, selection: { radar: [now - 3 * 3600 - 600, now - 3 * 3600], satellite: now - 3 * 3600 - 120 }, hazardsValid: now - 2 * 3600 - 600 };
  assert.equal(stamp(s, on), "Radar 17:10Z obs · Satellite 17:08Z obs · Hazards 18:00Z issued");
  const ahead = { now, at: now + 6 * 3600, selection: { radar: [], satellite: null }, hazardsValid: now + 4 * 3600 + 3000 };
  assert.equal(stamp(ahead, on), "Radar: observations only · Satellite: observations only · Hazards 01:00Z fcst +4 h 50 min");
  assert.equal(stamp({ ...s, selection: { radar: [], satellite: null } }, { radar: true, satellite: false, hazards: false }), "Radar: none at this time");
});

test("hazards by flight level: within 3,000 ft of base or top, as GFA", () => {
  const turb = { base: "180", top: "400" };
  assert.equal(atLevel(turb, null), true);
  assert.equal(atLevel(turb, 300), true);
  assert.equal(atLevel(turb, 150), true, "FL150 is 3,000 ft under the base");
  assert.equal(atLevel(turb, 140), false);
  assert.equal(atLevel(turb, 430), true);
  assert.equal(atLevel(turb, 440), false);
  assert.equal(atLevel({ base: "SFC", top: "100" }, 30), true);
  assert.equal(atLevel({ base: "FZL", top: "230" }, 240), true);
  assert.equal(atLevel({ base: "FZL", top: "230" }, 300), false);
  assert.equal(atLevel({}, 450), true, "no base or top: shown");
  assert.equal(flightLevel("SFC"), 0);
  assert.equal(flightLevel("x"), null);
});

test("times as a dispatcher reads them", () => {
  const t = Date.UTC(2026, 10, 16, 18, 40) / 1000;
  assert.equal(zulu(t), "18:40Z");
  assert.equal(relative(t, t), "now");
  assert.equal(relative(t - 2 * 3600 - 1200, t), "-2 h 20 min");
  assert.equal(relative(t + 3 * 3600, t), "+3 h");
  assert.equal(relative(t - 600, t), "-10 min");
});

test("Web Mercator metres to the renderer's world coordinates", () => {
  const half = Math.PI * 6378137;
  assert.deepEqual(mercToWorld(0, 0), [0.5, 0.5]);
  assert.deepEqual(mercToWorld(-half, half), [0, 0]);
  assert.deepEqual(mercToWorld(half, -half), [1, 1]);
});

test("the time bar: its range, a slider value to a time, positions", () => {
  const now = Date.UTC(2026, 10, 16, 20, 7) / 1000;
  const r = range(now, 18 * 3600);
  assert.equal(r[0] % 600, 0);
  assert.equal(r[1] % 600, 0);
  assert.ok(r[0] <= now - 18 * 3600 && r[0] > now - 18 * 3600 - 600);
  assert.ok(r[1] >= now + 12 * 3600 && r[1] < now + 12 * 3600 + 600);
  assert.equal(chosen(now + 100, now), null, "within a step of now is live");
  assert.equal(chosen(now - 3 * 3600, now), Math.floor((now - 3 * 3600) / 600) * 600);
  assert.equal(along(r[0], r), 0);
  assert.equal(along(r[1] + 9999, r), 1);
  assert.ok(Math.abs(along(now, r) - 0.6) < 0.01, "now sits 18 of 30 hours along");
});

test("stepping: from live, one step goes a whole step away in either direction", () => {
  const r = (now) => range(now, 18 * 3600);
  for (const now of [Date.UTC(2026, 10, 16, 20, 7) / 1000, Date.UTC(2026, 10, 16, 20, 0, 1) / 1000, Date.UTC(2026, 10, 16, 20, 9, 59) / 1000, Date.UTC(2026, 10, 16, 20, 10) / 1000]) {
    const fwd = stepFrom(null, now, 1, r(now));
    const back = stepFrom(null, now, -1, r(now));
    assert.ok(fwd !== null && fwd > now && fwd - now <= 1200 && fwd % 600 === 0, `forward from ${now}: ${fwd}`);
    assert.ok(back !== null && back < now && now - back <= 1200 && back % 600 === 0, `back from ${now}: ${back}`);
  }
  const now = Date.UTC(2026, 10, 16, 20, 7) / 1000;
  const t = stepFrom(null, now, -1, r(now));
  assert.equal(stepFrom(t, now, 1, r(now)), null, "stepping back towards now lands on live");
  assert.equal(stepFrom(r(now)[1], now, 1, r(now)), r(now)[1], "the end of the bar holds");
  assert.equal(stepFrom(r(now)[0], now, -1, r(now)), r(now)[0], "and so does the start");
});

test("the METAR stamp: how many stations, or why none", () => {
  const now = Date.UTC(2026, 10, 16, 22, 0) / 1000;
  const m = (stations, range) => ({ stations, range });
  assert.equal(metarStamp(null, now, now), "METARs: loading…");
  assert.equal(metarStamp(m([["KLAX"], ["KSFO"]], [now - 600, now]), now, now), "METARs 2 stations");
  assert.equal(metarStamp(m([], [now - 600, now]), now - 3 * 3600, now), "METARs: collected from 21:50Z", "before collection began");
  assert.equal(metarStamp(m([], [now - 600, now]), now + 3 * 3600, now), "METARs: observations only");
  assert.equal(metarStamp(m([], [null, null]), now, now), "METARs: none");
});

test("a click finds the nearest station within its radius", () => {
  const project = ([lon, lat]) => ({ x: lon, y: lat }); // a trivial projection for the test
  const st = [["A", 100, 100, 1, 0], ["B", 106, 100, 3, 0], ["C", 300, 300, 2, 0]];
  assert.equal(nearestStation(st, 104, 100, 10, project)[0], "B");
  assert.equal(nearestStation(st, 101, 101, 10, project)[0], "A");
  assert.equal(nearestStation(st, 200, 200, 10, project), null, "nothing within reach");
});
