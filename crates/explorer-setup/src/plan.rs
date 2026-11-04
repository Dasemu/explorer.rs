// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure install / uninstall planning.
//!
//! The Windows layer (`win::exec`) gathers facts ([`Facts`]), asks this
//! module for an ordered list of [`Step`]s and executes them one by one.
//! Every decision (what to stop, which registry values to write, what to
//! roll back, what to launch and how) is made here and unit-tested on
//! Linux.

use explorer_core::i18n::{tr, Lang};
use explorer_core::shellreg;

use crate::task_xml;

/// Folder name under `%ProgramFiles%`. The location is fixed: the index
/// helper runs elevated and must only execute binaries that
/// non-administrators cannot replace (ADR 0010).
pub const INSTALL_DIR_NAME: &str = "explorer-rs";
pub const SUPERVISOR_EXE: &str = "explorer-rs.exe";
pub const INDEX_HELPER_EXE: &str = "explorer-rs-index-helper.exe";
pub const SETUP_EXE: &str = "explorer-rs-setup.exe";
/// Copy of the setup program kept in the install folder for Add/Remove
/// Programs.
pub const UNINSTALLER_EXE: &str = "uninstall.exe";
/// List of installed files (one relative path per line), used for upgrades
/// and uninstall so only our own files are ever deleted.
pub const FILE_MANIFEST: &str = "install-manifest.txt";
/// Start menu folder (all users) under `%ProgramData%\...\Programs`.
pub const START_MENU_FOLDER: &str = "explorer.rs";
/// Add/Remove Programs key under HKLM.
pub const ARP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\explorer-rs";
pub const URL_INFO_ABOUT: &str = "https://github.com/Dasemu/explorer.rs";
pub const PUBLISHER: &str = "explorer.rs contributors";

/// Arguments of the "Restore Windows Explorer" shortcut target
/// (`explorer-rs.exe`, implemented by the supervisor).
pub const RESTORE_EXPLORER_ARGS: &str = "--restore-explorer";
/// Arguments of the "explorer.rs settings" shortcut (runs unelevated as the
/// signed-in user: creates config.toml if missing and opens it in Notepad).
pub const EDIT_CONFIG_ARGS: &str = "--edit-config";

/// How explorer.rs starts after sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartMode {
    /// HKU\<sid>\...\Run value: `explorer-rs.exe --dev` next to explorer.exe.
    #[default]
    Alongside,
    /// Per-user Winlogon Shell = `explorer-rs.exe --shell`.
    Shell,
    /// Installed only; started by hand from the Start menu.
    None,
}

impl StartMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "alongside" | "dev" => Some(Self::Alongside),
            "shell" => Some(Self::Shell),
            "none" | "manual" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alongside => "alongside",
            Self::Shell => "shell",
            Self::None => "none",
        }
    }
}

/// The user's choices on the Options page (or from the command line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choices {
    pub mode: StartMode,
    pub file_search: bool,
    pub launch: bool,
}

/// What a previous installation recorded under HKLM\Software\explorer-rs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreviousInstall {
    pub version: Option<String>,
    pub mode: Option<StartMode>,
    pub file_search: Option<bool>,
}

/// Initial choices: command line, else previous install, else defaults
/// (alongside, fast file search on, start now).
pub fn initial_choices(prev: Option<&PreviousInstall>, args: &crate::cli::InstallArgs) -> Choices {
    Choices {
        mode: args
            .mode
            .or_else(|| prev.and_then(|p| p.mode))
            .unwrap_or_default(),
        file_search: args
            .file_search
            .or_else(|| prev.and_then(|p| p.file_search))
            .unwrap_or(true),
        launch: !args.no_launch,
    }
}

/// Facts about the machine, gathered by the Windows layer before planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// `%ProgramFiles%\explorer-rs`, no trailing backslash.
    pub install_dir: String,
    /// All-users Start menu folder for our shortcuts.
    pub start_menu_dir: String,
    /// SID of the interactive (signed-in) user. Per-user settings are
    /// written to `HKEY_USERS\<sid>` even when elevation used another
    /// administrator account.
    pub user_sid: String,
    /// Windows Explorer currently provides the desktop (shell window).
    pub explorer_shell_running: bool,
    /// Some explorer-rs*.exe from the install folder is running.
    pub ours_running: bool,
}

impl Facts {
    pub fn exe(&self, name: &str) -> String {
        format!(r"{}\{}", self.install_dir, name)
    }

    /// explorer.rs appears to be the user's shell right now: it runs and
    /// Windows Explorer does not.
    pub fn ours_is_shell(&self) -> bool {
        self.ours_running && !self.explorer_shell_running
    }
}

/// A Start menu shortcut (`.lnk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    /// File name including `.lnk`.
    pub file_name: String,
    pub target: String,
    pub arguments: String,
    pub description: String,
    pub work_dir: String,
    /// Icon file (index 0).
    pub icon: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegValue {
    Sz(String),
    Dword(u32),
}

/// One unit of work. Executed in order by `win::exec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Close, then terminate, every explorer-rs*.exe running from the
    /// install folder (see [`stop_order`]).
    StopProcesses,
    /// Write every payload file (temp name + rename; locked files are
    /// renamed to `.old` and deleted at reboot).
    ExtractFiles,
    /// Delete files listed in the previous manifest but not in this payload.
    RemoveStaleFiles(Vec<String>),
    /// Copy the running setup program to `<dir>\uninstall.exe`.
    InstallUninstaller,
    /// Write `<dir>\install-manifest.txt`.
    WriteFileManifest(Vec<String>),
    /// `schtasks /Create /XML` (UTF-16 file) `/TN name /F`.
    CreateTask {
        name: String,
        xml: String,
    },
    /// `schtasks /Delete /TN name /F` (missing task = success).
    DeleteTask(String),
    CreateShortcuts(Vec<Shortcut>),
    /// Values under HKLM\Software\explorer-rs.
    WriteMachineState(Vec<(String, RegValue)>),
    /// Add/Remove Programs entry.
    WriteArp(Vec<(String, RegValue)>),
    SetRunValue {
        sid: String,
        command: String,
    },
    DeleteRunValue {
        sid: String,
    },
    RegisterShell {
        sid: String,
        exe: String,
    },
    RestoreShell {
        sid: String,
    },
    /// Start Windows Explorer for the interactive user if no shell window
    /// exists (never leave the user without a desktop).
    StartExplorerIfMissing,
    /// Start a program as the interactive user, never elevated.
    Launch {
        exe: String,
        args: String,
    },
    // ---- uninstall only ----
    /// For the interactive user and every other loaded hive: restore the
    /// shell and delete the Run value if they point into the install
    /// folder (see [`hive_cleanup`]).
    CleanupUserHives,
    RemoveStartMenu,
    DeleteArp,
    DeleteMachineState,
    /// Delete the files listed in the manifest, `.old` leftovers, then the
    /// folder if empty.
    DeleteInstalledFiles,
    /// Delete the interactive user's %APPDATA%\explorer-rs and
    /// %LOCALAPPDATA%\explorer-rs.
    RemoveUserData,
}

impl Step {
    /// A failing non-fatal step is logged as a warning and setup continues.
    pub fn is_fatal(&self) -> bool {
        !matches!(
            self,
            Step::RemoveStaleFiles(_)
                | Step::DeleteTask(_)
                | Step::Launch { .. }
                | Step::StartExplorerIfMissing
                | Step::RemoveUserData
                | Step::RemoveStartMenu
                | Step::CleanupUserHives
                | Step::DeleteInstalledFiles
                | Step::DeleteArp
                | Step::DeleteMachineState
        )
    }

    /// Progress-page text.
    pub fn describe(&self, lang: Lang) -> String {
        let s = |en, es| tr(lang, en, es).to_string();
        match self {
            Step::StopProcesses => s("Stopping explorer.rs", "Deteniendo explorer.rs"),
            Step::ExtractFiles => s("Copying files", "Copiando archivos"),
            Step::RemoveStaleFiles(_) => s(
                "Removing files of the previous version",
                "Eliminando archivos de la versión anterior",
            ),
            Step::InstallUninstaller => {
                s("Installing the uninstaller", "Instalando el desinstalador")
            }
            Step::WriteFileManifest(_) => s(
                "Recording installed files",
                "Registrando los archivos instalados",
            ),
            Step::CreateTask { .. } => s(
                "Registering the fast file search helper",
                "Registrando el asistente de búsqueda rápida",
            ),
            Step::DeleteTask(_) => s("Removing scheduled tasks", "Eliminando tareas programadas"),
            Step::CreateShortcuts(_) => s(
                "Creating Start menu shortcuts",
                "Creando accesos directos del menú Inicio",
            ),
            Step::WriteMachineState(_) => s("Saving settings", "Guardando la configuración"),
            Step::WriteArp(_) => s(
                "Registering with Installed apps",
                "Registrando en Aplicaciones instaladas",
            ),
            Step::SetRunValue { .. } => s(
                "Starting at sign-in alongside Windows Explorer",
                "Inicio al iniciar sesión junto al Explorador de Windows",
            ),
            Step::DeleteRunValue { .. } => s(
                "Removing start at sign-in",
                "Quitando el inicio al iniciar sesión",
            ),
            Step::RegisterShell { .. } => s(
                "Registering explorer.rs as your shell",
                "Registrando explorer.rs como tu shell",
            ),
            Step::RestoreShell { .. } => s(
                "Restoring Windows Explorer as your shell",
                "Restaurando el Explorador de Windows como tu shell",
            ),
            Step::StartExplorerIfMissing => s(
                "Starting Windows Explorer",
                "Iniciando el Explorador de Windows",
            ),
            Step::Launch { .. } => s("Starting explorer.rs", "Iniciando explorer.rs"),
            Step::CleanupUserHives => s(
                "Restoring user settings",
                "Restaurando la configuración de usuario",
            ),
            Step::RemoveStartMenu => s(
                "Removing Start menu shortcuts",
                "Eliminando accesos directos del menú Inicio",
            ),
            Step::DeleteArp => s(
                "Removing from Installed apps",
                "Quitando de Aplicaciones instaladas",
            ),
            Step::DeleteMachineState => s("Removing settings", "Eliminando la configuración"),
            Step::DeleteInstalledFiles => s("Deleting files", "Eliminando archivos"),
            Step::RemoveUserData => s(
                "Deleting your settings and logs",
                "Eliminando tu configuración y registros",
            ),
        }
    }
}

/// `"<dir>\explorer-rs.exe" --dev`: the Run value / dev-mode command line.
pub fn dev_command(install_dir: &str) -> String {
    format!("\"{install_dir}\\{SUPERVISOR_EXE}\" --dev")
}

/// The Start menu shortcuts, named in the installer's language.
pub fn shortcuts(install_dir: &str, lang: Lang) -> Vec<Shortcut> {
    let exe = |n: &str| format!(r"{install_dir}\{n}");
    let icon = exe(SUPERVISOR_EXE);
    let mk = |name: &str, target: String, args: &str, desc: &str| Shortcut {
        file_name: format!("{name}.lnk"),
        target,
        arguments: args.to_string(),
        description: desc.to_string(),
        work_dir: install_dir.to_string(),
        icon: icon.clone(),
    };
    vec![
        mk(
            "explorer.rs",
            exe(SUPERVISOR_EXE),
            "--dev",
            tr(
                lang,
                "Start explorer.rs alongside Windows Explorer",
                "Iniciar explorer.rs junto al Explorador de Windows",
            ),
        ),
        mk(
            tr(
                lang,
                "Restore Windows Explorer",
                "Restaurar el Explorador de Windows",
            ),
            exe(SUPERVISOR_EXE),
            RESTORE_EXPLORER_ARGS,
            tr(
                lang,
                "Make Windows Explorer your shell again and start it",
                "Volver a usar el Explorador de Windows como shell e iniciarlo",
            ),
        ),
        mk(
            tr(lang, "explorer.rs settings", "Configuración de explorer.rs"),
            exe(SUPERVISOR_EXE),
            EDIT_CONFIG_ARGS,
            tr(
                lang,
                "Open your explorer.rs configuration file",
                "Abrir tu archivo de configuración de explorer.rs",
            ),
        ),
        mk(
            tr(lang, "Uninstall explorer.rs", "Desinstalar explorer.rs"),
            exe(UNINSTALLER_EXE),
            "--uninstall",
            tr(lang, "Remove explorer.rs", "Quitar explorer.rs"),
        ),
    ]
}

/// Add/Remove Programs values.
pub fn arp_values(install_dir: &str, version: &str, size_bytes: u64) -> Vec<(String, RegValue)> {
    let un = format!(r"{install_dir}\{UNINSTALLER_EXE}");
    let v = |k: &str, s: String| (k.to_string(), RegValue::Sz(s));
    vec![
        v("DisplayName", "explorer.rs".into()),
        v("DisplayVersion", version.into()),
        v("Publisher", PUBLISHER.into()),
        v("InstallLocation", install_dir.into()),
        v("DisplayIcon", format!(r"{install_dir}\{SUPERVISOR_EXE},0")),
        v("UninstallString", format!("\"{un}\" --uninstall")),
        v(
            "QuietUninstallString",
            format!("\"{un}\" --uninstall --silent"),
        ),
        v("URLInfoAbout", URL_INFO_ABOUT.into()),
        (
            "EstimatedSize".into(),
            RegValue::Dword(u32::try_from(size_bytes.div_ceil(1024)).unwrap_or(u32::MAX)),
        ),
        ("NoModify".into(), RegValue::Dword(1)),
        ("NoRepair".into(), RegValue::Dword(1)),
    ]
}

/// HKLM\Software\explorer-rs values.
pub fn machine_state(install_dir: &str, version: &str, c: &Choices) -> Vec<(String, RegValue)> {
    use explorer_core::setup::*;
    vec![
        (VALUE_INSTALL_DIR.into(), RegValue::Sz(install_dir.into())),
        (VALUE_VERSION.into(), RegValue::Sz(version.into())),
        (VALUE_MODE.into(), RegValue::Sz(c.mode.as_str().into())),
        (
            VALUE_FILE_SEARCH.into(),
            RegValue::Dword(c.file_search as u32),
        ),
    ]
}

/// Arguments to start explorer.rs with at the end of setup, if at all.
/// explorer.rs is restarted when it was running before (setup stopped it)
/// or when the user asked for it. `--shell` only when shell mode was chosen
/// and Windows Explorer is not running (otherwise two shells would fight).
pub fn launch_args(
    mode: StartMode,
    launch: bool,
    was_running: bool,
    explorer_shell_running: bool,
) -> Option<&'static str> {
    if !(launch || was_running) {
        return None;
    }
    if mode == StartMode::Shell && !explorer_shell_running {
        Some("--shell")
    } else {
        Some("--dev")
    }
}

/// Case-insensitive, separator-normalised "is `path` inside `dir`".
pub fn path_in_dir(path: &str, dir: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let (p, d) = (norm(path), norm(dir));
    !d.is_empty() && p.len() > d.len() && p.starts_with(&d) && p.as_bytes()[d.len()] == b'\\'
}

/// True if a command line (Run value, Shell value) starts a program from
/// inside `dir` (quoted or not).
pub fn command_in_dir(command: &str, dir: &str) -> bool {
    let c = command.trim();
    let exe = match c.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        // Unquoted paths may contain spaces: cut after the first ".exe".
        None => match c.to_ascii_lowercase().find(".exe") {
            Some(i) => &c[..i + 4],
            None => c.split(' ').next().unwrap_or(""),
        },
    };
    path_in_dir(exe, dir)
}

/// Which of the running processes to stop, in order: the supervisor first
/// (so it does not restart the components), then everything else from the
/// install folder. Never the setup program itself (`self_pid`).
pub fn stop_order(procs: &[(u32, String)], install_dir: &str, self_pid: u32) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = procs
        .iter()
        .filter(|(pid, path)| {
            *pid != self_pid && path_in_dir(path, install_dir) && {
                let name = file_name(path).to_lowercase();
                name.starts_with("explorer-rs") && name.ends_with(".exe")
            }
        })
        .cloned()
        .collect();
    out.sort_by_key(|(pid, path)| (!file_name(path).eq_ignore_ascii_case(SUPERVISOR_EXE), *pid));
    out
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// What to do in one user hive on uninstall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HiveCleanup {
    pub restore_shell: bool,
    pub delete_run_value: bool,
}

/// `interactive`: the signed-in user, whose shell is restored whenever it is
/// explorer.rs; other hives only when their values point into `dir`.
pub fn hive_cleanup(
    shell: Option<&str>,
    run_value: Option<&str>,
    install_dir: &str,
    interactive: bool,
) -> HiveCleanup {
    let ours_here = |v: &str| command_in_dir(v, install_dir);
    HiveCleanup {
        restore_shell: shell.is_some_and(|s| shellreg::is_ours(s) && (ours_here(s) || interactive)),
        delete_run_value: run_value
            .is_some_and(|r| ours_here(r) || (interactive && shellreg::is_ours(r))),
    }
}

/// Parse `install-manifest.txt`; invalid lines are ignored (never delete
/// anything outside the install folder).
pub fn parse_manifest(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| crate::payload::validate_path(l))
        .map(str::to_string)
        .collect()
}

pub fn format_manifest(files: &[String]) -> String {
    let mut s = String::from("# explorer.rs installed files (used by setup and uninstall)\n");
    for f in files {
        s.push_str(f);
        s.push('\n');
    }
    s
}

/// Files of the previous install that the new payload no longer contains.
pub fn stale_files(old_manifest: &[String], new_files: &[String]) -> Vec<String> {
    old_manifest
        .iter()
        .filter(|o| {
            !new_files.iter().any(|n| n.eq_ignore_ascii_case(o))
                && !o.eq_ignore_ascii_case(UNINSTALLER_EXE)
                && !o.eq_ignore_ascii_case(FILE_MANIFEST)
        })
        .cloned()
        .collect()
}

/// Everything the manifest records for this install: payload files plus
/// the uninstaller and the manifest itself.
pub fn manifest_files(payload_files: &[String]) -> Vec<String> {
    let mut v = payload_files.to_vec();
    v.push(UNINSTALLER_EXE.into());
    v.push(FILE_MANIFEST.into());
    v
}

/// The install plan.
pub fn install_plan(
    facts: &Facts,
    choices: &Choices,
    version: &str,
    payload_files: &[String],
    payload_size: u64,
    old_manifest: &[String],
    lang: Lang,
) -> Vec<Step> {
    let dir = &facts.install_dir;
    let sid = facts.user_sid.clone();
    let mut steps = vec![Step::StopProcesses, Step::ExtractFiles];
    let stale = stale_files(old_manifest, payload_files);
    if !stale.is_empty() {
        steps.push(Step::RemoveStaleFiles(stale));
    }
    steps.push(Step::InstallUninstaller);
    steps.push(Step::WriteFileManifest(manifest_files(payload_files)));
    steps.push(Step::DeleteTask(task_xml::LEGACY_INDEX_HELPER_TASK.into()));
    if choices.file_search {
        steps.push(Step::CreateTask {
            name: task_xml::INDEX_HELPER_TASK.into(),
            xml: task_xml::index_helper_task(&sid, &facts.exe(INDEX_HELPER_EXE), dir),
        });
    } else {
        steps.push(Step::DeleteTask(task_xml::INDEX_HELPER_TASK.into()));
    }
    steps.push(Step::CreateShortcuts(shortcuts(dir, lang)));
    steps.push(Step::WriteMachineState(machine_state(
        dir, version, choices,
    )));
    steps.push(Step::WriteArp(arp_values(dir, version, payload_size)));
    // Per-user start mode, last among the fatal steps so a failure earlier
    // never leaves a half-registered shell.
    match choices.mode {
        StartMode::Alongside => {
            steps.push(Step::RestoreShell { sid: sid.clone() });
            steps.push(Step::SetRunValue {
                sid,
                command: dev_command(dir),
            });
        }
        StartMode::Shell => {
            steps.push(Step::DeleteRunValue { sid: sid.clone() });
            steps.push(Step::RegisterShell {
                sid,
                exe: facts.exe(SUPERVISOR_EXE),
            });
        }
        StartMode::None => {
            steps.push(Step::DeleteRunValue { sid: sid.clone() });
            steps.push(Step::RestoreShell { sid });
        }
    }
    if !facts.explorer_shell_running && choices.mode != StartMode::Shell {
        steps.push(Step::StartExplorerIfMissing);
    }
    if let Some(args) = launch_args(
        choices.mode,
        choices.launch,
        facts.ours_running,
        facts.explorer_shell_running,
    ) {
        steps.push(Step::Launch {
            exe: facts.exe(SUPERVISOR_EXE),
            args: args.into(),
        });
    }
    steps
}

/// Steps undoing what must not stay half-done after a fatal failure: a
/// registered shell is restored.
pub fn rollback(executed: &[Step]) -> Vec<Step> {
    executed
        .iter()
        .rev()
        .filter_map(|s| match s {
            Step::RegisterShell { sid, .. } => Some(Step::RestoreShell { sid: sid.clone() }),
            _ => None,
        })
        .collect()
}

/// The uninstall plan.
pub fn uninstall_plan(remove_user_data: bool) -> Vec<Step> {
    let mut steps = vec![
        Step::StopProcesses,
        Step::CleanupUserHives,
        Step::DeleteTask(task_xml::INDEX_HELPER_TASK.into()),
        Step::DeleteTask(task_xml::LEGACY_INDEX_HELPER_TASK.into()),
        Step::RemoveStartMenu,
        Step::DeleteArp,
        Step::DeleteMachineState,
        Step::DeleteInstalledFiles,
    ];
    if remove_user_data {
        steps.push(Step::RemoveUserData);
    }
    steps.push(Step::StartExplorerIfMissing);
    // Last: the launch task may have been used by the step above.
    steps.push(Step::DeleteTask(task_xml::LAUNCH_TASK.into()));
    steps
}

/// Expand a `User Shell Folders` value (REG_EXPAND_SZ, usually
/// `%USERPROFILE%\AppData\Roaming`) for another user: `%USERPROFILE%` is
/// replaced by that user's profile path; other variables are left as is.
pub fn expand_user_folder(raw: &str, profile: &str) -> String {
    const VAR: &str = "%USERPROFILE%";
    let mut out = String::with_capacity(raw.len() + profile.len());
    let mut rest = raw;
    while let Some(i) = rest.to_ascii_uppercase().find(VAR) {
        out.push_str(&rest[..i]);
        out.push_str(profile.trim_end_matches('\\'));
        rest = &rest[i + VAR.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::InstallArgs;

    const DIR: &str = r"C:\Program Files\explorer-rs";

    fn facts() -> Facts {
        Facts {
            install_dir: DIR.into(),
            start_menu_dir: r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\explorer.rs"
                .into(),
            user_sid: "S-1-5-21-1-2-3-1001".into(),
            explorer_shell_running: true,
            ours_running: false,
        }
    }

    fn files() -> Vec<String> {
        vec!["explorer-rs.exe".into(), "LICENSE".into()]
    }

    fn choices(mode: StartMode) -> Choices {
        Choices {
            mode,
            file_search: true,
            launch: true,
        }
    }

    fn plan(f: &Facts, c: &Choices, old: &[String]) -> Vec<Step> {
        install_plan(f, c, "0.1.0", &files(), 4096, old, Lang::En)
    }

    #[test]
    fn mode_parsing() {
        for m in [StartMode::Alongside, StartMode::Shell, StartMode::None] {
            assert_eq!(StartMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(StartMode::parse(" Shell "), Some(StartMode::Shell));
        assert_eq!(StartMode::parse("x"), None);
    }

    #[test]
    fn initial_choices_precedence() {
        let prev = PreviousInstall {
            version: Some("0.0.9".into()),
            mode: Some(StartMode::Shell),
            file_search: Some(false),
        };
        let none = InstallArgs::default();
        assert_eq!(
            initial_choices(None, &none),
            Choices {
                mode: StartMode::Alongside,
                file_search: true,
                launch: true
            }
        );
        assert_eq!(
            initial_choices(Some(&prev), &none),
            Choices {
                mode: StartMode::Shell,
                file_search: false,
                launch: true
            }
        );
        let args = InstallArgs {
            mode: Some(StartMode::None),
            file_search: Some(true),
            no_launch: true,
            silent: true,
        };
        assert_eq!(
            initial_choices(Some(&prev), &args),
            Choices {
                mode: StartMode::None,
                file_search: true,
                launch: false
            }
        );
    }

    #[test]
    fn alongside_plan() {
        let p = plan(&facts(), &choices(StartMode::Alongside), &[]);
        assert_eq!(p[0], Step::StopProcesses);
        assert_eq!(p[1], Step::ExtractFiles);
        assert!(p.contains(&Step::SetRunValue {
            sid: "S-1-5-21-1-2-3-1001".into(),
            command: format!(r#""{DIR}\explorer-rs.exe" --dev"#),
        }));
        assert!(p.contains(&Step::RestoreShell {
            sid: "S-1-5-21-1-2-3-1001".into()
        }));
        assert!(!p.iter().any(|s| matches!(s, Step::RegisterShell { .. })));
        assert!(p.iter().any(
            |s| matches!(s, Step::CreateTask { name, .. } if name == task_xml::INDEX_HELPER_TASK)
        ));
        assert_eq!(
            p.last(),
            Some(&Step::Launch {
                exe: format!(r"{DIR}\explorer-rs.exe"),
                args: "--dev".into()
            })
        );
        assert!(!p.contains(&Step::StartExplorerIfMissing));
        // Manifest includes the uninstaller and itself.
        assert!(p.contains(&Step::WriteFileManifest(vec![
            "explorer-rs.exe".into(),
            "LICENSE".into(),
            "uninstall.exe".into(),
            "install-manifest.txt".into()
        ])));
    }

    #[test]
    fn shell_plan_registers_shell_last_before_launch() {
        let p = plan(&facts(), &choices(StartMode::Shell), &[]);
        let reg = p
            .iter()
            .position(|s| matches!(s, Step::RegisterShell { .. }))
            .unwrap();
        // Everything after the shell registration is non-fatal.
        assert!(p[reg + 1..].iter().all(|s| !s.is_fatal()));
        assert!(p.contains(&Step::DeleteRunValue {
            sid: "S-1-5-21-1-2-3-1001".into()
        }));
        // Explorer runs: start explorer.rs in dev mode now, shell next sign-in.
        assert!(matches!(p.last(), Some(Step::Launch { args, .. }) if args == "--dev"));
    }

    #[test]
    fn none_plan_and_no_file_search() {
        let mut c = choices(StartMode::None);
        c.file_search = false;
        c.launch = false;
        let p = plan(&facts(), &c, &[]);
        assert!(p.contains(&Step::DeleteTask(task_xml::INDEX_HELPER_TASK.into())));
        assert!(!p.iter().any(|s| matches!(s, Step::CreateTask { .. })));
        assert!(p.contains(&Step::RestoreShell {
            sid: "S-1-5-21-1-2-3-1001".into()
        }));
        assert!(!p.iter().any(|s| matches!(s, Step::Launch { .. })));
        let state = machine_state(DIR, "0.1.0", &c);
        assert!(state.contains(&("FileSearch".into(), RegValue::Dword(0))));
        assert!(state.contains(&("Mode".into(), RegValue::Sz("none".into()))));
    }

    #[test]
    fn upgrade_while_explorer_rs_is_the_shell() {
        let mut f = facts();
        f.explorer_shell_running = false;
        f.ours_running = true;
        assert!(f.ours_is_shell());
        // Stays in shell mode: restart as the shell even without "start now".
        let mut c = choices(StartMode::Shell);
        c.launch = false;
        let p = plan(&f, &c, &[]);
        assert!(matches!(p.last(), Some(Step::Launch { args, .. }) if args == "--shell"));
        assert!(!p.contains(&Step::StartExplorerIfMissing));
        // Switching to alongside: bring Windows Explorer back, then dev mode.
        let p = plan(&f, &choices(StartMode::Alongside), &[]);
        let n = p.len();
        assert_eq!(p[n - 2], Step::StartExplorerIfMissing);
        assert!(matches!(&p[n - 1], Step::Launch { args, .. } if args == "--dev"));
    }

    #[test]
    fn stale_files_on_upgrade() {
        let old = vec![
            "explorer-rs.exe".to_string(),
            "EXPLORER-RS-OLD.exe".into(),
            "uninstall.exe".into(),
            "install-manifest.txt".into(),
            "license".into(),
        ];
        let p = plan(&facts(), &choices(StartMode::Alongside), &old);
        assert!(p.contains(&Step::RemoveStaleFiles(vec!["EXPLORER-RS-OLD.exe".into()])));
        assert!(!plan(&facts(), &choices(StartMode::Alongside), &[])
            .iter()
            .any(|s| matches!(s, Step::RemoveStaleFiles(_))));
    }

    #[test]
    fn rollback_restores_registered_shell() {
        let sid = "S-1".to_string();
        let executed = vec![
            Step::ExtractFiles,
            Step::RegisterShell {
                sid: sid.clone(),
                exe: "x".into(),
            },
            Step::Launch {
                exe: "x".into(),
                args: "--shell".into(),
            },
        ];
        assert_eq!(rollback(&executed), vec![Step::RestoreShell { sid }]);
        assert!(rollback(&[Step::ExtractFiles]).is_empty());
    }

    #[test]
    fn launch_decisions() {
        use StartMode::{Alongside, Shell};
        assert_eq!(launch_args(Alongside, false, false, true), None);
        assert_eq!(launch_args(Alongside, true, false, true), Some("--dev"));
        assert_eq!(launch_args(Alongside, false, true, false), Some("--dev"));
        assert_eq!(launch_args(Shell, true, false, true), Some("--dev"));
        assert_eq!(launch_args(Shell, true, false, false), Some("--shell"));
        assert_eq!(
            launch_args(StartMode::None, true, false, true),
            Some("--dev")
        );
    }

    #[test]
    fn arp() {
        let v = arp_values(DIR, "0.2.0", 10 * 1024 + 1);
        let get = |k: &str| v.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get("DisplayName"), Some(RegValue::Sz("explorer.rs".into())));
        assert_eq!(get("DisplayVersion"), Some(RegValue::Sz("0.2.0".into())));
        assert_eq!(get("Publisher"), Some(RegValue::Sz(PUBLISHER.into())));
        assert_eq!(get("EstimatedSize"), Some(RegValue::Dword(11)));
        assert_eq!(get("NoModify"), Some(RegValue::Dword(1)));
        assert_eq!(get("NoRepair"), Some(RegValue::Dword(1)));
        assert_eq!(
            get("UninstallString"),
            Some(RegValue::Sz(format!(
                r#""{DIR}\uninstall.exe" --uninstall"#
            )))
        );
        assert_eq!(
            get("QuietUninstallString"),
            Some(RegValue::Sz(format!(
                r#""{DIR}\uninstall.exe" --uninstall --silent"#
            )))
        );
        assert_eq!(
            get("DisplayIcon"),
            Some(RegValue::Sz(format!(r"{DIR}\explorer-rs.exe,0")))
        );
        assert_eq!(
            get("URLInfoAbout"),
            Some(RegValue::Sz(URL_INFO_ABOUT.into()))
        );
        assert_eq!(get("InstallLocation"), Some(RegValue::Sz(DIR.into())));
    }

    #[test]
    fn shortcut_set() {
        let s = shortcuts(DIR, Lang::Es);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].file_name, "explorer.rs.lnk");
        assert_eq!(s[0].arguments, "--dev");
        assert_eq!(s[1].file_name, "Restaurar el Explorador de Windows.lnk");
        assert_eq!(s[1].arguments, "--restore-explorer");
        assert!(s[2].target.ends_with(SUPERVISOR_EXE));
        assert_eq!(s[2].arguments, "--edit-config");
        assert!(s[3].target.ends_with(UNINSTALLER_EXE));
        assert_eq!(s[3].arguments, "--uninstall");
        for sc in &s {
            assert!(
                crate::payload::validate_path(&sc.file_name),
                "{}",
                sc.file_name
            );
        }
    }

    #[test]
    fn paths() {
        assert!(path_in_dir(
            r"c:\program files\EXPLORER-RS\explorer-rs.exe",
            DIR
        ));
        assert!(path_in_dir(r"C:/Program Files/explorer-rs/sub/x.exe", DIR));
        assert!(!path_in_dir(
            r"C:\Program Files\explorer-rs2\explorer-rs.exe",
            DIR
        ));
        assert!(!path_in_dir(DIR, DIR));
        assert!(!path_in_dir(r"C:\x.exe", ""));
        assert!(command_in_dir(&dev_command(DIR), DIR));
        assert!(command_in_dir(
            r"C:\Program Files\explorer-rs\explorer-rs.exe --shell",
            DIR
        ));
        assert!(!command_in_dir(
            r#""C:\Users\me\explorer-rs\explorer-rs.exe" --dev"#,
            DIR
        ));
    }

    #[test]
    fn process_stop_order() {
        let procs = vec![
            (10, format!(r"{DIR}\explorer-rs-taskbar.exe")),
            (11, r"C:\Windows\explorer.exe".to_string()),
            (12, format!(r"{DIR}\explorer-rs.exe")),
            (13, format!(r"{DIR}\uninstall.exe")),
            (14, r"C:\elsewhere\explorer-rs.exe".to_string()),
            (15, format!(r"{DIR}\explorer-rs-index-helper.exe")),
            (16, format!(r"{DIR}\explorer-rs-setup.exe")),
        ];
        let order: Vec<u32> = stop_order(&procs, DIR, 16).iter().map(|p| p.0).collect();
        assert_eq!(order, vec![12, 10, 15]);
    }

    #[test]
    fn hive_cleanup_rules() {
        let ours = shellreg::shell_command(&format!(r"{DIR}\explorer-rs.exe"));
        let elsewhere = r#""D:\portable\explorer-rs.exe" --shell"#;
        let run = dev_command(DIR);
        assert_eq!(
            hive_cleanup(Some(&ours), Some(&run), DIR, false),
            HiveCleanup {
                restore_shell: true,
                delete_run_value: true
            }
        );
        // Another user's portable copy is left alone...
        assert_eq!(
            hive_cleanup(Some(elsewhere), None, DIR, false),
            HiveCleanup::default()
        );
        // ...but the interactive user always gets Windows Explorer back.
        assert!(hive_cleanup(Some(elsewhere), None, DIR, true).restore_shell);
        assert_eq!(
            hive_cleanup(Some("explorer.exe"), Some("other.exe"), DIR, true),
            HiveCleanup::default()
        );
    }

    #[test]
    fn manifest_roundtrip_and_safety() {
        let f = manifest_files(&files());
        assert_eq!(parse_manifest(&format_manifest(&f)), f);
        assert_eq!(
            parse_manifest("# c\n\nok.exe\n..\\evil\n../evil\nC:\\x\n  sub/a.txt  \n"),
            vec!["ok.exe".to_string(), "sub/a.txt".into()]
        );
    }

    #[test]
    fn uninstall_steps() {
        let p = uninstall_plan(false);
        assert_eq!(p[0], Step::StopProcesses);
        assert!(!p.contains(&Step::RemoveUserData));
        assert!(uninstall_plan(true).contains(&Step::RemoveUserData));
        let n = p.len();
        assert_eq!(p[n - 2], Step::StartExplorerIfMissing);
        assert_eq!(p[n - 1], Step::DeleteTask(task_xml::LAUNCH_TASK.into()));
        // Shell restored before files disappear.
        let cleanup = p.iter().position(|s| *s == Step::CleanupUserHives).unwrap();
        let files = p
            .iter()
            .position(|s| *s == Step::DeleteInstalledFiles)
            .unwrap();
        assert!(cleanup < files);
    }

    #[test]
    fn user_folder_expansion() {
        assert_eq!(
            expand_user_folder(r"%USERPROFILE%\AppData\Roaming", r"C:\Users\Ana\"),
            r"C:\Users\Ana\AppData\Roaming"
        );
        assert_eq!(
            expand_user_folder(r"%userprofile%\AppData\Local", r"C:\Users\Ana"),
            r"C:\Users\Ana\AppData\Local"
        );
        assert_eq!(
            expand_user_folder(r"D:\Roaming", r"C:\Users\Ana"),
            r"D:\Roaming"
        );
    }

    #[test]
    fn every_step_has_both_translations() {
        let mut all = plan(&facts(), &choices(StartMode::Shell), &["x".into()]);
        all.extend(uninstall_plan(true));
        for s in &all {
            assert!(!s.describe(Lang::En).is_empty());
            assert_ne!(s.describe(Lang::En), s.describe(Lang::Es), "{s:?}");
        }
    }
}
