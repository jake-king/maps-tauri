// The native renderer's search box. Coordinates are read in the frontend by the WASM build of
// crates/coords (no round trip); place names go to the Rust `search` command over the GeoNames index.
// Both answer with the same Carmen features the MapLibre geocoder used. Choosing a result flies the
// map there and drops a marker.

const DEBOUNCE_MS = 120;

export function createSearchBox(map, { invoke, coordFeatures, container }) {
  const root = document.createElement("div");
  root.style.cssText = "position:absolute;top:10px;left:10px;z-index:20;width:300px;font:13px 'Noto Sans',sans-serif;";
  root.innerHTML = `
    <input type="search" placeholder="Search places or type a coordinate" autocomplete="off" spellcheck="false"
      style="width:100%;box-sizing:border-box;padding:8px 10px;border:1px solid #bbb;border-radius:6px;
             background:#fff;box-shadow:0 1px 4px #0002;font:inherit;outline:none">
    <ul style="list-style:none;margin:4px 0 0;padding:0;background:#fff;border-radius:6px;box-shadow:0 2px 8px #0003;
               overflow:hidden;display:none"></ul>`;
  container.appendChild(root);
  const input = root.querySelector("input");
  const list = root.querySelector("ul");
  let results = [];
  let active = -1;
  let seq = 0;
  let timer = null;

  const marker = document.createElement("div");
  marker.style.cssText = "position:absolute;z-index:15;width:16px;height:16px;margin:-16px 0 0 -8px;display:none;" +
    "border-radius:50% 50% 50% 0;transform:rotate(-45deg);background:#e5484d;border:2px solid #fff;box-shadow:0 1px 3px #0006;pointer-events:none";
  container.appendChild(marker);
  let markerAt = null;
  map.on("render", () => {
    if (!markerAt) return;
    const p = map.project(markerAt);
    marker.style.left = `${p.x}px`;
    marker.style.top = `${p.y}px`;
  });

  function render() {
    list.innerHTML = "";
    list.style.display = results.length ? "block" : "none";
    results.forEach((f, i) => {
      const li = document.createElement("li");
      li.textContent = f.place_name;
      li.style.cssText = `padding:7px 10px;cursor:pointer;border-top:${i ? "1px solid #eee" : "0"};` +
        `background:${i === active ? "#eef4ff" : "#fff"}`;
      li.addEventListener("mousedown", (e) => {
        e.preventDefault();
        choose(i);
      });
      list.appendChild(li);
    });
  }

  async function query(text) {
    const mine = ++seq;
    let feats = [];
    if (text.trim().length >= 2) {
      try {
        feats = JSON.parse(await coordFeatures(text, 8)); // loads the parser on first use
        if (!feats.length) feats = await invoke("search", { query: text, limit: 8 });
      } catch (e) {
        feats = [];
        invoke("report", { msg: `search failed: ${e?.message ?? e}` }).catch(() => {});
      }
    }
    if (mine !== seq) return results; // a newer keystroke won
    results = feats;
    active = feats.length ? 0 : -1;
    render();
    return results;
  }

  function choose(i) {
    const f = results[i];
    if (!f) return;
    const [lng, lat] = f.center;
    input.value = f.text ?? f.place_name;
    results = [];
    render();
    markerAt = [lng, lat];
    marker.style.display = "block";
    map.easeTo({ center: [lng, lat], zoom: Math.max(map.getZoom(), f.place_type?.[0] === "coordinate" ? 15 : 14), duration: 800 });
  }

  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => query(input.value), DEBOUNCE_MS);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!results.length) return;
      active = (active + (e.key === "ArrowDown" ? 1 : results.length - 1)) % results.length;
      render();
    } else if (e.key === "Enter") {
      e.preventDefault();
      choose(Math.max(0, active));
    } else if (e.key === "Escape") {
      results = [];
      render();
      input.blur();
    }
  });

  // for MAPS_SEARCH: type a query, take the first result, as a user would
  return {
    /** Paint the box's text and the marker where the DOM has them, for a snapshot. */
    paint(ctx, dpr) {
      ctx.save();
      ctx.scale(dpr, dpr);
      ctx.fillStyle = "#fff";
      ctx.fillRect(10, 10, 300, 34);
      ctx.strokeStyle = "#bbb";
      ctx.strokeRect(10.5, 10.5, 299, 33);
      ctx.fillStyle = "#222";
      ctx.font = "13px 'Noto Sans', sans-serif";
      ctx.textBaseline = "middle";
      ctx.fillText(input.value, 20, 27);
      if (marker.style.display === "block") {
        const x = parseFloat(marker.style.left), y = parseFloat(marker.style.top);
        ctx.fillStyle = "#e5484d";
        ctx.strokeStyle = "#fff";
        ctx.lineWidth = 2;
        ctx.beginPath();
        ctx.arc(x, y - 10, 8, 0, 2 * Math.PI);
        ctx.fill();
        ctx.stroke();
        ctx.beginPath();
        ctx.moveTo(x - 5, y - 5);
        ctx.lineTo(x, y);
        ctx.lineTo(x + 5, y - 5);
        ctx.fill();
      }
      ctx.restore();
    },
    async run(text) {
      input.value = text;
      const r = await query(text);
      if (r.length) choose(0);
      return r.map((f) => f.place_name);
    },
  };
}
