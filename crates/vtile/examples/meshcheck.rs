//! Whole-archive check of the mesher: for every polygon feature of every tile, the triangles'
//! total area must equal the polygon's area (exteriors minus holes). Also times decode + mesh per
//! tile and compares mesh size with the compressed tile.

use std::path::Path;
use std::time::Instant;

use vtile::mesh::{classify_rings, signed_area, Mesher};
use vtile::{mvt, pmtiles::Archive};

fn tri_area(m: &vtile::mesh::Mesh, from: usize) -> f64 {
    m.fill_idx[from..]
        .chunks_exact(3)
        .map(|t| {
            let (a, b, c) = (m.fill[t[0] as usize], m.fill[t[1] as usize], m.fill[t[2] as usize]);
            let (ax, ay, bx, by, cx, cy) = (a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64, c.y as f64);
            ((bx - ax) * (cy - ay) - (cx - ax) * (by - ay)).abs() / 2.0
        })
        .sum()
}

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).expect("usage: meshcheck <archive.pmtiles>");
    let a = Archive::open(Path::new(&path))?;
    let mut m = Mesher::default();
    let mut rings = Vec::new();
    let (mut polys, mut bad, mut worst, mut worst_at) = (0u64, 0u64, 0.0f64, String::new());
    let (mut mesh_bytes, mut ms_total, mut tiles) = (0usize, 0.0f64, 0u32);
    let mut out = Vec::new();
    let mut per_zoom: std::collections::BTreeMap<u8, [usize; 6]> = Default::default();
    for (z, x, y) in a.all_tiles()? {
        let Some(bytes) = a.tile(z, x, y)? else { continue };
        // timing: decode + full mesh, as the server will do it
        let t0 = Instant::now();
        let layers = mvt::decode(&bytes)?;
        m.build(&layers, z).to_bytes(&mut out);
        ms_total += t0.elapsed().as_secs_f64() * 1000.0;
        mesh_bytes += out.len();
        tiles += 1;
        let e = per_zoom.entry(z).or_insert([0usize; 6]);
        e[0] += 1;
        e[1] += m.mesh.fill.len() * 6 + m.mesh.fill_idx.len() * 2;
        e[2] += m.mesh.line.len() * 8 + m.mesh.line_idx.len() * 2;
        e[3] += bytes.len();
        e[4] += m.mesh.fill.len() + m.mesh.line.len();
        e[5] += (m.mesh.fill.len() > 65535 || m.mesh.line.len() > 65535) as usize;
        // area conservation, feature by feature
        for l in &layers {
            for f in &l.features {
                if f.geom_type != 3 {
                    continue;
                }
                mvt::geometry(&f.geometry, &mut rings);
                let want: f64 = classify_rings(&rings)
                    .into_iter()
                    .map(|r| {
                        let ext = signed_area(&rings[r.start]).abs();
                        let holes: f64 = rings[r.start + 1..r.end].iter().map(|h| signed_area(h).abs()).sum();
                        (ext - holes) / 2.0
                    })
                    .sum();
                m.mesh.clear();
                m.fill_feature(f, 0);
                let got = tri_area(&m.mesh, 0);
                polys += 1;
                let err = if want > 0.0 { (got - want).abs() / want } else { got };
                if err > 1e-6 {
                    bad += 1;
                }
                if err > worst {
                    worst = err;
                    worst_at = format!("{z}/{x}/{y} {} feature {} (want {want}, got {got})", l.name, f.id);
                }
            }
        }
    }
    println!("{tiles} tiles, {polys} polygon features: {bad} off by more than 1e-6; worst {worst:.2e} at {worst_at}");
    println!("earcut failures: {}", m.earcut_failures);
    println!("zoom tiles  fills KB  lines KB  decoded MVT KB  vertices  tiles over 65535 vertices (per tile averages)");
    for (z, e) in &per_zoom {
        let n = e[0].max(1);
        println!("{z:4} {:5} {:9} {:9} {:15} {:9} {:3}", e[0], e[1] / n / 1024, e[2] / n / 1024, e[3] / n / 1024, e[4] / n, e[5]);
    }
    println!("decode + mesh: {:.2} ms/tile avg; meshes {:.1} MB total ({} KB/tile avg)",
             ms_total / tiles as f64, mesh_bytes as f64 / 1048576.0, mesh_bytes / tiles as usize / 1024);
    Ok(())
}
