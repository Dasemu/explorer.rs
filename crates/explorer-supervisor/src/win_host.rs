// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows side of the guardian: crash log persistence, emergency hotkeys,
//! graceful shutdown, session end, recovery dialogs, `--restore-explorer`
//! and `--status` data. No COM is used here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use explorer_core::config::Config;
use explorer_core::crashlog::{unix_now, CrashLogFile, CrashState, Event, FallbackDecision};
use explorer_core::guardian::FallbackReason;
use explorer_core::i18n::{tr, trf, Lang};
use explorer_supervisor::guardian::{AfterFallback, Host, HostCommand};
use explorer_supervisor::status::StatusInfo;
use explorer_win::session::{NamedEvent, SysEvent, SHELL_DUTIES_EVENT};
use explorer_win::shellreg::Hive;
use explorer_win::{dialog, process};

const CAPTION: &str = "explorer.rs";

struct Shared {
    log: Mutex<CrashLogFile>,
    ending: AtomicBool,
    commands: Mutex<Vec<HostCommand>>,
    shell: bool,
}

impl Shared {
    /// Update the crash log (re-read first if another process wrote it).
    fn with_state(&self, f: impl FnOnce(&mut CrashState)) {
        if let Ok(mut log) = self.log.lock() {
            if let Err(e) = log.update(f) {
                tracing::warn!(error = %e, "cannot write crash log");
            }
        }
    }

    fn push(&self, cmd: HostCommand) {
        if let Ok(mut c) = self.commands.lock() {
            c.push(cmd);
        }
    }
}

pub struct WinHost {
    shared: Arc<Shared>,
    lang: Lang,
    /// Held for the whole sign-in so the supervisor's "shell duties done"
    /// marker survives supervisor restarts.
    _duties_event: Option<NamedEvent>,
}

impl WinHost {
    /// `resume`: restarted by setup after an update; the shell duties
    /// (startup apps, ready events) already ran in this sign-in.
    pub fn new(log: CrashLogFile, shell: bool, resume: bool, config: &Config) -> Self {
        let shared = Arc::new(Shared {
            log: Mutex::new(log),
            ending: AtomicBool::new(false),
            commands: Mutex::new(Vec::new()),
            shell,
        });
        let s = shared.clone();
        let sink: explorer_win::session::Sink = Arc::new(move |ev| match ev {
            SysEvent::EmergencyExplorer => {
                tracing::warn!("emergency hotkey: starting explorer.exe");
                let _ = process::start_explorer();
            }
            SysEvent::RestartAll => s.push(HostCommand::RestartAll),
            SysEvent::Shutdown => s.push(HostCommand::Shutdown),
            SysEvent::Restored => {
                tracing::warn!("Windows Explorer was restored as the shell (--restore-explorer)");
                s.with_state(|st| st.on_restored(unix_now(), false));
            }
            SysEvent::QueryEndSession => {}
            SysEvent::SessionEnding => {
                s.ending.store(true, Ordering::SeqCst);
                if s.shell {
                    s.with_state(|st| st.mark_session_end(unix_now()));
                }
            }
        });
        if !explorer_win::session::spawn_listener(true, sink) {
            tracing::error!("could not start the hotkey/session thread");
        }
        let duties = if shell {
            NamedEvent::create(SHELL_DUTIES_EVENT)
        } else {
            None
        };
        if resume {
            if let Some(e) = &duties {
                // Mark the duties as done so no supervisor re-runs them.
                e.set();
            }
        }
        Self {
            shared,
            lang: explorer_win::theme::ui_lang(config.general.language),
            _duties_event: duties,
        }
    }
}

fn hotkey_help(lang: Lang) -> &'static str {
    tr(
        lang,
        "Emergency keys: Ctrl+Alt+Shift+E starts Windows Explorer; Ctrl+Alt+Shift+R restarts explorer.rs.",
        "Teclas de emergencia: Ctrl+Alt+Mayús+E abre el Explorador de Windows; Ctrl+Alt+Mayús+R reinicia explorer.rs.",
    )
}

fn restore_shell() -> bool {
    match Hive::CurrentUser.restore_shell() {
        Ok(changed) => {
            tracing::warn!(changed, "Windows Explorer restored as the shell");
            true
        }
        Err(e) => {
            tracing::error!(error = %e, "restoring the shell failed");
            false
        }
    }
}

fn auto_restore_text(lang: Lang, ok: bool) -> &'static str {
    if ok {
        tr(
            lang,
            "explorer.rs did not start correctly in 3 sign-ins in a row, so Windows Explorer has been restored as your desktop permanently.\n\nTo use explorer.rs again, register it again with the installer. The logs are in %LOCALAPPDATA%\\explorer-rs\\logs.",
            "explorer.rs no se inició correctamente en 3 inicios de sesión seguidos, así que se ha restaurado el Explorador de Windows como escritorio de forma permanente.\n\nPara volver a usar explorer.rs, regístralo de nuevo con el instalador. Los registros están en %LOCALAPPDATA%\\explorer-rs\\logs.",
        )
    } else {
        tr(
            lang,
            "explorer.rs did not start correctly in 3 sign-ins in a row, but Windows Explorer could not be restored automatically. Please run \"Restore Windows Explorer\" from the Start menu, or see docs\\RECOVERY.md.",
            "explorer.rs no se inició correctamente en 3 inicios de sesión seguidos, pero no se pudo restaurar el Explorador de Windows automáticamente. Ejecuta \"Restaurar el Explorador de Windows\" desde el menú Inicio o consulta docs\\RECOVERY.md.",
        )
    }
}

/// Too many failed sign-ins: restore Windows Explorer permanently, start
/// it and tell the user. `record` stores the restore in the crash log.
fn auto_restore_with(lang: Lang, record: impl FnOnce()) {
    tracing::error!(
        "explorer.rs failed in too many consecutive sign-ins; restoring Windows Explorer"
    );
    let ok = restore_shell();
    if ok {
        record();
    }
    if !process::explorer_shell_running() {
        let _ = process::start_explorer();
    }
    dialog::info(CAPTION, auto_restore_text(lang, ok));
}

/// [`auto_restore_with`] at sign-in, before any guardian loop runs.
pub fn auto_restore(lang: Lang, log: &mut CrashLogFile) {
    auto_restore_with(lang, || {
        if let Err(e) = log.update(|st| st.on_restored(unix_now(), true)) {
            tracing::warn!(error = %e, "cannot write crash log");
        }
    });
}

impl Host for WinHost {
    fn poll_command(&mut self) -> Option<HostCommand> {
        self.shared.commands.lock().ok().and_then(|mut c| c.pop())
    }

    fn session_ending(&self) -> bool {
        self.shared.ending.load(Ordering::SeqCst)
    }

    fn fallback(&mut self, reason: FallbackReason) -> AfterFallback {
        let lang = self.lang;
        if !self.shared.shell {
            return AfterFallback::Exit;
        }
        let mut decision = FallbackDecision::AutoRestore;
        self.shared
            .with_state(|st| decision = st.on_fallback(unix_now(), reason.as_str()));
        match decision {
            FallbackDecision::AutoRestore => {
                // The lock is only taken inside `record`, never across the
                // modal dialog (the session-end handler must not wait).
                let shared = self.shared.clone();
                auto_restore_with(lang, move || {
                    shared.with_state(|st| st.on_restored(unix_now(), true))
                });
                AfterFallback::Exit
            }
            FallbackDecision::Ask {
                failures_including_this: n,
            } => {
                let text = trf(
                    lang,
                    format!(
                        "explorer.rs stopped working repeatedly ({}). Windows Explorer will be started for this session.\n\n\
                         Yes: restore Windows Explorer permanently (explorer.rs will no longer start when you sign in).\n\n\
                         No: keep explorer.rs and try again next time you sign in. Failed sign-ins in a row: {n} of 3; after 3, Windows Explorer is restored automatically.\n\n{}",
                        reason.as_str(),
                        hotkey_help(lang)
                    ),
                    format!(
                        "explorer.rs ha dejado de funcionar repetidamente ({}). Se abrirá el Explorador de Windows para esta sesión.\n\n\
                         Sí: restaurar el Explorador de Windows de forma permanente (explorer.rs ya no se iniciará al iniciar sesión).\n\n\
                         No: mantener explorer.rs y volver a intentarlo en el próximo inicio de sesión. Inicios fallidos seguidos: {n} de 3; tras 3 se restaura el Explorador de Windows automáticamente.\n\n{}",
                        reason.as_str(),
                        hotkey_help(lang)
                    ),
                );
                // Ask first: with the registration restored, explorer.exe
                // starts as the full shell (taskbar and desktop).
                let restore = dialog::ask_yes_no(CAPTION, &text, true);
                if restore && restore_shell() {
                    self.shared
                        .with_state(|st| st.on_restored(unix_now(), false));
                }
                if !process::explorer_shell_running() {
                    let _ = process::start_explorer();
                }
                if restore {
                    AfterFallback::Exit
                } else {
                    AfterFallback::Idle
                }
            }
        }
    }

    fn record(&mut self, ev: Event) {
        self.shared.with_state(|st| st.record(ev));
    }

    fn healthy(&mut self) {
        if self.shared.shell {
            self.shared.with_state(|st| {
                st.mark_healthy(unix_now());
            });
        }
    }

    fn explorer_shell_running(&self) -> bool {
        process::explorer_shell_running()
    }

    fn request_stop(&mut self, pid: u32) -> bool {
        process::post_shutdown(pid) > 0
    }

    fn stopped(&mut self) {
        self.shared
            .with_state(|st| st.mark_clean_stop(unix_now(), "graceful shutdown"));
    }
}

/// `--restore-explorer`: restore the per-user shell; if explorer.rs is the
/// running shell, stop it (guardian first, gracefully) and start Windows
/// Explorer.
///
/// Order: Windows Explorer is started only after explorer.rs's desktop and
/// taskbar are gone, because explorer.exe opens a plain folder window
/// instead of becoming the shell while another program owns the shell
/// window. To never leave the user with an empty screen, explorer.exe is
/// checked to exist before anything is stopped, and if it cannot be
/// started afterwards explorer.rs is started again.
pub fn restore_explorer_command(console: bool) -> i32 {
    let paths = explorer_core::paths::AppPaths::from_env();
    let (config, _) = Config::load_or_default(&paths.config_file());
    let lang = explorer_win::theme::ui_lang(config.general.language);
    let _log = explorer_core::logging::init(
        "restore",
        &paths.log_dir,
        &config.general.log_level,
        config.general.log_keep_files,
    );
    let result = Hive::CurrentUser.restore_shell();
    let guardian_pid = process::guardian_window()
        .map(process::window_pid)
        .filter(|&p| p != 0);
    if result.is_ok() {
        record_restore(&paths, guardian_pid.is_some());
    }

    let mut start_failed = false;
    if !process::explorer_shell_running() {
        // explorer.rs is the shell: stop it, then start Windows Explorer.
        if !process::explorer_path().exists() {
            tracing::error!(path = %process::explorer_path().display(), "explorer.exe not found; explorer.rs keeps running");
            start_failed = true;
        } else {
            stop_explorer_rs(guardian_pid);
            if process::start_explorer().is_err() {
                start_failed = true;
                // Never leave an empty screen: bring explorer.rs back.
                tracing::error!("starting explorer.exe failed; restarting explorer.rs");
                if let Ok(me) = std::env::current_exe() {
                    let _ = std::process::Command::new(me)
                        .args(["--shell", "--resume"])
                        .spawn();
                }
            }
        }
    }
    let (code, text) = match result {
        _ if start_failed => (
            1,
            tr(
                lang,
                "Windows Explorer could not be started. See docs\\RECOVERY.md for manual steps (Ctrl+Shift+Esc opens Task Manager: File > Run new task > explorer.exe).",
                "No se pudo iniciar el Explorador de Windows. Consulta docs\\RECOVERY.md para los pasos manuales (Ctrl+Mayús+Esc abre el Administrador de tareas: Archivo > Ejecutar nueva tarea > explorer.exe).",
            ),
        ),
        Ok(true) => (
            0,
            tr(
                lang,
                "Windows Explorer has been restored as your shell. explorer.rs will not start at your next sign-in.",
                "Se ha restaurado el Explorador de Windows como shell. explorer.rs no se iniciará en el próximo inicio de sesión.",
            ),
        ),
        Ok(false) => (
            0,
            tr(
                lang,
                "explorer.rs was not registered as your shell; nothing to restore.",
                "explorer.rs no estaba registrado como shell; no hay nada que restaurar.",
            ),
        ),
        Err(e) => {
            tracing::error!(error = %e, "restore failed");
            (
                1,
                tr(
                    lang,
                    "Restoring Windows Explorer failed. See docs\\RECOVERY.md for manual steps.",
                    "No se pudo restaurar el Explorador de Windows. Consulta docs\\RECOVERY.md para los pasos manuales.",
                ),
            )
        }
    };
    if console {
        println!("{text}");
    } else {
        dialog::info(CAPTION, text);
    }
    code
}

/// Record the restore in the crash log. While a guardian runs it is the
/// only writer, so it is asked to record it itself.
fn record_restore(paths: &explorer_core::paths::AppPaths, guardian: bool) {
    if guardian && process::post_to_guardian(explorer_core::ipc::MSG_RESTORED) {
        tracing::info!("asked the guardian to record the restore");
        return;
    }
    let (mut log, _) = CrashLogFile::open(explorer_core::crashlog::crashlog_file(paths));
    if let Err(e) = log.update(|st| st.on_restored(unix_now(), false)) {
        tracing::warn!(error = %e, "cannot write crash log");
    }
}

/// Stop explorer.rs in this session: ask the guardian (identified by its
/// hotkey window) to stop gracefully, then stop what is left (an orphaned
/// supervisor, components) gracefully and finally terminate leftovers, the
/// guardian first so nothing is restarted.
fn stop_explorer_rs(guardian_pid: Option<u32>) {
    if let Some(pid) = guardian_pid {
        tracing::info!(pid, "asking the guardian to stop");
        if process::post_to_guardian(explorer_core::ipc::MSG_SHUTDOWN)
            && process::wait_exit(pid, Duration::from_secs(12))
        {
            tracing::info!("guardian stopped");
        } else {
            tracing::warn!(pid, "guardian did not stop; terminating it");
            process::terminate(pid);
            let _ = process::wait_exit(pid, Duration::from_secs(2));
        }
    }
    // SAFETY: plain query.
    let me = unsafe { explorer_win::windows::Win32::System::Threading::GetCurrentProcessId() };
    let ours = || -> Vec<process::ProcessInfo> {
        process::session_processes()
            .into_iter()
            .filter(|p| p.pid != me && explorer_core::guardian::is_shell_process(&p.exe))
            .collect()
    };
    let left = ours();
    if left.is_empty() {
        return;
    }
    let pids: Vec<u32> = left.iter().map(|p| p.pid).collect();
    process::post_shutdown_all(&pids);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !ours().is_empty() {
        std::thread::sleep(Duration::from_millis(100));
    }
    // Supervisors (explorer-rs.exe) before components so nothing restarts.
    let mut left = ours();
    left.sort_by_key(|p| !p.exe.eq_ignore_ascii_case("explorer-rs.exe"));
    for p in left {
        tracing::info!(pid = p.pid, exe = %p.exe, "terminating");
        process::terminate(p.pid);
    }
}

/// Live data for `--status`.
pub fn fill_status(info: &mut StatusInfo) {
    use explorer_win::session::mutex_exists;
    info.guardian_running = mutex_exists(crate::cli::GUARDIAN_MUTEX);
    info.supervisor_running = mutex_exists(crate::cli::SUPERVISOR_MUTEX);
    let procs = process::session_processes();
    info.explorer_running = process::explorer_shell_running();
    for (name, running) in info.components.iter_mut() {
        let exe = format!("explorer-rs-{name}.exe");
        *running = procs.iter().any(|p| p.exe.eq_ignore_ascii_case(&exe));
    }
    info.shell_value = Hive::CurrentUser.shell();
}
