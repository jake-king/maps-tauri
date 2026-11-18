//! Instruction-count benchmarks of the weather decoders (gungraun, under Valgrind's Callgrind): a
//! radar frame through the app's whole path, and a GFS wind field. Inputs come from the recorded
//! tape of the end-to-end fixtures (`fixtures/tape`, or `$MAPS_BENCH_TAPE`); `tools/perf.sh`
//! compares a branch with `main` on these counts.
// the harness macros generate modules and items with no doc comments
#![allow(missing_docs)]

use std::hint::black_box;
use std::path::PathBuf;

use gungraun::{library_benchmark, library_benchmark_group, main};
use wx::{grib, radar, wind};

/// The response the tape recorded for the first URL (in sorted order) matching `pred`.
fn recorded(pred: impl Fn(&str) -> bool) -> Vec<u8> {
    let dir = std::env::var_os("MAPS_BENCH_TAPE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/tape"),
        PathBuf::from,
    );
    let index = std::fs::read_to_string(dir.join("index.tsv")).expect("tape index");
    // split as wx::tape writes it, on \n only
    let mut hits: Vec<(&str, &str)> = index
        .split('\n')
        .filter_map(|l| l.split_once('\t'))
        .map(|(key, rest)| (rest.split('\t').next().unwrap_or(rest), key))
        .filter(|(url, _)| pred(url))
        .collect();
    hits.sort_unstable();
    let (url, key) = hits.first().expect("no matching answer on the tape");
    std::fs::read(dir.join(key)).unwrap_or_else(|e| panic!("{url}: {e}"))
}

fn radar_frame() -> Vec<u8> {
    recorded(|u| u.ends_with(".grib2.gz"))
}

fn gfs_wind() -> Vec<u8> {
    recorded(|u| u.contains("filter_gfs"))
}

// what the app does with each radar frame: decompress, decode, thin by 2 (as weather.rs does)
#[library_benchmark]
#[bench::mrms(radar_frame())]
fn radar_path(gz: Vec<u8>) -> usize {
    let raw = radar::gunzip(black_box(&gz), 64 << 20).expect("gunzip");
    let g = grib::decode(&raw).expect("decode");
    black_box(radar::reduce(&g, 2).codes.len())
}

#[library_benchmark]
#[bench::gfs_250hpa(gfs_wind())]
fn wind_field(bytes: Vec<u8>) -> usize {
    black_box(
        wind::from_grib(black_box(&bytes), 250)
            .expect("wind")
            .u
            .len(),
    )
}

library_benchmark_group!(name = weather, benchmarks = [radar_path, wind_field]);
main!(library_benchmark_groups = weather);
