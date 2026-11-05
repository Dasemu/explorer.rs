// SPDX-License-Identifier: GPL-3.0-or-later
//! Restart policy for supervised components.
//!
//! Each component (taskbar, launcher) runs in its own process. When one exits
//! the supervisor asks [`RestartTracker::on_exit`] what to do. Policy:
//! - exit code [`EXIT_STOP`] (0): intentional stop, do not restart;
//! - exit code [`EXIT_SHUTDOWN_ALL`]: the user asked to quit explorer.rs;
//! - anything else (crash, panic, kill): restart with exponential backoff;
//! - more than `crash_loop_max` crashes inside `crash_loop_window`: give up
//!   on that component only. Other components keep running.
//!
//! Time is passed in explicitly (milliseconds) so the policy is testable.

use std::collections::VecDeque;

use crate::config::SupervisorConfig;

/// Component exited on purpose; do not restart it.
pub const EXIT_STOP: i32 = 0;
/// Component asks the supervisor to stop everything and exit.
pub const EXIT_SHUTDOWN_ALL: i32 = 77;
/// Component asks to be restarted immediately (e.g. after config change).
pub const EXIT_RESTART: i32 = 78;

/// Exit code of a component whose message loop ended. `intentional` is the
/// code set by a deliberate stop/quit path (quit menu, restart request,
/// graceful shutdown message). Anything else (main window destroyed by
/// someone, a stray `WM_QUIT`) is unexpected and asks for a restart.
pub fn component_exit_code(intentional: Option<i32>) -> i32 {
    intentional.unwrap_or(EXIT_RESTART)
}

/// A component that ran at least this long is considered healthy: its
/// backoff is reset.
pub const HEALTHY_UPTIME_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    RestartAfter(u64),
    Stop,
    ShutdownAll,
    GiveUp,
}

#[derive(Debug, Clone)]
pub struct RestartTracker {
    initial_ms: u64,
    max_ms: u64,
    loop_max: u32,
    loop_window_ms: u64,
    crashes: VecDeque<u64>,
    consecutive: u32,
}

impl RestartTracker {
    pub fn new(cfg: &SupervisorConfig) -> Self {
        Self {
            initial_ms: cfg.restart_initial_delay_ms,
            max_ms: cfg.restart_max_delay_ms,
            loop_max: cfg.crash_loop_max,
            loop_window_ms: cfg.crash_loop_window_s * 1000,
            crashes: VecDeque::new(),
            consecutive: 0,
        }
    }

    /// `code` is `None` when the process was killed without an exit code.
    pub fn on_exit(&mut self, code: Option<i32>, uptime_ms: u64, now_ms: u64) -> Decision {
        match code {
            Some(EXIT_STOP) => return Decision::Stop,
            Some(EXIT_SHUTDOWN_ALL) => return Decision::ShutdownAll,
            Some(EXIT_RESTART) => {
                self.consecutive = 0;
                return Decision::RestartAfter(0);
            }
            _ => {}
        }
        if uptime_ms >= HEALTHY_UPTIME_MS {
            self.consecutive = 0;
        }
        self.crashes.push_back(now_ms);
        while let Some(&t) = self.crashes.front() {
            if now_ms.saturating_sub(t) > self.loop_window_ms {
                self.crashes.pop_front();
            } else {
                break;
            }
        }
        if self.crashes.len() as u32 > self.loop_max {
            return Decision::GiveUp;
        }
        let delay = self
            .initial_ms
            .saturating_mul(1u64 << self.consecutive.min(20))
            .min(self.max_ms);
        self.consecutive = self.consecutive.saturating_add(1);
        Decision::RestartAfter(delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_exit_codes() {
        assert_eq!(component_exit_code(Some(EXIT_STOP)), EXIT_STOP);
        assert_eq!(
            component_exit_code(Some(EXIT_SHUTDOWN_ALL)),
            EXIT_SHUTDOWN_ALL
        );
        // Not deliberate: the supervisor restarts the component.
        assert_eq!(component_exit_code(None), EXIT_RESTART);
    }

    fn tracker() -> RestartTracker {
        RestartTracker::new(&SupervisorConfig {
            restart_initial_delay_ms: 500,
            restart_max_delay_ms: 4000,
            crash_loop_max: 3,
            crash_loop_window_s: 60,
            ..SupervisorConfig::default()
        })
    }

    #[test]
    fn exit_codes() {
        let mut t = tracker();
        assert_eq!(t.on_exit(Some(0), 1, 0), Decision::Stop);
        assert_eq!(
            t.on_exit(Some(EXIT_SHUTDOWN_ALL), 1, 0),
            Decision::ShutdownAll
        );
        assert_eq!(
            t.on_exit(Some(EXIT_RESTART), 1, 0),
            Decision::RestartAfter(0)
        );
    }

    #[test]
    fn exponential_backoff_capped() {
        let mut t = tracker();
        // Spread crashes out so the crash-loop window never fills.
        let mut now = 0;
        let mut delays = vec![];
        for _ in 0..5 {
            now += 61_000;
            match t.on_exit(Some(1), 10, now) {
                Decision::RestartAfter(d) => delays.push(d),
                d => panic!("unexpected {d:?}"),
            }
        }
        assert_eq!(delays, vec![500, 1000, 2000, 4000, 4000]);
    }

    #[test]
    fn healthy_uptime_resets_backoff() {
        let mut t = tracker();
        t.on_exit(Some(1), 10, 100_000);
        t.on_exit(Some(1), 10, 200_000);
        assert_eq!(
            t.on_exit(None, HEALTHY_UPTIME_MS, 300_000),
            Decision::RestartAfter(500)
        );
    }

    #[test]
    fn crash_loop_gives_up() {
        let mut t = tracker();
        for i in 0..3 {
            assert!(matches!(
                t.on_exit(Some(1), 10, i * 1000),
                Decision::RestartAfter(_)
            ));
        }
        assert_eq!(t.on_exit(Some(1), 10, 4000), Decision::GiveUp);
    }
}
