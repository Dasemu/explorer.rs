// SPDX-License-Identifier: GPL-3.0-or-later
//! Policy of the guardian process.
//!
//! `explorer-rs.exe --dev|--shell` is a tiny guardian that runs the real
//! supervisor (`explorer-rs.exe --supervise ...`) as a child and restarts it
//! when it exits unexpectedly. The supervisor reports component-level
//! disasters through its exit code ([`EXIT_FALLBACK`]). This module decides
//! what the guardian does with each supervisor exit; see ADR 0014.

use crate::config::SupervisorConfig;
use crate::supervisor::{Decision, RestartTracker, EXIT_RESTART, EXIT_SHUTDOWN_ALL, EXIT_STOP};

/// Supervisor exit code: a crash loop (e.g. of the taskbar) in shell mode
/// requires the fallback to explorer.exe.
pub const EXIT_FALLBACK: i32 = 79;
/// Supervisor exit code: another supervisor already runs in this session.
pub const EXIT_ALREADY_RUNNING: i32 = 80;

/// Environment variable set by the guardian on the supervisor: Unix time in
/// milliseconds when the session (guardian) started.
pub const SESSION_START_ENV: &str = "EXPLORER_RS_SESSION_START";
/// Environment variable set by the guardian on the supervisor ("1"): the
/// guardian handles fallbacks, the supervisor only reports them.
pub const GUARDED_ENV: &str = "EXPLORER_RS_GUARDED";

/// Failures within this time after sign-in count towards a boot loop.
pub const BOOT_WINDOW_MS: u64 = 120_000;
/// This many failures inside [`BOOT_WINDOW_MS`] are a boot loop.
pub const BOOT_LOOP_FAILURES: usize = 3;
/// Delay before retrying when another supervisor holds the instance lock.
pub const ALREADY_RUNNING_RETRY_MS: u64 = 5_000;

/// True if `failures_ms` (milliseconds since the session started) contain
/// a boot loop.
pub fn boot_loop(failures_ms: &[u64]) -> bool {
    failures_ms.iter().filter(|&&t| t <= BOOT_WINDOW_MS).count() >= BOOT_LOOP_FAILURES
}

/// Why a fallback is needed (also logged in the crash log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// The supervisor failed [`BOOT_LOOP_FAILURES`] times right after sign-in.
    SupervisorBootLoop,
    /// The supervisor crash-looped later on.
    SupervisorCrashLoop,
    /// The supervisor reported a component crash loop ([`EXIT_FALLBACK`]).
    ComponentCrashLoop,
}

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::SupervisorBootLoop => "supervisor boot loop",
            FallbackReason::SupervisorCrashLoop => "supervisor crash loop",
            FallbackReason::ComponentCrashLoop => "component crash loop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardianDecision {
    /// Start the supervisor again after this many milliseconds.
    Restart { delay_ms: u64, crash: bool },
    /// The user quit explorer.rs (or the session ends): exit quietly.
    Exit,
    /// Give up. In shell mode: start explorer.exe and ask the user.
    Fallback(FallbackReason),
}

/// Restart bookkeeping for the supervisor child.
#[derive(Debug, Clone)]
pub struct GuardianPolicy {
    shell: bool,
    tracker: RestartTracker,
    failures_ms: Vec<u64>,
}

impl GuardianPolicy {
    pub fn new(cfg: &SupervisorConfig, shell: bool) -> Self {
        Self {
            shell,
            tracker: RestartTracker::new(cfg),
            failures_ms: Vec::new(),
        }
    }

    /// The supervisor exited. `since_session_ms` = time since the guardian
    /// started; `session_ending` = sign-out / shutdown in progress.
    pub fn on_exit(
        &mut self,
        code: Option<i32>,
        uptime_ms: u64,
        since_session_ms: u64,
        session_ending: bool,
    ) -> GuardianDecision {
        if session_ending {
            return GuardianDecision::Exit;
        }
        match code {
            Some(EXIT_STOP) | Some(EXIT_SHUTDOWN_ALL) => return GuardianDecision::Exit,
            Some(EXIT_FALLBACK) => {
                return if self.shell {
                    GuardianDecision::Fallback(FallbackReason::ComponentCrashLoop)
                } else {
                    GuardianDecision::Exit
                };
            }
            Some(EXIT_RESTART) => {
                return GuardianDecision::Restart {
                    delay_ms: 0,
                    crash: false,
                }
            }
            Some(EXIT_ALREADY_RUNNING) => {
                return GuardianDecision::Restart {
                    delay_ms: ALREADY_RUNNING_RETRY_MS,
                    crash: false,
                }
            }
            _ => {}
        }
        self.failures_ms.push(since_session_ms);
        if self.shell && boot_loop(&self.failures_ms) {
            return GuardianDecision::Fallback(FallbackReason::SupervisorBootLoop);
        }
        match self.tracker.on_exit(code, uptime_ms, since_session_ms) {
            Decision::RestartAfter(delay_ms) => GuardianDecision::Restart {
                delay_ms,
                crash: true,
            },
            Decision::GiveUp if self.shell => {
                GuardianDecision::Fallback(FallbackReason::SupervisorCrashLoop)
            }
            _ => GuardianDecision::Exit,
        }
    }

    /// The user asked for a restart (hotkey): forget past failures.
    pub fn reset(&mut self, cfg: &SupervisorConfig) {
        self.tracker = RestartTracker::new(cfg);
        self.failures_ms.clear();
    }
}

/// Supervisor side: should a component crash end in the explorer.exe
/// fallback? Only the taskbar matters (without it there is no way to reach
/// anything), and only in shell mode.
pub fn component_needs_fallback(
    shell: bool,
    component: &str,
    decision: Decision,
    failures_since_session_ms: &[u64],
) -> bool {
    shell
        && component == "taskbar"
        && (decision == Decision::GiveUp || boot_loop(failures_since_session_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> SupervisorConfig {
        SupervisorConfig {
            restart_initial_delay_ms: 500,
            restart_max_delay_ms: 4000,
            crash_loop_max: 5,
            crash_loop_window_s: 60,
            ..SupervisorConfig::default()
        }
    }

    #[test]
    fn boot_loop_window() {
        assert!(!boot_loop(&[1000, 2000]));
        assert!(boot_loop(&[1000, 2000, 119_000]));
        assert!(!boot_loop(&[1000, 2000, 121_000]));
    }

    #[test]
    fn exit_codes() {
        let mut p = GuardianPolicy::new(&cfg(), true);
        assert_eq!(p.on_exit(Some(0), 10, 10, false), GuardianDecision::Exit);
        assert_eq!(p.on_exit(Some(77), 10, 10, false), GuardianDecision::Exit);
        assert_eq!(
            p.on_exit(Some(EXIT_FALLBACK), 10, 10, false),
            GuardianDecision::Fallback(FallbackReason::ComponentCrashLoop)
        );
        assert_eq!(
            p.on_exit(Some(EXIT_ALREADY_RUNNING), 10, 10, false),
            GuardianDecision::Restart {
                delay_ms: ALREADY_RUNNING_RETRY_MS,
                crash: false
            }
        );
        assert_eq!(p.on_exit(Some(5), 10, 10, true), GuardianDecision::Exit);
        let mut dev = GuardianPolicy::new(&cfg(), false);
        assert_eq!(
            dev.on_exit(Some(EXIT_FALLBACK), 10, 10, false),
            GuardianDecision::Exit
        );
    }

    #[test]
    fn shell_boot_loop_falls_back() {
        let mut p = GuardianPolicy::new(&cfg(), true);
        assert!(matches!(
            p.on_exit(Some(1), 100, 1_000, false),
            GuardianDecision::Restart { crash: true, .. }
        ));
        assert!(matches!(
            p.on_exit(None, 100, 5_000, false),
            GuardianDecision::Restart { crash: true, .. }
        ));
        assert_eq!(
            p.on_exit(Some(1), 100, 20_000, false),
            GuardianDecision::Fallback(FallbackReason::SupervisorBootLoop)
        );
    }

    #[test]
    fn dev_mode_restarts_then_gives_up() {
        let mut p = GuardianPolicy::new(&cfg(), false);
        let mut last = GuardianDecision::Exit;
        for i in 0..6 {
            last = p.on_exit(Some(1), 10, 1_000 + i * 100, false);
            if i < 5 {
                assert!(matches!(last, GuardianDecision::Restart { .. }), "{i}");
            }
        }
        assert_eq!(last, GuardianDecision::Exit);
        p.reset(&cfg());
        assert!(matches!(
            p.on_exit(Some(1), 10, 5_000, false),
            GuardianDecision::Restart { delay_ms: 500, .. }
        ));
    }

    #[test]
    fn late_crash_loop_in_shell_falls_back() {
        let mut p = GuardianPolicy::new(&cfg(), true);
        let mut last = GuardianDecision::Exit;
        for i in 0..6 {
            last = p.on_exit(Some(1), 10, 600_000 + i * 1000, false);
        }
        assert_eq!(
            last,
            GuardianDecision::Fallback(FallbackReason::SupervisorCrashLoop)
        );
    }

    #[test]
    fn taskbar_fallback_rule() {
        let d = Decision::RestartAfter(500);
        assert!(!component_needs_fallback(true, "taskbar", d, &[1, 2]));
        assert!(component_needs_fallback(true, "taskbar", d, &[1, 2, 3]));
        assert!(component_needs_fallback(
            true,
            "taskbar",
            Decision::GiveUp,
            &[]
        ));
        assert!(!component_needs_fallback(
            true,
            "launcher",
            Decision::GiveUp,
            &[]
        ));
        assert!(!component_needs_fallback(
            false,
            "taskbar",
            Decision::GiveUp,
            &[1, 2, 3]
        ));
    }
}
