//! Reads JSON-encoded strings, one per line, and writes one JSON object per line with this
//! crate's answer for each. `tools/diff.py` runs it beside the Python original.

use std::io::{self, BufRead, BufWriter, Write};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut out = BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let q: String = serde_json::from_str(&line?).expect("each line is a JSON string");
        let coords: Vec<[f64; 2]> = coords::parse_coords(&q).into_iter().map(|(a, b)| [a, b]).collect();
        let row = serde_json::json!({
            "coords": coords,
            "fold": coords::fold(&q),
            "fts": coords::fts_query(&q),
        });
        writeln!(out, "{row}")?;
    }
    Ok(())
}
