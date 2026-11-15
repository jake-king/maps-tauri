pub mod search;
mod tiles;

use std::path::PathBuf;
use std::sync::Mutex;

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
    tauri::Builder::default()
        .manage(gazetteer)
        // Asynchronous so a file read never runs on the UI thread: the handler hands the request
        // to a blocking-pool thread and answers through the responder when the read is done.
        .register_asynchronous_uri_scheme_protocol("tiles", move |_ctx, request, responder| {
            let data = data.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let res = tiles::handle(&data, &request);
                let range = request.headers().get("range").and_then(|v| v.to_str().ok()).unwrap_or("-");
                println!("[tiles] {} {} range={range} -> {} ({} B)",
                         request.method(), request.uri().path(), res.status().as_u16(), res.body().len());
                responder.respond(res);
            });
        })
        .invoke_handler(tauri::generate_handler![report, search])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
