//! Weather for the native renderer, synced to one chosen time as the Aviation Weather Center's GFA
//! does it: MRMS radar and GOES satellite clouds (observations, from 18 hours back to now) and the
//! G-AIRMET hazard snapshots (forecasts, up to 12 hours ahead).
//!
//! What exists on the servers (the radar index, the satellite image times) is refreshed every round;
//! the frames and images themselves are fetched when a chosen time first needs them, kept compressed
//! in memory and on disk, and dropped least recently used past a cap. A background round keeps the
//! present warm: the newest radar frame first, then the hazards, the newest satellite image, and the
//! rest of the present's radar loop. With the network gone, whatever was fetched is still shown.
//!
//! Nothing here touches the network until the user turns weather on (`wx_enable`): the benchmarks
//! and the offline gate never do, so they stay offline.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use wx::hazards::{self, Hazard, Shape};
use wx::{radar, satellite, timeline};

/// Each radar frame thinned by this factor: 0.01° (1 km) to 0.02°, 3500 x 1750 cells.
pub const THIN: usize = 2;
pub const FORECASTS: [u32; 5] = [0, 3, 6, 9, 12];
/// How far back a chosen time may go (seconds): the GFA's 18 hours.
pub const HISTORY: i64 = 18 * 3600;
const LIVE_EVERY: Duration = Duration::from_secs(120);
const HAZARDS_EVERY: i64 = 15 * 60;
/// Compressed radar frames and satellite images kept (memory and disk). An 18-hour window at the
/// bar's 10-minute step is 108 radar frames; a radar frame compresses to a few hundred KB.
pub const RADAR_KEEP: usize = 160;
pub const SAT_KEEP: usize = 40;

/// Where the weather comes from: NOAA in the app, a fake in the tests.
pub trait Source: Send + Sync {
    fn index(&self) -> Result<Vec<(String, i64)>, String>;
    fn frame(&self, name: &str) -> Result<Vec<u8>, String>;
    fn gairmet(&self, fore: u32) -> Result<String, String>;
    fn satellite_times(&self) -> Result<Vec<i64>, String>;
    /// the server's PNG for the image valid at `t`
    fn satellite(&self, t: i64) -> Result<Vec<u8>, String>;
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
    fn satellite_times(&self) -> Result<Vec<i64>, String> {
        wx::fetch::satellite_times(&self.0)
    }
    fn satellite(&self, t: i64) -> Result<Vec<u8>, String> {
        wx::fetch::satellite_image(&self.0, t)
    }
}

/// The radar grid every frame shares (MRMS CONUS, thinned).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Grid {
    ni: usize,
    nj: usize,
    lat0: f64,
    lon0: f64,
    dlat: f64,
    dlon: f64,
}

#[derive(Default)]
struct Store {
    data: BTreeMap<i64, Arc<Vec<u8>>>,
    used: HashMap<i64, u64>,
}

#[derive(Default)]
struct Inner {
    /// radar frames on the server: (file name, time)
    index: Vec<(String, i64)>,
    sat_times: Vec<i64>,
    radar: Store,
    sat: Store,
    grid: Option<Grid>,
    /// each forecast hour's features, replaced only by a successful fetch of that hour
    gairmet: HashMap<u32, Vec<Hazard>>,
    radar_ok: Option<i64>,
    hazards_ok: Option<i64>,
    sat_ok: Option<i64>,
    /// the last round's failures, cleared by the next round that has none
    errors: Vec<String>,
    /// nothing fetched yet this session: what is shown came from the disk
    from_disk: bool,
    busy: bool,
    hazards_loading: bool,
    clock: u64,
    /// the present the last round refreshed: its loop and image are never evicted by browsing
    live_at: Option<i64>,
}

pub struct Weather {
    inner: Mutex<Inner>,
    db: Option<Mutex<Connection>>,
    started: AtomicBool,
    source: Mutex<Option<Arc<dyn Source>>>,
    /// keys being fetched now ("r" radar, "s" satellite): a second request waits for the first
    inflight: Mutex<HashSet<(char, i64)>>,
    radar_keep: usize,
    sat_keep: usize,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn deflate(b: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(b).expect("deflate into memory");
    e.finish().expect("deflate into memory")
}

fn inflate(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(b)
        .take(64 << 20)
        .read_to_end(&mut out)
        .map_err(|e| format!("inflate: {e}"))?;
    Ok(out)
}

impl Store {
    fn get(&mut self, t: i64, clock: &mut u64) -> Option<Arc<Vec<u8>>> {
        let d = self.data.get(&t)?.clone();
        *clock += 1;
        self.used.insert(t, *clock);
        Some(d)
    }
    /// Insert, then drop least-recently-used past `keep`, never one of `pinned`. Returns what went.
    fn put(
        &mut self,
        t: i64,
        d: Arc<Vec<u8>>,
        clock: &mut u64,
        keep: usize,
        pinned: &[i64],
    ) -> Vec<i64> {
        *clock += 1;
        self.data.insert(t, d);
        self.used.insert(t, *clock);
        let mut gone = Vec::new();
        while self.data.len() > keep {
            let victim = self
                .used
                .iter()
                .filter(|(k, _)| !pinned.contains(k))
                .min_by_key(|(_, &u)| u)
                .map(|(&k, _)| k);
            let Some(v) = victim else { break };
            self.data.remove(&v);
            self.used.remove(&v);
            gone.push(v);
        }
        gone
    }
}

impl Weather {
    /// `path` None: no disk copy (weather works, but not offline).
    pub fn open(path: Option<&Path>) -> Weather {
        Self::open_with(path, RADAR_KEEP, SAT_KEEP)
    }

    fn open_with(path: Option<&Path>, radar_keep: usize, sat_keep: usize) -> Weather {
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
            source: Mutex::new(None),
            inflight: Mutex::new(HashSet::new()),
            radar_keep,
            sat_keep,
        }
    }

    /// Start the background round, once: first what the disk holds, then NOAA every two minutes.
    pub fn enable(self: &Arc<Self>, source: Arc<dyn Source>, changed: Arc<dyn Fn() + Send + Sync>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.source.lock().expect("source lock") = Some(source.clone());
        let me = self.clone();
        std::thread::Builder::new()
            .name("weather".into())
            .spawn(move || {
                me.load_from_disk();
                changed();
                loop {
                    me.round(source.as_ref(), now(), changed.as_ref());
                    std::thread::sleep(LIVE_EVERY);
                }
            })
            .expect("spawn the weather thread");
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("weather lock")
    }

    fn load_from_disk(&self) {
        let Some(db) = &self.db else { return };
        let (radar, sat, gair, grid) = {
            let c = db.lock().expect("weather db lock");
            (
                read_blobs(&c, "radar").unwrap_or_default(),
                read_blobs(&c, "satellite").unwrap_or_default(),
                read_gairmet(&c).unwrap_or_default(),
                read_grid(&c),
            )
        };
        let mut gone_radar = Vec::new();
        let mut gone_sat = Vec::new();
        let mut g = self.lock();
        let g = &mut *g;
        if g.radar.data.is_empty() && grid.is_some() {
            g.grid = grid;
            for (t, d) in radar {
                gone_radar.extend(
                    g.radar
                        .put(t, Arc::new(d), &mut g.clock, self.radar_keep, &[]),
                );
                g.from_disk = true;
            }
        }
        if g.sat.data.is_empty() {
            for (t, d) in sat {
                gone_sat.extend(g.sat.put(t, Arc::new(d), &mut g.clock, self.sat_keep, &[]));
                g.from_disk = true;
            }
        }
        for (fore, json) in gair {
            if let Ok((h, _)) = hazards::parse(&json) {
                g.gairmet.entry(fore).or_insert(h);
                g.from_disk = true;
            }
        }
        // what did not fit under the caps leaves the disk too, or it would be reloaded every start
        self.delete_blobs("radar", &gone_radar);
        self.delete_blobs("satellite", &gone_sat);
    }

    /// The radar times that exist: the server's, plus any held (offline, or since rotated off).
    fn radar_times(&self) -> Vec<i64> {
        let g = self.lock();
        let mut t: Vec<i64> = g
            .index
            .iter()
            .map(|x| x.1)
            .chain(g.radar.data.keys().copied())
            .collect();
        t.sort_unstable();
        t.dedup();
        t
    }

    fn sat_times(&self) -> Vec<i64> {
        let g = self.lock();
        let mut t: Vec<i64> = g
            .sat_times
            .iter()
            .copied()
            .chain(g.sat.data.keys().copied())
            .collect();
        t.sort_unstable();
        t.dedup();
        t
    }

    fn hazard_times(&self) -> Vec<i64> {
        let g = self.lock();
        let mut t: Vec<i64> = g
            .gairmet
            .values()
            .filter_map(|h| h.first().map(|x| x.valid))
            .collect();
        t.sort_unstable();
        t.dedup();
        t
    }

    /// What to show at `at`.
    pub fn select(&self, at: i64) -> timeline::Selection {
        timeline::select(
            at,
            &self.radar_times(),
            &self.sat_times(),
            &self.hazard_times(),
        )
    }

    /// Run `fetch` once per key at a time: a second caller for the same key waits for the first and
    /// then finds the result held.
    fn single_flight<T>(
        &self,
        key: (char, i64),
        held: impl Fn() -> Option<T>,
        fetch: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        loop {
            if let Some(v) = held() {
                return Ok(v);
            }
            if self.inflight.lock().expect("inflight").insert(key) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // released however this ends, a panic included: a key left in flight would make every later
        // ask for that time wait forever (and with it the webview's refresh)
        struct Release<'a>(&'a Mutex<HashSet<(char, i64)>>, (char, i64));
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&self.1);
            }
        }
        let _release = Release(&self.inflight, key);
        match held() {
            Some(v) => Ok(v),
            None => fetch(),
        }
    }

    /// One radar frame's cells (wx::radar codes), fetched if not held.
    pub fn radar_frame(&self, src: &dyn Source, t: i64) -> Result<Vec<u8>, String> {
        let held = || {
            let mut g = self.lock();
            let g = &mut *g;
            g.radar.get(t, &mut g.clock)
        };
        let d = self.single_flight(('r', t), held, || {
            let name = self
                .lock()
                .index
                .iter()
                .find(|x| x.1 == t)
                .map(|x| x.0.clone())
                .ok_or_else(|| format!("no radar frame at {} on the server", satellite::iso(t)))?;
            let gz = src.frame(&name)?;
            let g = wx::grib::decode(&radar::gunzip(&gz, wx::fetch::MAX_FRAME)?)
                .map_err(|e| e.to_string())?;
            let r = radar::reduce(&g, THIN);
            let grid = Grid {
                ni: r.ni,
                nj: r.nj,
                lat0: r.lat0,
                lon0: r.lon0,
                dlat: r.dlat,
                dlon: r.dlon,
            };
            let d = Arc::new(deflate(&r.codes));
            let pinned = self.pinned_radar();
            let mut regrid = false;
            let gone = {
                let mut g = self.lock();
                let g = &mut *g;
                if g.grid.is_some_and(|old| old != grid) {
                    // a different grid upstream: the held frames no longer line up with the new one,
                    // in memory or on disk (else a restart would load them under the new grid)
                    g.radar = Store::default();
                    regrid = true;
                }
                g.grid = Some(grid);
                g.from_disk = false;
                g.radar
                    .put(t, d.clone(), &mut g.clock, self.radar_keep, &pinned)
            };
            if regrid {
                self.clear_table("radar");
            }
            self.store_blob("radar", t, &d, &gone);
            self.store_grid(grid);
            Ok(d)
        })?;
        inflate(&d)
    }

    /// One satellite image's grey levels (wx::satellite), fetched if not held.
    pub fn satellite_image(&self, src: &dyn Source, t: i64) -> Result<Vec<u8>, String> {
        let held = || {
            let mut g = self.lock();
            let g = &mut *g;
            g.sat.get(t, &mut g.clock)
        };
        let d = self.single_flight(('s', t), held, || {
            let png = src.satellite(t)?;
            let (_, h) = satellite::bbox();
            let img = satellite::decode(&png, satellite::WIDTH, h)?;
            let d = Arc::new(deflate(&img.grey));
            // the lock is released before select() takes it again (std's Mutex is not re-entrant)
            let live_at = self.lock().live_at;
            let pinned: Vec<i64> = live_at
                .and_then(|at| self.select(at).satellite)
                .into_iter()
                .collect();
            let gone = {
                let mut g = self.lock();
                let g = &mut *g;
                g.from_disk = false;
                g.sat
                    .put(t, d.clone(), &mut g.clock, self.sat_keep, &pinned)
            };
            self.store_blob("satellite", t, &d, &gone);
            Ok(d)
        })?;
        inflate(&d)
    }

    /// The present's loop is never evicted by browsing the past.
    fn pinned_radar(&self) -> Vec<i64> {
        let at = self.lock().live_at;
        at.map(|at| self.select(at).radar).unwrap_or_default()
    }

    /// One live update, most useful first so something shows at once: what exists on the servers,
    /// the newest radar frame, the hazards (if older than 15 minutes), the newest satellite image,
    /// then the rest of the present's radar loop. `changed` is called after every piece that
    /// arrives. A failure keeps what was there.
    pub fn round(&self, src: &dyn Source, now: i64, changed: &(dyn Fn() + Sync)) {
        let errors = Mutex::new(Vec::new());
        let err = |e: String| errors.lock().expect("errors").push(e);
        self.lock().busy = true;
        changed();
        match src.index() {
            Ok(idx) => self.lock().index = idx,
            Err(e) => err(format!("radar index: {e}")),
        }
        match src.satellite_times() {
            Ok(t) => self.lock().sat_times = t,
            Err(e) => err(format!("satellite times: {e}")),
        }
        self.lock().live_at = Some(now);
        let sel = self.select(now);
        let mut radar_left: Vec<i64> = sel.radar.iter().rev().copied().collect();
        if !radar_left.is_empty() {
            let newest = radar_left.remove(0);
            match self.radar_frame(src, newest) {
                Ok(_) => {
                    self.lock().radar_ok = Some(now);
                    changed();
                }
                Err(e) => err(format!("radar: {e}")),
            }
        }

        let due = self
            .lock()
            .hazards_ok
            .is_none_or(|t| now - t >= HAZARDS_EVERY);
        if due {
            self.lock().hazards_loading = true;
            changed();
            let mut ok = 0;
            for fore in FORECASTS {
                match src
                    .gairmet(fore)
                    .and_then(|s| hazards::parse(&s).map(|(h, _)| (s, h)))
                {
                    Ok((s, h)) => {
                        self.store_gairmet(fore, &s);
                        let mut g = self.lock();
                        g.gairmet.insert(fore, h);
                        g.from_disk = false;
                        drop(g);
                        ok += 1;
                        changed();
                    }
                    Err(e) => err(format!("hazards fore={fore}: {e}")),
                }
            }
            let mut g = self.lock();
            g.hazards_loading = false;
            if ok == FORECASTS.len() {
                g.hazards_ok = Some(now);
            }
        }

        if let Some(t) = sel.satellite {
            match self.satellite_image(src, t) {
                Ok(_) => {
                    self.lock().sat_ok = Some(now);
                    changed();
                }
                Err(e) => err(format!("satellite: {e}")),
            }
        }

        // the rest of the present's loop, three requests at a time
        let queue = Mutex::new(radar_left.into_iter());
        std::thread::scope(|sc| {
            for _ in 0..3 {
                sc.spawn(|| loop {
                    let next = queue.lock().expect("queue").next();
                    let Some(t) = next else { break };
                    match self.radar_frame(src, t) {
                        Ok(_) => changed(),
                        Err(e) => err(format!("radar: {e}")),
                    }
                });
            }
        });

        let mut g = self.lock();
        g.errors = errors.into_inner().expect("errors");
        g.busy = false;
        drop(g);
        changed();
    }

    fn store_blob(&self, table: &str, t: i64, d: &[u8], gone: &[i64]) {
        let Some(db) = &self.db else { return };
        let c = db.lock().expect("weather db lock");
        let r = c
            .execute(
                &format!("INSERT OR REPLACE INTO {table}(time, data) VALUES (?1, ?2)"),
                params![t, d],
            )
            .and_then(|_| {
                for v in gone {
                    c.execute(&format!("DELETE FROM {table} WHERE time = ?1"), params![v])?;
                }
                Ok(())
            });
        if let Err(e) = r {
            eprintln!("[weather] {table} not kept on disk ({e})");
        }
    }

    fn delete_blobs(&self, table: &str, times: &[i64]) {
        if times.is_empty() {
            return;
        }
        if let Some(db) = &self.db {
            let c = db.lock().expect("weather db lock");
            for t in times {
                let _ = c.execute(&format!("DELETE FROM {table} WHERE time = ?1"), params![t]);
            }
        }
    }

    fn clear_table(&self, table: &str) {
        if let Some(db) = &self.db {
            let _ = db
                .lock()
                .expect("weather db lock")
                .execute(&format!("DELETE FROM {table}"), []);
        }
    }

    fn store_grid(&self, g: Grid) {
        if let Some(db) = &self.db {
            let v = json!([g.ni, g.nj, g.lat0, g.lon0, g.dlat, g.dlon]).to_string();
            let _ = db.lock().expect("weather db lock").execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES ('grid', ?1)",
                params![v],
            );
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

    /// What the webview needs to draw at `at`: the selection, the hazard snapshot, the grids, what
    /// exists over the bar's range, and how fresh it all is.
    pub fn state(&self, at: i64, now: i64) -> Value {
        let sel = self.select(at);
        let radar_t = self.radar_times();
        let sat_t = self.sat_times();
        let hz_t = self.hazard_times();
        let g = self.lock();
        let held_radar: Vec<i64> = sel
            .radar
            .iter()
            .copied()
            .filter(|t| g.radar.data.contains_key(t))
            .collect();
        let snap = sel
            .hazards
            .map(|v| snapshot(&g.gairmet, v))
            .unwrap_or_default();
        let ([x0, y0, x1, y1], sat_h) = satellite::bbox();
        let span = |v: &[i64]| json!([v.first(), v.last()]);
        json!({
            "enabled": self.started.load(Ordering::SeqCst),
            "now": now,
            "at": at,
            "history": HISTORY,
            "selection": { "radar": sel.radar, "satellite": sel.satellite, "hazards": sel.hazards },
            "heldRadar": held_radar,
            "grid": g.grid.map(|r| json!({ "ni": r.ni, "nj": r.nj, "lat0": r.lat0, "lon0": r.lon0, "dlat": r.dlat, "dlon": r.dlon })),
            "satellite": { "width": satellite::WIDTH, "height": sat_h, "bbox3857": [x0, y0, x1, y1] },
            "available": { "radar": span(&radar_t), "satellite": span(&sat_t), "hazards": hz_t },
            "hazards": snap.iter().map(hazard_json).collect::<Vec<_>>(),
            "hazardsValid": sel.hazards,
            "radarOk": g.radar_ok,
            "hazardsOk": g.hazards_ok,
            "satelliteOk": g.sat_ok,
            "fromDisk": g.from_disk,
            "loading": { "busy": g.busy, "hazards": g.hazards_loading },
            "errors": g.errors,
        })
    }

    pub fn source(&self) -> Option<Arc<dyn Source>> {
        self.source.lock().expect("source lock").clone()
    }
}

/// The features of ONE forecast hour valid at `valid` (the lowest hour if two are: a new package
/// issued mid-round leaves two hours valid at the same time, and merging them would draw an area
/// twice).
fn snapshot(by_fore: &HashMap<u32, Vec<Hazard>>, valid: i64) -> Vec<Hazard> {
    let fore = by_fore
        .iter()
        .filter(|(_, h)| h.first().is_some_and(|x| x.valid == valid))
        .map(|(f, _)| *f)
        .min();
    match fore {
        Some(f) => by_fore[&f]
            .iter()
            .filter(|h| h.valid == valid)
            .cloned()
            .collect(),
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
    // v1 kept raw NOAA files in radar(time, gz); v2 keeps the reduced frames: start that table over
    let v1: bool = c
        .query_row(
            "SELECT count(*) FROM pragma_table_info('radar') WHERE name = 'gz'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n > 0)
        .unwrap_or(false);
    if v1 {
        c.execute_batch("DROP TABLE radar;")?;
    }
    c.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS radar(time INTEGER PRIMARY KEY, data BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS satellite(time INTEGER PRIMARY KEY, data BLOB NOT NULL);
         CREATE TABLE IF NOT EXISTS gairmet(fore INTEGER PRIMARY KEY, json TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;
    Ok(c)
}

fn read_blobs(c: &Connection, table: &str) -> rusqlite::Result<Vec<(i64, Vec<u8>)>> {
    let mut s = c.prepare(&format!("SELECT time, data FROM {table} ORDER BY time"))?;
    let rows = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

fn read_grid(c: &Connection) -> Option<Grid> {
    let v: String = c
        .query_row("SELECT value FROM meta WHERE key = 'grid'", [], |r| {
            r.get(0)
        })
        .ok()?;
    let a: Vec<f64> = serde_json::from_str(&v).ok()?;
    (a.len() == 6).then(|| Grid {
        ni: a[0] as usize,
        nj: a[1] as usize,
        lat0: a[2],
        lon0: a[3],
        dlat: a[4],
        dlon: a[5],
    })
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

    /// A satellite PNG of the real size, every pixel grey `g` (0 = transparent).
    fn sat_png(g: u8) -> Vec<u8> {
        let (_, h) = satellite::bbox();
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, satellite::WIDTH, h);
            e.set_color(png::ColorType::GrayscaleAlpha);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            let px: Vec<u8> = std::iter::repeat_n(
                [g, if g == 0 { 0 } else { 255 }],
                (satellite::WIDTH * h) as usize,
            )
            .flatten()
            .collect();
            w.write_image_data(&px).unwrap();
        }
        out
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
    /// "now" in these tests: 19:00Z, with radar every 2 minutes for the 24 hours before it
    const NOW: i64 = T18 + 3600;

    /// A NOAA that serves what the test says, counts what it is asked, and fails when told to.
    struct Fake {
        radar: M<Vec<(String, i64)>>,
        sat: M<Vec<i64>>,
        down: AtomicBool,
        frame_calls: M<Vec<i64>>,
        sat_calls: M<Vec<i64>>,
        gair_calls: M<u32>,
    }

    impl Fake {
        fn new(radar_times: &[i64], sat_times: &[i64]) -> Fake {
            Fake {
                radar: M::new(radar_times.iter().map(|t| (format!("f{t}"), *t)).collect()),
                sat: M::new(sat_times.to_vec()),
                down: AtomicBool::new(false),
                frame_calls: M::new(vec![]),
                sat_calls: M::new(vec![]),
                gair_calls: M::new(0),
            }
        }
        fn live() -> Fake {
            let radar: Vec<i64> = (0..720).rev().map(|k| NOW - k * 120).collect();
            let sat: Vec<i64> = (0..78).rev().map(|k| NOW - 120 - k * 300).collect();
            Fake::new(&radar, &sat)
        }
        fn up(&self) -> Result<(), String> {
            if self.down.load(Ordering::SeqCst) {
                Err("unreachable".into())
            } else {
                Ok(())
            }
        }
    }

    impl Source for Fake {
        fn index(&self) -> Result<Vec<(String, i64)>, String> {
            self.up()?;
            Ok(self.radar.lock().unwrap().clone())
        }
        fn frame(&self, name: &str) -> Result<Vec<u8>, String> {
            self.frame_calls
                .lock()
                .unwrap()
                .push(name[1..].parse().unwrap());
            self.up()?;
            Ok(frame_gz(40.0))
        }
        fn gairmet(&self, fore: u32) -> Result<String, String> {
            *self.gair_calls.lock().unwrap() += 1;
            self.up()?;
            Ok(turb(fore))
        }
        fn satellite_times(&self) -> Result<Vec<i64>, String> {
            self.up()?;
            Ok(self.sat.lock().unwrap().clone())
        }
        fn satellite(&self, t: i64) -> Result<Vec<u8>, String> {
            self.sat_calls.lock().unwrap().push(t);
            self.up()?;
            Ok(sat_png(180))
        }
    }

    fn sel(s: &Value) -> Vec<i64> {
        s["selection"]["radar"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect()
    }

    #[test]
    fn the_live_round_fetches_the_present_loop_newest_first_then_hazards_then_satellite() {
        let w = Weather::open(None);
        let src = Fake::live();
        let seen = M::new(Vec::new()); // (held loop frames, hazards, satellite ok) at each signal
        w.round(&src, NOW, &|| {
            let s = w.state(NOW, NOW);
            seen.lock().unwrap().push((
                s["heldRadar"].as_array().unwrap().len(),
                s["hazards"].as_array().unwrap().len(),
                !s["satelliteOk"].is_null(),
            ));
        });
        let s = w.state(NOW, NOW);
        assert_eq!(
            sel(&s),
            (0..5).rev().map(|k| NOW - k * 600).collect::<Vec<_>>(),
            "5 frames, 10 minutes apart, ending now"
        );
        assert_eq!(
            src.frame_calls.lock().unwrap().len(),
            5,
            "each fetched once"
        );
        assert_eq!(src.frame_calls.lock().unwrap()[0], NOW, "the newest first");
        let seen = seen.into_inner().unwrap();
        let first_frame = seen.iter().position(|x| x.0 > 0).unwrap();
        let first_hz = seen.iter().position(|x| x.1 > 0).unwrap();
        let first_sat = seen.iter().position(|x| x.2).unwrap();
        assert!(
            seen[first_hz].0 == 1 && seen[first_sat].0 == 1,
            "hazards and satellite before the older frames: {seen:?}"
        );
        assert!(first_frame < first_hz && first_hz < first_sat);
        assert_eq!(s["selection"]["satellite"], NOW - 120);
        assert_eq!(s["heldRadar"].as_array().unwrap().len(), 5);
        assert!(
            s["errors"].as_array().unwrap().is_empty(),
            "{}",
            s["errors"]
        );
    }

    #[test]
    fn a_fetch_that_panics_does_not_leave_its_time_stuck() {
        struct Boom(Fake, AtomicBool);
        impl Source for Boom {
            fn index(&self) -> Result<Vec<(String, i64)>, String> {
                self.0.index()
            }
            fn frame(&self, name: &str) -> Result<Vec<u8>, String> {
                if !self.1.swap(true, Ordering::SeqCst) {
                    panic!("malformed frame");
                }
                self.0.frame(name)
            }
            fn gairmet(&self, f: u32) -> Result<String, String> {
                self.0.gairmet(f)
            }
            fn satellite_times(&self) -> Result<Vec<i64>, String> {
                self.0.satellite_times()
            }
            fn satellite(&self, t: i64) -> Result<Vec<u8>, String> {
                self.0.satellite(t)
            }
        }
        let w = Arc::new(Weather::open(None));
        let src = Arc::new(Boom(Fake::live(), AtomicBool::new(false)));
        w.lock().index = src.0.radar.lock().unwrap().clone();
        let t = NOW - 3 * 3600;
        let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            w.radar_frame(src.as_ref(), t)
        }));
        assert!(first.is_err(), "the first fetch panicked");
        // a detached thread: if the second ask hangs, the test fails at the timeout instead of hanging
        let (tx, rx) = std::sync::mpsc::channel();
        let (w2, src2) = (w.clone(), src.clone());
        std::thread::spawn(move || {
            let _ = tx.send(w2.radar_frame(src2.as_ref(), t));
        });
        let second = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the second ask for that time hung");
        assert_eq!(second.unwrap(), vec![radar::code(40.0)]);
    }

    #[test]
    fn a_new_radar_grid_clears_the_old_frames_from_disk_too() {
        let dir = std::env::temp_dir().join(format!("wx-grid-{}", std::process::id()));
        let path = dir.join("weather.sqlite");
        let _ = std::fs::remove_dir_all(&dir);
        let src = Fake::live();
        {
            let w = Weather::open(Some(&path));
            w.round(&src, NOW, &|| {});
            // upstream changes its grid: what is held was cut on a different one
            *w.lock().grid.as_mut().unwrap() = Grid {
                ni: 9,
                nj: 9,
                lat0: 0.0,
                lon0: 0.0,
                dlat: 1.0,
                dlon: 1.0,
            };
            w.radar_frame(&src, NOW - 4 * 3600).unwrap();
            assert_eq!(
                w.lock().radar.data.len(),
                1,
                "the old grid's frames dropped from memory"
            );
        }
        let w = Weather::open(Some(&path));
        w.load_from_disk();
        assert_eq!(
            w.lock().radar.data.keys().copied().collect::<Vec<_>>(),
            vec![NOW - 4 * 3600],
            "and from disk"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rows_dropped_past_the_cap_at_load_leave_the_disk() {
        let dir = std::env::temp_dir().join(format!("wx-cap-{}", std::process::id()));
        let path = dir.join("weather.sqlite");
        let _ = std::fs::remove_dir_all(&dir);
        let src = Fake::live();
        {
            let w = Weather::open(Some(&path));
            w.round(&src, NOW, &|| {});
        }
        {
            let w = Weather::open_with(Some(&path), 2, 2);
            w.load_from_disk();
        }
        let c = Connection::open(&path).unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM radar", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2, "the disk matches what memory kept");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_past_time_is_fetched_on_demand_once() {
        let w = Weather::open(None);
        let src = Fake::live();
        w.round(&src, NOW, &|| {});
        let past = NOW - 6 * 3600;
        let s = w.state(past, NOW);
        assert_eq!(sel(&s).len(), 5);
        assert!(
            s["heldRadar"].as_array().unwrap().is_empty(),
            "not fetched until asked"
        );
        let before = src.frame_calls.lock().unwrap().len();
        let codes = w.radar_frame(&src, past).unwrap();
        assert_eq!(codes, vec![radar::code(40.0)]);
        w.radar_frame(&src, past).unwrap();
        assert_eq!(
            src.frame_calls.lock().unwrap().len(),
            before + 1,
            "the second ask is served from memory"
        );
        assert!(
            w.radar_frame(&src, past + 1)
                .unwrap_err()
                .contains("no radar frame"),
            "a time the server does not have"
        );
    }

    #[test]
    fn concurrent_asks_for_one_frame_fetch_it_once() {
        let w = Weather::open(None);
        let src = Fake::live();
        w.round(&src, NOW, &|| {});
        let t = NOW - 3 * 3600;
        let before = src.frame_calls.lock().unwrap().len();
        std::thread::scope(|sc| {
            for _ in 0..4 {
                sc.spawn(|| w.radar_frame(&src, t).unwrap());
            }
        });
        assert_eq!(src.frame_calls.lock().unwrap().len(), before + 1);
    }

    #[test]
    fn browsing_the_past_evicts_least_recently_used_but_never_the_present() {
        let w = Weather::open_with(None, 7, 2);
        let src = Fake::live();
        w.round(&src, NOW, &|| {}); // the present's 5 frames, pinned
        for h in 1..=6 {
            w.radar_frame(&src, NOW - h * 3600).unwrap();
        }
        let held: Vec<i64> = w.lock().radar.data.keys().copied().collect();
        assert_eq!(held.len(), 7);
        for k in 0..5 {
            assert!(
                held.contains(&(NOW - k * 600)),
                "present frame {k} evicted: {held:?}"
            );
        }
        assert!(
            held.contains(&(NOW - 6 * 3600)) && held.contains(&(NOW - 5 * 3600)),
            "the two most recent asks stay: {held:?}"
        );
    }

    #[test]
    fn satellite_on_demand_and_decoded() {
        let w = Weather::open(None);
        let src = Fake::live();
        w.round(&src, NOW, &|| {});
        let grey = w.satellite_image(&src, NOW - 3 * 3600 - 120).unwrap();
        let (_, h) = satellite::bbox();
        assert_eq!(grey.len(), (satellite::WIDTH * h) as usize);
        assert!(grey.iter().all(|&g| g == 180));
        let s = w.state(NOW - 3 * 3600, NOW);
        assert_eq!(s["selection"]["satellite"], NOW - 3 * 3600 - 120);
    }

    #[test]
    fn a_failure_keeps_what_was_there_and_says_so() {
        let w = Weather::open(None);
        let src = Fake::live();
        w.round(&src, NOW, &|| {});
        src.down.store(true, Ordering::SeqCst);
        w.round(&src, NOW + HAZARDS_EVERY, &|| {});
        let s = w.state(NOW, NOW + HAZARDS_EVERY);
        assert_eq!(
            s["heldRadar"].as_array().unwrap().len(),
            5,
            "frames kept through the outage"
        );
        assert_eq!(s["hazards"].as_array().unwrap().len(), 1);
        let errors: Vec<&str> = s["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap())
            .collect();
        assert!(
            errors.iter().any(|e| e.starts_with("radar index"))
                && errors.iter().any(|e| e.starts_with("hazards fore=0"))
                && errors.iter().any(|e| e.starts_with("satellite times")),
            "{errors:?}"
        );
        src.down.store(false, Ordering::SeqCst);
        w.round(&src, NOW + 2 * HAZARDS_EVERY, &|| {});
        assert!(
            w.state(NOW, NOW)["errors"].as_array().unwrap().is_empty(),
            "errors clear on a good round"
        );
    }

    #[test]
    fn hazards_follow_the_chosen_time_one_forecast_hour_at_a_time() {
        let w = Weather::open(None);
        w.round(&Fake::live(), NOW, &|| {});
        for (at, valid) in [
            (T18, T18),
            (T18 + 3600, T18),
            (T18 + 2 * 3600, T18 + 3 * 3600),
            (T18 + 13 * 3600, T18 + 12 * 3600),
        ] {
            let s = w.state(at, NOW);
            assert_eq!(s["hazards"].as_array().unwrap().len(), 1, "at {at}");
            assert_eq!(s["hazardsValid"], valid, "at {at}");
        }
        let far = w.state(T18 + 40 * 3600, NOW);
        assert!(
            far["hazards"].as_array().unwrap().is_empty() && far["hazardsValid"].is_null(),
            "no snapshot near: none shown"
        );
        let mut by_fore = HashMap::new();
        for fore in [0, 3] {
            by_fore.insert(fore, hazards::parse(&turb(0)).unwrap().0);
        }
        assert_eq!(
            snapshot(&by_fore, T18).len(),
            1,
            "two hours valid at once: one set, not both"
        );
    }

    #[test]
    fn hazards_are_refetched_every_fifteen_minutes_not_every_round() {
        let w = Weather::open(None);
        let src = Fake::live();
        w.round(&src, NOW, &|| {});
        w.round(&src, NOW + 120, &|| {});
        assert_eq!(*src.gair_calls.lock().unwrap(), FORECASTS.len() as u32);
        w.round(&src, NOW + HAZARDS_EVERY, &|| {});
        assert_eq!(*src.gair_calls.lock().unwrap(), 2 * FORECASTS.len() as u32);
    }

    #[test]
    fn what_was_fetched_is_shown_again_offline() {
        let dir = std::env::temp_dir().join(format!("wx-test-{}", std::process::id()));
        let path = dir.join("weather.sqlite");
        let _ = std::fs::remove_dir_all(&dir);
        {
            let w = Weather::open(Some(&path));
            let src = Fake::live();
            w.round(&src, NOW, &|| {});
            w.radar_frame(&src, NOW - 5 * 3600).unwrap();
        }
        let w = Weather::open(Some(&path));
        w.load_from_disk();
        let src = Fake::new(&[], &[]);
        src.down.store(true, Ordering::SeqCst);
        w.round(&src, NOW, &|| {});
        let s = w.state(NOW, NOW);
        assert_eq!(s["fromDisk"], true);
        assert_eq!(
            s["heldRadar"].as_array().unwrap().len(),
            5,
            "the present's loop from disk"
        );
        assert_eq!(s["hazards"].as_array().unwrap().len(), 1);
        assert_eq!(s["selection"]["satellite"], NOW - 120);
        assert_eq!(
            w.radar_frame(&src, NOW - 5 * 3600).unwrap(),
            vec![radar::code(40.0)],
            "a past frame from disk, with the network down"
        );
        assert_eq!(src.frame_calls.lock().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_v1_cache_file_is_upgraded_not_misread() {
        let dir = std::env::temp_dir().join(format!("wx-v1-{}", std::process::id()));
        let path = dir.join("weather.sqlite");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE radar(time INTEGER PRIMARY KEY, gz BLOB NOT NULL); INSERT INTO radar VALUES (1, x'00');").unwrap();
        }
        let w = Weather::open(Some(&path));
        w.load_from_disk();
        assert!(w.lock().radar.data.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
