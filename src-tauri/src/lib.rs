mod devstats;
pub mod search;
mod tiles;

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
/// MapLibre's maxTileCacheZoomLevels, `MAPS_WORKERS=<n>` its worker count, `MAPS_PREFETCH=<n>` its
/// prefetchZoomDelta (the frontend's defaults when unset).
#[tauri::command]
fn dev_flags() -> serde_json::Value {
    let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u32>().ok());
    serde_json::json!({
        "bench": std::env::var("MAPS_BENCH").is_ok_and(|v| v == "1"),
        "cacheZoomLevels": num("MAPS_CACHE_ZOOMS"),
        "workers": num("MAPS_WORKERS"),
        "prefetch": num("MAPS_PREFETCH"),
        "noLabels": std::env::var("MAPS_NO_LABELS").is_ok_and(|v| v == "1"),
        "warmZoom": num("MAPS_WARM_ZOOM"),
        "pixelCheck": std::env::var("MAPS_PIXEL_CHECK").is_ok_and(|v| v == "1"),
        "fadeMs": num("MAPS_FADE_MS"),
        // MAPS_MEMPROBE=blank|idle|pan: report per-process memory at that stage, then exit
        "memprobe": std::env::var("MAPS_MEMPROBE").ok(),
        // MAPS_RENDERER=rust: the native renderer (Rust decodes and meshes, WebGL draws)
        "renderer": std::env::var("MAPS_RENDERER").ok(),
        // MAPS_NO_FALLBACK=1: the native renderer draws nothing for a tile still loading (a control)
        "noFallback": std::env::var("MAPS_NO_FALLBACK").is_ok_and(|v| v == "1"),
        // MAPS_PREFETCH_RING=<n>: tiles beyond the view the native renderer fetches ahead (default 1)
        "prefetchRing": num("MAPS_PREFETCH_RING"),
        // MAPS_BENCH_SPEED=<px/s>: the pan benchmark's speed (default 4000), for stress runs
        "benchSpeed": num("MAPS_BENCH_SPEED"),
        // MAPS_SNAPSHOT=<name>: settle at a fixed view, save the frame as logs/snapshot-<name>.png, exit
        "snapshot": std::env::var("MAPS_SNAPSHOT").ok(),
        "snapshotZoom": std::env::var("MAPS_SNAPSHOT_ZOOM").ok().and_then(|v| v.parse::<f64>().ok()),
        // MAPS_SNAPSHOT_CENTER=lng,lat
        "snapshotCenter": std::env::var("MAPS_SNAPSHOT_CENTER").ok().and_then(|v| {
            let mut it = v.split(',').map(|x| x.trim().parse::<f64>());
            match (it.next(), it.next()) { (Some(Ok(a)), Some(Ok(b))) => Some(vec![a, b]), _ => None }
        }),
        // MAPS_ZOOM_BENCH=1 (with MAPS_PIXEL_CHECK=1): the zoom out/in benchmark, worst frame saved
        "zoomBench": std::env::var("MAPS_ZOOM_BENCH").is_ok_and(|v| v == "1"),
        // MAPS_SEARCH=<query>: run the native search box end to end and snapshot the result
        "search": std::env::var("MAPS_SEARCH").ok(),
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

/// The archive the native renderer reads, and where it came from ("remote" or "local").
struct ArchiveState(Arc<Option<vtile::pmtiles::Archive>>, &'static str);

/// The archive's bounds, center and zoom range, read by vtile from its header: what the native
/// renderer needs to place its camera, without loading a JavaScript tile library.
#[tauri::command]
fn archive_info(state: tauri::State<'_, ArchiveState>) -> Result<serde_json::Value, String> {
    let a = state.0.as_ref().as_ref().ok_or("no archive could be opened")?;
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

/// Saves a frame the frontend captured (a PNG data URL) as logs/snapshot-<name>.png, for looking at
/// a renderer's output without the screen. Development only.
#[tauri::command]
fn save_snapshot(name: String, data_url: String) -> Result<String, String> {
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("snapshot name: letters, digits, - and _ only".into());
    }
    let b64 = data_url.strip_prefix("data:image/png;base64,").ok_or("not a PNG data URL")?;
    let png = base64_decode(b64).ok_or("bad base64")?;
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../logs")).join(format!("snapshot-{name}.png"));
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
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=' && !c.is_ascii_whitespace()).collect();
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
        .filter(|n| !n.is_empty() && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)))
        .unwrap_or_else(|| {
            if data_dir().join("westla.pmtiles").exists() { "westla.pmtiles" } else { "basemap.pmtiles" }.into()
        })
}

/// Where the archive and the gazetteer live: `MAPS_DATA`, else the repo's `data/`.
fn data_dir() -> PathBuf {
    std::env::var_os("MAPS_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../data")))
}

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
    // The native renderer reads the remote planet when one is configured and reachable, else the
    // local extract. The MapLibre app always reads the local file through tiles://basemap.pmtiles.
    let (archive, source) = match remote_url().map(|u| vtile::pmtiles::Archive::open_url(&u)) {
        Some(Ok(a)) => {
            println!("[archive] remote planet, zooms {}-{}", a.header.min_zoom, a.header.max_zoom);
            (Some(a), "remote")
        }
        other => {
            if let Some(Err(e)) = other {
                eprintln!("[archive] remote planet unreachable ({e}); using the local extract");
            }
            (vtile::pmtiles::Archive::open(&data.join(&archive_file)).ok(), "local")
        }
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
    let cache_mb: usize = std::env::var("MAPS_MESH_CACHE_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mesh_cache = Arc::new(tiles::MeshCache::new(cache_mb << 20));
    let handler_cache = mesh_cache.clone();
    let handler_stages = stage_times.clone();
    let log_tiles = std::env::var("MAPS_LOG_TILES").is_ok_and(|v| v == "1");
    // A control for the benchmark's gray-box measure: slow every tile response down on purpose.
    // If gray frames do not climb as this grows, the measure is not measuring.
    let tile_delay = std::time::Duration::from_millis(
        std::env::var("MAPS_TILE_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
    );
    tauri::Builder::default()
        .manage(gazetteer)
        .manage(ArchiveState(archive.clone(), source))
        .manage(tile_times)
        .manage(stage_times)
        .manage(mesh_cache)
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
                let res = match (tiles::parse_mesh_path(request.uri().path()), archive.as_ref()) {
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
                    let range = request.headers().get("range").and_then(|v| v.to_str().ok()).unwrap_or("-");
                    println!("[tiles] {} {} range={range} -> {} ({} B)",
                             request.method(), request.uri().path(), res.status().as_u16(), res.body().len());
                }
                responder.respond(res);
            });
        })
        .invoke_handler(tauri::generate_handler![report, search, dev_stats, dev_flags, bench_done, archive_info, save_snapshot])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
