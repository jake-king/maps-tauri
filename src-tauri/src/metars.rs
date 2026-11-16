//! METAR history, collected: every few minutes the AWC cache file (each station's latest report) is
//! fetched and any report not yet held is kept, in memory and in a SQLite table, for the time bar's
//! 18 hours. The API's own history is capped at 400 observations a request (one past hour over the US
//! was 30 requests and 80 seconds), so history starts when this app first ran with METARs on.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use wx::metar::{self, Metar};

/// Reports are kept this long (the bar's history plus a report's validity).
pub const KEEP: i64 = crate::weather::HISTORY + metar::VALID_FOR;
pub const EVERY: Duration = Duration::from_secs(300);

#[derive(Default)]
struct Inner {
    /// each station's reports, oldest first
    by_station: HashMap<String, Vec<Metar>>,
    /// bumped when anything new arrives: the webview refetches only then
    version: u64,
    fetched: Option<i64>,
    error: Option<String>,
}

pub struct Metars {
    inner: Mutex<Inner>,
    db: Option<Mutex<Connection>>,
    started: AtomicBool,
}

impl Metars {
    pub fn open(path: Option<&Path>) -> Metars {
        let db = path.and_then(|p| {
            let r = Connection::open(p).and_then(|c| {
                c.busy_timeout(Duration::from_secs(2))?;
                c.execute_batch(
                    "PRAGMA journal_mode=WAL;
                     CREATE TABLE IF NOT EXISTS metar(id TEXT NOT NULL, time INTEGER NOT NULL, lon REAL NOT NULL,
                       lat REAL NOT NULL, cat INTEGER NOT NULL, raw TEXT NOT NULL, PRIMARY KEY(id, time));",
                )?;
                Ok(c)
            });
            match r {
                Ok(c) => Some(Mutex::new(c)),
                Err(e) => {
                    eprintln!("[metars] {} will not open ({e}); history is not kept", p.display());
                    None
                }
            }
        });
        Metars {
            inner: Mutex::new(Inner::default()),
            db,
            started: AtomicBool::new(false),
        }
    }

    /// Start collecting, once: what the disk holds, then the cache file every five minutes.
    /// `changed` is told when anything new arrives.
    pub fn start(
        self: &Arc<Self>,
        fetch: impl Fn() -> Result<Vec<Metar>, String> + Send + 'static,
        changed: Arc<dyn Fn() + Send + Sync>,
    ) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        std::thread::Builder::new()
            .name("metars".into())
            .spawn(move || {
                me.load(crate::unix_now());
                changed();
                loop {
                    if me.refresh(&fetch, crate::unix_now()) > 0 {
                        changed();
                    }
                    std::thread::sleep(EVERY);
                }
            })
            .expect("spawn the metars thread");
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("metars lock")
    }

    /// What the disk holds from earlier runs, back to `now - KEEP`.
    pub fn load(&self, now: i64) {
        let Some(db) = &self.db else { return };
        let rows: Vec<Metar> = {
            let c = db.lock().expect("metars db lock");
            let r = c
                .prepare(
                    "SELECT id, time, lon, lat, cat, raw FROM metar WHERE time >= ?1 ORDER BY time",
                )
                .and_then(|mut s| {
                    s.query_map(params![now - KEEP], |r| {
                        Ok(Metar {
                            id: r.get(0)?,
                            time: r.get(1)?,
                            lon: r.get(2)?,
                            lat: r.get(3)?,
                            cat: r.get(4)?,
                            raw: r.get(5)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                });
            r.unwrap_or_default()
        };
        self.add(rows, now, false);
    }

    /// Keep the reports not yet held; drop what is older than `now - KEEP`. Returns how many were new.
    pub fn add(&self, reports: Vec<Metar>, now: i64, persist: bool) -> usize {
        let mut fresh = Vec::new();
        {
            let mut g = self.lock();
            for m in reports {
                if m.time < now - KEEP {
                    continue;
                }
                let v = g.by_station.entry(m.id.clone()).or_default();
                let i = v.partition_point(|x| x.time < m.time);
                if v.get(i).is_some_and(|x| x.time == m.time) {
                    continue; // already held: the cache file repeats a report until the next one
                }
                v.insert(i, m.clone());
                fresh.push(m);
            }
            for v in g.by_station.values_mut() {
                let cut = v.partition_point(|x| x.time < now - KEEP);
                v.drain(..cut);
            }
            g.by_station.retain(|_, v| !v.is_empty());
            if !fresh.is_empty() {
                g.version += 1;
            }
        }
        if persist && !fresh.is_empty() {
            if let Some(db) = &self.db {
                let mut c = db.lock().expect("metars db lock");
                let r = c.transaction().and_then(|tx| {
                    for m in &fresh {
                        tx.execute("INSERT OR IGNORE INTO metar(id, time, lon, lat, cat, raw) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![m.id, m.time, m.lon, m.lat, m.cat, m.raw])?;
                    }
                    tx.execute("DELETE FROM metar WHERE time < ?1", params![now - KEEP])?;
                    tx.commit()
                });
                if let Err(e) = r {
                    eprintln!("[metars] not kept on disk ({e})");
                }
            }
        }
        fresh.len()
    }

    /// One refresh from `fetch` (the cache file); a failure keeps what was held and is reported.
    pub fn refresh(&self, fetch: impl Fn() -> Result<Vec<Metar>, String>, now: i64) -> usize {
        match fetch() {
            Ok(m) => {
                let n = self.add(m, now, true);
                let mut g = self.lock();
                g.fetched = Some(now);
                g.error = None;
                n
            }
            Err(e) => {
                self.lock().error = Some(e);
                0
            }
        }
    }

    /// The reports in force at `at`, compact for the webview: [id, lon, lat, category, time] each.
    pub fn at(&self, at: i64) -> Value {
        let g = self.lock();
        let list = metar::at_time(g.by_station.values().map(Vec::as_slice), at);
        let (first, last) = g
            .by_station
            .values()
            .fold((None::<i64>, None::<i64>), |(a, b), v| {
                (
                    Some(a.map_or(v[0].time, |a| a.min(v[0].time))),
                    Some(b.map_or(v[v.len() - 1].time, |b| b.max(v[v.len() - 1].time))),
                )
            });
        json!({
            "version": g.version,
            "at": at,
            "stations": list.iter().map(|m| json!([m.id, m.lon, m.lat, m.cat, m.time])).collect::<Vec<_>>(),
            "range": [first, last],
            "fetched": g.fetched,
            "error": g.error,
        })
    }

    /// One station's report in force at `at`: the raw text and when it was observed.
    pub fn report(&self, id: &str, at: i64) -> Option<(String, i64)> {
        let g = self.lock();
        let v = g.by_station.get(id)?;
        metar::at_time(std::iter::once(v.as_slice()), at)
            .first()
            .map(|m| (m.raw.clone(), m.time))
    }

    #[cfg(test)]
    pub fn version(&self) -> u64 {
        self.lock().version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, t: i64, cat: u8) -> Metar {
        Metar {
            id: id.into(),
            time: t,
            lon: -118.0,
            lat: 34.0,
            cat,
            raw: format!("METAR {id} at {t}"),
        }
    }

    const NOW: i64 = 1_794_870_000;

    #[test]
    fn collects_new_reports_once_and_bumps_the_version_only_then() {
        let s = Metars::open(None);
        assert_eq!(
            s.add(
                vec![m("KLAX", NOW - 600, 1), m("KSFO", NOW - 300, 4)],
                NOW,
                false
            ),
            2
        );
        let v = s.version();
        // the next fetch of the cache file repeats both and adds a newer KLAX
        assert_eq!(
            s.add(
                vec![
                    m("KLAX", NOW - 600, 1),
                    m("KSFO", NOW - 300, 4),
                    m("KLAX", NOW, 2)
                ],
                NOW,
                false
            ),
            1
        );
        assert_eq!(s.version(), v + 1);
        assert_eq!(s.add(vec![m("KLAX", NOW, 2)], NOW, false), 0);
        assert_eq!(
            s.version(),
            v + 1,
            "nothing new: the webview need not refetch"
        );
    }

    #[test]
    fn the_reports_in_force_at_a_time() {
        let s = Metars::open(None);
        s.add(
            vec![
                m("KLAX", NOW - 7200, 1),
                m("KLAX", NOW - 3600, 3),
                m("KSFO", NOW - 10_000, 4),
            ],
            NOW,
            false,
        );
        let at = |t: i64| {
            s.at(t)["stations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| (x[0].as_str().unwrap().to_string(), x[3].as_u64().unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            at(NOW - 3000),
            vec![("KLAX".to_string(), 3)],
            "KSFO's report is past 75 minutes"
        );
        assert_eq!(at(NOW - 5000), vec![("KLAX".to_string(), 1)]);
        assert_eq!(
            s.report("KLAX", NOW - 5000),
            Some((
                "METAR KLAX at ".to_string() + &(NOW - 7200).to_string(),
                NOW - 7200
            ))
        );
        assert_eq!(s.report("KLAX", NOW - 9000), None);
        assert_eq!(s.at(NOW)["range"], json!([NOW - 10_000, NOW - 3600]));
    }

    #[test]
    fn keeps_the_bar_s_history_and_no_more() {
        let s = Metars::open(None);
        s.add(
            vec![m("OLD", NOW - KEEP - 1, 1), m("KLAX", NOW - KEEP + 60, 1)],
            NOW,
            false,
        );
        assert!(s.report("OLD", NOW - KEEP).is_none(), "too old to keep");
        s.add(vec![], NOW + 120, false);
        assert!(s.lock().by_station.is_empty(), "pruned as time moves on");
    }

    #[test]
    fn a_failed_refresh_keeps_what_was_held_and_says_why() {
        let s = Metars::open(None);
        s.refresh(|| Ok(vec![m("KLAX", NOW, 1)]), NOW);
        s.refresh(|| Err("504".into()), NOW + 300);
        let st = s.at(NOW + 300);
        assert_eq!(st["stations"].as_array().unwrap().len(), 1);
        assert_eq!(st["error"], "504");
        assert_eq!(st["fetched"], NOW);
        s.refresh(|| Ok(vec![]), NOW + 600);
        assert!(
            s.at(NOW)["error"].is_null(),
            "cleared by the next good fetch"
        );
    }

    #[test]
    fn history_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("metars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("metars.sqlite");
        {
            let s = Metars::open(Some(&path));
            s.refresh(
                || Ok(vec![m("KLAX", NOW - 3600, 3), m("KSFO", NOW - 600, 4)]),
                NOW,
            );
            s.refresh(|| Ok(vec![m("KLAX", NOW, 1)]), NOW + 300);
        }
        let s = Metars::open(Some(&path));
        s.load(NOW + 300);
        assert_eq!(s.report("KLAX", NOW - 1000).unwrap().1, NOW - 3600);
        assert_eq!(s.report("KLAX", NOW + 300).unwrap().1, NOW);
        assert_eq!(s.at(NOW)["stations"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
