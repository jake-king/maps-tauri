//! The weather services' text answers, parsed by hand-written readers: METAR CSV, the radar
//! directory listing (HTML), the satellite capabilities (XML), and the hazard and SIGMET JSON.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let _ = wx::metar::parse_cache(s);
    let _ = wx::metar::category(s);
    let _ = wx::radar::frames_in_index(s);
    let _ = wx::satellite::times(s);
    let _ = wx::hazards::parse_time(s);
    let _ = wx::hazards::parse(s);
    let _ = wx::sigmet::parse_international(s);
    let _ = wx::sigmet::parse_domestic(s);
});
