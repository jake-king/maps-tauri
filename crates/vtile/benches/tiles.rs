//! Instruction-count benchmarks of the tile path (gungraun, under Valgrind's Callgrind): decoding a
//! vector tile, meshing it, and reading the archive's directories. Counts, not times, so they mean
//! the same on a busy shared runner as on an idle laptop; `tools/perf.sh` compares a branch with
//! `main` on them. Inputs are the largest tile of the end-to-end archive at three zooms; setup
//! (reading and decompressing) runs outside the measured function.
// the harness macros generate modules and items with no doc comments
#![allow(missing_docs)]

use std::hint::black_box;
use std::path::PathBuf;

use gungraun::{library_benchmark, library_benchmark_group, main};
use vtile::mesh::Mesher;
use vtile::mvt::{self, Layer};
use vtile::pmtiles::{self, Archive};

/// The end-to-end archive (`data/westla.pmtiles`, fetched by `tools/forge-job.sh` with the
/// fixtures), or `$MAPS_BENCH_ARCHIVE`.
fn archive_path() -> PathBuf {
    std::env::var_os("MAPS_BENCH_ARCHIVE").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/westla.pmtiles"),
        PathBuf::from,
    )
}

fn archive() -> Archive {
    let path = archive_path();
    Archive::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn tile(z: u8, x: u32, y: u32) -> Vec<u8> {
    archive()
        .tile(z, x, y)
        .expect("read")
        .unwrap_or_else(|| panic!("no tile {z}/{x}/{y}"))
}

fn decoded(z: u8, x: u32, y: u32) -> (Vec<Layer>, u8) {
    (mvt::decode(&tile(z, x, y)).expect("decode"), z)
}

#[library_benchmark]
#[bench::z5(tile(5, 5, 12))]
#[bench::z9(tile(9, 87, 204))]
#[bench::z14(tile(14, 2804, 6552))]
fn decode(bytes: Vec<u8>) -> Vec<Layer> {
    black_box(mvt::decode(black_box(&bytes)).expect("decode"))
}

#[library_benchmark]
#[bench::z5(decoded(5, 5, 12))]
#[bench::z9(decoded(9, 87, 204))]
#[bench::z14(decoded(14, 2804, 6552))]
fn mesh(input: (Vec<Layer>, u8)) -> usize {
    let (layers, z) = input;
    let mut m = Mesher::default();
    black_box(m.build(black_box(&layers), z).fill_idx.len())
}

/// The archive's root directory, decompressed: what `parse_directory` reads when an archive opens.
fn root_directory() -> Vec<u8> {
    let b = std::fs::read(archive_path()).expect("read archive");
    let h = pmtiles::parse_header(&b).expect("header");
    let (at, len) = (h.root_offset as usize, h.root_length as usize);
    let mut out = Vec::new();
    pmtiles::gunzip(&b[at..at + len], &mut out).expect("gunzip");
    out
}

#[library_benchmark]
#[bench::root(root_directory())]
fn directory(bytes: Vec<u8>) -> usize {
    let entries = pmtiles::parse_directory(black_box(&bytes)).expect("directory");
    // a lookup of every tile id from zoom 0 to 9, hits and misses alike
    let mut hits = 0;
    for id in 0..pmtiles::zxy_to_tile_id(10, 0, 0) {
        hits += usize::from(pmtiles::find(&entries, black_box(id)).is_some());
    }
    black_box(hits)
}

library_benchmark_group!(name = tile_path, benchmarks = [decode, mesh, directory]);
main!(library_benchmark_groups = tile_path);
