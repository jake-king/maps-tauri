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
fn dev_stats(tiles: tauri::State<'_, Arc<devstats::TileTimes>>) -> serde_json::Value {
    serde_json::json!({
        "memory": devstats::process_tree_memory(),
        "tiles": tiles.take(),
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
    let tile_times = Arc::new(devstats::TileTimes::default());
    let handler_times = tile_times.clone();
    let log_tiles = std::env::var("MAPS_LOG_TILES").is_ok_and(|v| v == "1");
    // A control for the benchmark's gray-box measure: slow every tile response down on purpose.
    // If gray frames do not climb as this grows, the measure is not measuring.
    let tile_delay = std::time::Duration::from_millis(
        std::env::var("MAPS_TILE_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0),
    );
    tauri::Builder::default()
        .manage(gazetteer)
        .manage(tile_times)
        // Asynchronous so a file read never runs on the UI thread: the handler hands the request
        // to a blocking-pool thread and answers through the responder when the read is done.
        .register_asynchronous_uri_scheme_protocol("tiles", move |_ctx, request, responder| {
            let data = data.clone();
            let times = handler_times.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if !tile_delay.is_zero() {
                    std::thread::sleep(tile_delay);
                }
                let t0 = Instant::now();
                let res = tiles::handle(&data, &request);
                times.record(t0.elapsed(), res.body().len());
                if log_tiles {
                    let range = request.headers().get("range").and_then(|v| v.to_str().ok()).unwrap_or("-");
                    println!("[tiles] {} {} range={range} -> {} ({} B)",
                             request.method(), request.uri().path(), res.status().as_u16(), res.body().len());
                }
                responder.respond(res);
            });
        })
        .invoke_handler(tauri::generate_handler![report, search, dev_stats, dev_flags, bench_done])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
