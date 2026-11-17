//! A tape of NOAA's answers, so a run can be repeated exactly without the network: on every
//! platform in CI, and offline. `MAPS_TAPE_RECORD=<dir>` saves each response under a key derived from
//! its URL (and the start time in `now.txt`); `MAPS_TAPE_REPLAY=<dir>` answers from the tape alone and
//! starts the clock at the recorded time, running on at real speed. A request not on the tape is an
//! error naming its URL, never a silent fetch.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Off,
    Record(PathBuf),
    Replay(PathBuf),
}

pub struct Tape {
    pub mode: Mode,
    /// Unix seconds at `started`: the recorded start when replaying, the real time otherwise
    start: i64,
    started: Instant,
    index: Mutex<()>,
}

fn real_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The file a URL's response is kept in: FNV-1a of the URL, so the same request finds the same file.
pub fn key(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}.bin")
}

impl Tape {
    pub fn new(mode: Mode) -> Result<Tape, String> {
        let start = match &mode {
            Mode::Off => real_now(),
            Mode::Record(dir) => {
                std::fs::create_dir_all(dir).map_err(|e| format!("tape {}: {e}", dir.display()))?;
                // a second run recording onto the same tape keeps the first run's start
                let held = std::fs::read_to_string(dir.join("now.txt"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok());
                match held {
                    Some(t) => t,
                    None => {
                        let t = real_now();
                        std::fs::write(dir.join("now.txt"), t.to_string())
                            .map_err(|e| format!("tape: {e}"))?;
                        t
                    }
                }
            }
            Mode::Replay(dir) => std::fs::read_to_string(dir.join("now.txt"))
                .map_err(|e| format!("tape {}: no now.txt ({e})", dir.display()))?
                .trim()
                .parse()
                .map_err(|e| format!("tape now.txt: {e}"))?,
        };
        Ok(Tape {
            mode,
            start,
            started: Instant::now(),
            index: Mutex::new(()),
        })
    }

    /// The clock: the recorded start plus the time since this run began, when replaying.
    pub fn now(&self) -> i64 {
        match self.mode {
            Mode::Replay(_) => self.start + self.started.elapsed().as_secs() as i64,
            _ => real_now(),
        }
    }

    /// The response for `url`: from the tape when replaying (None if it is not there), else None.
    pub fn replay(&self, url: &str) -> Option<Result<Vec<u8>, String>> {
        let Mode::Replay(dir) = &self.mode else {
            return None;
        };
        Some(
            std::fs::read(dir.join(key(url)))
                .map_err(|_| format!("{url}: not on the tape ({})", dir.display())),
        )
    }

    /// Keep `body` as the response for `url`, when recording.
    pub fn record(&self, url: &str, body: &[u8]) {
        let Mode::Record(dir) = &self.mode else {
            return;
        };
        let _g = self.index.lock().expect("tape index");
        let k = key(url);
        if std::fs::write(dir.join(&k), body).is_ok() {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("index.tsv"))
            {
                let _ = writeln!(f, "{k}\t{url}");
            }
        }
    }
}

fn from_env() -> Tape {
    let var = |k: &str| {
        std::env::var(k)
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let mode = match (var("MAPS_TAPE_REPLAY"), var("MAPS_TAPE_RECORD")) {
        (Some(d), _) => Mode::Replay(d),
        (None, Some(d)) => Mode::Record(d),
        _ => Mode::Off,
    };
    Tape::new(mode).unwrap_or_else(|e| panic!("{e}"))
}

/// The process's tape, from the environment on first use.
pub fn global() -> &'static Tape {
    static T: OnceLock<Tape> = OnceLock::new();
    T.get_or_init(from_env)
}

/// The app's clock (Unix seconds): the tape's when replaying, else the system's.
pub fn now() -> i64 {
    global().now()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tape-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn recorded_answers_replay_by_url_and_nothing_else_does() {
        let d = dir("rr");
        let rec = Tape::new(Mode::Record(d.clone())).unwrap();
        assert_eq!(
            rec.replay("https://x/a"),
            None,
            "recording: not answered from the tape"
        );
        rec.record("https://x/a?b=1", b"one");
        rec.record("https://x/a?b=2", b"two");
        let start: i64 = std::fs::read_to_string(d.join("now.txt"))
            .unwrap()
            .parse()
            .unwrap();
        let rep = Tape::new(Mode::Replay(d.clone())).unwrap();
        assert_eq!(rep.replay("https://x/a?b=1"), Some(Ok(b"one".to_vec())));
        assert_eq!(rep.replay("https://x/a?b=2"), Some(Ok(b"two".to_vec())));
        let miss = rep.replay("https://x/a?b=3").unwrap().unwrap_err();
        assert!(
            miss.contains("https://x/a?b=3") && miss.contains("not on the tape"),
            "{miss}"
        );
        let n = rep.now();
        assert!(
            n >= start && n <= start + 2,
            "the clock starts at the recorded time"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("index.tsv"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        // a second recording run onto the same tape keeps the first's start
        std::thread::sleep(std::time::Duration::from_millis(1100));
        Tape::new(Mode::Record(d.clone())).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("now.txt")).unwrap(),
            start.to_string()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn replay_without_a_tape_is_an_error() {
        assert!(Tape::new(Mode::Replay(dir("none"))).is_err());
    }

    #[test]
    fn keys_are_stable_and_distinct() {
        assert_eq!(key("https://a"), key("https://a"));
        assert_ne!(key("https://a?x=1"), key("https://a?x=2"));
    }
}
