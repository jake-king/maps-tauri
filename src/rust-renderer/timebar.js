// The weather time bar, after the GFA's time slider: one time for every layer, from 18 hours back
// (observations) to 12 hours ahead (forecasts), in UTC. Under the slider, what exists over the range:
// radar and satellite history as strips, hazard snapshot times as marks, and "now". Elements are made
// with createElement and styled from script: the content security policy refuses style="" in markup.

import { zulu, relative } from "./weather.js";

export const STEP = 600; // the bar moves in 10-minute steps
export const AHEAD = 12 * 3600; // G-AIRMETs reach 12 hours ahead
export const LEVELS = [null, 30, 60, 90, 120, 150, 180, 240, 300, 340, 390, 450];

/** The bar's range for `now`: [start, end], on the step. */
export function range(now, history) {
  return [Math.floor((now - history) / STEP) * STEP, Math.ceil((now + AHEAD) / STEP) * STEP];
}

/** A slider value to the time to show: null (live) within one step of now, else the value. */
export function chosen(value, now) {
  return Math.abs(value - now) < STEP ? null : Math.floor(value / STEP) * STEP;
}

/** One step from `at` (null = live) in direction `dir`, kept inside the bar's range `[start, end]`.
 *  From live the first step goes to the nearest mark a whole step away, so it always leaves live (a
 *  mark less than a step from now would read as live again); a step that lands within a step of now
 *  returns to live. */
export function stepFrom(at, now, dir, [start, end]) {
  let t = at == null ? (dir > 0 ? Math.ceil((now + STEP) / STEP) : Math.floor((now - STEP) / STEP)) * STEP : at + dir * STEP;
  t = Math.max(start, Math.min(end, t));
  return Math.abs(t - now) < STEP ? null : t;
}

/** Where a time-lapse starts: the chosen past time, or (from live or the future, which has no
 *  observations to play) the last three hours. */
export function playStart(at, now, [start]) {
  if (at != null && at < now) return at;
  return Math.max(start, Math.floor((now - 3 * 3600) / STEP) * STEP);
}

/** The time-lapse's next step after `t`, or null when that would be within a step of now: live. */
export function nextPlay(t, now) {
  const n = t + STEP;
  return now - n < STEP ? null : n;
}

/** Every step of a time-lapse from `at` (see playStart), in order, up to the last before live. */
export function playSteps(at, now, r) {
  const out = [];
  for (let t = playStart(at, now, r); t !== null && out.length < 200; t = nextPlay(t, now)) out.push(t);
  return out;
}

/** Position of `t` along the bar, 0..1, clamped. */
export function along(t, [start, end]) {
  return Math.max(0, Math.min(1, (t - start) / (end - start)));
}

const el = (tag, css, text) => {
  const e = document.createElement(tag);
  if (css) e.style.cssText = css;
  if (text != null) e.textContent = text;
  return e;
};

export class TimeBar {
  constructor(container, { onTime, onLevel, onLoop, isReady = () => true, onPlay = () => {}, bufferSteps = async () => {}, prepareStep = () => {}, isPrepared = () => true }) {
    this.onTime = onTime;
    Object.assign(this, { isReady, onPlay, bufferSteps, prepareStep, isPrepared });
    this.root = el("div", "position:absolute;left:290px;right:16px;bottom:10px;z-index:20;padding:6px 10px 4px;background:#fffe;" +
      "border-radius:6px;box-shadow:0 1px 4px #0003;font:12px 'Noto Sans',sans-serif;color:#222;display:none;user-select:none;");
    const row = el("div", "display:flex;align-items:center;gap:6px;margin-bottom:4px;");
    const button = (label, title, fn) => {
      const b = el("button", "font:12px 'Noto Sans',sans-serif;padding:2px 8px;border:1px solid #bbb;border-radius:4px;background:#fff;cursor:pointer;", label);
      b.title = title;
      b.addEventListener("click", fn);
      row.append(b);
      return b;
    };
    button("◀", "10 minutes back", () => this.step(-1));
    this.nowButton = button("Now", "Back to live", () => this.pick(null));
    button("▶", "10 minutes on", () => this.step(1));
    this.playButton = button("Play", "Play the hours up to now", () => this.toggle());
    this.label = el("span", "min-width:150px;font-weight:600;margin:0 6px;");
    row.append(this.label);
    const loop = el("label", "cursor:pointer;");
    this.loopBox = el("input", "vertical-align:middle;margin:0 4px 0 0;");
    this.loopBox.type = "checkbox";
    this.loopBox.checked = true;
    this.loopBox.addEventListener("change", () => onLoop(this.loopBox.checked));
    loop.append(this.loopBox, "Loop radar");
    row.append(loop);
    row.append(el("span", "flex:1;"));
    const fl = el("label", "", "Level ");
    fl.title = "Hazards within 3,000 ft of this level; winds at it (all levels: FL340)";
    this.levelSel = el("select", "font:12px 'Noto Sans',sans-serif;");
    for (const v of LEVELS) {
      const o = el("option", null, v == null ? "all levels" : `FL${String(v).padStart(3, "0")}`);
      o.value = v == null ? "" : String(v);
      this.levelSel.append(o);
    }
    this.levelSel.addEventListener("change", () => onLevel(this.levelSel.value === "" ? null : Number(this.levelSel.value)));
    fl.append(this.levelSel);
    row.append(fl);
    this.root.append(row);

    // the track: availability strips and marks under a range input that takes the drag
    this.track = el("div", "position:relative;height:40px;");
    this.radarStrip = el("div", "position:absolute;top:4px;height:5px;background:#16a34a99;border-radius:2px;");
    this.radarStrip.title = "radar history";
    this.satStrip = el("div", "position:absolute;top:11px;height:5px;background:#2563eb99;border-radius:2px;");
    this.satStrip.title = "satellite history";
    this.metarStrip = el("div", "position:absolute;top:18px;height:4px;background:#d97706aa;border-radius:2px;");
    this.metarStrip.title = "METAR history collected";
    this.marks = el("div", "position:absolute;inset:0;pointer-events:none;");
    this.nowLine = el("div", "position:absolute;top:0;height:22px;width:2px;background:#dc2626;");
    this.ticks = el("div", "position:absolute;left:0;right:0;top:24px;height:14px;font-size:10px;color:#555;pointer-events:none;");
    this.input = el("input", "position:absolute;left:0;right:0;top:0;width:100%;height:22px;margin:0;background:transparent;cursor:pointer;");
    this.input.type = "range";
    this.input.step = String(STEP);
    // while a hand is on the slider, updates from Rust must not move it back
    this.input.addEventListener("pointerdown", () => (this.dragging = true));
    for (const ev of ["pointerup", "pointercancel"]) window.addEventListener(ev, () => (this.dragging = false));
    this.input.addEventListener("input", () => {
      clearTimeout(this.debounce);
      const v = Number(this.input.value);
      this.showLabel(chosen(v, this.now) ?? this.now);
      this.debounce = setTimeout(() => this.pick(chosen(v, this.now)), 120);
    });
    this.track.append(this.radarStrip, this.satStrip, this.metarStrip, this.marks, this.nowLine, this.input, this.ticks);
    this.root.append(this.track);
    container.append(this.root);
    this.at = null;
    this.now = Date.now() / 1000;
  }

  show(on) {
    this.root.style.display = on ? "block" : "none";
  }

  pick(at) {
    this.at = at;
    this.layout();
    this.onTime(at);
  }

  /** Time-lapse. First every step's data is buffered (the button counts it), then it plays: a step
   *  moves on once it is on screen, at least 600 ms has passed, and the next step's textures are
   *  already on the GPU, so each change is a swap, not a load. Pressing again stops it. */
  async toggle() {
    if (this.player || this.buffering) return this.stop();
    const steps = playSteps(this.at, this.now, range(this.now, this.history ?? 18 * 3600));
    if (!steps.length) return;
    this.buffering = true;
    this.onPlay(true);
    this.playButton.textContent = `Buffering 0/${steps.length}`;
    await this.bufferSteps(steps, (k, n) => {
      if (this.buffering) this.playButton.textContent = `Buffering ${k}/${n}`;
    });
    if (!this.buffering) return; // stopped while buffering
    this.buffering = false;
    this.playButton.textContent = "Pause";
    let i = 0;
    this.pick(steps[0]);
    let shownAt = performance.now();
    this.prepareStep(steps[1] ?? null);
    this.player = setInterval(() => {
      const next = i + 1 < steps.length ? steps[i + 1] : null;
      if (!this.isReady() || performance.now() - shownAt < 600 || !this.isPrepared(next)) return;
      i += 1;
      this.pick(next);
      shownAt = performance.now();
      if (next === null) return this.stop();
      this.prepareStep(i + 1 < steps.length ? steps[i + 1] : null);
    }, 50);
  }

  stop() {
    clearInterval(this.player);
    this.player = null;
    this.buffering = false;
    this.playButton.textContent = "Play";
    this.onPlay(false);
  }

  step(dir) {
    this.pick(stepFrom(this.at, this.now, dir, range(this.now, this.history ?? 18 * 3600)));
  }

  showLabel(t) {
    this.label.textContent = this.at == null && t === this.now ? `${zulu(this.now)} · live` : `${zulu(t)} · ${relative(t, this.now)}`;
  }

  /** New state from Rust: move "now", the range, and what exists. */
  update(s, metarRange) {
    if (!s) return;
    this.now = s.now;
    this.history = s.history;
    this.available = s.available;
    this.metarRange = metarRange;
    this.layout();
  }

  layout() {
    const r = range(this.now, this.history ?? 18 * 3600);
    this.input.min = String(r[0]);
    this.input.max = String(r[1]);
    if (!this.dragging) this.input.value = String(this.at ?? this.now);
    this.nowButton.disabled = this.at == null;
    this.showLabel(this.at ?? this.now);
    const pct = (t) => `${(along(t, r) * 100).toFixed(2)}%`;
    const strip = (e, span) => {
      const [a, b] = span ?? [];
      e.style.display = a == null ? "none" : "block";
      if (a == null) return;
      e.style.left = pct(a);
      e.style.width = `calc(${pct(b)} - ${pct(a)})`;
    };
    strip(this.radarStrip, this.available?.radar);
    strip(this.satStrip, this.available?.satellite);
    strip(this.metarStrip, this.metarRange);
    this.nowLine.style.left = pct(this.now);
    this.marks.replaceChildren(...(this.available?.hazards ?? []).map((t) => {
      const m = el("div", `position:absolute;top:2px;width:3px;height:16px;background:#c026d3;left:${pct(t)};`);
      m.title = `hazards valid ${zulu(t)}`;
      return m;
    }));
    const ticks = [];
    for (let t = Math.ceil(r[0] / 10800) * 10800; t <= r[1]; t += 10800) {
      ticks.push(el("span", `position:absolute;left:${pct(t)};transform:translateX(-50%);`, zulu(t).slice(0, 2) + "Z"));
    }
    this.ticks.replaceChildren(...ticks);
  }
}
