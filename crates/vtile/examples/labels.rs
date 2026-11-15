//! The labels the mesher extracts from one tile.
//!   cargo run --release -p vtile --example labels -- <archive> <z> <x> <y> [limit]
use std::path::Path;
use vtile::{mesh::Mesher, mvt, pmtiles::Archive};
fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arch = Archive::open(Path::new(&a[0]))?;
    let (z, x, y): (u8, u32, u32) = (
        a[1].parse().unwrap(),
        a[2].parse().unwrap(),
        a[3].parse().unwrap(),
    );
    let limit: usize = a.get(4).map_or(10, |l| l.parse().unwrap());
    let layers = mvt::decode(&arch.tile(z, x, y)?.expect("tile"))?;
    let mut m = Mesher::default();
    let mesh = m.build(&layers, z);
    let mut out = Vec::new();
    mesh.to_bytes(&mut out);
    println!(
        "z{z}/{x}/{y}: {} labels, label block {} B of {} B",
        mesh.labels.len(),
        u32::from_le_bytes(out[32..36].try_into().unwrap()),
        out.len()
    );
    for l in mesh.labels.iter().take(limit) {
        println!(
            "  {:?} z>={} prio {} at ({},{}) angle {:.2} run {}: {}",
            l.kind, l.min_zoom, l.priority, l.x, l.y, l.angle, l.run, l.text
        );
    }
    Ok(())
}
