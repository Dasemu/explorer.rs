// SPDX-License-Identifier: GPL-3.0-or-later
//! Executes a plan (`crate::plan`) step by step. Thin by design: every
//! decision was made by the pure planner; this only performs I/O, logs and
//! reports progress.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use explorer_core::i18n::Lang;
use explorer_core::setup::{MACHINE_KEY, RUN_SUBKEY, RUN_VALUE_NAME};
use explorer_win::shellreg::Hive;
use explorer_win::windows::Win32::UI::Shell::{FOLDERID_CommonPrograms, FOLDERID_ProgramFiles};

use crate::payload::Payload;
use crate::plan::{self, Facts, PreviousInstall, RegValue, StartMode, Step};
use crate::win::reg::{self, Root};
use crate::win::{launch, sys};

/// Progress reported to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    Step {
        index: usize,
        total: usize,
        text: String,
    },
    Warning(String),
}

/// Result of a successful run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Outcome {
    pub reboot_required: bool,
    pub warnings: usize,
}

pub struct Exec<'a> {
    pub facts: Facts,
    pub payload: Option<&'a Payload>,
    pub lang: Lang,
    /// The running setup program (copied as the uninstaller).
    pub self_exe: PathBuf,
    pub report: &'a mut dyn FnMut(Event),
    outcome: Outcome,
}

/// Collect the facts the planner needs.
pub fn gather_facts() -> Result<Facts> {
    let program_files = sys::known_folder(&FOLDERID_ProgramFiles)?;
    let programs = sys::known_folder(&FOLDERID_CommonPrograms)?;
    let install_dir = format!(r"{program_files}\{}", plan::INSTALL_DIR_NAME);
    let user_sid = explorer_win::shellreg::interactive_user_sid()
        .context("cannot determine the signed-in user")?;
    let (mine, other) = plan::split_sessions(&sys::processes(), sys::current_session());
    let me = std::process::id();
    let ours_running = !plan::stop_order(&mine, &install_dir, me).is_empty();
    let foreign = plan::stop_order(&other, &install_dir, me);
    if !foreign.is_empty() {
        tracing::warn!(
            ?foreign,
            "explorer.rs runs in other sessions (other signed-in users); it is not stopped and its files are replaced at the next restart"
        );
    }
    Ok(Facts {
        start_menu_dir: format!(r"{programs}\{}", plan::START_MENU_FOLDER),
        install_dir,
        user_sid,
        explorer_shell_running: explorer_win::process::explorer_shell_running(),
        ours_running,
        ours_other_sessions: foreign.len(),
    })
}

/// What a previous installation recorded, if any.
pub fn previous_install() -> Option<PreviousInstall> {
    use explorer_core::setup::*;
    let version = reg::get_string(Root::Hklm, MACHINE_KEY, VALUE_VERSION);
    let mode =
        reg::get_string(Root::Hklm, MACHINE_KEY, VALUE_MODE).and_then(|m| StartMode::parse(&m));
    let file_search = reg::get_dword(Root::Hklm, MACHINE_KEY, VALUE_FILE_SEARCH).map(|v| v != 0);
    (version.is_some() || mode.is_some() || file_search.is_some()).then_some(PreviousInstall {
        version,
        mode,
        file_search,
    })
}

/// Files recorded by the previous install (empty if none).
pub fn old_manifest(install_dir: &str) -> Vec<String> {
    std::fs::read_to_string(Path::new(install_dir).join(plan::FILE_MANIFEST))
        .map(|t| plan::parse_manifest(&t))
        .unwrap_or_default()
}

impl<'a> Exec<'a> {
    pub fn new(
        facts: Facts,
        payload: Option<&'a Payload>,
        lang: Lang,
        report: &'a mut dyn FnMut(Event),
    ) -> Self {
        Self {
            facts,
            payload,
            lang,
            self_exe: std::env::current_exe().unwrap_or_default(),
            report,
            outcome: Outcome::default(),
        }
    }

    fn dir(&self) -> PathBuf {
        PathBuf::from(&self.facts.install_dir)
    }

    fn warn(&mut self, text: String) {
        tracing::warn!("{text}");
        self.outcome.warnings += 1;
        (self.report)(Event::Warning(text));
    }

    /// Run all steps. A fatal failure rolls back what must not stay
    /// half-done (`plan::rollback`) and returns the error.
    pub fn run(mut self, steps: &[Step]) -> Result<Outcome> {
        let total = steps.len();
        let mut executed: Vec<Step> = Vec::new();
        for (index, step) in steps.iter().enumerate() {
            let text = step.describe(self.lang);
            tracing::info!(step = %step.describe(Lang::En), "step {}/{}", index + 1, total);
            (self.report)(Event::Step {
                index,
                total,
                text: text.clone(),
            });
            match self.step(step) {
                Ok(()) => executed.push(step.clone()),
                Err(e) if !step.is_fatal() => {
                    self.warn(format!("{text}: {e:#}"));
                }
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), step = %step.describe(Lang::En), "step failed");
                    for undo in plan::rollback(&executed) {
                        tracing::info!(step = %undo.describe(Lang::En), "rollback");
                        if let Err(re) = self.step(&undo) {
                            tracing::error!(error = %format!("{re:#}"), "rollback step failed");
                        }
                    }
                    return Err(e.context(text));
                }
            }
        }
        Ok(self.outcome)
    }

    fn step(&mut self, step: &Step) -> Result<()> {
        let sid_run_key = |sid: &str| format!(r"{sid}\{RUN_SUBKEY}");
        match step {
            Step::StopProcesses => self.stop_processes(),
            Step::ExtractFiles => {
                let payload = self.payload.context("no payload")?;
                let dir = self.dir();
                for e in &payload.entries {
                    let dest = dir.join(e.path.replace('/', "\\"));
                    if sys::write_file(&dest, &e.data)? {
                        self.outcome.reboot_required = true;
                    }
                }
                tracing::info!(files = payload.entries.len(), dir = %dir.display(), "files written");
                Ok(())
            }
            Step::RemoveStaleFiles(files) => {
                for f in files {
                    if crate::payload::validate_path(f) {
                        let p = self.dir().join(f.replace('/', "\\"));
                        if sys::delete_file(&p)? {
                            self.outcome.reboot_required = true;
                        }
                    }
                }
                Ok(())
            }
            Step::InstallUninstaller => {
                let dest = self.dir().join(plan::UNINSTALLER_EXE);
                if same_file(&self.self_exe, &dest) {
                    return Ok(());
                }
                if sys::copy_file(&self.self_exe, &dest)? {
                    self.outcome.reboot_required = true;
                }
                Ok(())
            }
            Step::WriteFileManifest(files) => {
                let dest = self.dir().join(plan::FILE_MANIFEST);
                sys::write_file(&dest, plan::format_manifest(files).as_bytes())?;
                Ok(())
            }
            Step::CreateTask { name, xml } => explorer_win::sched::create_from_xml(
                name,
                &crate::task_xml::to_utf16_file(xml),
                &crate::win::log::secure_data_dir()?,
            ),
            Step::DeleteTask(name) => explorer_win::sched::delete(name).map(|_| ()),
            Step::RunTask(name) => explorer_win::sched::run_now(name),
            Step::CreateShortcuts(list) => {
                let dir = PathBuf::from(&self.facts.start_menu_dir);
                // Start clean so shortcuts named in another language vanish.
                if dir.exists() {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("create {}", dir.display()))?;
                for s in list {
                    explorer_win::shortcut::create(
                        &dir.join(&s.file_name),
                        &explorer_win::shortcut::ShortcutSpec {
                            target: &s.target,
                            arguments: &s.arguments,
                            work_dir: &s.work_dir,
                            description: &s.description,
                            icon: Some((&s.icon, 0)),
                        },
                    )?;
                }
                Ok(())
            }
            Step::WriteMachineState(values) => reg::set_values(Root::Hklm, MACHINE_KEY, values),
            Step::WriteArp(values) => reg::set_values(Root::Hklm, plan::ARP_KEY, values),
            Step::SetRunValue { sid, command } => reg::set_values(
                Root::Hku,
                &sid_run_key(sid),
                &[(RUN_VALUE_NAME.into(), RegValue::Sz(command.clone()))],
            ),
            Step::DeleteRunValue { sid } => {
                reg::delete_value(Root::Hku, &sid_run_key(sid), RUN_VALUE_NAME)
            }
            Step::RegisterShell { sid, exe } => Hive::User(sid.clone()).register_shell(exe),
            Step::RestoreShell { sid } => Hive::User(sid.clone()).restore_shell().map(|_| ()),
            Step::StartExplorerIfMissing => {
                if explorer_win::process::explorer_shell_running() {
                    return Ok(());
                }
                let exe = launch::explorer_exe();
                let dir = Path::new(&exe)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                launch::start_unelevated(&exe, "", &dir, &self.facts.user_sid)?;
                let deadline = Instant::now() + Duration::from_secs(15);
                while !explorer_win::process::explorer_shell_running() && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Ok(())
            }
            Step::Launch { exe, args } => {
                let m = launch::start_unelevated(
                    exe,
                    args,
                    &self.facts.install_dir,
                    &self.facts.user_sid,
                )?;
                tracing::info!(?m, exe, args, "explorer.rs started");
                Ok(())
            }
            Step::CleanupUserHives => self.cleanup_user_hives(),
            Step::RemoveStartMenu => {
                let dir = PathBuf::from(&self.facts.start_menu_dir);
                if dir.exists() {
                    std::fs::remove_dir_all(&dir)
                        .with_context(|| format!("delete {}", dir.display()))?;
                }
                Ok(())
            }
            Step::DeleteArp => reg::delete_key(Root::Hklm, plan::ARP_KEY),
            Step::DeleteMachineState => reg::delete_key(Root::Hklm, MACHINE_KEY),
            Step::DeleteInstalledFiles => self.delete_installed_files(),
            Step::RemoveUserData => self.remove_user_data(),
        }
    }

    fn stop_processes(&mut self) -> Result<()> {
        let me = std::process::id();
        // Twice: the supervisor may restart a component between listing and
        // stopping.
        // Only this session: other signed-in users keep their explorer.rs
        // (their locked files are replaced at the next restart).
        let mut failed = Vec::new();
        for _ in 0..2 {
            let (mine, other) = plan::split_sessions(&sys::processes(), sys::current_session());
            let foreign = plan::stop_order(&other, &self.facts.install_dir, me);
            if !foreign.is_empty() {
                tracing::info!(?foreign, "left running (other sessions)");
            }
            let order = plan::stop_order(&mine, &self.facts.install_dir, me);
            if order.is_empty() {
                return Ok(());
            }
            tracing::info!(?order, "stopping");
            // The guardian gives its supervisor up to 8 s; components exit
            // within about a second of the shutdown message.
            failed = sys::stop_processes(&order, Duration::from_secs(6));
        }
        if !failed.is_empty() {
            self.warn(format!("could not stop: {failed:?}"));
        }
        Ok(())
    }

    fn cleanup_user_hives(&mut self) -> Result<()> {
        let dir = self.facts.install_dir.clone();
        let interactive = self.facts.user_sid.clone();
        let mut sids = vec![interactive.clone()];
        for k in reg::subkeys(Root::Hku, "") {
            if k.starts_with("S-1-5-21-") && !k.ends_with("_Classes") && k != interactive {
                sids.push(k);
            }
        }
        for sid in sids {
            let hive = Hive::User(sid.clone());
            let run_key = format!(r"{sid}\{RUN_SUBKEY}");
            let run = reg::get_string(Root::Hku, &run_key, RUN_VALUE_NAME);
            let shell = hive.shell();
            let c = plan::hive_cleanup(shell.as_deref(), run.as_deref(), &dir, sid == interactive);
            tracing::info!(sid, ?shell, ?run, ?c, "user hive");
            if c.restore_shell {
                if let Err(e) = hive.restore_shell() {
                    self.warn(format!("restore shell for {sid}: {e:#}"));
                }
            }
            if c.delete_run_value {
                if let Err(e) = reg::delete_value(Root::Hku, &run_key, RUN_VALUE_NAME) {
                    self.warn(format!("delete Run value for {sid}: {e:#}"));
                }
            }
        }
        tracing::info!(
            "limitation: only signed-in users (loaded registry hives) were checked; \
             signed-out users whose shell is explorer.rs must restore it themselves \
             (docs/RECOVERY.md)"
        );
        Ok(())
    }

    fn delete_installed_files(&mut self) -> Result<()> {
        let dir = self.dir();
        if !dir.exists() {
            return Ok(());
        }
        let manifest = old_manifest(&self.facts.install_dir);
        if manifest.is_empty() {
            // No manifest (very old or damaged install): the folder is our
            // fixed, protected location, remove it entirely.
            tracing::warn!("no install manifest; removing the whole folder");
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(error = %e, "remove_dir_all failed; deleting file by file");
                self.delete_tree_files(&dir);
            }
        } else {
            for f in manifest
                .iter()
                .filter(|f| !f.eq_ignore_ascii_case(plan::FILE_MANIFEST))
                .chain(std::iter::once(&plan::FILE_MANIFEST.to_string()))
            {
                let p = dir.join(f.replace('/', "\\"));
                match sys::delete_file(&p) {
                    // A silent uninstall's original uninstall.exe still runs
                    // (it waits for us); its removal at reboot is harmless.
                    Ok(true) if f.eq_ignore_ascii_case(plan::UNINSTALLER_EXE) => {}
                    Ok(true) => self.outcome.reboot_required = true,
                    Ok(false) => {}
                    Err(e) => self.warn(format!("{}: {e:#}", p.display())),
                }
            }
            // Leftovers of earlier upgrades (*.old, *.new-<pid>).
            self.delete_leftovers(&dir);
        }
        remove_empty_dirs(&dir);
        if dir.exists() {
            tracing::info!(dir = %dir.display(), "install folder not empty (files in use or not ours); kept");
        }
        Ok(())
    }

    fn delete_leftovers(&mut self, dir: &Path) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                self.delete_leftovers(&p);
                continue;
            }
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.ends_with(".old") || name.contains(".new-") {
                if let Ok(true) = sys::delete_file(&p) {
                    self.outcome.reboot_required = true;
                }
            }
        }
    }

    fn delete_tree_files(&mut self, dir: &Path) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                self.delete_tree_files(&p);
            } else if let Ok(true) = sys::delete_file(&p) {
                self.outcome.reboot_required = true;
            }
        }
    }

    fn remove_user_data(&mut self) -> Result<()> {
        let (roaming, local) = user_data_roots(&self.facts.user_sid)?;
        for base in [roaming, local] {
            let d = base.join(explorer_core::APP_DIR_NAME);
            if d.is_absolute() && d.exists() {
                tracing::info!(dir = %d.display(), "deleting user data");
                if let Err(e) = std::fs::remove_dir_all(&d) {
                    self.warn(format!("{}: {e}", d.display()));
                }
            }
        }
        Ok(())
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_lowercase()
    };
    n(a) == n(b)
}

/// Remove `dir` and its subfolders bottom-up, where empty.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                remove_empty_dirs(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// The interactive user's roaming and local AppData folders, resolved from
/// the registry (the elevated process's own environment may belong to
/// another account).
pub fn user_data_roots(sid: &str) -> Result<(PathBuf, PathBuf)> {
    let profile_raw = reg::get_string(
        Root::Hklm,
        &format!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\{sid}"),
        "ProfileImagePath",
    )
    .context("profile path not found")?;
    let profile = expand_env(&profile_raw);
    let folders =
        format!(r"{sid}\Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders");
    let get = |name: &str, default: &str| -> PathBuf {
        let raw = reg::get_string(Root::Hku, &folders, name)
            .unwrap_or_else(|| format!(r"%USERPROFILE%\{default}"));
        PathBuf::from(expand_env(&plan::expand_user_folder(&raw, &profile)))
    };
    let roaming = get("AppData", r"AppData\Roaming");
    let local = get("Local AppData", r"AppData\Local");
    if !roaming.is_absolute() || !local.is_absolute() {
        bail!("unexpected AppData paths: {roaming:?} {local:?}");
    }
    Ok((roaming, local))
}

fn expand_env(s: &str) -> String {
    use explorer_win::windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    let src = explorer_win::util::WideString::new(s);
    let mut buf = vec![0u16; 1024];
    // SAFETY: buffer and its length are valid.
    let n = unsafe { ExpandEnvironmentStringsW(src.pcwstr(), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return s.to_string();
    }
    explorer_win::util::from_wide(&buf[..n])
}
