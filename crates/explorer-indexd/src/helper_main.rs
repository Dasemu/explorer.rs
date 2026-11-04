// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs-index-helper`: minimal elevated helper that reads the NTFS
//! MFT and USN change journal on behalf of the unprivileged index daemon.
//!
//! It is never started automatically. Install it explicitly as a logon
//! scheduled task with `scripts/install-index-helper.ps1` (see
//! docs/decisions/0010-index-privilege-model.md). What it does:
//! - refuses to run unless elevated;
//! - serves `\\.\pipe\explorer-rs.index-helper.<user SID>`, whose DACL
//!   admits only the same user and whose medium mandatory label lets the
//!   non-elevated daemon connect; remote clients are rejected;
//! - accepts only clients whose executable is `explorer-rs-indexd.exe` in
//!   the helper's own directory;
//! - answers only "journal info", "MFT chunk" and "journal chunk" requests
//!   for fixed NTFS volumes it enumerated itself, returning raw record
//!   bytes. It parses nothing and writes nothing.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("explorer-rs-index-helper only exists on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    explorer_win::console::attach_parent();
    std::process::exit(win::run());
}

#[cfg(windows)]
mod win {
    use explorer_core::config::Config;
    use explorer_core::paths::AppPaths;
    use explorer_index::helper::serve;
    use explorer_index::source::Stop;
    use explorer_index::transport::{helper_endpoint, Access, Listener};
    use explorer_index::usn::{UsnBackend, UsnError};
    use explorer_index::win::{ntfs::DirectBackend, volumes};
    use explorer_win::security;

    pub fn run() -> i32 {
        let paths = AppPaths::from_env();
        let _ = paths.ensure();
        let (config, _) = Config::load_or_default(&paths.config_file());
        let _log = explorer_core::logging::init(
            "index-helper",
            &paths.log_dir,
            &config.general.log_level,
            config.general.log_keep_files,
        );
        if !security::is_elevated() {
            tracing::error!(
                "the index helper must run elevated; see scripts/install-index-helper.ps1"
            );
            return 2;
        }
        let Some(_instance) = explorer_win::single_instance::acquire("ExplorerRs.IndexHelper")
        else {
            tracing::warn!("another index helper is already running");
            return 0;
        };
        let volumes: Vec<u8> = volumes::fixed_drives()
            .iter()
            .filter(|r| {
                volumes::filesystem_name(r).is_some_and(|fs| fs.eq_ignore_ascii_case("NTFS"))
            })
            .filter_map(|r| volumes::drive_letter(r))
            .collect();
        let expected = match std::env::current_exe() {
            Ok(p) => p.with_file_name("explorer-rs-indexd.exe"),
            Err(e) => {
                tracing::error!(error = %e, "cannot locate own executable");
                return 1;
            }
        };
        let endpoint = helper_endpoint();
        let mut listener = match Listener::bind(&endpoint, Access::CurrentUserFromElevated) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!(endpoint = %endpoint, error = %e, "cannot create helper pipe");
                return 1;
            }
        };
        tracing::info!(
            volumes = %String::from_utf8_lossy(&volumes),
            endpoint = %endpoint,
            client = %expected.display(),
            "index helper ready"
        );
        let stop = Stop::new();
        loop {
            let conn = match listener.accept(&stop) {
                Ok(Some(c)) => c,
                Ok(None) => return 0,
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    continue;
                }
            };
            let peer = conn
                .peer_pid
                .and_then(|pid| security::process_image_path(pid).ok());
            let allowed = peer.as_ref().is_some_and(|p| {
                p.to_string_lossy()
                    .eq_ignore_ascii_case(&expected.to_string_lossy())
            });
            if !allowed {
                tracing::warn!(peer = ?peer, "refusing helper client that is not explorer-rs-indexd");
                conn.close();
                continue;
            }
            let vols = volumes.clone();
            let _ = std::thread::Builder::new()
                .name("index-helper-conn".into())
                .spawn(move || {
                    let (mut r, mut w, closer) = conn.split();
                    let mut open = |v: u8| -> Result<Box<dyn UsnBackend>, UsnError> {
                        DirectBackend::open(v).map(|b| Box::new(b) as Box<dyn UsnBackend>)
                    };
                    if let Err(e) = serve(&mut r, &mut w, &vols, &mut open) {
                        tracing::warn!(error = %e, "helper connection ended with an error");
                    }
                    closer();
                });
        }
    }
}
