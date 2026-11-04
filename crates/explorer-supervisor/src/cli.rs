// SPDX-License-Identifier: GPL-3.0-or-later
//! Guardian entry point and the one-shot commands (`--restore-explorer`,
//! `--edit-config`, `--status`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use explorer_core::config::Config;
use explorer_core::crashlog::{crashlog_file, unix_now, CrashState, Event, EventKind};
#[cfg(not(windows))]
use explorer_core::guardian::FallbackReason;
use explorer_core::guardian::{GUARDED_ENV, SESSION_START_ENV};
use explorer_core::ipc::RunMode;
use explorer_core::paths::AppPaths;
use explorer_supervisor::guardian::{self, Options};
#[cfg(not(windows))]
use explorer_supervisor::guardian::{AfterFallback, Host, HostCommand};
use explorer_supervisor::status::{render, StatusInfo};

use crate::Args;

/// Name of the guardian's single-instance mutex.
#[cfg(windows)]
pub const GUARDIAN_MUTEX: &str = "ExplorerRs.Guardian";
/// Name of the supervisor's single-instance mutex.
#[cfg(windows)]
pub const SUPERVISOR_MUTEX: &str = "ExplorerRs.Supervisor";

/// Command line of the supervisor child.
pub fn supervisor_args(mode: RunMode, args: &Args) -> Vec<OsString> {
    let mut v: Vec<OsString> = vec!["--supervise".into(), format!("--{}", mode.as_str()).into()];
    if let Some(c) = &args.config {
        v.push("--config".into());
        v.push(c.clone().into_os_string());
    }
    if let Some(o) = &args.only {
        v.push("--only".into());
        v.push(o.into());
    }
    v
}

fn config_path(paths: &AppPaths, over: Option<&Path>) -> PathBuf {
    over.map(Path::to_path_buf)
        .unwrap_or_else(|| paths.config_file())
}

/// `explorer-rs --dev|--shell`.
pub fn guardian_main(mode: RunMode, args: &Args) -> i32 {
    let paths = AppPaths::from_env();
    let _ = paths.ensure();
    let cfg_path = config_path(&paths, args.config.as_deref());
    let (config, config_err) = Config::load_or_default(&cfg_path);
    let _log = explorer_core::logging::init(
        "guardian",
        &paths.log_dir,
        &config.general.log_level,
        config.general.log_keep_files,
    );
    if let Some(e) = config_err {
        tracing::error!(path = %cfg_path.display(), error = %e, "config error, using defaults");
    }
    tracing::info!(mode = mode.as_str(), "guardian starting");

    #[cfg(windows)]
    let _instance = match explorer_win::single_instance::acquire(GUARDIAN_MUTEX) {
        Some(g) => g,
        None => {
            tracing::warn!("another guardian is already running; exiting");
            return 0;
        }
    };

    let log_path = crashlog_file(&paths);
    let (mut state, err) = CrashState::load(&log_path);
    if let Some(e) = err {
        tracing::warn!(error = %e, "crash log unreadable; starting a new one");
    }
    let shell = mode == RunMode::Shell;
    if shell {
        let decision = state.begin_shell_session(unix_now());
        tracing::info!(?decision, "shell sign-in");
        if let Err(e) = state.save(&log_path) {
            tracing::warn!(error = %e, "cannot write crash log");
        }
        if decision == explorer_core::crashlog::SigninDecision::AutoRestore {
            #[cfg(windows)]
            {
                win_auto_restore(&config, &mut state, &log_path);
                return 0;
            }
        }
    } else {
        state
            .record(Event::new(unix_now(), EventKind::SessionStart, "guardian").with_detail("dev"));
        let _ = state.save(&log_path);
    }

    let program = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("explorer-rs"));
    let session_start_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let opts = Options {
        program,
        args: supervisor_args(mode, args),
        envs: vec![
            (GUARDED_ENV.into(), "1".into()),
            (SESSION_START_ENV.into(), session_start_ms.to_string()),
        ],
        shell,
        supervisor: config.supervisor.clone(),
        poll: Duration::from_millis(200),
        healthy_after: Duration::from_secs(explorer_core::crashlog::HEALTHY_SESSION_S),
    };

    #[cfg(windows)]
    let code = {
        let mut host = crate::win_host::WinHost::new(state, log_path, shell, &config);
        guardian::run(&opts, &mut host)
    };
    #[cfg(not(windows))]
    let code = {
        let mut host = FileHost {
            state,
            path: log_path,
        };
        guardian::run(&opts, &mut host)
    };
    tracing::info!(code, "guardian exiting");
    code
}

#[cfg(windows)]
fn win_auto_restore(config: &Config, state: &mut CrashState, log_path: &Path) {
    let lang = explorer_win::theme::ui_lang(config.general.language);
    crate::win_host::auto_restore(lang, state, log_path);
}

/// Non-Windows host: crash log only (development and tests).
#[cfg(not(windows))]
struct FileHost {
    state: CrashState,
    path: PathBuf,
}

#[cfg(not(windows))]
impl Host for FileHost {
    fn poll_command(&mut self) -> Option<HostCommand> {
        None
    }
    fn session_ending(&self) -> bool {
        false
    }
    fn fallback(&mut self, reason: FallbackReason) -> AfterFallback {
        self.state.on_fallback(unix_now(), reason.as_str());
        let _ = self.state.save(&self.path);
        tracing::error!("fallback to explorer.exe is only available on Windows");
        AfterFallback::Exit
    }
    fn record(&mut self, ev: Event) {
        self.state.record(ev);
        let _ = self.state.save(&self.path);
    }
    fn healthy(&mut self) {
        self.state.mark_healthy(unix_now());
        let _ = self.state.save(&self.path);
    }
}

/// `--restore-explorer`.
pub fn restore_explorer(console: bool) -> i32 {
    #[cfg(windows)]
    {
        crate::win_host::restore_explorer_command(console)
    }
    #[cfg(not(windows))]
    {
        let _ = console;
        eprintln!("--restore-explorer only works on Windows");
        1
    }
}

/// `--restart`: tell the running guardian to restart every component.
pub fn restart() -> i32 {
    #[cfg(windows)]
    {
        if explorer_win::session::request_restart_all() {
            0
        } else {
            eprintln!("explorer.rs (guardian) is not running");
            1
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("--restart only works on Windows");
        1
    }
}

/// `--edit-config`.
pub fn edit_config(over: Option<&Path>) -> i32 {
    let paths = AppPaths::from_env();
    let path = config_path(&paths, over);
    match explorer_supervisor::ensure_config_file(&path) {
        Ok(created) => {
            if created {
                println!("created {}", path.display());
            }
        }
        Err(e) => {
            eprintln!("cannot create {}: {e}", path.display());
            return 1;
        }
    }
    #[cfg(windows)]
    {
        match std::process::Command::new("notepad.exe").arg(&path).spawn() {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("cannot start notepad.exe: {e}");
                1
            }
        }
    }
    #[cfg(not(windows))]
    {
        println!("{}", path.display());
        0
    }
}

/// `--status [--output FILE]`.
pub fn status(over: Option<&Path>, output: Option<&Path>) -> i32 {
    let paths = AppPaths::from_env();
    let cfg_path = config_path(&paths, over);
    let text = std::fs::read_to_string(&cfg_path).ok();
    let config_error = text
        .as_deref()
        .and_then(|t| Config::from_toml(t).err())
        .map(|e| e.to_string());
    let config = text
        .as_deref()
        .and_then(|t| Config::from_toml(t).ok())
        .unwrap_or_default();
    let log_path = crashlog_file(&paths);
    let (state, _) = CrashState::load(&log_path);
    let explicit = text
        .as_deref()
        .is_some_and(explorer_core::components::components_explicit);
    let names = explorer_core::components::resolve(
        &config.supervisor,
        explicit,
        crate::supervise::installer_file_search(),
    );
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut info = StatusInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        config_path: cfg_path.display().to_string(),
        config_exists: text.is_some(),
        config_error,
        crashlog_path: log_path.display().to_string(),
        failed_signins: state.failed_signins,
        auto_restored_at: state
            .auto_restored_at
            .map(explorer_core::crashlog::format_utc),
        crash_lines: state.last_lines(15),
        components: names.iter().map(|n| (n.clone(), false)).collect(),
        ..Default::default()
    };
    #[cfg(windows)]
    crate::win_host::fill_status(&mut info);
    let report = render(&info);
    match output {
        Some(p) => match std::fs::write(p, &report) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("cannot write {}: {e}", p.display());
                1
            }
        },
        None => {
            print!("{report}");
            0
        }
    }
}
