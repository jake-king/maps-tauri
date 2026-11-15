//! Reads JSON-encoded queries, one per line, and writes each one's search result as one JSON
//! array per line. `tools/search_diff.py` runs it beside the Python `search()`.

use std::io::{self, BufRead, BufWriter, Write};
use std::path::Path;

fn main() -> io::Result<()> {
    let db_path = std::env::args().nth(1).expect("usage: search_eval <geonames.db>");
    let db = maps_tauri_lib::search::open(Path::new(&db_path)).expect("open gazetteer");
    let mut out = BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let q: String = serde_json::from_str(&line?).expect("each line is a JSON string");
        let feats = maps_tauri_lib::search::search(&db, &q, 8);
        writeln!(out, "{}", serde_json::Value::Array(feats))?;
    }
    Ok(())
}
