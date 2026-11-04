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
/// First delay before retrying when another supervisor holds the instance
/// lock; doubles on every further attempt up to
/// [`ALREADY_RUNNING_MAX_RETRY_MS`].
pub const ALREADY_RUNNING_RETRY_MS: u64 = 5_000;
/// Upper bound of the "another supervisor is running" retry delay.
pub const ALREADY_RUNNING_MAX_RETRY_MS: u64 = 60_000;
/// Environment variable set by the guardian on the supervisor: the
/// guardian's process id. The supervisor watches that process and handles
/// fallbacks itself once the guardian is gone.
pub const GUARDIAN_PID_ENV: &str = "EXPLORER_RS_GUARDIAN_PID";

/// Delay before the `streak`-th consecutive retry (1-based) after
/// [`EXIT_ALREADY_RUNNING`]: 5 s, 10 s, 20 s, 40 s, then 60 s.
pub fn already_running_delay(streak: u32) -> u64 {
    let shift = streak.saturating_sub(1).min(16);
    ALREADY_RUNNING_RETRY_MS
        .saturating_mul(1u64 << shift)
        .min(ALREADY_RUNNING_MAX_RETRY_MS)
}

/// The guardian's pid as passed to a supervisor, if it runs guarded
/// (`GUARDED_ENV` = "1") and the pid is valid.
pub fn guardian_pid(guarded: Option<&str>, pid: Option<&str>) -> Option<u32> {
    if guarded != Some("1") {
        return None;
    }
    pid.and_then(|p| p.trim().parse::<u32>().ok())
        .filter(|&p| p != 0)
}

/// Mode to start the supervisor in. A shell-mode guardian starts it in dev
/// mode while the Windows Explorer shell runs (after a fallback where the
/// user kept explorer.rs, or when restarted by Ctrl+Alt+Shift+R), so two
/// shells never fight over the taskbar and the desktop.
pub fn supervisor_mode(shell_guardian: bool, explorer_shell_running: bool) -> crate::ipc::RunMode {
    if shell_guardian && !explorer_shell_running {
        crate::ipc::RunMode::Shell
    } else {
        crate::ipc::RunMode::Dev
    }
}

/// Executables that make up a running explorer.rs shell (guardian or
/// supervisor, components). Not setup, the uninstaller or the elevated
/// index helper.
pub const SHELL_PROCESSES: &[&str] = &[
    "explorer-rs.exe",
    "explorer-rs-taskbar.exe",
    "explorer-rs-launcher.exe",
    "explorer-rs-desktop.exe",
    "explorer-rs-indexd.exe",
];

/// True if `image` (file name or full path) is one of [`SHELL_PROCESSES`].
pub fn is_shell_process(image: &str) -> bool {
    let name = image.rsplit(['\\', '/']).next().unwrap_or(image);
    SHELL_PROCESSES.iter().any(|p| p.eq_ignore_ascii_case(name))
}

/// True if `image` (a file name or full path) is Windows Explorer.
pub fn is_explorer_image(image: &str) -> bool {
    image
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|n| n.eq_ignore_ascii_case("explorer.exe"))
}

/// "The Windows Explorer shell is running": the process owning the shell
/// window (`GetShellWindow`) or a `Shell_TrayWnd` window is explorer.exe.
/// A File Explorer window (also explorer.exe) owns neither, and our own
/// `Shell_TrayWnd` belongs to explorer-rs-taskbar.exe, so neither counts.
pub fn explorer_shell_running<'a>(
    shell_window_owner: Option<&str>,
    tray_window_owners: impl IntoIterator<Item = &'a str>,
) -> bool {
    shell_window_owner.is_some_and(is_explorer_image)
        || tray_window_owners.into_iter().any(is_explorer_image)
}

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
    already_running: u32,
}

impl GuardianPolicy {
    pub fn new(cfg: &SupervisorConfig, shell: bool) -> Self {
        Self {
            shell,
            tracker: RestartTracker::new(cfg),
            failures_ms: Vec::new(),
            already_running: 0,
        }
    }

    /// Consecutive [`EXIT_ALREADY_RUNNING`] exits so far.
    pub fn already_running_streak(&self) -> u32 {
        self.already_running
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
        if code == Some(EXIT_ALREADY_RUNNING) {
            // Another supervisor (e.g. orphaned by a guardian that died)
            // holds the lock: back off instead of polling every 5 s.
            self.already_running = self.already_running.saturating_add(1);
            return GuardianDecision::Restart {
                delay_ms: already_running_delay(self.already_running),
                crash: false,
            };
        }
        self.already_running = 0;
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
        self.already_running = 0;
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
        assert_eq!(p.already_running_streak(), 1);
        assert_eq!(p.on_exit(Some(5), 10, 10, true), GuardianDecision::Exit);
        let mut dev = GuardianPolicy::new(&cfg(), false);
        assert_eq!(
            dev.on_exit(Some(EXIT_FALLBACK), 10, 10, false),
            GuardianDecision::Exit
        );
    }

    #[test]
    fn already_running_backs_off() {
        assert_eq!(already_running_delay(1), 5_000);
        assert_eq!(already_running_delay(2), 10_000);
        assert_eq!(already_running_delay(4), 40_000);
        assert_eq!(already_running_delay(5), 60_000);
        assert_eq!(already_running_delay(u32::MAX), 60_000);
        let mut p = GuardianPolicy::new(&cfg(), true);
        let delays: Vec<u64> = (0..6)
            .map(
                |_| match p.on_exit(Some(EXIT_ALREADY_RUNNING), 10, 10, false) {
                    GuardianDecision::Restart { delay_ms, crash } => {
                        assert!(!crash);
                        delay_ms
                    }
                    d => panic!("{d:?}"),
                },
            )
            .collect();
        assert_eq!(delays, [5_000, 10_000, 20_000, 40_000, 60_000, 60_000]);
        // Any other exit (the supervisor finally ran) resets the streak.
        let _ = p.on_exit(Some(EXIT_RESTART), 10, 10, false);
        assert_eq!(p.already_running_streak(), 0);
        let _ = p.on_exit(Some(EXIT_ALREADY_RUNNING), 10, 10, false);
        p.reset(&cfg());
        assert_eq!(p.already_running_streak(), 0);
    }

    #[test]
    fn guardian_pid_parsing() {
        assert_eq!(guardian_pid(Some("1"), Some("1234")), Some(1234));
        assert_eq!(guardian_pid(Some("1"), Some(" 42 ")), Some(42));
        assert_eq!(guardian_pid(Some("1"), Some("0")), None);
        assert_eq!(guardian_pid(Some("1"), Some("x")), None);
        assert_eq!(guardian_pid(Some("1"), None), None);
        assert_eq!(guardian_pid(None, Some("1234")), None);
        assert_eq!(guardian_pid(Some("0"), Some("1234")), None);
    }

    #[test]
    fn supervisor_mode_never_starts_a_second_shell() {
        use crate::ipc::RunMode;
        assert_eq!(supervisor_mode(true, false), RunMode::Shell);
        assert_eq!(supervisor_mode(true, true), RunMode::Dev);
        assert_eq!(supervisor_mode(false, false), RunMode::Dev);
        assert_eq!(supervisor_mode(false, true), RunMode::Dev);
    }

    #[test]
    fn shell_process_names() {
        assert!(is_shell_process("explorer-rs.exe"));
        assert!(is_shell_process(
            r"C:\Program Files\explorer-rs\EXPLORER-RS-TASKBAR.EXE"
        ));
        assert!(is_shell_process("explorer-rs-indexd.exe"));
        assert!(!is_shell_process("explorer-rs-setup.exe"));
        assert!(!is_shell_process("explorer-rs-index-helper.exe"));
        assert!(!is_shell_process("explorer.exe"));
    }

    #[test]
    fn explorer_shell_detection() {
        assert!(is_explorer_image(r"C:\Windows\explorer.exe"));
        assert!(is_explorer_image("EXPLORER.EXE"));
        assert!(!is_explorer_image(
            r"C:\Program Files\explorer-rs\explorer-rs-taskbar.exe"
        ));
        assert!(!is_explorer_image(r"C:\x\notexplorer.exe"));
        // Shell window owned by explorer.exe.
        assert!(explorer_shell_running(Some(r"C:\Windows\explorer.exe"), []));
        // Only our own tray window (explorer.rs is the shell), plus a File
        // Explorer window that owns no shell window: not the Explorer shell.
        assert!(!explorer_shell_running(
            Some(r"C:\Program Files\explorer-rs\explorer-rs-desktop.exe"),
            [r"C:\Program Files\explorer-rs\explorer-rs-taskbar.exe"]
        ));
        assert!(!explorer_shell_running(None, []));
        // Dev mode: our tray window sits in front of Explorer's.
        assert!(explorer_shell_running(
            None,
            [
                r"C:\Program Files\explorer-rs\explorer-rs-taskbar.exe",
                r"C:\Windows\explorer.exe"
            ]
        ));
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
