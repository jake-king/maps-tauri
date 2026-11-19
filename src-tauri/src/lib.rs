//! The desktop app: Tauri's setup, the `tiles://` scheme, and the commands the frontend calls.
//!
//! `main.rs` only calls [`run`]. [`search`] is public so the search examples and parity tools can
//! use the same code as the app.

mod devstats;
mod diskcache;
mod metars;
pub mod search;
mod tiles;
#[cfg(desktop)]
mod update;
mod weather;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rusqlite::Connection;
use serde_json::Value;

/// Prints a status line from the frontend, so a run can be checked from the terminal.
#[tauri::command]
fn report(msg: String) {
    println!("[frontend] {msg}");
}

/// The gazetteer connection. `None` when data/geonames.db is missing: the map still works and
/// search answers coordinates only.
struct Gazetteer(Option<Mutex<Connection>>);

/// The search box's backend. `async` in the attribute runs this sync function on Tauri's thread
/// pool instead of the main thread, so a slow query never freezes the window.
#[tauri::command(async)]
fn search(query: String, limit: Option<usize>, db: tauri::State<'_, Gazetteer>) -> Vec<Value> {
    let limit = limit.unwrap_or(search::DEFAULT_LIMIT);
    match &db.0 {
        Some(conn) => search::search(&conn.lock().expect("gazetteer lock"), &query, limit),
        None => coords::parse_coords(&query)
            .into_iter()
            .take(limit)
            .map(|(lat, lon)| coords::coord_feature(lat, lon))
            .collect(),
    }
}

/// One poll of the F4 overlay's backend numbers: per-process memory and the tile handler's timings
/// since the previous poll.
#[tauri::command(async)]
fn dev_stats(
    tiles: tauri::State<'_, Arc<devstats::TileTimes>>,
    stages: tauri::State<'_, Arc<devstats::StageTimes>>,
    cache: tauri::State<'_, Arc<tiles::MeshCache>>,
) -> serde_json::Value {
    let (entries, bytes, hits, misses) = cache.take_stats();
    serde_json::json!({
        "memory": devstats::process_tree_memory(),
        "tiles": tiles.take(),
        "stages": stages.take(),
        "mesh_cache": { "entries": entries, "bytes": bytes, "hits": hits, "misses": misses },
    })
}

/// Development switches from the environment, so a benchmark can compare settings without code
/// edits. `MAPS_BENCH=1` runs the pan benchmark at startup; `MAPS_CACHE_ZOOMS=<n>` overrides
/// MapLibre's maxTileCacheZoomLevels, `MAPS_WORKERS=<n>` its worker count (the frontend's defaults
/// when unset).
#[tauri::command]
fn dev_flags() -> serde_json::Value {
    let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
    serde_json::json!({
        "bench": std::env::var("MAPS_BENCH").is_ok_and(|v| v == "1"),
        "cacheZoomLevels": num("MAPS_CACHE_ZOOMS"),
        "workers": num("MAPS_WORKERS"),
        "noLabels": std::env::var("MAPS_NO_LABELS").is_ok_and(|v| v == "1"),
        "pixelCheck": std::env::var("MAPS_PIXEL_CHECK").is_ok_and(|v| v == "1"),
        // MAPS_MEMPROBE=blank|idle|pan: report per-process memory at that stage, then exit
        "memprobe": std::env::var("MAPS_MEMPROBE").ok(),
        // MAPS_RENDERER=maplibre: MapLibre GL instead of the native renderer (the default)
        "renderer": std::env::var("MAPS_RENDERER").ok(),
        // MAPS_WEATHER=1: weather on at startup, then settle and snapshot it (an end-to-end check)
        "weather": std::env::var("MAPS_WEATHER").is_ok_and(|v| v == "1"),
        // MAPS_WIND_MOTION=1: winds alone, then a pan and a zoom, frames saved during and after each
        "windMotion": std::env::var("MAPS_WIND_MOTION").is_ok_and(|v| v == "1"),
        // MAPS_F4=0|1: the overlay closed or open, whatever the last session left (it is remembered,
        // and an open overlay draws every frame: every benchmark sets it)
        "f4": std::env::var("MAPS_F4").ok().map(|v| v == "1"),
        // MAPS_NO_FALLBACK=1: the native renderer draws nothing for a tile still loading (a control)
        "noFallback": std::env::var("MAPS_NO_FALLBACK").is_ok_and(|v| v == "1"),
        // MAPS_PREFETCH_RING=<n>: tiles beyond the view the native renderer fetches ahead (default 1)
        "prefetchRing": num("MAPS_PREFETCH_RING"),
        // MAPS_BENCH_SPEED=<px/s>: the pan benchmark's speed (default 4000), for stress runs
        "benchSpeed": num("MAPS_BENCH_SPEED"),
        // MAPS_SNAPSHOT=<name>: settle at a fixed view, save the frame as logs/snapshot-<name>.png, exit
        "snapshot": std::env::var("MAPS_SNAPSHOT").ok(),
        "snapshotZoom": std::env::var("MAPS_SNAPSHOT_ZOOM").ok().and_then(|v| v.parse::<f64>().ok()),
        // MAPS_SNAPSHOT_BEARING=<degrees clockwise from north>
        "snapshotBearing": std::env::var("MAPS_SNAPSHOT_BEARING").ok().and_then(|v| v.parse::<f64>().ok()),
        // MAPS_SNAPSHOT_CENTER=lng,lat
        "snapshotCenter": std::env::var("MAPS_SNAPSHOT_CENTER").ok().and_then(|v| {
            let mut it = v.split(',').map(|x| x.trim().parse::<f64>());
            match (it.next(), it.next()) { (Some(Ok(a)), Some(Ok(b))) => Some(vec![a, b]), _ => None }
        }),
        // MAPS_ZOOM_BENCH=1 (with MAPS_PIXEL_CHECK=1): the zoom out/in benchmark, worst frame saved
        "zoomBench": std::env::var("MAPS_ZOOM_BENCH").is_ok_and(|v| v == "1"),
        // MAPS_STRESS=<ops>: the stress benchmark, that many seeded camera operations (MAPS_STRESS_SEED, default 1)
        "stress": num("MAPS_STRESS"),
        "stressSeed": num("MAPS_STRESS_SEED"),
        // MAPS_SEARCH=<query>: run the native search box end to end and snapshot the result
        "search": std::env::var("MAPS_SEARCH").ok(),
        // MAPS_CSP_PROBE=1: try one request the content security policy must refuse (a control for
        // tools/offline.sh); the address is TEST-NET-1, never routed, in case the policy lets it out
        "cspProbe": std::env::var("MAPS_CSP_PROBE").is_ok_and(|v| v == "1"),
        // MAPS_EMPTY_STYLE=1: a background layer only, no source, tiles or fonts (a memory control)
        "emptyStyle": std::env::var("MAPS_EMPTY_STYLE").is_ok_and(|v| v == "1"),
        // on unless MAPS_CACHED_PARENTS=0 (bench/2026-11-15-px-parents*.json)
        "cachedParents": !std::env::var("MAPS_CACHED_PARENTS").is_ok_and(|v| v == "0"),
    })
}

/// The benchmark's result: printed as one `[bench]` JSON line, and under `MAPS_BENCH=1` the app
/// then exits, so a scripted run ends by itself instead of leaving a window open.
#[tauri::command]
fn bench_done(result: serde_json::Value, app: tauri::AppHandle) {
    println!("[bench] {result}");
    if std::env::var("MAPS_BENCH").is_ok_and(|v| v == "1") {
        app.exit(0);
    }
}

/// The archive the native renderer reads, and where it came from ("remote", "cached": the remote
/// planet's disk cache with no network, or "local").
struct ArchiveState(Arc<Option<vtile::pmtiles::Archive>>, &'static str);

/// The archive's bounds, center and zoom range, read by vtile from its header: what the native
/// renderer needs to place its camera, without loading a JavaScript tile library.
#[tauri::command]
fn archive_info(state: tauri::State<'_, ArchiveState>) -> Result<serde_json::Value, String> {
    let a = state
        .0
        .as_ref()
        .as_ref()
        .ok_or("no archive could be opened")?;
    let h = &a.header;
    Ok(serde_json::json!({
        "minZoom": h.min_zoom, "maxZoom": h.max_zoom,
        "minLon": h.bounds[0], "minLat": h.bounds[1], "maxLon": h.bounds[2], "maxLat": h.bounds[3],
        "source": state.1,
    }))
}

/// The remote planet, if one is configured and not switched off: `MAPS_ARCHIVE_URL`, else the URL in
/// the gitignored `data/remote.url` (so a LAN hostname stays out of the repository).
/// `MAPS_LOCAL_ONLY=1` switches it off (the benchmark scripts set it).
fn remote_url() -> Option<String> {
    if std::env::var("MAPS_LOCAL_ONLY").is_ok_and(|v| v == "1") {
        return None;
    }
    std::env::var("MAPS_ARCHIVE_URL")
        .ok()
        .or_else(|| std::fs::read_to_string(data_dir().join("remote.url")).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
}

/// The remote planet through the disk cache ("remote"), or, when the server cannot be reached, the
/// cache's copy of it ("cached": what was seen before, nothing else). None when neither opens.
fn open_remote(url: &str) -> Option<(vtile::pmtiles::Archive, &'static str)> {
    let cache_path = diskcache::configured_path();
    let max = diskcache::configured_max_bytes();
    let online = match &cache_path {
        Some(path) => vtile::pmtiles::Archive::open_url_cached(url, |version| {
            // a cache file that will not open costs the cache, never the planet
            match diskcache::DiskCache::open(path, url, version, max) {
                Ok(c) => {
                    println!(
                        "[cache] {} ({} MB stored for this version)",
                        path.display(),
                        c.bytes() >> 20
                    );
                    Ok(Box::new(c))
                }
                Err(e) => {
                    eprintln!(
                        "[cache] {} will not open ({e}); reading without a cache",
                        path.display()
                    );
                    Ok(Box::new(diskcache::NoCache))
                }
            }
        }),
        None => vtile::pmtiles::Archive::open_url(url),
    };
    let e = match online {
        Ok(a) => {
            println!(
                "[archive] remote planet, zooms {}-{}",
                a.header.min_zoom, a.header.max_zoom
            );
            return Some((a, "remote"));
        }
        Err(e) => e,
    };
    let cached = cache_path
        .and_then(|p| diskcache::DiskCache::open_last(&p, url, max).ok().flatten())
        .and_then(|c| {
            let what = format!("version {}, {} MB", c.version(), c.bytes() >> 20);
            vtile::pmtiles::Archive::open_cached(Box::new(c))
                .ok()
                .map(|a| (a, what))
        });
    match cached {
        Some((a, what)) => {
            eprintln!("[archive] remote planet unreachable ({e}); reading its cached copy ({what}), offline");
            Some((a, "cached"))
        }
        None => {
            eprintln!("[archive] remote planet unreachable ({e}); using the local extract");
            None
        }
    }
}

/// Saves a frame the frontend captured (a PNG data URL) as `snapshot-<name>.png` in `MAPS_SNAPSHOT_DIR`
/// (default logs/), for looking at
/// a renderer's output without the screen. Development only.
#[tauri::command]
fn save_snapshot(name: String, data_url: String) -> Result<String, String> {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("snapshot name: letters, digits, - and _ only".into());
    }
    let b64 = data_url
        .strip_prefix("data:image/png;base64,")
        .ok_or("not a PNG data URL")?;
    let png = base64_decode(b64).ok_or("bad base64")?;
    // MAPS_SNAPSHOT_DIR: where tools/e2e.py collects them per platform; else logs/
    let dir = std::env::var("MAPS_SNAPSHOT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../logs")));
    let path = dir.join(format!("snapshot-{name}.png"));
    std::fs::write(&path, png).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

/// Standard base64 (RFC 4648) without a crate: 30 lines for a development command.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes: Vec<u8> = s
        .bytes()
        .filter(|&c| c != b'=' && !c.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// The basemap archive's file name in the data directory: `MAPS_ARCHIVE` (letters, digits, `-`, `_`,
/// `.` only); else `westla.pmtiles` (Malibu to downtown, for using the app) when present; else
/// `basemap.pmtiles`, the small Santa Monica cutout. The benchmark scripts pin `basemap.pmtiles`
/// so their numbers stay comparable over time.
fn archive_name() -> String {
    std::env::var("MAPS_ARCHIVE")
        .ok()
        .filter(|n| {
            !n.is_empty()
                && !n.starts_with('.')
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        })
        .unwrap_or_else(|| {
            if data_dir().join("westla.pmtiles").exists() {
                "westla.pmtiles"
            } else {
                "basemap.pmtiles"
            }
            .into()
        })
}

/// Where the archive and the gazetteer live: `MAPS_DATA`, else the repo's `data/`.
fn data_dir() -> PathBuf {
    std::env::var_os("MAPS_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../data")))
}

/// Turn the weather layer on: the first call starts fetching from NOAA (nothing does before it).
#[tauri::command]
fn wx_enable(
    app: tauri::AppHandle,
    w: tauri::State<'_, Arc<weather::Weather>>,
    m: tauri::State<'_, Arc<metars::Metars>>,
) -> Value {
    use tauri::Emitter;
    // every frame and forecast hour that arrives: tell the webview at once rather than wait for a poll
    let changed = move || {
        let _ = app.emit("wx-changed", ());
    };
    let changed: Arc<dyn Fn() + Send + Sync> = Arc::new(changed);
    w.enable(Arc::new(weather::Noaa::new()), changed.clone());
    let agent = wx::fetch::agent();
    m.start(move || wx::fetch::metars(&agent), changed);
    w.state(unix_now(), unix_now())
}

/// What to show at `at` (Unix seconds; default now): the radar loop, satellite image and hazard
/// snapshot for that time, what exists over the time bar, and how fresh it all is.
#[tauri::command]
fn wx_state(at: Option<i64>, w: tauri::State<'_, Arc<weather::Weather>>) -> Value {
    let now = unix_now();
    w.state(at.unwrap_or(now), now)
}

/// One radar frame's cells, one byte each (`wx::radar`'s code), as raw bytes rather than JSON,
/// fetched from NOAA first if not held. The fetch blocks (up to a minute on a bad network), so it runs
/// on the blocking pool: a sync command marked `async` would run on an async worker instead, which
/// Tauri's other async commands share.
#[tauri::command]
async fn wx_frame(
    time: i64,
    w: tauri::State<'_, Arc<weather::Weather>>,
) -> Result<tauri::ipc::Response, String> {
    let w = Arc::clone(&w);
    let src = w.source().ok_or("weather is off")?;
    let bytes = tauri::async_runtime::spawn_blocking(move || w.radar_frame(src.as_ref(), time))
        .await
        .map_err(|e| format!("radar frame task: {e}"))??;
    Ok(tauri::ipc::Response::new(bytes))
}

/// One satellite image's grey levels (`wx::satellite`), one byte a pixel, fetched first if not held;
/// on the blocking pool, as `wx_frame`.
#[tauri::command]
async fn wx_satellite(
    time: i64,
    w: tauri::State<'_, Arc<weather::Weather>>,
) -> Result<tauri::ipc::Response, String> {
    let w = Arc::clone(&w);
    let src = w.source().ok_or("weather is off")?;
    let bytes = tauri::async_runtime::spawn_blocking(move || w.satellite_image(src.as_ref(), time))
        .await
        .map_err(|e| format!("satellite task: {e}"))??;
    Ok(tauri::ipc::Response::new(bytes))
}

/// The GFS wind field at flight level `fl` (default FL340) for `at` (default now): a 48-byte header
/// (`wx::wind::encode`) then U and V as f32; fetched from NOMADS first if not held, on the blocking pool.
#[tauri::command]
async fn wx_wind(
    at: Option<i64>,
    fl: Option<u32>,
    w: tauri::State<'_, Arc<weather::Weather>>,
) -> Result<tauri::ipc::Response, String> {
    let w = Arc::clone(&w);
    let src = w.source().ok_or("weather is off")?;
    let now = unix_now();
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        w.wind(src.as_ref(), at.unwrap_or(now), fl, now)
    })
    .await
    .map_err(|e| format!("wind task: {e}"))??;
    Ok(tauri::ipc::Response::new(bytes))
}

/// One global infrared mosaic image (hourly, the world outside GOES's view), grey levels on the GOES
/// scale, fetched first if not held; on the blocking pool.
#[tauri::command]
async fn wx_satellite_global(
    time: i64,
    w: tauri::State<'_, Arc<weather::Weather>>,
) -> Result<tauri::ipc::Response, String> {
    let w = Arc::clone(&w);
    let src = w.source().ok_or("weather is off")?;
    let bytes =
        tauri::async_runtime::spawn_blocking(move || w.satellite_global_image(src.as_ref(), time))
            .await
            .map_err(|e| format!("global satellite task: {e}"))??;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Everything time `at` will need, fetched into Rust's caches without sending it to the webview: the
/// newest radar frame of its loop, its satellite and global images, its SIGMETs, and (with `fl`) its
/// winds. For buffering a time-lapse before it plays; returns the selection.
#[tauri::command]
async fn wx_prefetch(
    at: i64,
    winds: bool,
    fl: Option<u32>,
    w: tauri::State<'_, Arc<weather::Weather>>,
) -> Result<Value, String> {
    let w = Arc::clone(&w);
    let src = w.source().ok_or("weather is off")?;
    let now = unix_now();
    tauri::async_runtime::spawn_blocking(move || {
        let sel = w.select(at);
        if let Some(&t) = sel.radar.last() {
            let _ = w.radar_frame(src.as_ref(), t);
        }
        if let Some(t) = sel.satellite {
            let _ = w.satellite_image(src.as_ref(), t);
        }
        if let Some(t) = w.global_for(at) {
            let _ = w.satellite_global_image(src.as_ref(), t);
        }
        let _ = w.sigmets(src.as_ref(), at, now);
        if winds {
            let _ = w.wind(src.as_ref(), at, fl, now);
        }
        w.state(at, now)["selection"].clone()
    })
    .await
    .map_err(|e| format!("prefetch task: {e}"))
}

/// The SIGMETs (international and US convective) in force at `at` (default now). A past time's are
/// fetched from aviationweather.gov first if not held, so this runs on the blocking pool.
#[tauri::command]
async fn wx_sigmets(
    at: Option<i64>,
    w: tauri::State<'_, Arc<weather::Weather>>,
) -> Result<Value, String> {
    let w = Arc::clone(&w);
    let src = w.source().ok_or("weather is off")?;
    let now = unix_now();
    tauri::async_runtime::spawn_blocking(move || w.sigmets(src.as_ref(), at.unwrap_or(now), now))
        .await
        .map_err(|e| format!("sigmets task: {e}"))
}

/// The METARs in force at `at` (default now), compact: [id, lon, lat, category, time] each, with the
/// range collected and a version that changes when anything new arrives.
#[tauri::command]
fn wx_metars(at: Option<i64>, m: tauri::State<'_, Arc<metars::Metars>>) -> Value {
    m.at(at.unwrap_or_else(unix_now))
}

/// One station's report in force at `at`: its raw text and observation time.
#[tauri::command]
fn wx_metar(
    id: String,
    at: Option<i64>,
    m: tauri::State<'_, Arc<metars::Metars>>,
) -> Option<(String, i64)> {
    m.report(&id, at.unwrap_or_else(unix_now))
}

/// Now, in Unix seconds: the system clock, or the tape's when replaying recorded weather (`wx::tape`).
pub(crate) fn unix_now() -> i64 {
    wx::tape::now()
}

/// Start the app: open the tile archive, the gazetteer and the weather sources, register the commands and the `tiles://`
/// scheme, then run Tauri's event loop until the window closes.
///
/// # Panics
///
/// If Tauri fails to start (no display, a broken configuration).
pub fn run() {
    let data = data_dir();
    let gazetteer = match search::open(&data.join("geonames.db")) {
        Ok(conn) => Gazetteer(Some(Mutex::new(conn))),
        Err(e) => {
            eprintln!("gazetteer unavailable ({e}); search answers coordinates only");
            Gazetteer(None)
        }
    };
    // The native renderer reads the same archive through vtile; opened once, read with pread from
    // any thread.
    let archive_file = archive_name();
    // The native renderer reads the remote planet when one is configured and reachable (through the
    // disk cache), else the cached copy of it with no network, else the local extract. The MapLibre
    // app always reads the local file through tiles://basemap.pmtiles.
    let (archive, source) = match remote_url() {
        Some(url) => open_remote(&url).map_or_else(
            || {
                (
                    vtile::pmtiles::Archive::open(&data.join(&archive_file)).ok(),
                    "local",
                )
            },
            |(a, s)| (Some(a), s),
        ),
        None => (
            vtile::pmtiles::Archive::open(&data.join(&archive_file)).ok(),
            "local",
        ),
    };
    let archive = Arc::new(archive);
    let tile_times = Arc::new(devstats::TileTimes::default());
    let handler_times = tile_times.clone();
    let stage_times = Arc::new(devstats::StageTimes::default());
    // The Rust mesh cache, MAPS_MESH_CACHE_MB (default 0: off). With the renderer's prefetch ring
    // every tile was ready before it was visible in every benchmark run, up to 16,000 px/s, with or
    // without this cache; it cost ~20 MB and halved only requests already ahead of the view
    // (bench/2026-11-15-{nc-ring1,small-gpu-rust*,fast-rust*}.json). Kept for cases prefetch cannot
    // cover, off until one is measured.
    let cache_mb: usize = std::env::var("MAPS_MESH_CACHE_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mesh_cache = Arc::new(tiles::MeshCache::new(cache_mb << 20));
    let handler_cache = mesh_cache.clone();
    let handler_stages = stage_times.clone();
    let log_tiles = std::env::var("MAPS_LOG_TILES").is_ok_and(|v| v == "1");
    // A control for the benchmark's gray-box measure: slow every tile response down on purpose.
    // If gray frames do not climb as this grows, the measure is not measuring.
    let tile_delay = std::time::Duration::from_millis(
        std::env::var("MAPS_TILE_DELAY_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    );
    tauri::Builder::default()
        .manage(gazetteer)
        .manage(ArchiveState(archive.clone(), source))
        .manage(tile_times)
        .manage(stage_times)
        .manage(mesh_cache)
        .manage(Arc::new(weather::Weather::open(
            weather::configured_path().as_deref(),
        )))
        .manage(Arc::new(metars::Metars::open(
            weather::configured_path()
                .map(|p| p.with_file_name("metars.sqlite"))
                .as_deref(),
        )))
        // Asynchronous so a file read never runs on the UI thread: the handler hands the request
        // to a blocking-pool thread and answers through the responder when the read is done.
        .register_asynchronous_uri_scheme_protocol("tiles", move |_ctx, request, responder| {
            let data = data.clone();
            let archive_file = archive_file.clone();
            let times = handler_times.clone();
            let archive = archive.clone();
            let stages = handler_stages.clone();
            let cache = handler_cache.clone();
            let queued = Instant::now();
            tauri::async_runtime::spawn_blocking(move || {
                use std::sync::atomic::Ordering;
                let wait = queued.elapsed();
                let inflight = stages.inflight.fetch_add(1, Ordering::Relaxed) + 1;
                if !tile_delay.is_zero() {
                    std::thread::sleep(tile_delay);
                }
                let t0 = Instant::now();
                let res = match (
                    tiles::parse_mesh_path(request.uri().path()),
                    archive.as_ref(),
                ) {
                    (Some((z, x, y)), Some(a)) => {
                        let (res, st) = tiles::handle_mesh_cached(a, &cache, z, x, y);
                        stages.record(wait, &st, inflight);
                        res
                    }
                    _ => tiles::handle(&data, &archive_file, &request),
                };
                stages.inflight.fetch_sub(1, Ordering::Relaxed);
                times.record(t0.elapsed(), res.body().len());
                if log_tiles {
                    let range = request
                        .headers()
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-");
                    println!(
                        "[tiles] {} {} range={range} -> {} ({} B)",
                        request.method(),
                        request.uri().path(),
                        res.status().as_u16(),
                        res.body().len()
                    );
                }
                responder.respond(res);
            });
        })
        // The updater exists only in a build that names a release manifest (update.rs); with
        // MAPS_UPDATE=install it installs what it finds and exits (tools/update-e2e.sh).
        .setup(|app| {
            #[cfg(desktop)]
            {
                use tauri::Manager as _;
                app.manage(update::Pending::default());
                if let Some(plugin) = update::plugin() {
                    app.handle().plugin(plugin)?;
                    if std::env::var("MAPS_UPDATE").is_ok_and(|v| v == "install") {
                        update::install_now(app.handle().clone());
                    }
                }
            }
            #[cfg(not(desktop))]
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            report,
            search,
            dev_stats,
            dev_flags,
            bench_done,
            archive_info,
            save_snapshot,
            wx_enable,
            wx_state,
            wx_frame,
            wx_satellite,
            wx_metars,
            wx_metar,
            wx_wind,
            wx_sigmets,
            wx_satellite_global,
            wx_prefetch,
            #[cfg(desktop)]
            update::update_check,
            #[cfg(desktop)]
            update::update_install
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
