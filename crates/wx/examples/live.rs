//! The whole chain against NOAA, live: list radar frames, fetch and decode the newest, reduce it,
//! and fetch every G-AIRMET snapshot. `cargo run --release -p wx --features http --example live`
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use wx::{fetch, grib, hazards, radar};

fn main() -> Result<(), String> {
    let a = fetch::agent();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let t = Instant::now();
    let idx = fetch::radar_index(&a)?;
    let (name, when) = idx.last().ok_or("no frames listed")?;
    println!(
        "index: {} frames, newest {name} ({} s old), {:?}",
        idx.len(),
        now - when,
        t.elapsed()
    );
    let t = Instant::now();
    let gz = fetch::radar_frame(&a, name)?;
    let fetched = t.elapsed();
    let t = Instant::now();
    let g = grib::decode(&radar::gunzip(&gz, fetch::MAX_FRAME)?).map_err(|e| e.to_string())?;
    let decoded = t.elapsed();
    let t = Instant::now();
    let r = radar::reduce(&g, 2);
    let echo = r.codes.iter().filter(|&&c| c > 0).count();
    println!("frame: {} KB gz, fetch {fetched:?}, decode {decoded:?}, reduce {:?} -> {} x {}, {echo} cells with echo", gz.len() / 1024, t.elapsed(), r.ni, r.nj);
    let mut all = Vec::new();
    for fore in [0, 3, 6, 9, 12] {
        match fetch::gairmet(&a, fore).and_then(|s| hazards::parse(&s)) {
            Ok((h, skipped)) => {
                println!(
                    "gairmet fore={fore}: {} features, {skipped} skipped",
                    h.len()
                );
                all.extend(h);
            }
            Err(e) => println!("gairmet fore={fore}: {e}"),
        }
    }
    let snap = hazards::nearest_snapshot(&all, now);
    println!(
        "shown now: {} features valid {} s from now",
        snap.len(),
        snap.first().map(|h| h.valid - now).unwrap_or(0)
    );
    Ok(())
}
