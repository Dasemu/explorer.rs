// SPDX-License-Identifier: GPL-3.0-or-later
//! `explorer-rs-indexd`: the file-name index daemon.
//!
//! Without a subcommand (or with `run`) it runs the daemon: loads the
//! snapshot, starts one source per root, serves the IPC endpoint and saves
//! snapshots periodically and on shutdown. The other subcommands are a small
//! command-line client, handy for testing (see
//! docs/TESTING-INDEX-ON-WINDOWS.md).
//!
//! Not in the supervisor's default component list until verified on
//! Windows; enable it with `components = ["taskbar", "launcher", "indexd"]`.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use explorer_core::config::Config;
use explorer_core::paths::AppPaths;
use explorer_core::supervisor::EXIT_STOP;
use explorer_index::transport::{self, Access, Listener};
use explorer_index_client::{ClientOptions, IndexClient, Outcome, QueryFlags};
use explorer_indexd::daemon::{resolve_roots, Daemon};
use explorer_indexd::server;

#[derive(Parser, Debug)]
#[command(
    name = "explorer-rs-indexd",
    version,
    about = "explorer.rs file-name index daemon"
)]
struct Args {
    /// Use this config file instead of %APPDATA%\explorer-rs\config.toml.
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the daemon (default).
    Run,
    /// Print the daemon status.
    Status,
    /// Search the running daemon.
    Query {
        /// Query text (see docs/INDEX.md for the syntax).
        text: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Plain terms match the full path.
        #[arg(long)]
        path: bool,
        /// Plain terms are fuzzy.
        #[arg(long)]
        fuzzy: bool,
    },
    /// Rescan one root, or all roots.
    Reindex { root: Option<String> },
    /// Ask the daemon to save a snapshot and exit.
    Shutdown,
}

fn main() {
    #[cfg(windows)]
    explorer_win::console::attach_parent();
    let args = Args::parse();
    let paths = AppPaths::from_env();
    let config_path = args.config.clone().unwrap_or_else(|| paths.config_file());
    let (config, config_err) = Config::load_or_default(&config_path);
    let code = match args.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => {
            let _ = paths.ensure();
            let _log = explorer_core::logging::init(
                "indexd",
                &paths.log_dir,
                &config.general.log_level,
                config.general.log_keep_files,
            );
            if let Some(e) = config_err {
                tracing::error!(path = %config_path.display(), error = %e, "config error, using defaults");
            }
            run_daemon(&config, &paths)
        }
        cmd => client(cmd),
    };
    std::process::exit(code);
}

fn run_daemon(config: &Config, paths: &AppPaths) -> i32 {
    #[cfg(windows)]
    let _instance = match explorer_win::single_instance::acquire("ExplorerRs.IndexD") {
        Some(g) => g,
        None => {
            tracing::warn!("another index daemon is already running; exiting");
            return EXIT_STOP;
        }
    };
    let endpoint = transport::daemon_endpoint();
    let listener = match Listener::bind(&endpoint, Access::CurrentUser) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(endpoint = %endpoint, error = %e, "cannot listen; is another daemon running?");
            return EXIT_STOP;
        }
    };
    let d = Daemon::new(config.index.clone(), paths.state_dir.join("index.snapshot"));
    let roots = resolve_roots(&config.index);
    tracing::info!(?roots, endpoint = %endpoint, "index daemon starting");
    d.start_roots(&roots);
    let srv = {
        let d = d.clone();
        std::thread::Builder::new()
            .name("index-server".into())
            .spawn(move || server::serve(d, listener))
            .expect("spawn server thread")
    };
    let interval = config.index.snapshot_interval_s;
    let tick = Duration::from_secs(if interval == 0 { 3600 } else { interval });
    while !d.stop.sleep(tick) {
        if interval > 0 {
            if let Err(e) = d.save_snapshot(false) {
                tracing::error!(error = %e, "periodic snapshot failed");
            }
        }
    }
    tracing::info!("index daemon stopping");
    d.shutdown();
    let _ = srv.join();
    EXIT_STOP
}

fn client(cmd: Cmd) -> i32 {
    let c = IndexClient::new(ClientOptions {
        client_name: "explorer-rs-indexd-cli".into(),
        request_timeout: Duration::from_secs(30),
        ..Default::default()
    });
    let wait = Duration::from_secs(35);
    let fail = |o: String| {
        eprintln!("{o}");
        1
    };
    match cmd {
        Cmd::Status => match c.status().wait(wait) {
            Outcome::Ready(s) => {
                println!(
                    "explorer-rs-indexd {}  entries={}  memory={} MiB  uptime={}s  snapshot_age={}",
                    s.version,
                    s.entries,
                    s.memory_bytes >> 20,
                    s.uptime_s,
                    s.snapshot_age_s.map_or("never".into(), |a| format!("{a}s"))
                );
                for r in s.roots {
                    println!(
                        "  {:<30} {:<5} {:<20} {} entries",
                        r.path, r.kind, r.state, r.entries
                    );
                }
                0
            }
            o => fail(format!("status failed: {o:?}")),
        },
        Cmd::Query {
            text,
            limit,
            offset,
            path,
            fuzzy,
        } => {
            let flags = QueryFlags {
                match_path: path,
                fuzzy,
            };
            match c
                .query(&text.join(" "), offset, limit, flags, None)
                .wait(wait)
            {
                Outcome::Ready(r) => {
                    for h in &r.hits {
                        let size = h.size.map(|s| format!("{s}")).unwrap_or_default();
                        println!("{}{}\t{size}", h.path, if h.is_dir { "\\" } else { "" });
                    }
                    eprintln!(
                        "{} of {} matches in {:.2} ms{}",
                        r.hits.len(),
                        r.total,
                        r.elapsed_us as f64 / 1000.0,
                        if r.more { " (more available)" } else { "" }
                    );
                    0
                }
                o => fail(format!("query failed: {o:?}")),
            }
        }
        Cmd::Reindex { root } => match c.reindex(root).wait(wait) {
            Outcome::Ready(()) => 0,
            o => fail(format!("reindex failed: {o:?}")),
        },
        Cmd::Shutdown => match c.shutdown_daemon().wait(wait) {
            Outcome::Ready(()) => 0,
            o => fail(format!("shutdown failed: {o:?}")),
        },
        Cmd::Run => unreachable!("handled by main"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses() {
        let a = Args::try_parse_from(["x", "query", "ext:rs", "main", "--limit", "5"]).unwrap();
        assert!(matches!(a.cmd, Some(Cmd::Query { limit: 5, .. })));
        let a = Args::try_parse_from(["x", "--config", "c.toml"]).unwrap();
        assert!(a.cmd.is_none() && a.config.is_some());
    }
}
