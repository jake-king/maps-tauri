mod tiles;

use std::path::PathBuf;

/// Prints a status line from the frontend, so a run can be checked from the terminal.
#[tauri::command]
fn report(msg: String) {
    println!("[frontend] {msg}");
}

/// Where the archive and the gazetteer live: `MAPS_DATA`, else the repo's `data/`.
fn data_dir() -> PathBuf {
    std::env::var_os("MAPS_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../data")))
}

pub fn run() {
    let data = data_dir();
    tauri::Builder::default()
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
        .invoke_handler(tauri::generate_handler![report])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
