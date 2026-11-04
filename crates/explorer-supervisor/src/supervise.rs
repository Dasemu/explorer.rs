// SPDX-License-Identifier: GPL-3.0-or-later
//! The supervisor (`explorer-rs --supervise --dev|--shell`).
//!
//! Starts each component as its own process and restarts it independently
//! (`explorer_core::supervisor`). Additionally:
//! - reloads the config file when it changes (polled every 2 s) and
//!   restarts the affected components, starting/stopping components added
//!   to or removed from `supervisor.components`; an invalid file is logged
//!   and ignored;
//! - during sign-out/shutdown, component exits are not crashes and nothing
//!   is restarted;
//! - in shell mode, a taskbar crash loop ends the supervisor with
//!   `EXIT_FALLBACK` so the guardian can fall back to explorer.exe, and the
//!   once-per-sign-in shell duties (ready events, startup apps) run;
//! - the guardian's process is watched: if it dies, the supervisor handles
//!   fallbacks itself (starts explorer.exe) instead of exiting with
//!   `EXIT_FALLBACK` to nobody;
//! - `ExplorerRs.Shutdown` (from the guardian or setup) stops every
//!   component gracefully, then the supervisor exits with 0.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use explorer_core::components::{affected_by_change, components_explicit, list_diff, resolve};
use explorer_core::config::Config;
#[cfg(windows)]
use explorer_core::guardian::EXIT_ALREADY_RUNNING;
use explorer_core::guardian::{
    component_needs_fallback, EXIT_FALLBACK, GUARDED_ENV, GUARDIAN_PID_ENV, SESSION_START_ENV,
};
use explorer_core::ipc::{RunMode, MODE_ENV};
use explorer_core::paths::AppPaths;
use explorer_core::supervisor::{
    Decision, RestartTracker, EXIT_RESTART, EXIT_SHUTDOWN_ALL, EXIT_STOP,
};

/// How often the config file's modification time is checked.
const CONFIG_POLL: Duration = Duration::from_secs(2);
/// After `WM_QUERYENDSESSION`, component exits within this time are not
/// treated as crashes and nothing is restarted (the session may end).
const QUERY_GRACE: Duration = Duration::from_secs(30);
/// How long a component gets to exit after the graceful shutdown message
/// before it is killed.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
/// After the session ends, wait at most this long for components to exit.
const END_WAIT: Duration = Duration::from_secs(10);

/// Session state shared with the listener thread.
#[derive(Default)]
pub struct SessionFlags {
    pub ending: AtomicBool,
    pub query_at: Mutex<Option<Instant>>,
    /// `ExplorerRs.Shutdown` received: stop everything gracefully.
    pub shutdown: AtomicBool,
}

impl SessionFlags {
    fn in_query_grace(&self, now: Instant) -> bool {
        self.query_at
            .lock()
            .ok()
            .and_then(|q| *q)
            .is_some_and(|t| now.duration_since(t) < QUERY_GRACE)
    }
}

/// The installer's file-search choice (`HKLM\Software\explorer-rs\FileSearch`).
pub fn installer_file_search() -> Option<u32> {
    #[cfg(windows)]
    {
        use explorer_core::components::{FILE_SEARCH_SUBKEY, FILE_SEARCH_VALUE};
        explorer_win::registry::read_dword(
            explorer_win::registry::Root::LocalMachine,
            FILE_SEARCH_SUBKEY,
            FILE_SEARCH_VALUE,
        )
    }
    #[cfg(not(windows))]
    {
        None
    }
}

struct Component {
    name: String,
    exe: PathBuf,
    child: Option<Child>,
    started: Instant,
    next_start: Option<Instant>,
    tracker: RestartTracker,
    done: bool,
    /// Crash times, milliseconds since the session started.
    failures_ms: Vec<u64>,
}

impl Component {
    fn new(name: String, exe_dir: &Path, cfg: &Config) -> Self {
        Self {
            exe: component_exe(exe_dir, &name),
            name,
            child: None,
            started: Instant::now(),
            next_start: Some(Instant::now()),
            tracker: RestartTracker::new(&cfg.supervisor),
            done: false,
            failures_ms: Vec::new(),
        }
    }
}

pub fn component_exe(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(
        "explorer-rs-{name}{}",
        std::env::consts::EXE_SUFFIX
    ))
}

/// Detects config file changes by (modification time, length). A change
/// is applied only once the file has been stable for one poll, so a
/// half-written file is not loaded.
struct ConfigWatch {
    path: PathBuf,
    applied: Option<(SystemTime, u64)>,
    seen: Option<(SystemTime, u64)>,
    last_poll: Instant,
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

impl ConfigWatch {
    fn new(path: PathBuf) -> Self {
        let s = stamp(&path);
        Self {
            path,
            applied: s,
            seen: s,
            last_poll: Instant::now(),
        }
    }

    /// True when the file changed and is now stable.
    fn poll(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_poll) < CONFIG_POLL {
            return false;
        }
        self.last_poll = now;
        let cur = stamp(&self.path);
        if cur != self.seen {
            self.seen = cur;
            return false;
        }
        if cur != self.applied {
            self.applied = cur;
            return true;
        }
        false
    }
}

fn read_config(path: &Path) -> Result<(Config, bool), String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Config::from_toml(&text)
            .map(|c| (c, components_explicit(&text)))
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Config::default(), false)),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// `explorer-rs --supervise --dev|--shell`.
pub fn main(mode: RunMode, config_override: Option<&Path>, only: Option<&str>) -> i32 {
    let paths = AppPaths::from_env();
    let _ = paths.ensure();
    let config_path = config_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| paths.config_file());
    let (loaded, config_err) = match read_config(&config_path) {
        Ok((c, explicit)) => ((c, explicit), None),
        Err(e) => ((Config::default(), false), Some(e)),
    };
    let (config, explicit) = loaded;
    let _log = explorer_core::logging::init(
        "supervisor",
        &paths.log_dir,
        &config.general.log_level,
        config.general.log_keep_files,
    );
    if let Some(e) = config_err {
        tracing::error!(path = %config_path.display(), error = %e, "config error, using defaults");
    }
    tracing::info!(mode = mode.as_str(), config = %config_path.display(), "supervisor starting");

    #[cfg(windows)]
    let _instance = match explorer_win::single_instance::acquire(crate::cli::SUPERVISOR_MUTEX) {
        Some(g) => g,
        None => {
            tracing::warn!("another supervisor is already running; exiting");
            return EXIT_ALREADY_RUNNING;
        }
    };

    let guardian_pid = explorer_core::guardian::guardian_pid(
        std::env::var(GUARDED_ENV).ok().as_deref(),
        std::env::var(GUARDIAN_PID_ENV).ok().as_deref(),
    );
    let guarded = Arc::new(AtomicBool::new(guardian_pid.is_some()));
    if std::env::var(GUARDED_ENV).is_ok_and(|v| v == "1") && guardian_pid.is_none() {
        tracing::warn!("guarded without a valid guardian pid; handling fallbacks myself");
    }
    #[cfg(windows)]
    if let Some(pid) = guardian_pid {
        watch_guardian(pid, guarded.clone());
    }
    let now_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let session_start_ms = std::env::var(SESSION_START_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&t| t <= now_ms)
        .unwrap_or(now_ms);
    // Map "session start" onto this process's monotonic clock.
    let session_epoch = Instant::now()
        .checked_sub(Duration::from_millis(now_ms - session_start_ms))
        .unwrap_or_else(Instant::now);

    let flags = Arc::new(SessionFlags::default());
    #[cfg(windows)]
    {
        let f = flags.clone();
        let sink: explorer_win::session::Sink = Arc::new(move |ev| {
            use explorer_win::session::SysEvent;
            match ev {
                SysEvent::QueryEndSession => {
                    if let Ok(mut q) = f.query_at.lock() {
                        *q = Some(Instant::now());
                    }
                }
                SysEvent::SessionEnding => {
                    tracing::info!("session ending");
                    f.ending.store(true, Ordering::SeqCst);
                }
                SysEvent::Shutdown => {
                    tracing::info!("graceful shutdown requested");
                    f.shutdown.store(true, Ordering::SeqCst);
                }
                _ => {}
            }
        });
        if !explorer_win::session::spawn_listener(false, sink) {
            tracing::warn!("could not start the session listener");
        }
        if mode == RunMode::Shell {
            spawn_shell_duties();
        }
    }

    let names = match only {
        Some(n) => vec![n.to_string()],
        None => resolve(&config.supervisor, explicit, installer_file_search()),
    };
    let mut sup = Supervisor {
        mode,
        guarded,
        config,
        config_override: config_override.map(Path::to_path_buf),
        only: only.map(str::to_string),
        exe_dir: std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from(".")),
        comps: Vec::new(),
        flags,
        session_epoch,
        #[cfg(windows)]
        job: match explorer_win::job::KillOnCloseJob::new() {
            Ok(j) => Some(j),
            Err(e) => {
                tracing::warn!(error = %e, "could not create job object; children may outlive the supervisor");
                None
            }
        },
    };
    sup.comps = names
        .into_iter()
        .map(|n| Component::new(n, &sup.exe_dir, &sup.config))
        .collect();
    if sup.comps.is_empty() {
        tracing::error!("no components configured");
        return 1;
    }
    let code = sup.run(ConfigWatch::new(config_path));
    tracing::info!(code, "supervisor exiting");
    code
}

struct Supervisor {
    mode: RunMode,
    /// A guardian handles fallbacks; cleared when the guardian dies.
    guarded: Arc<AtomicBool>,
    config: Config,
    config_override: Option<PathBuf>,
    only: Option<String>,
    exe_dir: PathBuf,
    comps: Vec<Component>,
    flags: Arc<SessionFlags>,
    session_epoch: Instant,
    #[cfg(windows)]
    job: Option<explorer_win::job::KillOnCloseJob>,
}

impl Supervisor {
    fn run(&mut self, mut watch: ConfigWatch) -> i32 {
        let mut ending_since: Option<Instant> = None;
        loop {
            let now = Instant::now();
            if self.flags.ending.load(Ordering::SeqCst) {
                let since = *ending_since.get_or_insert(now);
                let all_gone = self.comps.iter_mut().all(|c| match c.child.as_mut() {
                    Some(ch) => !matches!(ch.try_wait(), Ok(None)),
                    None => true,
                });
                if all_gone || now.duration_since(since) >= END_WAIT {
                    tracing::info!("session ended; supervisor exits");
                    return 0;
                }
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            if self.flags.shutdown.load(Ordering::SeqCst) {
                tracing::info!("stopping all components (graceful shutdown)");
                self.shutdown();
                return 0;
            }
            if watch.poll(now) {
                self.reload(&watch.path);
            }
            let paused = self.flags.in_query_grace(now);
            let shell = self.mode == RunMode::Shell;
            let mut fallback = false;
            let mut shutdown_all = false;
            for c in self.comps.iter_mut() {
                if let Some(child) = c.child.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            c.child = None;
                            let code = status.code();
                            if paused {
                                tracing::info!(component = %c.name, ?code, "component exited while the session may be ending; not a crash");
                                c.next_start = Some(now + QUERY_GRACE);
                                continue;
                            }
                            let uptime = now.duration_since(c.started).as_millis() as u64;
                            let since_ms =
                                now.duration_since(self.session_epoch).as_millis() as u64;
                            let decision = c.tracker.on_exit(code, uptime, since_ms);
                            let crash = !matches!(
                                code,
                                Some(EXIT_STOP) | Some(EXIT_SHUTDOWN_ALL) | Some(EXIT_RESTART)
                            );
                            if crash {
                                c.failures_ms.push(since_ms);
                            }
                            tracing::info!(component = %c.name, ?status, uptime_ms = uptime, ?decision, "component exited");
                            if crash
                                && component_needs_fallback(
                                    shell,
                                    &c.name,
                                    decision,
                                    &c.failures_ms,
                                )
                            {
                                tracing::error!(component = %c.name, "crash loop in shell mode; falling back to explorer.exe");
                                c.done = true;
                                fallback = true;
                                continue;
                            }
                            match decision {
                                Decision::RestartAfter(ms) => {
                                    c.next_start = Some(now + Duration::from_millis(ms))
                                }
                                Decision::Stop => c.done = true,
                                Decision::GiveUp => {
                                    c.done = true;
                                    tracing::error!(component = %c.name, "crash loop detected; giving up on this component");
                                }
                                Decision::ShutdownAll => {
                                    tracing::info!(component = %c.name, "shutdown requested");
                                    shutdown_all = true;
                                }
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(component = %c.name, error = %e, "try_wait failed")
                        }
                    }
                }
            }
            if shutdown_all {
                self.shutdown();
                return 0;
            }
            if !paused && self.spawn_due(now) {
                fallback = true;
            }
            if fallback {
                if self.guarded.load(Ordering::SeqCst) {
                    self.shutdown();
                    return EXIT_FALLBACK;
                }
                start_fallback_explorer();
            }
            if self.comps.iter().all(|c| c.done) {
                tracing::info!("all components stopped");
                return 0;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Start components whose time has come. Returns true if a failed
    /// spawn requires the explorer.exe fallback.
    fn spawn_due(&mut self, now: Instant) -> bool {
        let shell = self.mode == RunMode::Shell;
        let mut fallback = false;
        for c in self.comps.iter_mut() {
            if c.child.is_some() || c.done || !c.next_start.is_some_and(|t| now >= t) {
                continue;
            }
            c.next_start = None;
            match spawn(&c.exe, self.mode, self.config_override.as_deref()) {
                Ok(child) => {
                    #[cfg(windows)]
                    if let Some(j) = &self.job {
                        if let Err(e) = j.assign(&child) {
                            tracing::warn!(error = %e, "assigning child to job failed");
                        }
                    }
                    tracing::info!(component = %c.name, pid = child.id(), "component started");
                    c.child = Some(child);
                    c.started = now;
                }
                Err(e) => {
                    tracing::error!(component = %c.name, exe = %c.exe.display(), error = %e, "spawn failed");
                    // A failed spawn counts like a crash so backoff applies.
                    let since_ms = now.duration_since(self.session_epoch).as_millis() as u64;
                    c.failures_ms.push(since_ms);
                    let d = c.tracker.on_exit(Some(1), 0, since_ms);
                    if component_needs_fallback(shell, &c.name, d, &c.failures_ms) {
                        c.done = true;
                        fallback = true;
                        continue;
                    }
                    match d {
                        Decision::RestartAfter(ms) => {
                            c.next_start = Some(now + Duration::from_millis(ms))
                        }
                        _ => c.done = true,
                    }
                }
            }
        }
        fallback
    }

    /// Apply a changed config file.
    fn reload(&mut self, path: &Path) {
        let (new, explicit) = match read_config(path) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "config changed but is invalid; keeping the current configuration");
                return;
            }
        };
        tracing::info!("config file changed; applying");
        let new_names = match &self.only {
            Some(n) => vec![n.clone()],
            None => resolve(&new.supervisor, explicit, installer_file_search()),
        };
        let old_names: Vec<String> = self.comps.iter().map(|c| c.name.clone()).collect();
        let (to_start, to_stop) = list_diff(&old_names, &new_names);
        let affected = affected_by_change(&self.config, &new);
        let timing_changed = {
            let (a, b) = (&self.config.supervisor, &new.supervisor);
            a.restart_initial_delay_ms != b.restart_initial_delay_ms
                || a.restart_max_delay_ms != b.restart_max_delay_ms
                || a.crash_loop_max != b.crash_loop_max
                || a.crash_loop_window_s != b.crash_loop_window_s
        };
        for c in self.comps.iter_mut() {
            if to_stop.contains(&c.name) {
                tracing::info!(component = %c.name, "removed from config; stopping");
                stop_child(c);
                continue;
            }
            if timing_changed {
                c.tracker = RestartTracker::new(&new.supervisor);
            }
            if affected.contains(&c.name.as_str()) {
                tracing::info!(component = %c.name, "config section changed; restarting");
                stop_child(c);
                c.done = false;
                c.failures_ms.clear();
                c.next_start = Some(Instant::now());
            }
        }
        self.comps.retain(|c| !to_stop.contains(&c.name));
        for name in to_start {
            tracing::info!(component = %name, "added to config; starting");
            self.comps.push(Component::new(name, &self.exe_dir, &new));
        }
        self.config = new;
    }

    /// Stop every component: graceful shutdown message to all of them at
    /// once, then kill whatever is left after [`CLOSE_TIMEOUT`].
    fn shutdown(&mut self) {
        let mut children: Vec<Child> = self
            .comps
            .iter_mut()
            .filter_map(|c| {
                c.done = true;
                c.child.take()
            })
            .collect();
        stop_children(&mut children);
    }
}

/// Ask `children` to exit (`ExplorerRs.Shutdown`), wait up to
/// [`CLOSE_TIMEOUT`] in total, then kill the rest.
fn stop_children(children: &mut [Child]) {
    #[cfg(windows)]
    {
        let pids: Vec<u32> = children.iter().map(Child::id).collect();
        if explorer_win::process::post_shutdown_all(&pids) > 0 {
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            while Instant::now() < deadline
                && children
                    .iter_mut()
                    .any(|c| matches!(c.try_wait(), Ok(None)))
            {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    #[cfg(not(windows))]
    let _ = CLOSE_TIMEOUT;
    for c in children.iter_mut() {
        if matches!(c.try_wait(), Ok(None)) {
            let _ = c.kill();
        }
        let _ = c.wait();
    }
}

/// Watch the guardian process; once it is gone, nobody would act on
/// `EXIT_FALLBACK`, so the supervisor handles fallbacks itself.
#[cfg(windows)]
fn watch_guardian(pid: u32, guarded: Arc<AtomicBool>) {
    // Make sure the pid still names a guardian (same executable as us).
    let ours = std::env::current_exe().ok();
    let same = explorer_win::security::process_image_path(pid)
        .ok()
        .zip(ours)
        .is_some_and(|(a, b)| {
            a.to_string_lossy()
                .eq_ignore_ascii_case(&b.to_string_lossy())
        });
    let g = guarded.clone();
    let watching = same
        && explorer_win::process::on_process_exit(pid, move || {
            tracing::warn!(
                pid,
                "the guardian exited; this supervisor now handles fallbacks itself"
            );
            g.store(false, Ordering::SeqCst);
        });
    if !watching {
        tracing::warn!(pid, "guardian not found; handling fallbacks myself");
        guarded.store(false, Ordering::SeqCst);
    }
}

/// Ask a component to exit (`ExplorerRs.Shutdown`), then kill it.
fn stop_child(c: &mut Component) {
    if let Some(child) = c.child.take() {
        stop_children(&mut [child]);
    }
}

fn spawn(exe: &Path, mode: RunMode, config_override: Option<&Path>) -> std::io::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.env(MODE_ENV, mode.as_str())
        .env_remove(GUARDED_ENV)
        .stdin(Stdio::null());
    if let Some(p) = config_override {
        cmd.arg("--config").arg(p);
    }
    cmd.spawn()
}

/// Without a guardian (supervisor started by hand): start explorer.exe so
/// the user is never left with an empty screen.
fn start_fallback_explorer() {
    tracing::warn!("starting explorer.exe as a fallback");
    #[cfg(windows)]
    let _ = explorer_win::process::start_explorer();
}

/// Once per sign-in (shell mode): ready events and startup apps.
#[cfg(windows)]
fn spawn_shell_duties() {
    use explorer_win::session::{NamedEvent, SHELL_DUTIES_EVENT};
    let _ = std::thread::Builder::new()
        .name("shell-duties".into())
        .spawn(|| {
            let event = NamedEvent::create(SHELL_DUTIES_EVENT);
            match &event {
                Some(e) if !e.claim() => {
                    tracing::info!("shell duties already done in this sign-in; skipping");
                    return;
                }
                Some(_) => {}
                None => tracing::warn!("no session guard; running shell duties anyway"),
            }
            // Keep the event alive for this process's lifetime (the guardian
            // also holds it, so it survives supervisor restarts).
            std::mem::forget(event);
            wait_for_taskbar(Duration::from_secs(15));
            explorer_win::session::signal_shell_ready();
            // Give the tray a moment so tray icons of startup apps land.
            std::thread::sleep(Duration::from_secs(2));
            explorer_win::startup::run_all();
        });
}

#[cfg(windows)]
fn wait_for_taskbar(timeout: Duration) {
    use explorer_win::util::WideString;
    use explorer_win::windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    let deadline = Instant::now() + timeout;
    let classes = [
        WideString::new("Shell_TrayWnd"),
        WideString::new(explorer_core::ipc::TASKBAR_WINDOW_CLASS),
    ];
    while Instant::now() < deadline {
        // SAFETY: valid class-name strings.
        let found = classes.iter().any(|c| {
            unsafe { FindWindowW(c.pcwstr(), windows_null()) }.is_ok_and(|h| !h.is_invalid())
        });
        if found {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    tracing::warn!("taskbar window not found in time; continuing");
}

#[cfg(windows)]
fn windows_null() -> explorer_win::windows::core::PCWSTR {
    explorer_win::windows::core::PCWSTR::null()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_naming() {
        let p = component_exe(Path::new("/opt/x"), "taskbar");
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.starts_with("explorer-rs-taskbar"));
    }

    #[test]
    fn config_watch_waits_for_a_stable_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let mut w = ConfigWatch::new(p.clone());
        let t0 = Instant::now();
        let step = |n: u64| t0 + CONFIG_POLL * (n as u32) + Duration::from_millis(1);
        assert!(!w.poll(step(1)));
        std::fs::write(&p, "[taskbar]\nheight = 40\n").unwrap();
        assert!(!w.poll(step(2)), "first sighting only records the change");
        assert!(w.poll(step(3)), "stable on the next poll");
        assert!(!w.poll(step(4)), "applied once");
        // Polls closer than CONFIG_POLL are ignored.
        assert!(!w.poll(step(4)));
    }

    #[test]
    fn read_config_reports_errors_and_explicitness() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("config.toml");
        let (c, explicit) = read_config(&p).unwrap();
        assert_eq!(c, Config::default());
        assert!(!explicit);
        std::fs::write(&p, "[supervisor]\ncomponents = [\"taskbar\"]\n").unwrap();
        assert!(read_config(&p).unwrap().1);
        std::fs::write(&p, "[taskbar]\nheight = 1\n").unwrap();
        assert!(read_config(&p).is_err());
    }
}
