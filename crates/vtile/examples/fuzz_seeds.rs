//! Seed inputs for the fuzz targets that read map data (tools/fuzz.sh seeds), taken from a real
//! archive through the reader the app uses.
//!
//! `fuzz_seeds <archive.pmtiles> <corpus dir>` writes:
//! * `archive/`: the archive's first bytes, header and root directory, which is what the archive
//!   target opens (its tiles then read as absent, so the fuzzer works on the directories)
//! * `pmtiles_dir/`: the root directory, decompressed, as the directory target reads it
//! * `mvt_mesh/`: the two smallest tiles of each zoom, each after a byte giving its zoom
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use vtile::pmtiles::Archive;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let [_, archive, out] = args.as_slice() else {
        eprintln!("usage: fuzz_seeds <archive.pmtiles> <corpus dir>");
        std::process::exit(2);
    };
    let a = Archive::open(Path::new(archive))?;
    let (dir_a, dir_m) = (
        Path::new(out).join("archive"),
        Path::new(out).join("mvt_mesh"),
    );
    let dir_d = Path::new(out).join("pmtiles_dir");
    for d in [&dir_a, &dir_m, &dir_d] {
        fs::create_dir_all(d)?;
    }

    let head_len = usize::try_from(a.header.root_offset + a.header.root_length).expect("root fits");
    let bytes = fs::read(archive)?;
    fs::write(
        dir_a.join("header-and-root"),
        &bytes[..head_len.min(bytes.len())],
    )?;
    let root = &bytes[usize::try_from(a.header.root_offset).expect("root fits")..head_len];
    let mut raw = Vec::new();
    vtile::pmtiles::gunzip(root, &mut raw)?;
    fs::write(dir_d.join("root"), &raw)?;

    let mut by_zoom: BTreeMap<u8, Vec<Vec<u8>>> = BTreeMap::new();
    for (z, x, y) in a.all_tiles()? {
        if let Some(t) = a.tile(z, x, y)? {
            let v = by_zoom.entry(z).or_default();
            v.push(t);
            v.sort_by_key(Vec::len);
            v.truncate(2);
        }
    }
    let mut n = 0;
    for (z, tiles) in &by_zoom {
        for (i, t) in tiles.iter().enumerate() {
            let mut seed = vec![*z];
            seed.extend_from_slice(t);
            fs::write(dir_m.join(format!("z{z}-{i}")), seed)?;
            n += 1;
        }
    }
    println!("fuzz_seeds: header and root ({head_len} bytes), {n} tiles");
    Ok(())
}
