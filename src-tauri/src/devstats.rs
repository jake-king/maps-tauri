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

/// A process's PSS split by what its mappings are: the OS-level "bytes per block".
#[derive(Serialize, Default, Debug, PartialEq)]
pub struct MappingKinds {
    /// anonymous memory: JavaScript heaps, malloc, JIT code
    pub anon_kb: u64,
    /// GPU device mappings (/dev/dri, /dev/nvidia*): driver memory visible to the CPU
    pub gpu_kb: u64,
    /// shared memory (memfd, /dev/shm, SYSV): WebKit's IPC and graphics buffers
    pub shmem_kb: u64,
    /// shared libraries and other files mapped from disk
    pub file_kb: u64,
}

/// Sum each mapping's `Pss:` into a kind, from the text of `/proc/<pid>/smaps`.
pub fn classify_smaps(text: &str) -> MappingKinds {
    let mut k = MappingKinds::default();
    let mut kind = 0u8; // 0 anon, 1 gpu, 2 shmem, 3 file
    for line in text.lines() {
        let first = line.split_whitespace().next().unwrap_or("");
        // a mapping header starts with an address range like 7f00-7f10
        if first.contains('-') && first.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            let path = line.split_whitespace().nth(5).unwrap_or("");
            kind = if path.is_empty() || path.starts_with('[') {
                0
            } else if path.starts_with("/dev/dri") || path.starts_with("/dev/nvidia") {
                1
            } else if path.starts_with("/memfd:")
                || path.starts_with("/dev/shm")
                || path.starts_with("/SYSV")
            {
                2
            } else {
                3
            };
        } else if let Some(rest) = line.strip_prefix("Pss:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            match kind {
                0 => k.anon_kb += kb,
                1 => k.gpu_kb += kb,
                2 => k.shmem_kb += kb,
                _ => k.file_kb += kb,
            }
        }
    }
    k
}

/// The mesh handler's time by stage, since the last read: where a request's milliseconds go.
#[derive(Default, Clone, Serialize)]
pub struct StageWindow {
    pub requests: u64,
    /// from the URI-scheme callback to a blocking-pool thread starting it
    pub wait_ms: f64,
    pub read_ms: f64,
    pub decode_ms: f64,
    pub mesh_ms: f64,
    pub serialize_ms: f64,
    /// copying the serialized mesh out of the thread's reusable buffer into the response
    pub copy_ms: f64,
    /// requests running at once, seen by each request as it starts: sum (for the mean) and max
    pub inflight_sum: u64,
    pub inflight_max: u64,
    pub cache_hits: u64,
}

#[derive(Default)]
pub struct StageTimes {
    inner: Mutex<StageWindow>,
    pub inflight: std::sync::atomic::AtomicU64,
}

impl StageTimes {
    pub fn record(&self, wait: Duration, s: &crate::tiles::MeshStages, inflight: u64) {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        let mut w = self.inner.lock().expect("stage lock");
        w.requests += 1;
        w.wait_ms += ms(wait);
        w.read_ms += ms(s.read);
        w.decode_ms += ms(s.decode);
        w.mesh_ms += ms(s.mesh);
        w.serialize_ms += ms(s.serialize);
        w.copy_ms += ms(s.copy);
        w.inflight_sum += inflight;
        w.inflight_max = w.inflight_max.max(inflight);
        w.cache_hits += u64::from(s.cache_hit);
    }
    pub fn take(&self) -> StageWindow {
        std::mem::take(&mut *self.inner.lock().expect("stage lock"))
    }
}

#[derive(Serialize)]
pub struct ProcessMem {
    pub pid: u32,
    pub name: String,
    /// Proportional set size: shared pages split between the processes that map them, so the
    /// rows add up to a meaningful total. Falls back to RSS where smaps_rollup is unreadable.
    pub pss_kb: u64,
    /// The same PSS by mapping kind, when /proc/<pid>/smaps is readable.
    pub kinds: Option<MappingKinds>,
}

/// `(pid, ppid, name)` for every process, from `/proc/<pid>/stat`.
fn all_processes() -> Vec<(u32, u32, String)> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
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
            let kinds = std::fs::read_to_string(format!("/proc/{pid}/smaps"))
                .ok()
                .map(|t| classify_smaps(&t));
            Some(ProcessMem {
                pid,
                name: names.get(&pid).unwrap_or(&"?").to_string(),
                pss_kb: pss,
                kinds,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_names_with_spaces_and_parens() {
        assert_eq!(
            parse_stat("42 (maps-tauri) S 7 42 42"),
            Some((7, "maps-tauri".into()))
        );
        assert_eq!(
            parse_stat("9 (Web Content (x)) R 42 9 9"),
            Some((42, "Web Content (x)".into()))
        );
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn descendants_walks_the_whole_tree_and_nothing_else() {
        let p = |pid, ppid| (pid, ppid, String::new());
        let procs = vec![
            p(1, 0),
            p(10, 1),
            p(11, 10),
            p(12, 11),
            p(13, 10),
            p(20, 1),
            p(21, 20),
        ];
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
    fn smaps_kinds() {
        let t = "\
5600-5700 rw-p 00000000 00:00 0                                  [heap]
Rss: 9 kB
Pss: 100 kB
7f00-7f10 rw-s 00000000 00:05 12                                 /dev/dri/renderD128
Pss: 40 kB
7f10-7f20 rw-s 00000000 00:01 7                                  /memfd:WebKitSharedMemory (deleted)
Pss: 30 kB
7f20-7f30 r-xp 00001000 103:02 99                                /usr/lib/libwebkit2gtk-4.1.so.0
Pss: 20 kB
7f30-7f40 rw-p 00000000 00:00 0 
Pss: 5 kB
7f40-7f50 rw-s 00000000 00:05 3                                  /dev/nvidiactl
Pss: 2 kB
";
        assert_eq!(
            classify_smaps(t),
            MappingKinds {
                anon_kb: 105,
                gpu_kb: 42,
                shmem_kb: 30,
                file_kb: 20
            }
        );
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
