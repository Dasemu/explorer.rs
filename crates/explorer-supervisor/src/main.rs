// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs`: the supervisor process.
//!
//! Starts each component (taskbar, launcher) as its own process and restarts
//! it independently according to `explorer_core::supervisor`. A crash in one
//! component never affects the others. The supervisor itself is kept small
//! and free of UI code so that it has as little reason to crash as possible.
//!
//! Safety rule: without an explicit `--dev` or `--shell` flag the program
//! only prints help. Registering as the Windows shell is never done here;
//! see scripts/install-shell.ps1.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use clap::Parser;
use explorer_core::config::Config;
use explorer_core::ipc::{RunMode, MODE_ENV};
use explorer_core::paths::AppPaths;
use explorer_core::supervisor::{Decision, RestartTracker};

#[derive(Parser, Debug)]
#[command(
    name = "explorer-rs",
    version,
    about = "explorer.rs: native Windows shell replacement (supervisor)",
    after_help = "Use --dev to try explorer.rs alongside explorer.exe.\n\
                  --shell is only meant to be used by the per-user shell registration\n\
                  done with scripts/install-shell.ps1. See docs/RECOVERY.md first."
)]
struct Args {
    /// Windowed dev mode: run next to explorer.exe without replacing it.
    #[arg(long, conflicts_with = "shell")]
    dev: bool,
    /// Shell mode: explorer.rs is the user's shell (no explorer.exe).
    #[arg(long)]
    shell: bool,
    /// Use this config file instead of %APPDATA%\explorer-rs\config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Run only one component.
    #[arg(long, value_name = "COMPONENT", value_parser = ["taskbar", "launcher"])]
    only: Option<String>,
}

struct Component {
    name: String,
    exe: PathBuf,
    child: Option<Child>,
    started: Instant,
    next_start: Option<Instant>,
    tracker: RestartTracker,
    done: bool,
}

fn component_exe(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(
        "explorer-rs-{name}{}",
        std::env::consts::EXE_SUFFIX
    ))
}

fn main() {
    #[cfg(windows)]
    explorer_win::console::attach_parent();

    let args = Args::parse();
    let mode = match (args.dev, args.shell) {
        (true, _) => RunMode::Dev,
        (_, true) => RunMode::Shell,
        _ => {
            use clap::CommandFactory;
            let _ = Args::command().print_help();
            eprintln!("\nRefusing to start without --dev or --shell.");
            std::process::exit(2);
        }
    };

    let paths = AppPaths::from_env();
    let _ = paths.ensure();
    let config_path = args.config.clone().unwrap_or_else(|| paths.config_file());
    let (config, config_err) = Config::load_or_default(&config_path);
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
    let _instance = match explorer_win::single_instance::acquire("ExplorerRs.Supervisor") {
        Some(g) => g,
        None => {
            tracing::warn!("another supervisor is already running; exiting");
            return;
        }
    };

    let code = run(mode, &config, args.config.as_deref(), args.only.as_deref());
    tracing::info!(code, "supervisor exiting");
    drop(_log);
    std::process::exit(code);
}

fn run(mode: RunMode, config: &Config, config_override: Option<&Path>, only: Option<&str>) -> i32 {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));

    #[cfg(windows)]
    let job = match explorer_win::job::KillOnCloseJob::new() {
        Ok(j) => Some(j),
        Err(e) => {
            tracing::warn!(error = %e, "could not create job object; children may outlive the supervisor");
            None
        }
    };

    let names: Vec<String> = match only {
        Some(n) => vec![n.to_string()],
        None => config.supervisor.components.clone(),
    };
    let mut comps: Vec<Component> = names
        .into_iter()
        .map(|name| Component {
            exe: component_exe(&exe_dir, &name),
            name,
            child: None,
            started: Instant::now(),
            next_start: Some(Instant::now()),
            tracker: RestartTracker::new(&config.supervisor),
            done: false,
        })
        .collect();
    if comps.is_empty() {
        tracing::error!("no components configured");
        return 1;
    }

    let epoch = Instant::now();
    loop {
        let now = Instant::now();
        for c in comps.iter_mut() {
            if let Some(child) = c.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        c.child = None;
                        let uptime = now.duration_since(c.started).as_millis() as u64;
                        let decision = c.tracker.on_exit(
                            status.code(),
                            uptime,
                            now.duration_since(epoch).as_millis() as u64,
                        );
                        tracing::info!(component = %c.name, ?status, uptime_ms = uptime, ?decision, "component exited");
                        match decision {
                            Decision::RestartAfter(ms) => {
                                c.next_start = Some(now + Duration::from_millis(ms))
                            }
                            Decision::Stop => c.done = true,
                            Decision::GiveUp => {
                                c.done = true;
                                tracing::error!(component = %c.name, "crash loop detected; giving up on this component");
                                if mode == RunMode::Shell && c.name == "taskbar" {
                                    start_fallback_explorer();
                                }
                            }
                            Decision::ShutdownAll => {
                                tracing::info!(component = %c.name, "shutdown requested");
                                shutdown(&mut comps);
                                return 0;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(component = %c.name, error = %e, "try_wait failed"),
                }
            }
            if c.child.is_none() && !c.done && c.next_start.is_some_and(|t| now >= t) {
                c.next_start = None;
                match spawn(&c.exe, mode, config_override) {
                    Ok(child) => {
                        #[cfg(windows)]
                        if let Some(j) = &job {
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
                        // Treat a failed spawn like a crash so backoff applies.
                        let ms = now.duration_since(epoch).as_millis() as u64;
                        match c.tracker.on_exit(Some(1), 0, ms) {
                            Decision::RestartAfter(d) => {
                                c.next_start = Some(now + Duration::from_millis(d))
                            }
                            _ => {
                                c.done = true;
                                if mode == RunMode::Shell && c.name == "taskbar" {
                                    start_fallback_explorer();
                                }
                            }
                        }
                    }
                }
            }
        }
        if comps.iter().all(|c| c.done) {
            tracing::info!("all components stopped");
            return 0;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn spawn(exe: &Path, mode: RunMode, config_override: Option<&Path>) -> std::io::Result<Child> {
    let mut cmd = Command::new(exe);
    cmd.env(MODE_ENV, mode.as_str()).stdin(Stdio::null());
    if let Some(p) = config_override {
        cmd.arg("--config").arg(p);
    }
    cmd.spawn()
}

fn shutdown(comps: &mut [Component]) {
    for c in comps.iter_mut() {
        if let Some(mut child) = c.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        c.done = true;
    }
}

/// Last-resort UI in shell mode: start explorer.exe so the user is never
/// left with an empty screen. Because explorer.exe is not the registered
/// shell in this situation it typically opens a File Explorer window rather
/// than a full desktop (unverified), which is still enough to recover.
fn start_fallback_explorer() {
    tracing::warn!("starting explorer.exe as a fallback");
    if let Err(e) = Command::new("explorer.exe").spawn() {
        tracing::error!(error = %e, "could not start explorer.exe fallback");
    }
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
    fn requires_explicit_mode_flag() {
        let a = Args::try_parse_from(["explorer-rs"]).unwrap();
        assert!(!a.dev && !a.shell);
        assert!(Args::try_parse_from(["explorer-rs", "--dev", "--shell"]).is_err());
        assert!(Args::try_parse_from(["explorer-rs", "--dev", "--only", "tray"]).is_err());
    }
}
