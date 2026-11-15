//! Every tile of a local extract, read again from the archive it was extracted from (over HTTP Range
//! requests), must be byte-identical: the remote reader is then known to find and decode tiles
//! exactly as the local one does. Also times the remote reads.
//!   cargo run --release -p vtile --features http --example remote_diff -- <local.pmtiles> <url>
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;
use vtile::pmtiles::Archive;

fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let local = Archive::open(Path::new(&a[0]))?;
    let t0 = Instant::now();
    let remote = Archive::open_url(&a[1])?;
    println!("remote header + root directory: {:.0} ms, maxzoom {}", t0.elapsed().as_secs_f64() * 1000.0, remote.header.max_zoom);
    let (mut same, mut differ, mut missing) = (0, 0, 0);
    let mut times = Vec::new();
    for (z, x, y) in local.all_tiles()? {
        let want = local.tile(z, x, y)?.expect("listed");
        let t = Instant::now();
        let got = remote.tile(z, x, y)?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        match got {
            Some(g) if g == want => same += 1,
            Some(_) => {
                differ += 1;
                if differ <= 3 { println!("  differs: {z}/{x}/{y}"); }
            }
            None => missing += 1,
        }
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
    println!("{} tiles: {same} identical, {differ} differ, {missing} missing from the remote archive", times.len());
    println!("remote read per tile: median {:.1} ms, p90 {:.1}, max {:.1}; {} round trips for {} tiles (leaf cache)",
             pct(0.5), pct(0.9), pct(1.0), remote.reads.load(Ordering::Relaxed), times.len());
    Ok(())
}
