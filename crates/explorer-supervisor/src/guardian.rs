// SPDX-License-Identifier: GPL-3.0-or-later
//! The guardian restart loop (`explorer-rs.exe --dev|--shell`).
//!
//! It runs the supervisor (`explorer-rs.exe --supervise ...`) as a child
//! and applies `explorer_core::guardian::GuardianPolicy` to each exit.
//! Everything with side effects beyond spawning (crash log, dialogs,
//! hotkeys, session end) goes through [`Host`], so the loop itself is
//! portable and tested on Linux with a fake child.
//!
//! The guardian is deliberately tiny: no COM, no window except the session
//! listener and recovery message boxes, no config reload.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use explorer_core::config::SupervisorConfig;
use explorer_core::crashlog::{Event, EventKind};
use explorer_core::guardian::{FallbackReason, GuardianDecision, GuardianPolicy};

/// Requests from the user (emergency hotkeys).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostCommand {
    /// Kill the supervisor (and with it every component) and start again.
    RestartAll,
}

/// What the guardian does after a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFallback {
    /// Exit (Windows Explorer was restored).
    Exit,
    /// Stay alive without a supervisor; only hotkeys work (Ctrl+Alt+Shift+R
    /// starts explorer.rs again).
    Idle,
}

/// Side effects of the guardian.
pub trait Host {
    /// Next pending user command, if any.
    fn poll_command(&mut self) -> Option<HostCommand>;
    /// Sign-out / shutdown in progress.
    fn session_ending(&self) -> bool;
    /// A crash loop: start explorer.exe and ask the user (shell mode).
    fn fallback(&mut self, reason: FallbackReason) -> AfterFallback;
    /// Record an event in the crash log.
    fn record(&mut self, ev: Event);
    /// The supervisor has been running for [`Options::healthy_after`].
    fn healthy(&mut self);
}

/// How to run the supervisor child.
#[derive(Debug, Clone)]
pub struct Options {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub envs: Vec<(String, String)>,
    pub shell: bool,
    pub supervisor: SupervisorConfig,
    pub poll: Duration,
    pub healthy_after: Duration,
}

fn now_s() -> u64 {
    explorer_core::crashlog::unix_now()
}

/// Run until the supervisor exits on purpose (dev mode; shell mode idles
/// instead), the session ends, or a fallback ends the guardian. Returns the process exit code.
pub fn run(opts: &Options, host: &mut dyn Host) -> i32 {
    let session_start = Instant::now();
    let mut policy = GuardianPolicy::new(&opts.supervisor, opts.shell);
    let mut child: Option<(Child, Instant)> = None;
    let mut next_start: Option<Instant> = Some(session_start);
    let mut idle = false;
    let mut healthy_reported = false;

    loop {
        let now = Instant::now();
        if host.session_ending() {
            // Components get their own end-session messages; never restart
            // anything now, and leave quietly.
            tracing::info!("session ending; guardian exits");
            return 0;
        }
        while let Some(cmd) = host.poll_command() {
            match cmd {
                HostCommand::RestartAll => {
                    tracing::warn!("restart of all components requested");
                    if let Some((mut c, _)) = child.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                    host.record(
                        Event::new(now_s(), EventKind::Exit, "supervisor")
                            .with_detail("restart requested by hotkey"),
                    );
                    policy.reset(&opts.supervisor);
                    idle = false;
                    next_start = Some(now);
                }
            }
        }

        let mut exited: Option<(Option<i32>, u64)> = None;
        if let Some((c, started)) = child.as_mut() {
            match c.try_wait() {
                Ok(Some(status)) => {
                    exited = Some((
                        status.code(),
                        now.duration_since(*started).as_millis() as u64,
                    ));
                }
                Ok(None) => {
                    if !healthy_reported && now.duration_since(*started) >= opts.healthy_after {
                        healthy_reported = true;
                        host.healthy();
                    }
                }
                Err(e) => tracing::warn!(error = %e, "try_wait on supervisor failed"),
            }
        }
        if exited.is_some() {
            child = None;
        }

        if child.is_none() && !idle && next_start.is_some_and(|t| now >= t) {
            next_start = None;
            let mut cmd = Command::new(&opts.program);
            cmd.args(&opts.args).stdin(Stdio::null());
            for (k, v) in &opts.envs {
                cmd.env(k, v);
            }
            match cmd.spawn() {
                Ok(c) => {
                    tracing::info!(pid = c.id(), "supervisor started");
                    host.record(Event::new(now_s(), EventKind::Start, "supervisor"));
                    child = Some((c, now));
                }
                Err(e) => {
                    tracing::error!(error = %e, program = %opts.program.display(), "cannot start supervisor");
                    exited = Some((None, 0));
                }
            }
        }

        if let Some((code, uptime_ms)) = exited {
            let since = now.duration_since(session_start).as_millis() as u64;
            let decision = policy.on_exit(code, uptime_ms, since, host.session_ending());
            tracing::info!(?code, uptime_ms, ?decision, "supervisor exited");
            let kind = match decision {
                GuardianDecision::Restart { crash: true, .. } | GuardianDecision::Fallback(_) => {
                    EventKind::Crash
                }
                _ => EventKind::Exit,
            };
            host.record(Event::new(now_s(), kind, "supervisor").with_code(code));
            match decision {
                GuardianDecision::Restart { delay_ms, .. } => {
                    next_start = Some(now + Duration::from_millis(delay_ms));
                }
                GuardianDecision::Exit if opts.shell => {
                    // explorer.rs was quit on purpose. In shell mode stay
                    // around so the emergency hotkeys keep working
                    // (Ctrl+Alt+Shift+R starts explorer.rs again).
                    tracing::info!("supervisor stopped; guardian idles (hotkeys stay active)");
                    idle = true;
                }
                GuardianDecision::Exit => return 0,
                GuardianDecision::Fallback(reason) => {
                    tracing::error!(reason = reason.as_str(), "crash loop; falling back");
                    match host.fallback(reason) {
                        AfterFallback::Exit => return 0,
                        AfterFallback::Idle => idle = true,
                    }
                }
            }
        }

        std::thread::sleep(opts.poll);
    }
}
