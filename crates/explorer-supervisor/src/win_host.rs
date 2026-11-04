// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows side of the guardian: crash log persistence, emergency hotkeys,
//! session end, recovery dialogs, `--restore-explorer` and `--status` data.
//! No COM is used here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use explorer_core::config::Config;
use explorer_core::crashlog::{unix_now, CrashState, Event, FallbackDecision};
use explorer_core::guardian::FallbackReason;
use explorer_core::i18n::{tr, trf, Lang};
use explorer_supervisor::guardian::{AfterFallback, Host, HostCommand};
use explorer_supervisor::status::StatusInfo;
use explorer_win::session::{NamedEvent, SysEvent, SHELL_DUTIES_EVENT};
use explorer_win::shellreg::Hive;
use explorer_win::{dialog, process};

const CAPTION: &str = "explorer.rs";

struct Shared {
    state: Mutex<CrashState>,
    path: PathBuf,
    ending: AtomicBool,
    commands: Mutex<Vec<HostCommand>>,
    shell: bool,
}

impl Shared {
    fn with_state(&self, f: impl FnOnce(&mut CrashState)) {
        if let Ok(mut st) = self.state.lock() {
            f(&mut st);
            if let Err(e) = st.save(&self.path) {
                tracing::warn!(error = %e, "cannot write crash log");
            }
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
    pub fn new(state: CrashState, path: PathBuf, shell: bool, config: &Config) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            path,
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
            SysEvent::RestartAll => {
                if let Ok(mut c) = s.commands.lock() {
                    c.push(HostCommand::RestartAll);
                }
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
        Self {
            shared,
            lang: explorer_win::theme::ui_lang(config.general.language),
            _duties_event: if shell {
                NamedEvent::create(SHELL_DUTIES_EVENT)
            } else {
                None
            },
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

/// Too many failed sign-ins: restore Windows Explorer permanently, start
/// it and tell the user.
pub fn auto_restore(lang: Lang, state: &mut CrashState, path: &Path) {
    tracing::error!(
        "explorer.rs failed in too many consecutive sign-ins; restoring Windows Explorer"
    );
    let ok = restore_shell();
    if ok {
        state.on_restored(unix_now(), true);
    }
    if let Err(e) = state.save(path) {
        tracing::warn!(error = %e, "cannot write crash log");
    }
    if !process::explorer_running() {
        let _ = process::start_explorer();
    }
    let text = if ok {
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
    };
    dialog::info(CAPTION, text);
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
                // Work on a copy: the dialog is modal and the session-end
                // handler must never wait for this lock.
                let copy = self.shared.state.lock().ok().map(|st| st.clone());
                if let Some(mut st) = copy {
                    auto_restore(lang, &mut st, &self.shared.path);
                }
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
                if !process::explorer_running() {
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
}

/// `--restore-explorer`: restore the per-user shell, stop explorer.rs and
/// start explorer.exe if it is not running.
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
    let log_path = explorer_core::crashlog::crashlog_file(&paths);
    let (mut state, _) = CrashState::load(&log_path);
    state.on_restored(unix_now(), false);
    let _ = state.save(&log_path);

    let explorer_was_running = process::explorer_running();
    if !explorer_was_running {
        // Shell mode: stop explorer.rs (guardian first so nothing restarts),
        // then start explorer.exe as the shell.
        // SAFETY: plain query.
        let me = unsafe { explorer_win::windows::Win32::System::Threading::GetCurrentProcessId() };
        let mut ours: Vec<_> = process::session_processes()
            .into_iter()
            .filter(|p| p.pid != me && p.exe.to_ascii_lowercase().starts_with("explorer-rs"))
            .collect();
        ours.sort_by_key(|p| !p.exe.eq_ignore_ascii_case("explorer-rs.exe"));
        for p in ours {
            tracing::info!(pid = p.pid, exe = %p.exe, "stopping");
            process::terminate(p.pid);
        }
        let _ = process::start_explorer();
    }
    let (code, text) = match result {
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

/// Live data for `--status`.
pub fn fill_status(info: &mut StatusInfo) {
    use explorer_win::session::mutex_exists;
    info.guardian_running = mutex_exists(crate::cli::GUARDIAN_MUTEX);
    info.supervisor_running = mutex_exists(crate::cli::SUPERVISOR_MUTEX);
    let procs = process::session_processes();
    info.explorer_running = procs
        .iter()
        .any(|p| p.exe.eq_ignore_ascii_case("explorer.exe"));
    for (name, running) in info.components.iter_mut() {
        let exe = format!("explorer-rs-{name}.exe");
        *running = procs.iter().any(|p| p.exe.eq_ignore_ascii_case(&exe));
    }
    info.shell_value = Hive::CurrentUser.shell();
}
