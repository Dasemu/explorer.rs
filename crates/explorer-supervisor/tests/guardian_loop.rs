// SPDX-License-Identifier: GPL-3.0-or-later
//! Drives the guardian restart loop with a fake supervisor (a shell
//! script) to check restarts, exit handling and the boot-loop fallback.
#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use explorer_core::config::SupervisorConfig;
use explorer_core::crashlog::{Event, EventKind};
use explorer_core::guardian::FallbackReason;
use explorer_supervisor::guardian::{run, AfterFallback, Host, HostCommand, Options};

#[derive(Default)]
struct TestHost {
    events: Vec<Event>,
    fallbacks: Vec<FallbackReason>,
    commands: Vec<HostCommand>,
    healthy: bool,
    /// The Windows Explorer shell "runs".
    explorer: bool,
    /// Graceful stop requests (pids); the host sends SIGTERM when
    /// `stop_works`.
    stop_requests: Vec<u32>,
    stop_works: bool,
    stopped: bool,
}

impl Host for TestHost {
    fn poll_command(&mut self) -> Option<HostCommand> {
        self.commands.pop()
    }
    fn session_ending(&self) -> bool {
        false
    }
    fn fallback(&mut self, reason: FallbackReason) -> AfterFallback {
        self.fallbacks.push(reason);
        AfterFallback::Exit
    }
    fn record(&mut self, ev: Event) {
        self.events.push(ev);
    }
    fn healthy(&mut self) {
        self.healthy = true;
    }
    fn explorer_shell_running(&self) -> bool {
        self.explorer
    }
    fn request_stop(&mut self, pid: u32) -> bool {
        self.stop_requests.push(pid);
        if self.stop_works {
            let _ = std::process::Command::new("kill")
                .arg(pid.to_string())
                .status();
        }
        true
    }
    fn stopped(&mut self) {
        self.stopped = true;
    }
}

impl TestHost {
    fn count(&self, kind: EventKind) -> usize {
        self.events.iter().filter(|e| e.kind == kind).count()
    }
}

fn options(script: &str, shell: bool) -> Options {
    Options {
        program: "sh".into(),
        args: vec!["-c".into(), script.into()],
        dev_args: vec!["-c".into(), script.into()],
        envs: vec![("EXPLORER_RS_TEST".into(), "1".into())],
        shell,
        supervisor: SupervisorConfig {
            restart_initial_delay_ms: 10,
            restart_max_delay_ms: 40,
            crash_loop_max: 5,
            crash_loop_window_s: 60,
            ..SupervisorConfig::default()
        },
        poll: Duration::from_millis(10),
        healthy_after: Duration::from_secs(3600),
        stop_timeout: Duration::from_millis(300),
    }
}

fn counter_script(file: &Path, body: &str) -> String {
    // Appends a line per run, then runs `body` with $n = run number.
    format!(
        "echo x >> '{f}'; n=$(wc -l < '{f}'); {body}",
        f = file.display()
    )
}

#[test]
fn clean_exit_ends_the_guardian_in_dev_mode() {
    let mut host = TestHost::default();
    assert_eq!(run(&options("exit 0", false), &mut host), 0);
    assert_eq!(host.count(EventKind::Start), 1);
    assert_eq!(host.count(EventKind::Exit), 1);
    assert!(host.fallbacks.is_empty());
}

/// Ends the session after `n` polls.
struct EndingHost {
    inner: TestHost,
    polls: std::cell::Cell<u32>,
    n: u32,
}

impl Host for EndingHost {
    fn poll_command(&mut self) -> Option<HostCommand> {
        None
    }
    fn explorer_shell_running(&self) -> bool {
        self.inner.explorer
    }
    fn session_ending(&self) -> bool {
        self.polls.set(self.polls.get() + 1);
        self.polls.get() > self.n
    }
    fn fallback(&mut self, r: FallbackReason) -> AfterFallback {
        self.inner.fallback(r)
    }
    fn record(&mut self, ev: Event) {
        self.inner.record(ev)
    }
    fn healthy(&mut self) {}
}

#[test]
fn shell_mode_idles_after_quit_until_session_end() {
    let mut host = EndingHost {
        inner: TestHost::default(),
        polls: Default::default(),
        n: 100,
    };
    assert_eq!(run(&options("exit 0", true), &mut host), 0);
    assert_eq!(host.inner.count(EventKind::Start), 1, "never restarted");
    assert!(
        host.polls.get() > 100,
        "kept running until the session ended"
    );
}

#[test]
fn session_end_stops_restarting() {
    let mut host = EndingHost {
        inner: TestHost::default(),
        polls: Default::default(),
        n: 0,
    };
    assert_eq!(run(&options("exit 1", true), &mut host), 0);
    assert_eq!(host.inner.count(EventKind::Start), 0);
}

#[test]
fn crash_is_restarted_until_clean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("runs");
    let script = counter_script(&f, "if [ \"$n\" -lt 3 ]; then exit 1; fi; exit 0");
    let mut host = TestHost::default();
    assert_eq!(run(&options(&script, false), &mut host), 0);
    assert_eq!(host.count(EventKind::Start), 3);
    assert_eq!(host.count(EventKind::Crash), 2);
    assert!(host.fallbacks.is_empty());
}

#[test]
fn shell_boot_loop_falls_back_after_three_failures() {
    let mut host = TestHost::default();
    assert_eq!(run(&options("exit 3", true), &mut host), 0);
    assert_eq!(host.count(EventKind::Start), 3);
    assert_eq!(host.fallbacks, vec![FallbackReason::SupervisorBootLoop]);
}

#[test]
fn component_crash_loop_code_falls_back() {
    let mut host = TestHost::default();
    let code = explorer_core::guardian::EXIT_FALLBACK;
    assert_eq!(run(&options(&format!("exit {code}"), true), &mut host), 0);
    assert_eq!(host.fallbacks, vec![FallbackReason::ComponentCrashLoop]);
}

#[test]
fn dev_mode_gives_up_without_fallback() {
    let mut host = TestHost::default();
    assert_eq!(run(&options("exit 1", false), &mut host), 0);
    // crash_loop_max = 5: the 6th crash within the window gives up.
    assert_eq!(host.count(EventKind::Start), 6);
    assert!(host.fallbacks.is_empty());
}

#[test]
fn environment_reaches_the_child() {
    let mut host = TestHost::default();
    let script = "[ \"$EXPLORER_RS_TEST\" = 1 ] && exit 0; exit 1";
    assert_eq!(run(&options(script, false), &mut host), 0);
    assert_eq!(host.count(EventKind::Crash), 0);
}

#[test]
fn missing_program_counts_as_failure() {
    let mut host = TestHost::default();
    let mut o = options("exit 0", true);
    o.program = "/nonexistent/explorer-rs-test-binary".into();
    assert_eq!(run(&o, &mut host), 0);
    assert_eq!(host.fallbacks, vec![FallbackReason::SupervisorBootLoop]);
}

#[test]
fn restart_command_kills_and_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("runs");
    // First run sleeps (killed by the restart command); second exits 0.
    let script = counter_script(&f, "if [ \"$n\" -eq 1 ]; then sleep 30; fi; exit 0");
    // Issue the command on the 20th poll, while the first child sleeps.
    struct Delayed {
        inner: TestHost,
        polls: u32,
    }
    impl Host for Delayed {
        fn poll_command(&mut self) -> Option<HostCommand> {
            self.polls += 1;
            (self.polls == 20).then_some(HostCommand::RestartAll)
        }
        fn session_ending(&self) -> bool {
            false
        }
        fn fallback(&mut self, r: FallbackReason) -> AfterFallback {
            self.inner.fallback(r)
        }
        fn record(&mut self, ev: Event) {
            self.inner.record(ev)
        }
        fn healthy(&mut self) {}
    }
    let mut d = Delayed {
        inner: TestHost::default(),
        polls: 0,
    };
    let started = std::time::Instant::now();
    assert_eq!(run(&options(&script, false), &mut d), 0);
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(d.inner.count(EventKind::Start), 2);
    assert_eq!(d.inner.count(EventKind::Crash), 0);
}

#[test]
fn shell_guardian_starts_dev_mode_while_explorer_shell_runs() {
    let mut host = EndingHost {
        inner: TestHost {
            explorer: true,
            ..Default::default()
        },
        polls: Default::default(),
        n: 60,
    };
    let mut o = options("exit 3", true);
    // Dev mode exits cleanly; the shell arguments would boot-loop.
    o.dev_args = vec!["-c".into(), "exit 0".into()];
    assert_eq!(run(&o, &mut host), 0);
    assert_eq!(host.inner.count(EventKind::Start), 1);
    assert!(host.inner.fallbacks.is_empty());
}

/// Issues `cmd` on the 20th poll.
struct CommandAt {
    inner: TestHost,
    polls: u32,
    cmd: HostCommand,
}

impl Host for CommandAt {
    fn poll_command(&mut self) -> Option<HostCommand> {
        self.polls += 1;
        (self.polls == 20).then_some(self.cmd)
    }
    fn session_ending(&self) -> bool {
        false
    }
    fn fallback(&mut self, r: FallbackReason) -> AfterFallback {
        self.inner.fallback(r)
    }
    fn record(&mut self, ev: Event) {
        self.inner.record(ev)
    }
    fn healthy(&mut self) {}
    fn request_stop(&mut self, pid: u32) -> bool {
        self.inner.request_stop(pid)
    }
    fn stopped(&mut self) {
        self.inner.stopped()
    }
}

#[test]
fn shutdown_command_stops_the_supervisor_gracefully() {
    let mut h = CommandAt {
        inner: TestHost {
            stop_works: true,
            ..Default::default()
        },
        polls: 0,
        cmd: HostCommand::Shutdown,
    };
    let started = std::time::Instant::now();
    // `exec` so the pid the guardian sees is the sleeping process.
    assert_eq!(run(&options("exec sleep 30", true), &mut h), 0);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(h.inner.stop_requests.len(), 1);
    assert!(h.inner.stopped, "clean end recorded");
    assert_eq!(h.inner.count(EventKind::Start), 1);
}

#[test]
fn shutdown_command_kills_a_supervisor_that_does_not_stop() {
    let mut h = CommandAt {
        inner: TestHost::default(),
        polls: 0,
        cmd: HostCommand::Shutdown,
    };
    let started = std::time::Instant::now();
    assert_eq!(run(&options("exec sleep 30", false), &mut h), 0);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(h.inner.stopped);
}
