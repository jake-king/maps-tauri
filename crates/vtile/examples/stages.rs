//! Time each stage of serving one mesh, per tile, for one zoom level: read (pread + gunzip, via
//! Archive::tile), MVT decode, mesh build, serialization. Run alone, so the numbers are the floor
//! the in-app handler can be compared against.
//!   cargo run --release -p vtile --example stages -- data/basemap.pmtiles 14 [rounds]

use std::path::Path;
use std::time::Instant;

use vtile::{mesh::Mesher, mvt, pmtiles::Archive};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("archive path");
    let zoom: u8 = args.next().map_or(14, |z| z.parse().expect("zoom"));
    let rounds: usize = args.next().map_or(3, |r| r.parse().expect("rounds"));
    // optional: sleep between tiles (the app's requests arrive with idle gaps), and/or serve each
    // tile on a fresh thread (the app's requests land on whichever pool thread is free)
    let gap_ms: u64 = args.next().map_or(0, |g| g.parse().expect("gap ms"));
    let mode = args.next().unwrap_or_default();
    let fresh_thread = mode == "fresh";
    let spin = mode == "spin"; // busy-wait the gap: the core stays awake and clocked, caches untouched
    let a = Archive::open(Path::new(&path))?;
    let tiles: Vec<_> = a.all_tiles()?.into_iter().filter(|t| t.0 == zoom).collect();
    let mut m = Mesher::default();
    let mut out = Vec::new();
    let mut t = [0f64; 4];
    let mut worst = 0f64;
    let mut n = 0;
    for round in 0..rounds {
        for &(z, x, y) in &tiles {
            if gap_ms > 0 && spin {
                let until = Instant::now() + std::time::Duration::from_millis(gap_ms);
                while Instant::now() < until {
                    std::hint::spin_loop();
                }
            } else if gap_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(gap_ms));
            }
            if fresh_thread {
                let a = &a;
                let (d, w) = std::thread::scope(|sc| {
                    sc.spawn(move || {
                        let mut m = Mesher::default();
                        let mut out = Vec::new();
                        let s0 = Instant::now();
                        let bytes = a.tile(z, x, y).unwrap().unwrap();
                        let s1 = Instant::now();
                        let layers = mvt::decode(&bytes).unwrap();
                        let s2 = Instant::now();
                        m.build(&layers, z);
                        let s3 = Instant::now();
                        m.mesh.to_bytes(&mut out);
                        let s4 = Instant::now();
                        ([s1 - s0, s2 - s1, s3 - s2, s4 - s3], (s4 - s0).as_secs_f64() * 1000.0)
                    })
                    .join()
                    .unwrap()
                });
                if round > 0 {
                    for (i, dd) in d.iter().enumerate() {
                        t[i] += dd.as_secs_f64() * 1000.0;
                    }
                    worst = worst.max(w);
                    n += 1;
                }
                continue;
            }
            let s0 = Instant::now();
            let bytes = a.tile(z, x, y)?.expect("listed tile");
            let s1 = Instant::now();
            let layers = mvt::decode(&bytes)?;
            let s2 = Instant::now();
            m.build(&layers, z);
            let s3 = Instant::now();
            m.mesh.to_bytes(&mut out);
            let s4 = Instant::now();
            if round > 0 {
                // round 0 warms the page cache and the mesher's buffers
                for (i, d) in [s1 - s0, s2 - s1, s3 - s2, s4 - s3].iter().enumerate() {
                    t[i] += d.as_secs_f64() * 1000.0;
                }
                worst = worst.max((s4 - s0).as_secs_f64() * 1000.0);
                n += 1;
            }
        }
    }
    let n = n.max(1) as f64;
    println!("z{zoom} gap {gap_ms} ms{}{}: {} tiles x {} rounds, per tile avg ms: read+gunzip {:.2}, decode {:.2}, mesh {:.2}, serialize {:.2} = {:.2} (worst {:.2})",
             if fresh_thread { ", fresh thread" } else { "" }, if spin { " (spinning)" } else { "" }, tiles.len(), rounds - 1, t[0] / n, t[1] / n, t[2] / n, t[3] / n, t.iter().sum::<f64>() / n, worst);
    Ok(())
}
