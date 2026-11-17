import test from "node:test";
import assert from "node:assert/strict";
import { lut, codeToDbz, hazardLabel, age, status, stamp, metarStamp, nearestStation, atLevel, flightLevel, zulu, relative, mercToWorld, pointInRing, sigmetAt, sigmetLabel, placeLabels, sigmetPriority, DBZ_COLORS } from "../src/rust-renderer/weather.js";
import { range, chosen, along, stepFrom, playStart, nextPlay, playSteps } from "../src/rust-renderer/timebar.js";

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
  assert.equal(stamp(s, on), "Radar 17:10Z obs (US) · Satellite 17:08Z obs · Hazards 18:00Z issued");
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

test("time-lapse: where play starts, each next step, and the end at live", () => {
  const now = Date.UTC(2026, 10, 16, 22, 7) / 1000;
  const r = range(now, 18 * 3600);
  const start = playStart(null, now, r);
  assert.equal(start % 600, 0);
  assert.ok(now - start >= 3 * 3600 && now - start < 3 * 3600 + 600, "from live: the last three hours");
  assert.equal(playStart(now - 5 * 3600 - 600, now, r), now - 5 * 3600 - 600, "from a chosen past time: from there");
  assert.equal(playStart(now + 3 * 3600, now, r), start, "from the future: the last three hours too");
  let t = start, steps = 0;
  while (t !== null && steps < 100) {
    const n = nextPlay(t, now);
    if (n !== null) assert.equal(n - t, 600);
    t = n;
    steps++;
  }
  assert.equal(t, null, "it ends at live");
  assert.equal(steps, 18, "three hours of 10-minute steps, then live");
});

test("a click inside a SIGMET finds it; outside, nothing", () => {
  const square = [[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]];
  assert.equal(pointInRing(5, 5, square), true);
  assert.equal(pointInRing(15, 5, square), false);
  assert.equal(pointInRing(-1, -1, square), false);
  const project = ([x, y]) => ({ x, y });
  const sigs = [{ key: "a", rings: [square] }, { key: "b", rings: [[[20, 20]]] }];
  assert.equal(sigmetAt(sigs, 5, 5, project)?.key, "a");
  assert.equal(sigmetAt(sigs, 22, 21, project)?.key, "b", "a point SIGMET within reach of its marker");
  assert.equal(sigmetAt(sigs, 50, 50, project), null);
});

test("SIGMET labels say what, how bad, which levels, where", () => {
  assert.equal(sigmetLabel({ kind: "SIGMET", hazard: "TURB", qualifier: "SEV", base: 300, top: 380, fir: "EGGX SHANWICK" }), "SIGMET TURB SEV FL300-380 · SHANWICK");
  assert.equal(sigmetLabel({ kind: "SIGMET", hazard: "VA", qualifier: "ERUPTION MT REVENTADOR", base: 0, top: 150, fir: "SEFG GUAYAQUIL" }), "SIGMET VA ERUPTION MT REVENTADOR SFC-FL150 · GUAYAQUIL");
  assert.equal(sigmetLabel({ kind: "CONVECTIVE", hazard: "CONVECTIVE", base: null, top: 240 }), "CONVECTIVE SIGMET tops FL240");
});

test("the satellite stamp names both images: GOES and the world mosaic", () => {
  const now = Date.UTC(2026, 10, 16, 23, 10) / 1000;
  const on = { radar: false, satellite: true, hazards: false };
  const base = { now, at: now, selection: { radar: [], satellite: now - 300, satelliteGlobal: now - 600 } };
  assert.equal(stamp(base, on), "Satellite 23:05Z obs, world 23:00Z");
  assert.equal(stamp({ ...base, selection: { radar: [], satellite: null, satelliteGlobal: now - 600 } }, on), "Satellite world 23:00Z obs");
  assert.equal(stamp({ ...base, at: now + 7200, selection: { radar: [], satellite: null, satelliteGlobal: null } }, on), "Satellite: observations only");
});

test("the time-lapse's steps, listed up front so they can be buffered before playing", () => {
  const now = Date.UTC(2026, 10, 16, 22, 7) / 1000;
  const r = range(now, 18 * 3600);
  const steps = playSteps(null, now, r);
  assert.equal(steps.length, 18, "19:00 to 21:50: three hours back to the last step before live");
  assert.equal(steps[0], playStart(null, now, r));
  assert.ok(steps.every((t, i) => i === 0 || t - steps[i - 1] === 600));
  assert.equal(nextPlay(steps.at(-1), now), null, "after the last comes live");
  assert.deepEqual(playSteps(now - 1800, now, r), [now - 1800, now - 1200, now - 600], "from a chosen time, while a whole step from now remains");
});

test("with the loop held, the status counts only the frame shown, not the whole loop", () => {
  const now = Date.UTC(2026, 10, 16, 22, 10) / 1000;
  const sel = { radar: [now - 2400, now - 1800, now - 1200, now - 600, now], satellite: null, hazards: null };
  const s = { now, at: now, selection: sel, radarOk: now, satelliteOk: now, hazardsOk: now, hazardsValid: null, fromDisk: false, errors: [], loading: { busy: false, hazards: false } };
  const on = { radar: true, satellite: false, hazards: false };
  assert.equal(status(s, on, { radar: 1, radarWant: 1 }), "", "the newest frame is all a held loop needs");
  assert.equal(status(s, on, { radar: 0, radarWant: 1 }), "radar: loading 0 of 1…");
  assert.equal(status(s, on, { radar: 3 }), "radar: loading 3 of 5…", "looping: the whole loop");
});

const overlaps = (a, b) => a.x - a.w / 2 < b.x + b.w / 2 && b.x - b.w / 2 < a.x + a.w / 2 && a.y - a.h / 2 < b.y + b.h / 2 && b.y - b.h / 2 < a.y + a.h / 2;

test("hazard labels never overlap: the more important is placed first, the rest move or drop", () => {
  const items = [
    { x: 100, y: 100, w: 120, h: 14, priority: 1 },
    { x: 105, y: 102, w: 120, h: 14, priority: 5 },
    { x: 100, y: 100, w: 120, h: 14, priority: 3 },
    { x: 400, y: 300, w: 80, h: 14, priority: 2 },
  ];
  const placed = placeLabels(items, 800, 600);
  assert.ok(placed[1], "the most important is placed");
  assert.deepEqual([placed[1].x, placed[1].y], [105, 102], "where it wanted to be");
  const boxes = placed.map((p, i) => p && { ...p, w: items[i].w, h: items[i].h }).filter(Boolean);
  for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) assert.ok(!overlaps(boxes[i], boxes[j]), `labels ${i} and ${j} overlap`);
  assert.ok(placed[2], "the next one found room nearby");
  assert.ok(placed[3], "one far away is untouched");
  assert.deepEqual([placed[3].x, placed[3].y], [400, 300]);
});

test("a crowd keeps only what fits, and nothing off screen", () => {
  const crowd = Array.from({ length: 40 }, (_, i) => ({ x: 200, y: 200, w: 150, h: 14, priority: i % 3 }));
  const placed = placeLabels(crowd, 800, 600);
  const kept = placed.filter(Boolean).length;
  assert.ok(kept >= 3 && kept < 40, `${kept} of 40 kept`);
  assert.equal(placeLabels([{ x: -200, y: 50, w: 100, h: 14, priority: 9 }], 800, 600)[0], null, "off screen: not placed");
});

test("which hazard label wins when space is short", () => {
  const p = (hazard, qualifier) => sigmetPriority({ hazard, qualifier });
  assert.ok(p("VA") > p("TS") && p("TC") > p("CONVECTIVE"), "ash and cyclones first");
  assert.ok(p("TS") > p("TURB", "SEV") && p("CONVECTIVE") > p("ICE", "SEV"), "then thunderstorms");
  assert.ok(p("TURB", "SEV") > p("MTW") && p("MTW") > p("TURB", "MOD") && p("TURB", "MOD") > 1, "then severe, then the rest, all above G-AIRMETs");
});
