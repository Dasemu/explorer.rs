// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows side of the installer: entry point, silent mode, uninstaller
//! relocation, wizard.

pub mod exec;
pub mod launch;
pub mod log;
pub mod reg;
pub mod sys;
pub mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use explorer_core::i18n::{tr, trf, Lang, LanguageSetting};

use crate::cli::{self, exit, Command, InstallArgs, UninstallArgs};
use crate::payload;
use crate::plan;

/// Entry point of `explorer-rs-setup.exe`. Returns the process exit code.
pub fn run(payload_bytes: &[u8]) -> i32 {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let parsed = cli::parse(raw.clone());
    let lang = parsed
        .as_ref()
        .ok()
        .and_then(|c| c.lang)
        .unwrap_or_else(|| explorer_win::theme::ui_lang(LanguageSetting::System));
    let cli = match parsed {
        Ok(c) => c,
        Err(e) => {
            ui::error_box(&format!("{e}\n\n{}", cli::USAGE));
            return exit::ERROR;
        }
    };
    if cli.command == Command::Help {
        explorer_win::console::attach_parent();
        println!("{}", cli::USAGE);
        ui::error_box(cli::USAGE);
        return exit::OK;
    }

    let log_path = log::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        args = ?raw,
        exe = ?std::env::current_exe().ok(),
        "explorer-rs-setup starting"
    );
    let _com = explorer_win::util::ComApartment::sta();

    if !explorer_win::security::is_elevated() {
        tracing::error!("not elevated");
        report_error(
            silent(&cli.command),
            tr(
                lang,
                "Setup must run as administrator.",
                "El instalador debe ejecutarse como administrador.",
            ),
        );
        return exit::ERROR;
    }
    sys::enable_debug_privilege();

    let code = match cli.command {
        Command::Install(a) => install(payload_bytes, a, lang, log_path),
        Command::Uninstall(u) => uninstall(u, lang, log_path),
        Command::Help => exit::OK,
    };
    tracing::info!(code, "explorer-rs-setup finished");
    code
}

fn silent(c: &Command) -> bool {
    match c {
        Command::Install(a) => a.silent,
        Command::Uninstall(u) => u.silent,
        Command::Help => false,
    }
}

fn report_error(silent: bool, text: &str) {
    tracing::error!("{text}");
    if !silent {
        ui::error_box(text);
    }
}

fn outcome_code(r: &anyhow::Result<exec::Outcome>) -> i32 {
    match r {
        Ok(o) if o.reboot_required => exit::REBOOT_REQUIRED,
        Ok(_) => exit::OK,
        Err(_) => exit::ERROR,
    }
}

fn install(bytes: &[u8], args: InstallArgs, lang: Lang, log_path: Option<PathBuf>) -> i32 {
    let payload = match payload::unpack(bytes) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            report_error(
                args.silent,
                &trf(
                    lang,
                    format!("This setup program is damaged or incomplete: {e}"),
                    format!("Este instalador está dañado o incompleto: {e}"),
                ),
            );
            return exit::ERROR;
        }
    };
    let facts = match exec::gather_facts() {
        Ok(f) => f,
        Err(e) => {
            report_error(args.silent, &format!("{e:#}"));
            return exit::ERROR;
        }
    };
    let previous = exec::previous_install();
    let choices = plan::initial_choices(previous.as_ref(), &args);
    tracing::info!(?facts, ?previous, ?choices, version = %payload.product_version, "install");

    if args.silent {
        let steps = plan::install_plan(
            &facts,
            &choices,
            &payload.product_version,
            &payload.paths(),
            payload.total_size(),
            &exec::old_manifest(&facts.install_dir),
            lang,
        );
        let mut report = |e: exec::Event| tracing::debug!(?e, "progress");
        let r = exec::Exec::new(facts, Some(&payload), lang, &mut report).run(&steps);
        if let Err(e) = &r {
            tracing::error!(error = %format!("{e:#}"), "silent install failed");
        }
        return outcome_code(&r);
    }
    ui::run_install(ui::InstallUi {
        facts,
        previous,
        choices,
        payload,
        lang,
        log_path,
    })
}

fn uninstall(args: UninstallArgs, lang: Lang, log_path: Option<PathBuf>) -> i32 {
    let facts = match exec::gather_facts() {
        Ok(f) => f,
        Err(e) => {
            report_error(args.silent, &format!("{e:#}"));
            return exit::ERROR;
        }
    };
    let me = std::env::current_exe().unwrap_or_default();

    match &args.from_temp {
        None => {
            // Running from the install folder (Installed apps, Start menu):
            // continue from a copy elsewhere so every file can be deleted.
            if plan::path_in_dir(&me.to_string_lossy(), &facts.install_dir) {
                match relaunch_from_copy(&me, &facts.install_dir, &args, lang) {
                    // Interactive: exit so the copy can delete this file.
                    // Silent: automation waits for our exit code, so wait for
                    // the copy (this file is then removed at the next reboot).
                    Ok(mut child) if args.silent => {
                        return child
                            .wait()
                            .ok()
                            .and_then(|s| s.code())
                            .unwrap_or(exit::ERROR);
                    }
                    Ok(_) => return exit::OK,
                    Err(e) => tracing::warn!(
                        error = %format!("{e:#}"),
                        "could not relaunch from a copy; uninstalling in place"
                    ),
                }
            }
        }
        Some(dir) => {
            // Only ever remove our fixed install folder.
            if !dir
                .to_string_lossy()
                .trim_end_matches('\\')
                .eq_ignore_ascii_case(&facts.install_dir)
            {
                report_error(
                    args.silent,
                    &format!("unexpected uninstall folder {}", dir.display()),
                );
                return exit::ERROR;
            }
            if let Some(pid) = args.wait_pid {
                sys::wait_exit(pid, Duration::from_secs(30));
            }
        }
    }

    let installed = Path::new(&facts.install_dir).exists() || exec::previous_install().is_some();
    if !installed {
        let msg = tr(
            lang,
            "explorer.rs is not installed.",
            "explorer.rs no está instalado.",
        );
        tracing::info!("{msg}");
        if !args.silent {
            ui::error_box(msg);
        }
        return exit::OK;
    }

    let code = if args.silent {
        let steps = plan::uninstall_plan(args.remove_user_data);
        let mut report = |e: exec::Event| tracing::debug!(?e, "progress");
        let r = exec::Exec::new(facts, None, lang, &mut report).run(&steps);
        if let Err(e) = &r {
            tracing::error!(error = %format!("{e:#}"), "silent uninstall failed");
        }
        outcome_code(&r)
    } else {
        ui::run_uninstall(ui::UninstallUi {
            facts,
            lang,
            log_path,
        })
    };

    if args.from_temp.is_some() {
        // This copy lives in the protected log folder; delete it at reboot.
        if let Err(e) = sys::delete_file(&me) {
            tracing::warn!(error = %format!("{e:#}"), "could not schedule removal of the uninstaller copy");
        }
    }
    code
}

/// Copy the uninstaller into `%ProgramData%\explorer-rs` (administrators
/// only, unlike %TEMP%, so nothing can swap the copy or plant DLLs next to
/// it before it runs elevated) and start it with `--uninstall-from-temp`.
fn relaunch_from_copy(
    me: &Path,
    install_dir: &str,
    args: &UninstallArgs,
    lang: Lang,
) -> anyhow::Result<std::process::Child> {
    use anyhow::Context;
    // log::init() created and protected this folder.
    let dir = log::log_dir()?;
    if !dir.is_dir() {
        anyhow::bail!("{} is missing", dir.display());
    }
    let copy = dir.join(format!("uninstall-{}.exe", std::process::id()));
    std::fs::copy(me, &copy).with_context(|| format!("copy to {}", copy.display()))?;
    let mut cmd = std::process::Command::new(&copy);
    cmd.arg("--uninstall-from-temp").arg(install_dir);
    if !args.silent {
        cmd.arg(format!("--wait-pid={}", std::process::id()));
    }
    cmd.arg(format!(
        "--lang={}",
        match lang {
            Lang::En => "en",
            Lang::Es => "es",
        }
    ));
    if args.silent {
        cmd.arg("--silent");
    }
    if args.remove_user_data {
        cmd.arg("--remove-user-data");
    }
    let child = cmd.spawn().context("start the uninstaller copy")?;
    tracing::info!(copy = %copy.display(), pid = child.id(), "continuing from a copy");
    Ok(child)
}
