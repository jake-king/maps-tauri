//! Live weather for the native renderer: the newest MRMS radar frames and the G-AIRMET hazard
//! snapshots, fetched from NOAA on a background thread, kept on disk, and handed to the webview.
//!
//! Nothing here touches the network until the user turns weather on (`wx_enable`): the benchmarks
//! and the offline gate never press the button, so they stay offline. What was fetched is written to
//! a small SQLite file; with the network gone, the last frames and hazards are read back from it and
//! shown with their age.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use wx::hazards::{self, Hazard, Shape};
use wx::radar::{self, Radar};

/// Radar frames kept and animated: MRMS publishes one about every two minutes, so ~20 minutes.
pub const FRAMES: usize = 10;
/// Each frame thinned by this factor: 0.01° (1 km) to 0.02°, 3500 x 1750 cells, 6 MB on the GPU.
pub const THIN: usize = 2;
pub const FORECASTS: [u32; 5] = [0, 3, 6, 9, 12];
const RADAR_EVERY: Duration = Duration::from_secs(120);
const HAZARDS_EVERY: i64 = 15 * 60;

/// Where the weather comes from: NOAA in the app, a fake in the tests.
pub trait Source: Send + Sync {
    fn index(&self) -> Result<Vec<(String, i64)>, String>;
    fn frame(&self, name: &str) -> Result<Vec<u8>, String>;
    fn gairmet(&self, fore: u32) -> Result<String, String>;
}

pub struct Noaa(wx::fetch::Agent);

impl Noaa {
    pub fn new() -> Noaa {
        Noaa(wx::fetch::agent())
    }
}

impl Source for Noaa {
    fn index(&self) -> Result<Vec<(String, i64)>, String> {
        wx::fetch::radar_index(&self.0)
    }
    fn frame(&self, name: &str) -> Result<Vec<u8>, String> {
        wx::fetch::radar_frame(&self.0, name)
    }
    fn gairmet(&self, fore: u32) -> Result<String, String> {
        wx::fetch::gairmet(&self.0, fore)
    }
}

#[derive(Default)]
struct Inner {
    frames: BTreeMap<i64, Arc<Radar>>,
    /// each forecast hour's features, replaced only by a successful fetch of that hour
    gairmet: HashMap<u32, Vec<Hazard>>,
    radar_ok: Option<i64>,
    hazards_ok: Option<i64>,
    /// the last round's failures, cleared by the next round that has none
    errors: Vec<String>,
    /// true until the first successful fetch: what is shown came from the disk
    from_disk: bool,
    /// a round is running; how many frames it wants; whether hazards are being fetched
    busy: bool,
    wanted: usize,
    hazards_loading: bool,
}

pub struct Weather {
    inner: Mutex<Inner>,
    db: Option<Mutex<Connection>>,
    started: AtomicBool,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn decode(gz: &[u8]) -> Result<Radar, String> {
    let g =
        wx::grib::decode(&radar::gunzip(gz, wx::fetch::MAX_FRAME)?).map_err(|e| e.to_string())?;
    Ok(radar::reduce(&g, THIN))
}

impl Weather {
    /// `path` None: no disk copy (weather works, but not offline).
    pub fn open(path: Option<&Path>) -> Weather {
        let db = path.and_then(|p| match open_db(p) {
            Ok(c) => Some(Mutex::new(c)),
            Err(e) => {
                eprintln!(
                    "[weather] {} will not open ({e}); weather is not kept for offline",
                    p.display()
                );
                None
            }
        });
        Weather {
            inner: Mutex::new(Inner::default()),
            db,
            started: AtomicBool::new(false),
        }
    }

    /// Start the background thread, once: first what the disk holds, then NOAA every two minutes.
    pub fn enable(self: &Arc<Self>, source: Arc<dyn Source>, changed: Arc<dyn Fn() + Send + Sync>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name("weather".into())
            .spawn(move || {
                me.load_from_disk();
                changed();
                loop {
                    me.round(source.as_ref(), now(), changed.as_ref());
                    std::thread::sleep(RADAR_EVERY);
                }
            })
            .expect("spawn the weather thread");
    }

    fn load_from_disk(&self) {
        let Some(db) = &self.db else { return };
        let (frames, gair) = {
            let c = db.lock().expect("weather db lock");
            (
                read_frames(&c).unwrap_or_default(),
                read_gairmet(&c).unwrap_or_default(),
            )
        };
        let mut decoded = BTreeMap::new();
        for (t, gz) in frames {
            match decode(&gz) {
                Ok(r) => {
                    decoded.insert(t, Arc::new(r));
                }
                Err(e) => eprintln!("[weather] cached frame {t} unreadable ({e}); skipped"),
            }
        }
        let mut g = self.inner.lock().expect("weather lock");
        if g.frames.is_empty() && !decoded.is_empty() {
            g.frames = decoded;
            g.from_disk = true;
        }
        for (fore, json) in gair {
            if let Ok((h, _)) = hazards::parse(&json) {
                g.gairmet.entry(fore).or_insert(h);
                g.from_disk = true;
            }
        }
    }

    /// One update, most useful first so something shows at once: the newest radar frame, then the
    /// hazards (if older than 15 minutes), then the older frames for the loop, three at a time.
    /// `changed` is called after every piece that arrives, so the webview never waits for a whole
    /// round. A failure keeps what was there.
    pub fn round(&self, src: &dyn Source, now: i64, changed: &(dyn Fn() + Sync)) {
        let errors = Mutex::new(Vec::new());
        self.inner.lock().expect("weather lock").busy = true;
        changed();
        let want: Vec<(String, i64)> = match src.index() {
            Ok(idx) => idx.iter().rev().take(FRAMES).rev().cloned().collect(),
            Err(e) => {
                errors
                    .lock()
                    .expect("errors")
                    .push(format!("radar index: {e}"));
                Vec::new()
            }
        };
        self.inner.lock().expect("weather lock").wanted = want.len();
        let held = |t: &i64| {
            self.inner
                .lock()
                .expect("weather lock")
                .frames
                .contains_key(t)
        };
        // newest first, then the rest newest to oldest
        let mut missing: Vec<&(String, i64)> =
            want.iter().rev().filter(|(_, t)| !held(t)).collect();
        let fetch = |name: &str, t: i64| match src
            .frame(name)
            .and_then(|gz| decode(&gz).map(|r| (gz, r)))
        {
            Ok((gz, r)) => {
                self.store_frame(t, &gz);
                let mut g = self.inner.lock().expect("weather lock");
                g.frames.insert(t, Arc::new(r));
                g.from_disk = false;
                drop(g);
                changed();
            }
            Err(e) => errors
                .lock()
                .expect("errors")
                .push(format!("radar {name}: {e}")),
        };
        if !missing.is_empty() {
            let (name, t) = missing.remove(0);
            fetch(name, *t);
        }

        let due = self
            .inner
            .lock()
            .expect("weather lock")
            .hazards_ok
            .is_none_or(|t| now - t >= HAZARDS_EVERY);
        if due {
            self.inner.lock().expect("weather lock").hazards_loading = true;
            changed();
            let mut ok = 0;
            for fore in FORECASTS {
                match src
                    .gairmet(fore)
                    .and_then(|s| hazards::parse(&s).map(|(h, _)| (s, h)))
                {
                    Ok((s, h)) => {
                        self.store_gairmet(fore, &s);
                        let mut g = self.inner.lock().expect("weather lock");
                        g.gairmet.insert(fore, h);
                        g.from_disk = false;
                        drop(g);
                        ok += 1;
                        changed();
                    }
                    Err(e) => errors
                        .lock()
                        .expect("errors")
                        .push(format!("hazards fore={fore}: {e}")),
                }
            }
            let mut g = self.inner.lock().expect("weather lock");
            g.hazards_loading = false;
            if ok == FORECASTS.len() {
                g.hazards_ok = Some(now);
            }
        }

        // the older frames, three requests at a time
        let queue = Mutex::new(missing.into_iter());
        std::thread::scope(|sc| {
            for _ in 0..3 {
                sc.spawn(|| loop {
                    let next = queue.lock().expect("queue").next();
                    let Some((name, t)) = next else { break };
                    fetch(name, *t);
                });
            }
        });

        let mut g = self.inner.lock().expect("weather lock");
        if let Some(newest) = want.last() {
            let keep: Vec<i64> = want.iter().map(|w| w.1).collect();
            // prune only to frames the server still lists, and only once the newest is held
            if g.frames.contains_key(&newest.1) {
                g.frames.retain(|t, _| keep.contains(t));
                g.radar_ok = Some(now);
                drop(g);
                self.prune_frames(&keep);
                g = self.inner.lock().expect("weather lock");
            }
        }
        // the newest keeps failing: still bounded, the oldest go first
        while g.frames.len() > 2 * FRAMES {
            g.frames.pop_first();
        }
        g.errors = errors.into_inner().expect("errors");
        g.busy = false;
        drop(g);
        changed();
    }

    fn store_frame(&self, t: i64, gz: &[u8]) {
        if let Some(db) = &self.db {
            let r = db.lock().expect("weather db lock").execute(
                "INSERT OR REPLACE INTO radar(time, gz) VALUES (?1, ?2)",
                params![t, gz],
            );
            if let Err(e) = r {
                eprintln!("[weather] radar frame not kept on disk ({e})");
            }
        }
    }

    fn prune_frames(&self, keep: &[i64]) {
        if let (Some(db), Some(oldest)) = (&self.db, keep.first()) {
            let _ = db
                .lock()
                .expect("weather db lock")
                .execute("DELETE FROM radar WHERE time < ?1", params![oldest]);
        }
    }

    fn store_gairmet(&self, fore: u32, json: &str) {
        if let Some(db) = &self.db {
            let r = db.lock().expect("weather db lock").execute(
                "INSERT OR REPLACE INTO gairmet(fore, json) VALUES (?1, ?2)",
                params![fore, json],
            );
            if let Err(e) = r {
                eprintln!("[weather] hazards not kept on disk ({e})");
            }
        }
    }

    /// What the webview needs to draw: frame times, the grid, the hazard snapshot nearest `now`, and
    /// how fresh it all is.
    pub fn state(&self, now: i64) -> Value {
        let g = self.inner.lock().expect("weather lock");
        let grid = g.frames.values().next_back().map(|r| json!({ "ni": r.ni, "nj": r.nj, "lat0": r.lat0, "lon0": r.lon0, "dlat": r.dlat, "dlon": r.dlon }));
        let snap = snapshot(&g.gairmet, now);
        json!({
            "enabled": self.started.load(Ordering::SeqCst),
            "frames": g.frames.keys().collect::<Vec<_>>(),
            "grid": grid,
            "hazardsValid": snap.first().map(|h| h.valid),
            "hazards": snap.iter().map(hazard_json).collect::<Vec<_>>(),
            "radarOk": g.radar_ok,
            "hazardsOk": g.hazards_ok,
            "fromDisk": g.from_disk,
            "loading": {
                "busy": g.busy,
                "held": g.frames.len().min(g.wanted),
                "wanted": g.wanted,
                "hazards": g.hazards_loading,
            },
            "errors": g.errors,
            "now": now,
        })
    }

    pub fn frame(&self, t: i64) -> Option<Arc<Radar>> {
        self.inner
            .lock()
            .expect("weather lock")
            .frames
            .get(&t)
            .cloned()
    }
}

/// The features of ONE forecast hour: the one whose valid time is nearest `now` (on a tie, the lower
/// hour). Merging hours would draw an area twice when two fetched hours share a valid time, which
/// happens when a new package is issued between two of a round's requests.
fn snapshot(by_fore: &HashMap<u32, Vec<Hazard>>, now: i64) -> Vec<Hazard> {
    let best = by_fore
        .iter()
        .filter_map(|(fore, h)| Some((*fore, h.first()?.valid)))
        .min_by_key(|&(fore, t)| ((t - now).abs(), t, fore));
    match best {
        Some((fore, t)) => hazards::nearest_snapshot(&by_fore[&fore], t),
        None => Vec::new(),
    }
}

fn hazard_json(h: &Hazard) -> Value {
    let (kind, coords) = match &h.shape {
        Shape::Polygon(rings) => ("polygon", json!(rings)),
        Shape::Line(p) => ("line", json!(p)),
    };
    json!({ "hazard": h.hazard, "severity": h.severity, "base": h.base, "top": h.top, "level": h.level, "kind": kind, "coords": coords })
}

fn open_db(p: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let c = Connection::open(p)?;
    c.busy_timeout(Duration::from_secs(2))?;
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS radar(time INTEGER PRIMARY KEY, gz BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS gairmet(fore INTEGER PRIMARY KEY, json TEXT NOT NULL);",
    )?;
    Ok(c)
}

fn read_frames(c: &Connection) -> rusqlite::Result<Vec<(i64, Vec<u8>)>> {
    let mut s = c.prepare("SELECT time, gz FROM radar ORDER BY time")?;
    let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

fn read_gairmet(c: &Connection) -> rusqlite::Result<Vec<(u32, String)>> {
    let mut s = c.prepare("SELECT fore, json FROM gairmet")?;
    let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

/// `MAPS_WEATHER_CACHE` (a path, or "0" for none), else beside the tile cache.
pub fn configured_path() -> Option<std::path::PathBuf> {
    match std::env::var("MAPS_WEATHER_CACHE").ok().as_deref() {
        Some("0") => None,
        Some(v) if !v.is_empty() => Some(v.into()),
        _ => crate::diskcache::configured_path().map(|p| p.with_file_name("weather.sqlite")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex as M;

    /// A frame: a tiny MRMS-shaped grid whose one cell holds `dbz`.
    fn frame_gz(dbz: f32) -> Vec<u8> {
        let raw = (dbz * 10.0 + 9990.0) as u16;
        let mut png = Vec::new();
        {
            let mut e = png::Encoder::new(&mut png, 2, 2);
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Sixteen);
            let mut w = e.write_header().unwrap();
            let px: Vec<u8> = [raw, 9990, 9990, 9990]
                .iter()
                .flat_map(|v| v.to_be_bytes())
                .collect();
            w.write_image_data(&px).unwrap();
        }
        let grib = crate_grib(2, 2, &png);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&grib).unwrap();
        gz.finish().unwrap()
    }

    /// A minimal MRMS-layout GRIB2 message around a PNG (the wx crate's own tests cover the format).
    fn crate_grib(ni: u32, nj: u32, png: &[u8]) -> Vec<u8> {
        fn sec(n: u8, body: &[u8]) -> Vec<u8> {
            let mut s = ((body.len() + 5) as u32).to_be_bytes().to_vec();
            s.push(n);
            s.extend_from_slice(body);
            s
        }
        let mut s3 = vec![0u8; 67];
        s3[1..5].copy_from_slice(&(ni * nj).to_be_bytes());
        s3[25..29].copy_from_slice(&ni.to_be_bytes());
        s3[29..33].copy_from_slice(&nj.to_be_bytes());
        s3[41..45].copy_from_slice(&54_995_000u32.to_be_bytes());
        s3[45..49].copy_from_slice(&230_005_000u32.to_be_bytes());
        s3[58..62].copy_from_slice(&10_000u32.to_be_bytes());
        s3[62..66].copy_from_slice(&10_000u32.to_be_bytes());
        let mut s5 = vec![0u8; 16];
        s5[0..4].copy_from_slice(&(ni * nj).to_be_bytes());
        s5[4..6].copy_from_slice(&41u16.to_be_bytes());
        s5[6..10].copy_from_slice(&(-9990.0f32).to_bits().to_be_bytes());
        s5[12..14].copy_from_slice(&1u16.to_be_bytes());
        s5[14] = 16;
        let mut body = sec(1, &[0; 16]);
        body.extend(sec(3, &s3));
        body.extend(sec(4, &[0; 29]));
        body.extend(sec(5, &s5));
        body.extend(sec(6, &[255]));
        body.extend(sec(7, png));
        let mut m = b"GRIB\0\0\xd1\x02".to_vec();
        m.extend_from_slice(&((16 + body.len() + 4) as u64).to_be_bytes());
        m.extend(body);
        m.extend_from_slice(b"7777");
        m
    }

    /// One TURB-HI area valid `fore` hours after 2026-11-16 18:00Z, as a G-AIRMET response holds it.
    fn turb(fore: u32) -> String {
        format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","properties":{{"hazard":"TURB-HI","severity":"MOD","base":"180","top":"400","validTime":"2026-11-{:02}T{:02}:00:00Z"}},"geometry":{{"type":"Polygon","coordinates":[[[-120,40],[-118,40],[-118,38],[-120,40]]]}}}}]}}"#,
            16 + (18 + fore) / 24,
            (18 + fore) % 24
        )
    }
    const T18: i64 = 1_794_852_000; // 2026-11-16T18:00:00Z

    /// A NOAA that serves what the test says, and fails when told to.
    struct Fake {
        frames: M<Vec<(String, i64)>>,
        down: AtomicBool,
        frame_calls: M<Vec<String>>,
        gair_calls: M<u32>,
    }

    impl Fake {
        fn new(times: &[i64]) -> Fake {
            Fake {
                frames: M::new(times.iter().map(|t| (format!("f{t}"), *t)).collect()),
                down: AtomicBool::new(false),
                frame_calls: M::new(vec![]),
                gair_calls: M::new(0),
            }
        }
    }

    impl Source for Fake {
        fn index(&self) -> Result<Vec<(String, i64)>, String> {
            if self.down.load(Ordering::SeqCst) {
                return Err("unreachable".into());
            }
            Ok(self.frames.lock().unwrap().clone())
        }
        fn frame(&self, name: &str) -> Result<Vec<u8>, String> {
            self.frame_calls.lock().unwrap().push(name.into());
            if self.down.load(Ordering::SeqCst) {
                return Err("unreachable".into());
            }
            Ok(frame_gz(40.0))
        }
        fn gairmet(&self, fore: u32) -> Result<String, String> {
            *self.gair_calls.lock().unwrap() += 1;
            if self.down.load(Ordering::SeqCst) {
                return Err("unreachable".into());
            }
            Ok(turb(fore))
        }
    }

    fn times(w: &Weather) -> Vec<i64> {
        w.state(0)["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect()
    }

    #[test]
    fn keeps_the_newest_frames_and_fetches_each_once() {
        let all: Vec<i64> = (0..15).map(|i| i * 120).collect();
        let src = Fake::new(&all);
        let w = Weather::open(None);
        w.round(&src, 2000, &|| {});
        assert_eq!(times(&w), all[5..].to_vec());
        assert_eq!(src.frame_calls.lock().unwrap().len(), FRAMES);
        // two new frames: only they are fetched, the two oldest dropped
        src.frames
            .lock()
            .unwrap()
            .extend([(String::from("f1800"), 1800), (String::from("f1920"), 1920)]);
        w.round(&src, 2100, &|| {});
        assert_eq!(src.frame_calls.lock().unwrap().len(), FRAMES + 2);
        assert_eq!(times(&w).first(), Some(&840));
        assert_eq!(times(&w).len(), FRAMES);
        let s = w.state(2100);
        assert_eq!(s["grid"]["ni"], 1); // 2 x 2 thinned by 2
        assert_eq!(w.frame(1920).unwrap().codes, vec![radar::code(40.0)]);
    }

    #[test]
    fn a_failure_keeps_what_was_there_and_says_so() {
        let src = Fake::new(&[100, 220]);
        let w = Weather::open(None);
        w.round(&src, 300, &|| {});
        assert_eq!(w.state(300)["hazards"].as_array().unwrap().len(), 1);
        src.down.store(true, Ordering::SeqCst);
        w.round(&src, 300 + HAZARDS_EVERY, &|| {});
        let s = w.state(300 + HAZARDS_EVERY);
        assert_eq!(times(&w), vec![100, 220]);
        assert_eq!(
            s["hazards"].as_array().unwrap().len(),
            1,
            "hazards kept through the outage"
        );
        assert_eq!(s["radarOk"], 300);
        let errors: Vec<&str> = s["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert!(
            errors.iter().any(|e| e.starts_with("radar index"))
                && errors.iter().any(|e| e.starts_with("hazards fore=0")),
            "{errors:?}"
        );
        src.down.store(false, Ordering::SeqCst);
        w.round(&src, 400 + HAZARDS_EVERY, &|| {});
        assert_eq!(
            w.state(0)["errors"].as_array().unwrap().len(),
            0,
            "errors clear on a good round"
        );
    }

    #[test]
    fn one_forecast_hour_is_shown_the_nearest_to_now() {
        let w = Weather::open(None);
        w.round(&Fake::new(&[1]), T18, &|| {});
        for (now, valid) in [
            (T18, T18),
            (T18 + 3600, T18),
            (T18 + 2 * 3600, T18 + 3 * 3600),
            (T18 + 40 * 3600, T18 + 12 * 3600),
        ] {
            let s = w.state(now);
            assert_eq!(s["hazards"].as_array().unwrap().len(), 1, "now {now}");
            assert_eq!(s["hazardsValid"], valid, "now {now}");
        }
        // two hours valid at the same time (a new package mid-round): one set, not both
        let mut by_fore = HashMap::new();
        for fore in [0, 3] {
            by_fore.insert(fore, hazards::parse(&turb(0)).unwrap().0);
        }
        assert_eq!(snapshot(&by_fore, T18).len(), 1);
    }

    #[test]
    fn the_newest_frame_first_then_hazards_then_the_rest_each_signalled() {
        let all: Vec<i64> = (0..10).map(|i| 1000 + i * 120).collect();
        let w = Weather::open(None);
        let seen = M::new(Vec::new()); // (frames held, hazards shown, loading) at each signal
        w.round(&Fake::new(&all), T18, &|| {
            let s = w.state(T18);
            seen.lock().unwrap().push((
                times(&w),
                s["hazards"].as_array().unwrap().len(),
                s["loading"].clone(),
            ));
        });
        let seen = seen.into_inner().unwrap();
        // the round announces itself before anything arrives
        assert_eq!(seen[0].0, Vec::<i64>::new());
        assert_eq!(seen[0].2["busy"], true);
        let first = seen.iter().position(|x| !x.0.is_empty()).unwrap();
        assert_eq!(seen[first].0, vec![2080], "the newest frame alone first");
        let hz = seen.iter().position(|x| x.1 > 0).unwrap();
        assert!(
            seen[hz].0.len() == 1,
            "hazards before any older frame: {:?}",
            seen[hz].0
        );
        // one signal per frame and per forecast hour, at least
        assert!(seen.len() >= 10 + FORECASTS.len(), "{} signals", seen.len());
        let last = &seen.last().unwrap();
        assert_eq!(last.0, all);
        assert_eq!(
            (
                last.2["busy"].clone(),
                last.2["held"].clone(),
                last.2["wanted"].clone()
            ),
            (json!(false), json!(10), json!(10))
        );
        // while the older frames came in, progress counted up
        assert!(seen.iter().any(|x| x.2["busy"] == true
            && x.2["held"].as_u64().unwrap() > 1
            && x.2["held"].as_u64().unwrap() < 10));
    }

    #[test]
    fn hazards_are_refetched_every_fifteen_minutes_not_every_round() {
        let src = Fake::new(&[100]);
        let w = Weather::open(None);
        w.round(&src, 1000, &|| {});
        w.round(&src, 1000 + 120, &|| {});
        assert_eq!(*src.gair_calls.lock().unwrap(), FORECASTS.len() as u32);
        w.round(&src, 1000 + HAZARDS_EVERY, &|| {});
        assert_eq!(*src.gair_calls.lock().unwrap(), 2 * FORECASTS.len() as u32);
    }

    #[test]
    fn what_was_fetched_is_shown_again_offline() {
        let dir = std::env::temp_dir().join(format!("wx-test-{}", std::process::id()));
        let path = dir.join("weather.sqlite");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let w = Weather::open(Some(&path));
            w.round(&Fake::new(&[100, 220, 340]), 400, &|| {});
        }
        let w = Weather::open(Some(&path));
        w.load_from_disk();
        let s = w.state(400);
        assert_eq!(times(&w), vec![100, 220, 340]);
        assert_eq!(s["fromDisk"], true);
        assert_eq!(s["hazards"].as_array().unwrap().len(), 1);
        // and the network still down: nothing lost, the error reported
        let src = Fake::new(&[]);
        src.down.store(true, Ordering::SeqCst);
        w.round(&src, 500, &|| {});
        assert_eq!(times(&w), vec![100, 220, 340]);
        assert_eq!(w.state(500)["fromDisk"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_partly_fetched_set_never_drops_older_frames() {
        // the newest frame fails: the frames held are not pruned to a list we could not complete
        struct Flaky(Fake);
        impl Source for Flaky {
            fn index(&self) -> Result<Vec<(String, i64)>, String> {
                self.0.index()
            }
            fn frame(&self, name: &str) -> Result<Vec<u8>, String> {
                if name == "f999" {
                    Err("timeout".into())
                } else {
                    self.0.frame(name)
                }
            }
            fn gairmet(&self, f: u32) -> Result<String, String> {
                self.0.gairmet(f)
            }
        }
        let w = Weather::open(None);
        w.round(&Fake::new(&[1, 2]), 10, &|| {});
        let all: Vec<i64> = (100..110).chain([999]).collect();
        w.round(&Flaky(Fake::new(&all)), 20, &|| {});
        assert!(
            times(&w).contains(&1) && times(&w).contains(&2),
            "{:?}",
            times(&w)
        );
        assert!(!times(&w).contains(&999));
    }
}
