//! For every tile in an archive, one JSON line summarising each layer: features, geometry types,
//! rings, vertices, a coordinate checksum, and `kind` values. `tools/mvt_diff.mjs` computes the same
//! with pmtiles.js and @mapbox/vector-tile and compares.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use vtile::{mvt, pmtiles::Archive};

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).expect("usage: summary <archive.pmtiles>");
    let a = Archive::open(Path::new(&path))?;
    let mut rings = Vec::new();
    for (z, x, y) in a.all_tiles()? {
        let Some(bytes) = a.tile(z, x, y)? else { continue };
        let mut layers = BTreeMap::new();
        for l in mvt::decode(&bytes)? {
            let (mut types, mut nrings, mut verts, mut sum) = ([0u64; 4], 0u64, 0u64, 0i64);
            let mut kinds: BTreeMap<String, u64> = BTreeMap::new();
            for f in &l.features {
                types[(f.geom_type as usize).min(3)] += 1;
                mvt::geometry(&f.geometry, &mut rings);
                nrings += rings.len() as u64;
                for r in &rings {
                    verts += r.len() as u64;
                    for p in r {
                        sum += i64::from(p[0]) + 3 * i64::from(p[1]);
                    }
                }
                let k = l.prop(f, "kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                *kinds.entry(k).or_default() += 1;
            }
            layers.insert(l.name.clone(), json!({
                "features": l.features.len(), "types": types, "rings": nrings, "vertices": verts,
                "checksum": sum, "extent": l.extent, "kinds": kinds,
            }));
        }
        println!("{}", json!({ "tile": format!("{z}/{x}/{y}"), "layers": layers }));
    }
    Ok(())
}
