//! Print the properties of a layer's features in one tile: what label data is actually there.
//!   cargo run --release -p vtile --example props -- <archive> <z> <x> <y> <layer> [limit]
use std::path::Path;
use vtile::{mvt, pmtiles::Archive};
fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arch = Archive::open(Path::new(&a[0]))?;
    let (z, x, y): (u8, u32, u32) = (a[1].parse().unwrap(), a[2].parse().unwrap(), a[3].parse().unwrap());
    let limit: usize = a.get(5).map_or(8, |l| l.parse().unwrap());
    let bytes = arch.tile(z, x, y)?.expect("tile");
    for l in mvt::decode(&bytes)? {
        if l.name != a[4] { continue; }
        println!("{} features in {}", l.features.len(), l.name);
        for f in l.features.iter().take(limit) {
            let mut kv: Vec<String> = f.tags.chunks_exact(2)
                .map(|p| (l.keys[p[0] as usize].clone(), &l.values[p[1] as usize]))
                .filter(|(k, _)| !k.starts_with("name:") || k == "name:en")
                .map(|(k, v)| format!("{k}={}", match v { mvt::Value::String(s) => s.clone(), other => format!("{:?}", other) }))
                .collect();
            kv.sort();
            println!("  type {} | {}", f.geom_type, kv.join(", "));
        }
    }
    Ok(())
}
