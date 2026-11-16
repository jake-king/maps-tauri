// The native renderer's app: Rust decodes and meshes the tiles, renderer.js draws them, labels.js
// places the names, search.js is the search box. The dev tools are the same as the MapLibre app's
// (src/devtools/harness.js), so the two can be compared number for number.

import { invoke } from "@tauri-apps/api/core";
import { RustMap } from "./renderer.js";
import { GpuLedger, installGpuLedger } from "../devtools/gpumem.js";
import { blockRows } from "../devtools/blocks.js";
import { snapshot } from "../devtools/snapshot.js";
import { say, memprobeBlank, startDevtools } from "../devtools/harness.js";
import { createSearchBox } from "./search.js";
import initCoords, { coordFeatures } from "../wasm/coords.js";
import { once } from "./once.js";
import { WeatherLayer, status as wxStatusText, stamp as wxStampText, atLevel } from "./weather.js";
import { TimeBar } from "./timebar.js";

const flags = await invoke("dev_flags");
const gpu = installGpuLedger(WebGL2RenderingContext.prototype, new GpuLedger());

await memprobeBlank(flags, "rust");

// Everything awaited at startup is awaited BEFORE the map exists: an await between creating the map
// and listening for its first "idle" let that event fire unheard (the WASM load below did, ~1 run in 5).
const info = await invoke("archive_info");
const bounds = { minLon: info.minLon, minLat: info.minLat, maxLon: info.maxLon, maxLat: info.maxLat };
const TILES = navigator.userAgent.includes("Windows") ? "http://tiles.localhost" : "tiles://localhost";
const CACHE_TILES = flags.cacheZoomLevels ? flags.cacheZoomLevels * 12 : 240; // same tile budget as MapLibre's 20 levels

// A world-sized archive (the remote planet) starts at Santa Monica, not mid-Atlantic.
const worldSized = bounds.maxLon - bounds.minLon > 90;
const map = new RustMap({
  container: "map",
  baseUrl: TILES,
  center: worldSized ? [-118.4912, 34.0105] : [(bounds.minLon + bounds.maxLon) / 2, (bounds.minLat + bounds.maxLat) / 2],
  zoom: 12,
  minZoom: info.minZoom,
  maxZoom: info.maxZoom,
  cacheSize: CACHE_TILES,
  fallback: !flags.noFallback,
  prefetchRing: flags.prefetchRing ?? 1,
  labels: !flags.noLabels,
});
// The coordinate parser (WASM) loads on the first search, not at startup: compiled, it holds about
// 17 MB of the web process (measured 2026-11-15), which a map nobody searches should not pay for.
// A failed load is retried on the next search rather than kept.
const coordsReady = once(initCoords);
const coordFeaturesLazy = async (text, limit) => {
  await coordsReady();
  return coordFeatures(text, limit);
};
const search = createSearchBox(map, { invoke, coordFeatures: coordFeaturesLazy, container: document.getElementById("map") });
// The map data's licence (ODbL) requires the credit to be visible. Plain text: a link would navigate
// the app's only window, and the content security policy would refuse it anyway.
const credit = document.createElement("div");
credit.textContent = "© OpenStreetMap contributors, Protomaps";
credit.style.cssText = "position:absolute;right:0;bottom:0;z-index:20;padding:2px 6px;background:#fffc;" +
  "font:11px 'Noto Sans',sans-serif;color:#333;pointer-events:none;";
document.getElementById("map").appendChild(credit);
// what the end-to-end search reports: the text, and that it lies inside the window
const creditShown = () => {
  const r = credit.getBoundingClientRect();
  const inside = r.width > 0 && r.height > 0 && r.right <= innerWidth && r.bottom <= innerHeight;
  return { text: credit.textContent, inside };
};
// Weather, synced to one chosen time (GFA's model): NOAA radar, GOES satellite clouds and G-AIRMET
// hazards, off until a box is ticked (nothing is fetched before).
const wxPanel = document.createElement("div");
wxPanel.style.cssText = "position:absolute;left:10px;bottom:10px;z-index:20;padding:6px 10px;background:#fffe;border-radius:6px;" +
  "box-shadow:0 1px 4px #0003;font:12px 'Noto Sans',sans-serif;color:#222;width:254px;box-sizing:border-box;";
const wxBox = (label) => {
  const l = document.createElement("label");
  l.style.cssText = "margin-right:10px;cursor:pointer;white-space:nowrap;";
  const b = document.createElement("input");
  b.type = "checkbox";
  b.style.cssText = "vertical-align:middle;margin:0 4px 0 0;";
  l.append(b, label);
  wxPanel.append(l);
  return b;
};
const radarBox = wxBox("Radar");
const satBox = wxBox("Satellite");
const hazardBox = wxBox("Hazards");
const metarBox = wxBox("METARs");
const windBox = wxBox("Winds");
const wxStatus = document.createElement("div");
wxStatus.style.cssText = "margin-top:3px;color:#555;font-size:11px;min-height:14px;";
wxPanel.append(wxStatus);
document.getElementById("map").appendChild(wxPanel);
// what each layer shows and when it is from, over the map (GFA puts the valid time on the map)
const wxStamp = document.createElement("div");
wxStamp.style.cssText = "position:absolute;top:10px;left:50%;transform:translateX(-50%);z-index:20;padding:4px 10px;background:#fffe;" +
  "border-radius:6px;box-shadow:0 1px 4px #0003;font:600 12px 'Noto Sans',sans-serif;color:#222;display:none;pointer-events:none;";
document.getElementById("map").appendChild(wxStamp);
const layers = () => ({ radar: radarBox.checked, satellite: satBox.checked, hazards: hazardBox.checked, metars: metarBox.checked, winds: windBox.checked });
const weather = new WeatherLayer(map, {
  onChange: (s) => {
    wxStatus.textContent = wxStatusText(s, layers(), weather.progress());
    wxStamp.textContent = wxStampText(s, layers(), weather.metars, weather.wind);
    wxStamp.style.display = wxStamp.textContent ? "block" : "none";
    timeBar.update(s, weather.metars?.range);
  },
});
const timeBar = new TimeBar(document.getElementById("map"), {
  onTime: (at) => weather.setTime(at).catch((e) => say(`map error: weather: ${e.message ?? e}`)),
  onLevel: (fl) => weather.setLevel(fl),
  onLoop: (on) => {
    weather.looping = on;
    map.triggerRepaint();
  },
  isReady: () => weather.displayReady(),
  // while playing the hours, each step shows its newest radar frame, not the 40-minute loop
  onPlay: (playing) => {
    weather.looping = !playing && timeBar.loopBox.checked;
    map.triggerRepaint();
  },
  prefetch: (at) => weather.prefetch(at).catch(() => {}),
});
const wxSet = () => {
  timeBar.show(radarBox.checked || satBox.checked || hazardBox.checked || metarBox.checked || windBox.checked);
  return weather.set(layers()).catch((e) => say(`map error: weather: ${e.message ?? e}`));
};
for (const b of [radarBox, satBox, hazardBox, metarBox, windBox]) b.addEventListener("change", wxSet);
// pixel check: a magenta background, so any uncovered screen area is countable
if (flags.pixelCheck) map.setBackground([1, 0, 1, 1]);
map.on("error", (e) => say(`map error: ${e.error?.message ?? e}`));

const rows = () => {
  const c = map.cacheStats();
  return [...blockRows(map, gpu), { label: "mesh cache (GPU copies)", bytes: c.bytes, note: `${c.loaded} tiles` }];
};
const tools = startDevtools({
  map, flags, renderer: "rust", bounds, rows,
  describe: () => ({ cache_tiles: CACHE_TILES, prefetch_ring: flags.prefetchRing ?? 1, archive: info.source }),
});
map.onVisibleWait = (ms) => tools.stats.tap?.("visible", ms);
map.onFetched = (ms) => {
  if (!tools.active()) return;
  tools.stats.fetch.push(ms);
  tools.stats.tap?.("fetch", ms);
};

map.whenIdle().then(async () => {
  say(`native renderer · ${info.source} archive, zooms ${info.minZoom}-${info.maxZoom} · ${map.cacheStats().loaded} tiles · z${map.getZoom().toFixed(1)}`);
  await tools.runModes({
    // MAPS_SEARCH=<query>: type it, take the first result, settle, snapshot the result
    search: async () => {
      const names = await search.run(flags.search);
      await new Promise((r) => map.once("moveend", r));
      if (!map.areTilesLoaded()) await new Promise((r) => map.once("idle", r));
      await new Promise((r) => setTimeout(r, 200));
      const path = await snapshot(map, "search", { center: map.getCenter(), zoom: map.getZoom() }, (ctx, dpr) => search.paint(ctx, dpr));
      return invoke("bench_done", { result: { search: flags.search, results: names, box: search.boxStyle(), credit: creditShown(), center: map.getCenter(), snapshot: path } });
    },
    // MAPS_WEATHER=1: both layers on, wait for every frame and the hazards (90 s at most), then a
    // picture of the whole country with the loop held on the newest frame
    // MAPS_WEATHER=1: all three layers on; wait for the present (its radar loop, satellite image and
    // hazards, 90 s at most), picture it; then 3 hours back, the same; then the registration probe
    weather: async () => {
      radarBox.checked = satBox.checked = hazardBox.checked = metarBox.checked = windBox.checked = true;
      await wxSet();
      const statuses = new Set([wxStatus.textContent]);
      const ready = () => {
        // the state must be for the time asked for, not the one before it; and "no hazard snapshot
        // near this time" only counts once hazards have been fetched (before that, none is not an answer)
        if (!weather.state || (weather.at != null && weather.state.at !== weather.at)) return false;
        const p = weather.progress(), sel = weather.state?.selection ?? {};
        // complete for that time: its radar loop, its satellite image if one exists, its hazards if a
        // snapshot is in reach (none is the right answer far from any)
        return sel.radar?.length > 0 && p.radar === sel.radar.length && (sel.satellite == null || p.satellite === true) && (sel.hazards == null ? weather.state.hazardsOk != null || weather.state.available?.hazards?.length > 0 : weather.state.hazards?.length > 0) && weather.metars != null && !!weather.wind;
      };
      // wait on the layer's own updates, as a user would: no extra polling from here
      const settle = async (limit) => {
        const s0 = performance.now();
        while (!ready() && performance.now() - s0 < limit) {
          statuses.add(wxStatus.textContent);
          await new Promise((r) => setTimeout(r, 100));
        }
        statuses.add(wxStatus.textContent);
        return Math.round(performance.now() - s0);
      };
      const liveMs = await settle(90000);
      weather.looping = false;
      const live = weather.report();
      const pic = (name) => snapshot(map, name, { center: [-97, 38.5], zoom: 3.6 }, (ctx) => ctx.drawImage(weather.canvas, 0, 0, ctx.canvas.width, ctx.canvas.height));
      const livePath = await pic("weather");
      // winds alone over the country, after the trails have built up for 3 s; then FL180
      const keep = layers();
      await weather.set({ radar: false, satellite: false, hazards: false, metars: false, winds: true });
      const usView = { center: [-97, 38.5], zoom: 3.6, bearing: 0 };
      map.jumpTo(usView);
      await new Promise((r) => setTimeout(r, 500));
      // the cost of animating: frame-loop stats and redraw time over 3 s of winds alone
      const s0 = map.loop.stats();
      const times = [];
      const r0 = map._render;
      map._render = (...a) => {
        const t = performance.now();
        r0(...a);
        times.push(performance.now() - t);
      };
      await new Promise((r) => setTimeout(r, 3000));
      map._render = r0;
      const s1 = map.loop.stats();
      times.sort((a, b) => a - b);
      const windCost = { ticks: s1.ticks - s0.ticks, draws: s1.draws - s0.draws, late: s1.late - s0.late, interval_ms: s1.interval, render_p50_ms: times[times.length >> 1], render_p99_ms: times[Math.floor(times.length * 0.99)] };
      const windPath = await snapshot(map, "weather-wind", usView);
      const windLive = weather.report().wind;
      weather.setLevel(180);
      const l0 = performance.now();
      while (weather.wind?.hpa !== 500 && performance.now() - l0 < 30000) await new Promise((r) => setTimeout(r, 100));
      const wind180 = { hpa: weather.wind?.hpa ?? null, ms: Math.round(performance.now() - l0) };
      weather.setLevel(null);
      await weather.set(keep);
      // a click on LAX's dot opens its report
      const lax = weather.metars?.stations?.find((x) => x[0] === "KLAX");
      let popup = null;
      if (lax) {
        map.jumpTo({ center: [lax[1], lax[2]], zoom: 9, bearing: 0 });
        await new Promise((r) => map.once("render", r));
        const p = map.project([lax[1], lax[2]]);
        await weather.clicked(p.x, p.y);
        popup = weather.popup?.style.display === "block" ? weather.popup.textContent : null;
        weather.closePopup();
      }
      const back = Math.floor((weather.state.now - 3 * 3600) / 600) * 600;
      const p0 = performance.now();
      timeBar.pick(back);
      await new Promise((r) => setTimeout(r, 50));
      const pastMs = await settle(90000);
      const past = { ...weather.report(), stamp: wxStamp.textContent };
      const pastPath = await pic("weather-past");
      const all = weather.state.hazards ?? [];
      const levels = Object.fromEntries([null, 100, 180, 340].map((fl) => [fl ?? "all", all.filter((h) => h.hazard !== "FZLVL" && atLevel(h, fl)).length]));
      // a change of mind mid-load: 6 hours back, then 4 hours back 30 ms later. The view must settle on
      // 4 hours back, and nothing fetched for 6 hours back may reach the GPU.
      const six = back - 3 * 3600, four = back - 3600;
      timeBar.pick(six);
      await new Promise((r) => setTimeout(r, 30));
      timeBar.pick(four);
      const switchMs = await settle(90000);
      const sixOnGpu = [...weather.frames.keys()].filter((t) => t > six - 2700 && t <= six).length + [...weather.sats.keys()].filter((t) => t > six - 1800 && t <= six).length;
      const changed = { at: weather.state.at, want: four, ready: ready(), ms: switchMs, six_on_gpu: sixOnGpu };
      // time-lapse: play from 2 hours back; record each step and whether it was complete when left
      timeBar.pick(Math.floor((weather.state.now - 2 * 3600) / 600) * 600);
      await settle(90000);
      // each step played, and whether its newest radar frame and satellite image were on the GPU when
      // the player left it (a frame for a time already left is dropped before upload, so "on the GPU
      // at the change" means it arrived while the step was current)
      const visited = [];
      const onGpu = (sel) => !!sel && (!sel.radar?.length || weather.frames.has(sel.radar.at(-1))) && (sel.satellite == null || weather.sats.has(sel.satellite));
      const playT0 = performance.now();
      timeBar.toggle();
      let cur = { at: weather.at, sel: null };
      while (timeBar.player && performance.now() - playT0 < 120000) {
        if (weather.at !== cur.at) {
          visited.push({ at: cur.at, complete: onGpu(cur.sel) });
          cur = { at: weather.at, sel: null };
        }
        if (weather.state?.at === cur.at) cur.sel = weather.state.selection;
        await new Promise((r) => setTimeout(r, 10));
      }
      visited.push({ at: cur.at, complete: cur.at === null || onGpu(cur.sel) });
      const play = { steps: visited.filter((v) => v.at !== null).map((v) => v.at), incomplete: visited.filter((v) => v.at !== null && !v.complete).map((v) => v.at), ended_live: weather.at === null && !timeBar.player, ms: Math.round(performance.now() - playT0) };
      timeBar.pick(null);
      const probe = await weather.probe();
      return invoke("bench_done", { result: { weather: live, wind: windLive, wind180, windCost, windPath, popup, play, live_ms: liveMs, past, past_ms: pastMs, past_wait_ms: Math.round(performance.now() - p0), back, levels, changed, statuses: [...statuses], probe, snapshots: [livePath, pastPath] } });
    },
  });
});
