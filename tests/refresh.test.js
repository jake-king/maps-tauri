// The weather layer's refresh against a fake backend: a change of time while the state for the old
// time is still on its way must not put the old time's textures on the GPU (found in CI's Ubuntu
// image under software GL, where wx_state took longer than the 30 ms between two picks).
import { test } from "node:test";
import assert from "node:assert/strict";

const calls = [];
const slow = new Map();
globalThis.window = {
  __TAURI_INTERNALS__: {
    transformCallback: () => 0,
    invoke: async (cmd, args) => {
      calls.push([cmd, args]);
      if (cmd === "wx_state") {
        const at = args.at;
        await new Promise((r) => setTimeout(r, slow.get(at) ?? 0));
        return {
          now: 100_000, at, grid: { lat0: 55, lon0: -130, dlat: 0.02, dlon: 0.02, ni: 10, nj: 10 },
          satellite: { width: 4, height: 4, bbox3857: [0, 0, 1, 1] },
          selection: { radar: [at - 600, at], satellite: at, satelliteGlobal: null, hazards: null },
        };
      }
      if (cmd === "wx_frame" || cmd === "wx_satellite") return new ArrayBuffer(16);
      return null;
    },
  },
};
const { WeatherLayer: Weather } = await import("../src/rust-renderer/weather.js");

test("a change of time mid-load: nothing fetched for the old time reaches the GPU", async () => {
  const uploaded = [];
  // a WebGL stand-in: every call succeeds and returns an object (the layer builds its programs up front)
  const gl = new Proxy({}, { get: (_, k) => (/Parameter$/.test(String(k)) ? () => true : typeof k === "string" && k === k.toUpperCase() ? 1 : () => ({})) });
  const el = () => ({ style: {}, getContext: () => ({}), before() {}, append() {}, addEventListener() {} });
  globalThis.document = { createElement: el };
  const map = { loop: { arrive: (fn) => fn(), addUpdater() {} }, triggerRepaint() {}, gl, on() {}, overlays: [], labelLayer: { canvas: el() }, canvas: el() };
  const w = new Weather(map);
  Object.assign(w, { timer: 1, radarOn: true, satOn: true, looping: false });
  w.uploadRadar = (t) => uploaded.push(["r", t]) && w.frames.set(t, {});
  w.uploadSat = (t) => uploaded.push(["s", t]) && w.sats.set(t, {});
  const six = 50_000, four = 57_200;
  slow.set(six, 60); // the old time's state is slow to come back
  const first = w.setTime(six);
  await new Promise((r) => setTimeout(r, 10));
  await w.setTime(four);
  await first;
  await new Promise((r) => setTimeout(r, 100));
  const old = uploaded.filter(([, t]) => t <= six);
  assert.deepEqual(old, [], `uploaded for the abandoned time: ${JSON.stringify(old)}`);
  assert.equal(w.state.at, four, "the state is the new time's");
  assert.ok(uploaded.some(([k, t]) => k === "s" && t === four), "the new time's satellite image is on the GPU");
  assert.ok(calls.some(([c, a]) => c === "wx_frame" && a.time === four), "the new time's newest radar frame was fetched");
  assert.ok(!calls.some(([c, a]) => (c === "wx_frame" || c === "wx_satellite") && a.time <= six), "nothing was even fetched for the old time");
});
