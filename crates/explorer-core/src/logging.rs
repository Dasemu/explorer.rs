// SPDX-License-Identifier: GPL-3.0-or-later
//! Logging setup shared by all binaries.
//!
//! Each component writes to its own daily-rotated file
//! (`<log_dir>/<component>.YYYY-MM-DD.log`), keeping `keep_files` files.
//! Panics are logged before the default hook runs, so a crashed component
//! always leaves a trace in its log even when it has no console.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter};

/// Environment variable overriding the configured log level. Accepts the
/// `tracing_subscriber::EnvFilter` syntax, e.g. `debug` or
/// `explorer_taskbar=trace,info`.
pub const LOG_ENV: &str = "EXPLORER_RS_LOG";

/// Keep this guard alive for the whole process; dropping it flushes logs.
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

/// Initialise logging for `component`. Never fails: if the log directory
/// cannot be used we log to stderr only, because a shell must keep running.
pub fn init(component: &str, log_dir: &Path, level: &str, keep_files: usize) -> LogGuard {
    let filter = EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new(level));
    let appender = std::fs::create_dir_all(log_dir).ok().and_then(|_| {
        RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(component)
            .filename_suffix("log")
            .max_log_files(keep_files.max(1))
            .build(log_dir)
            .ok()
    });
    let guard = match appender {
        Some(app) => {
            let (nb, guard) = tracing_appender::non_blocking(app);
            let file = fmt::layer()
                .with_writer(nb)
                .with_ansi(false)
                .with_target(true);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(file)
                .with(fmt::layer().with_writer(std::io::stderr))
                .try_init();
            Some(guard)
        }
        None => {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_writer(std::io::stderr))
                .try_init();
            tracing::warn!(dir = %log_dir.display(), "log directory unusable, logging to stderr only");
            None
        }
    };
    install_panic_hook(component.to_string());
    tracing::info!(
        component,
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "logging initialised"
    );
    LogGuard { _file: guard }
}

fn install_panic_hook(component: String) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let bt = std::backtrace::Backtrace::force_capture();
        tracing::error!(component = %component, panic = %info, backtrace = %bt, "PANIC");
        prev(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_log_file() {
        let d = tempfile::tempdir().unwrap();
        let g = init("unit-test", d.path(), "info", 3);
        tracing::info!("hello");
        drop(g);
        let files: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().collect();
        assert!(files
            .iter()
            .any(|f| f.file_name().to_string_lossy().starts_with("unit-test")));
    }
}
