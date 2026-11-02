// SPDX-License-Identifier: GPL-3.0-or-later
//! Output of the `--dump-*` diagnostics.
//!
//! The binaries use the Windows GUI subsystem, so printing to a console is
//! best-effort. `--output <file>` is the reliable way to collect a dump.

use std::io::Write;
use std::path::Path;

/// Write `text` to `output` (or stdout). Returns a process exit code.
pub fn emit(text: &str, output: Option<&Path>) -> i32 {
    match output {
        Some(p) => match std::fs::write(p, text) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("cannot write {}: {e}", p.display());
                1
            }
        },
        None => {
            let mut out = std::io::stdout().lock();
            let _ = out.write_all(text.as_bytes());
            let _ = out.flush();
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("dump.txt");
        assert_eq!(emit("hello\n", Some(&p)), 0);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\n");
        assert_eq!(emit("x", Some(&d.path().join("no/such/dir/f"))), 1);
    }
}
