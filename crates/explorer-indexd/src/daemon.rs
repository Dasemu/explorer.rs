// SPDX-License-Identifier: GPL-3.0-or-later
//! Daemon state: the shared index, one source thread per root, snapshots.
//!
//! Source selection per root (see docs/INDEX.md):
//! 1. Windows, `use_ntfs`, the root is an NTFS volume root (`C:\`): read the
//!    MFT and follow the USN journal, through a direct volume handle if this
//!    process is elevated, otherwise through the elevated helper;
//! 2. Windows otherwise: walk + `ReadDirectoryChangesW`;
//! 3. elsewhere, or if watching fails: walk + periodic rescans.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use explorer_core::config::IndexConfig;
use explorer_index::index::{Index, VolId, VolumeKind};
use explorer_index::protocol::{RootStatus, Status};
use explorer_index::source::{self, Change, ChangeSink, SourceState, Stop};
use explorer_index::{snapshot, walker, Rules};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct RootHandle {
    path: String,
    vol: VolId,
    stop: Arc<Stop>,
    thread: Option<JoinHandle<()>>,
}

pub struct Daemon {
    index: RwLock<Index>,
    pub rules: Rules,
    pub cfg: IndexConfig,
    states: Mutex<HashMap<VolId, SourceState>>,
    roots: Mutex<Vec<RootHandle>>,
    /// Global shutdown signal.
    pub stop: Stop,
    started: Instant,
    last_save: Mutex<Option<Instant>>,
    saved_generation: AtomicU64,
    snapshot_path: PathBuf,
    /// Volumes that fell back to walking because the elevated helper was
    /// not reachable; retried periodically ([`Daemon::retry_helper`]).
    helper_missing: Mutex<HashSet<VolId>>,
}

/// How often roots waiting for the elevated helper are retried.
pub const HELPER_RETRY: Duration = Duration::from_secs(60);

/// Roots (path) whose volume is waiting for the helper, in root order.
pub fn roots_waiting_for_helper(
    missing: &HashSet<VolId>,
    roots: &[(VolId, String)],
) -> Vec<String> {
    roots
        .iter()
        .filter(|(v, _)| missing.contains(v))
        .map(|(_, p)| p.clone())
        .collect()
}

/// Expand `%VAR%` and a leading `~` in a configured root.
pub fn expand_root(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(a) = rest.find('%') {
        let Some(b) = rest[a + 1..].find('%') else {
            break;
        };
        let name = &rest[a + 1..a + 1 + b];
        out.push_str(&rest[..a]);
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[a..a + b + 2]),
        }
        rest = &rest[a + b + 2..];
    }
    out.push_str(rest);
    if let Some(tail) = out.strip_prefix('~') {
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            return format!("{}{tail}", home.to_string_lossy());
        }
    }
    out
}

/// Roots to index: the configured ones, or the platform default.
pub fn resolve_roots(cfg: &IndexConfig) -> Vec<String> {
    if !cfg.roots.is_empty() {
        return cfg.roots.iter().map(|r| expand_root(r)).collect();
    }
    #[cfg(windows)]
    {
        explorer_index::win::volumes::fixed_drives()
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").map(|h| vec![h]).unwrap_or_default()
    }
}

impl ChangeSink for Daemon {
    fn apply(&self, changes: Vec<Change>) {
        let mut ix = self.write();
        let st = source::apply(&mut ix, &self.rules, changes);
        tracing::trace!(?st, "changes applied");
    }

    fn set_state(&self, vol: VolId, state: SourceState) {
        tracing::debug!(vol, state = %state.label(), "source state");
        lock(&self.states).insert(vol, state);
    }
}

impl Daemon {
    /// Create the daemon, loading the snapshot if it is valid. A corrupt
    /// snapshot is moved aside (`*.bad`) and the index is rebuilt.
    pub fn new(cfg: IndexConfig, snapshot_path: PathBuf) -> Arc<Self> {
        let t0 = Instant::now();
        let index = match snapshot::load(&snapshot_path) {
            Ok(ix) => {
                tracing::info!(
                    entries = ix.len(),
                    ms = t0.elapsed().as_millis() as u64,
                    "snapshot loaded"
                );
                ix
            }
            Err(snapshot::SnapshotError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("no snapshot yet; building the index from scratch");
                Index::new()
            }
            Err(e) => {
                tracing::warn!(error = %e, "snapshot unusable; rebuilding the index");
                let _ = std::fs::rename(&snapshot_path, snapshot_path.with_extension("bad"));
                Index::new()
            }
        };
        let generation = index.generation();
        Arc::new(Self {
            rules: Rules::from_config(&cfg),
            cfg,
            index: RwLock::new(index),
            states: Mutex::new(HashMap::new()),
            roots: Mutex::new(Vec::new()),
            stop: Stop::new(),
            started: Instant::now(),
            last_save: Mutex::new(None),
            saved_generation: AtomicU64::new(generation),
            snapshot_path,
            helper_missing: Mutex::new(HashSet::new()),
        })
    }

    pub fn read(&self) -> RwLockReadGuard<'_, Index> {
        self.index.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, Index> {
        self.index.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Drop snapshot volumes that are no longer configured, add new ones and
    /// start a source for every root.
    pub fn start_roots(self: &Arc<Self>, roots: &[String]) {
        {
            let mut ix = self.write();
            let stale: Vec<VolId> = ix
                .volumes()
                .filter(|(_, v)| !roots.iter().any(|r| same_root(r, &v.path)))
                .map(|(id, _)| id)
                .collect();
            for v in stale {
                tracing::info!(vol = v, "removing volume that is no longer configured");
                let _ = ix.remove_volume(v);
            }
        }
        for r in roots {
            if let Err(e) = self.add_root(r) {
                tracing::warn!(root = %r, error = %e, "cannot index root");
            }
        }
    }

    pub fn add_root(self: &Arc<Self>, path: &str) -> Result<(), String> {
        if !Path::new(path).is_dir() {
            return Err(format!("{path} is not a folder"));
        }
        let mut roots = lock(&self.roots);
        if roots.iter().any(|h| same_root(&h.path, path)) {
            return Err(format!("{path} is already indexed"));
        }
        let vol = {
            let mut ix = self.write();
            match ix.find_volume(path) {
                Some(v) => v,
                None => ix
                    .add_volume(path, VolumeKind::Walk)
                    .map_err(|e| e.to_string())?,
            }
        };
        let stop = Arc::new(Stop::new());
        let thread = self.spawn_source(vol, path.to_string(), stop.clone());
        roots.push(RootHandle {
            path: path.to_string(),
            vol,
            stop,
            thread: Some(thread),
        });
        Ok(())
    }

    pub fn remove_root(&self, path: &str) -> Result<(), String> {
        let h = {
            let mut roots = lock(&self.roots);
            let i = roots
                .iter()
                .position(|h| same_root(&h.path, path))
                .ok_or_else(|| format!("{path} is not indexed"))?;
            roots.remove(i)
        };
        stop_root(h.stop.as_ref(), h.thread);
        let _ = self.write().remove_volume(h.vol);
        lock(&self.states).remove(&h.vol);
        Ok(())
    }

    /// Restart the source of one root (or all), forcing a full rescan.
    pub fn reindex(self: &Arc<Self>, root: Option<&str>) -> Result<(), String> {
        let mut roots = lock(&self.roots);
        let mut found = false;
        for h in roots.iter_mut() {
            if root.is_some_and(|r| !same_root(r, &h.path)) {
                continue;
            }
            found = true;
            stop_root(h.stop.as_ref(), h.thread.take());
            self.write().set_journal(h.vol, None);
            h.stop = Arc::new(Stop::new());
            h.thread = Some(self.spawn_source(h.vol, h.path.clone(), h.stop.clone()));
        }
        if found {
            Ok(())
        } else {
            Err(format!("{} is not indexed", root.unwrap_or("")))
        }
    }

    fn spawn_source(self: &Arc<Self>, vol: VolId, path: String, stop: Arc<Stop>) -> JoinHandle<()> {
        let d = self.clone();
        d.set_state(vol, SourceState::Starting);
        std::thread::Builder::new()
            .name(format!("index-source-{vol}"))
            .spawn(move || {
                d.run_source(vol, &path, &stop);
                d.set_state(vol, SourceState::Stopped);
            })
            .expect("spawn source thread")
    }

    fn run_source(&self, vol: VolId, path: &str, stop: &Stop) {
        #[cfg(windows)]
        if self.cfg.use_ntfs && self.try_ntfs(vol, path, stop) {
            return;
        }
        {
            let mut ix = self.write();
            ix.set_volume_kind(vol, VolumeKind::Walk);
            ix.set_journal(vol, None);
        }
        #[cfg(windows)]
        match explorer_index::win::watch::run_watch(vol, Path::new(path), &self.rules, self, stop) {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(root = %path, error = %e, "cannot watch folder; falling back to periodic rescans")
            }
        }
        walker::run_walk(
            vol,
            Path::new(path),
            &self.rules,
            self,
            stop,
            Duration::from_secs(self.cfg.rescan_interval_s),
        );
    }

    /// Returns true when the NTFS source ran (and has stopped), false when
    /// the caller should fall back to walking.
    #[cfg(windows)]
    fn try_ntfs(&self, vol: VolId, path: &str, stop: &Stop) -> bool {
        use explorer_index::helper::HelperBackend;
        use explorer_index::usn::{run_ntfs, UsnBackend, UsnError};
        use explorer_index::win::{ntfs::DirectBackend, volumes};

        lock(&self.helper_missing).remove(&vol);
        let Some(letter) = volumes::drive_letter(path) else {
            return false;
        };
        match volumes::filesystem_name(path) {
            Some(fs) if fs.eq_ignore_ascii_case("NTFS") => {}
            other => {
                tracing::info!(root = %path, fs = ?other, "not NTFS; walking instead");
                return false;
            }
        }
        let mut failures = 0;
        loop {
            let backend: Result<Box<dyn UsnBackend>, UsnError> = match DirectBackend::open(letter) {
                Ok(b) => {
                    tracing::info!(root = %path, "reading the MFT directly (process is elevated)");
                    Ok(Box::new(b))
                }
                Err(_) => HelperBackend::connect(
                    &explorer_index::transport::helper_endpoint(),
                    letter,
                    Duration::from_secs(2),
                )
                .map(|b| {
                    tracing::info!(root = %path, "reading the MFT through the elevated helper");
                    Box::new(b) as Box<dyn UsnBackend>
                }),
            };
            let mut b = match backend {
                Ok(b) => b,
                Err(e) => {
                    tracing::info!(root = %path, error = %e, "no MFT access (helper not running?); walking instead, retrying the helper every minute");
                    lock(&self.helper_missing).insert(vol);
                    return false;
                }
            };
            let saved = {
                let mut ix = self.write();
                let was_ntfs = ix.volume(vol).is_some_and(|v| v.kind == VolumeKind::Ntfs);
                ix.set_volume_kind(vol, VolumeKind::Ntfs);
                if was_ntfs {
                    ix.volume(vol).and_then(|v| v.journal)
                } else {
                    None
                }
            };
            match run_ntfs(
                &mut *b,
                vol,
                path,
                saved,
                &self.rules,
                self,
                stop,
                Duration::from_millis(500),
            ) {
                Ok(()) => return true,
                Err(e) => {
                    failures += 1;
                    tracing::warn!(root = %path, error = %e, failures, "NTFS source failed");
                    self.set_state(vol, SourceState::Failed(e.to_string()));
                    if failures >= 3 {
                        return false;
                    }
                    if stop.sleep(Duration::from_secs(5)) {
                        return true;
                    }
                }
            }
        }
    }

    /// Roots that fell back to walking because the elevated helper was not
    /// running: if the helper is reachable now, restart their source so it
    /// switches to the MFT/USN journal (e.g. the helper task was started
    /// after the daemon, or restarted by setup).
    pub fn retry_helper(self: &Arc<Self>) {
        let waiting = {
            let missing = lock(&self.helper_missing);
            if missing.is_empty() {
                return;
            }
            let roots: Vec<(VolId, String)> = lock(&self.roots)
                .iter()
                .map(|h| (h.vol, h.path.clone()))
                .collect();
            roots_waiting_for_helper(&missing, &roots)
        };
        for root in waiting {
            if !self.helper_reachable(&root) {
                continue;
            }
            tracing::info!(root = %root, "the elevated helper is available now; switching to the MFT");
            if let Err(e) = self.reindex(Some(&root)) {
                tracing::warn!(root = %root, error = %e, "cannot restart the source");
            }
        }
    }

    #[cfg(windows)]
    fn helper_reachable(&self, root: &str) -> bool {
        use explorer_index::helper::HelperBackend;
        let Some(letter) = explorer_index::win::volumes::drive_letter(root) else {
            return false;
        };
        HelperBackend::connect(
            &explorer_index::transport::helper_endpoint(),
            letter,
            Duration::from_secs(2),
        )
        .is_ok()
    }

    #[cfg(not(windows))]
    fn helper_reachable(&self, _root: &str) -> bool {
        false
    }

    /// Write a snapshot if the index changed since the last one (or always
    /// with `force`). Encoding happens under the read lock, I/O without it.
    pub fn save_snapshot(&self, force: bool) -> std::io::Result<bool> {
        let (generation, bytes) = {
            let ix = self.read();
            let g = ix.generation();
            if !force && g == self.saved_generation.load(Ordering::Relaxed) {
                return Ok(false);
            }
            (g, snapshot::encode(&ix))
        };
        let t0 = Instant::now();
        snapshot::write_atomic(&self.snapshot_path, &bytes)?;
        self.saved_generation.store(generation, Ordering::Relaxed);
        *lock(&self.last_save) = Some(Instant::now());
        tracing::info!(
            bytes = bytes.len(),
            ms = t0.elapsed().as_millis() as u64,
            "snapshot saved"
        );
        Ok(true)
    }

    pub fn status(&self) -> Status {
        let roots: Vec<(String, VolId)> = lock(&self.roots)
            .iter()
            .map(|h| (h.path.clone(), h.vol))
            .collect();
        let states = lock(&self.states).clone();
        let ix = self.read();
        let root_status: Vec<RootStatus> = roots
            .iter()
            .map(|(path, vol)| RootStatus {
                path: path.clone(),
                kind: match ix.volume(*vol).map(|v| v.kind) {
                    Some(VolumeKind::Ntfs) => "ntfs".into(),
                    _ => "walk".into(),
                },
                state: states
                    .get(vol)
                    .map(SourceState::label)
                    .unwrap_or_else(|| "unknown".into()),
                entries: ix.volume_len(*vol) as u64,
            })
            .collect();
        Status {
            version: env!("CARGO_PKG_VERSION").into(),
            entries: ix.len() as u64,
            memory_bytes: ix.memory_usage() as u64,
            scanning: states
                .values()
                .any(|s| matches!(s, SourceState::Scanning | SourceState::Starting)),
            roots: root_status,
            uptime_s: self.started.elapsed().as_secs(),
            snapshot_age_s: lock(&self.last_save).map(|t| t.elapsed().as_secs()),
        }
    }

    /// Stop every source, wait for them and save a final snapshot.
    pub fn shutdown(&self) {
        self.stop.stop();
        let handles: Vec<RootHandle> = lock(&self.roots).drain(..).collect();
        for h in &handles {
            h.stop.stop();
        }
        for h in handles {
            if let Some(t) = h.thread {
                let _ = t.join();
            }
        }
        if let Err(e) = self.save_snapshot(false) {
            tracing::error!(error = %e, "final snapshot failed");
        }
    }
}

fn stop_root(stop: &Stop, thread: Option<JoinHandle<()>>) {
    stop.stop();
    if let Some(t) = thread {
        let _ = t.join();
    }
}

fn same_root(a: &str, b: &str) -> bool {
    a.trim_end_matches(['/', '\\'])
        .eq_ignore_ascii_case(b.trim_end_matches(['/', '\\']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_retry_selection() {
        let missing: HashSet<VolId> = [2].into_iter().collect();
        let roots = vec![(1, "C:\\".to_string()), (2, "D:\\".to_string())];
        assert_eq!(roots_waiting_for_helper(&missing, &roots), ["D:\\"]);
        assert!(roots_waiting_for_helper(&HashSet::new(), &roots).is_empty());
    }

    #[test]
    fn expand() {
        std::env::set_var("EXPLORER_RS_TEST_X", "abc");
        assert_eq!(expand_root("%EXPLORER_RS_TEST_X%\\d"), "abc\\d");
        assert_eq!(expand_root("%NOPE_NOT_SET%\\d"), "%NOPE_NOT_SET%\\d");
        assert_eq!(expand_root("50%"), "50%");
        assert!(same_root("C:\\", "c:"));
    }

    #[test]
    fn roots_snapshot_and_corrupt_recovery() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path().join("data");
        std::fs::create_dir_all(data.join("sub")).unwrap();
        std::fs::write(data.join("sub/hello.txt"), "x").unwrap();
        let snap = d.path().join("state/index.snapshot");
        let cfg = IndexConfig {
            rescan_interval_s: 0,
            ..Default::default()
        };
        let dm = Daemon::new(cfg.clone(), snap.clone());
        let root = data.to_string_lossy().to_string();
        dm.start_roots(std::slice::from_ref(&root));
        assert!(dm.add_root(&root).is_err(), "duplicate root refused");
        assert!(dm.add_root("/definitely/not/here").is_err());
        for _ in 0..200 {
            if dm.status().roots[0].state == "polling" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let st = dm.status();
        assert_eq!(st.roots[0].entries, 3, "{st:?}");
        dm.shutdown();
        assert!(snap.exists());

        // Restart: the snapshot is loaded before any scan.
        let dm2 = Daemon::new(cfg.clone(), snap.clone());
        assert_eq!(dm2.read().len(), 3);
        // A root that is no longer configured is dropped.
        dm2.start_roots(&[]);
        assert_eq!(dm2.read().len(), 0);
        dm2.shutdown();

        // Corrupt snapshot: moved aside, clean rebuild, no panic.
        std::fs::write(&snap, b"garbage").unwrap();
        let dm3 = Daemon::new(cfg, snap.clone());
        assert_eq!(dm3.read().len(), 0);
        assert!(snap.with_extension("bad").exists());
    }
}
