//! GRIB2 from NOAA (MRMS radar, GFS winds), decoded as the app decodes it.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wx::grib::decode_all(data);
    let _ = wx::wind::from_grib(data, 250);
});
