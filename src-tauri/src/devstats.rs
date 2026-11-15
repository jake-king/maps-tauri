//! Numbers for the F4 overlay that only the backend can see: memory for every process the app
//! runs, and how long the `tiles://` handler takes.
//!
//! Memory is Linux-only (`/proc`). On Linux, WebKitGTK renders in separate processes (a web
//! process, a network process, sometimes inside a bubblewrap sandbox), so the app's own RSS is a
//! small part of the total: the process tree is walked from this process down.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

/// Tile handler timings since the last read. `take` resets them, so each overlay poll sees one
/// interval.
#[derive(Default)]
pub struct TileTimes {
    inner: Mutex<TileWindow>,
}

#[derive(Default, Clone, Serialize)]
pub struct TileWindow {
    pub requests: u64,
    pub bytes: u64,
    pub total_ms: f64,
    pub worst_ms: f64,
}

impl TileTimes {
    pub fn record(&self, took: Duration, bytes: usize) {
        let ms = took.as_secs_f64() * 1000.0;
        let mut w = self.inner.lock().expect("tile timing lock");
        w.requests += 1;
        w.bytes += bytes as u64;
        w.total_ms += ms;
        w.worst_ms = w.worst_ms.max(ms);
    }

    pub fn take(&self) -> TileWindow {
        std::mem::take(&mut *self.inner.lock().expect("tile timing lock"))
    }
}

#[derive(Serialize)]
pub struct ProcessMem {
    pub pid: u32,
    pub name: String,
    /// Proportional set size: shared pages split between the processes that map them, so the
    /// rows add up to a meaningful total. Falls back to RSS where smaps_rollup is unreadable.
    pub pss_kb: u64,
}

/// `(pid, ppid, name)` for every process, from `/proc/<pid>/stat`.
fn all_processes() -> Vec<(u32, u32, String)> {
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(|e| {
        let pid: u32 = e.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        parse_stat(&stat).map(|(ppid, name)| (pid, ppid, name))
    })
    .collect()
}

/// `(ppid, comm)` from a `/proc/<pid>/stat` line. The name is in parentheses and may itself
/// contain spaces and parentheses, so it ends at the LAST `)`.
pub fn parse_stat(stat: &str) -> Option<(u32, String)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    let mut rest = stat.get(close + 1..)?.split_whitespace();
    let _state = rest.next()?;
    let ppid = rest.next()?.parse().ok()?;
    Some((ppid, name))
}

/// Every pid descended from `root`, root first.
pub fn descendants(root: u32, procs: &[(u32, u32, String)]) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid, _) in procs {
        children.entry(ppid).or_default().push(pid);
    }
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        if let Some(kids) = children.get(&out[i]) {
            out.extend(kids.iter().copied().filter(|k| *k != root));
        }
        i += 1;
    }
    out
}

/// The `kB` figure on the first line that starts with `key` (e.g. `Pss:` or `VmRSS:`).
pub fn kb_field(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find(|l| l.starts_with(key))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// Memory for this process and everything it started.
pub fn process_tree_memory() -> Vec<ProcessMem> {
    let procs = all_processes();
    let names: HashMap<u32, &str> = procs.iter().map(|(p, _, n)| (*p, n.as_str())).collect();
    descendants(std::process::id(), &procs)
        .into_iter()
        .filter_map(|pid| {
            let pss = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
                .ok()
                .and_then(|t| kb_field(&t, "Pss:"))
                .or_else(|| {
                    std::fs::read_to_string(format!("/proc/{pid}/status"))
                        .ok()
                        .and_then(|t| kb_field(&t, "VmRSS:"))
                })?;
            Some(ProcessMem { pid, name: names.get(&pid).unwrap_or(&"?").to_string(), pss_kb: pss })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_names_with_spaces_and_parens() {
        assert_eq!(parse_stat("42 (maps-tauri) S 7 42 42"), Some((7, "maps-tauri".into())));
        assert_eq!(parse_stat("9 (Web Content (x)) R 42 9 9"), Some((42, "Web Content (x)".into())));
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn descendants_walks_the_whole_tree_and_nothing_else() {
        let p = |pid, ppid| (pid, ppid, String::new());
        let procs = vec![p(1, 0), p(10, 1), p(11, 10), p(12, 11), p(13, 10), p(20, 1), p(21, 20)];
        assert_eq!(descendants(10, &procs), vec![10, 11, 13, 12]);
        assert_eq!(descendants(99, &procs), vec![99]);
    }

    #[test]
    fn kb_fields() {
        let t = "Rss:  2048 kB\nPss:  1024 kB\nPss_Anon: 7 kB\n";
        assert_eq!(kb_field(t, "Pss:"), Some(1024));
        assert_eq!(kb_field(t, "Rss:"), Some(2048));
        assert_eq!(kb_field(t, "Swap:"), None);
    }

    #[test]
    fn tile_window_resets_on_take() {
        let t = TileTimes::default();
        t.record(Duration::from_millis(2), 100);
        t.record(Duration::from_millis(6), 50);
        let w = t.take();
        assert_eq!((w.requests, w.bytes), (2, 150));
        assert!((w.total_ms - 8.0).abs() < 1e-9 && (w.worst_ms - 6.0).abs() < 1e-9);
        assert_eq!(t.take().requests, 0);
    }

    #[test]
    fn this_process_is_found() {
        let mem = process_tree_memory();
        assert_eq!(mem.first().map(|m| m.pid), Some(std::process::id()));
        assert!(mem[0].pss_kb > 0);
    }
}
