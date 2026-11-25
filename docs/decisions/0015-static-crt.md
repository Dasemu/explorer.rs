# ADR 0015: Link the C runtime statically

- Status: accepted
- Date: 2026-11-24

## Context
`x86_64-pc-windows-msvc` links the dynamic CRT by default, so every binary
imports `VCRUNTIME140.dll`. A clean Windows 11 does not ship it (it comes
with the Visual C++ Redistributable), and the installer and all other
`.exe` files fail to start with "VCRUNTIME140.dll was not found". The
installer must run on a machine where nothing else is installed.

## Decision
`.cargo/config.toml` sets `-C target-feature=+crt-static` for
`x86_64-pc-windows-msvc`. Cargo reads it for native builds
(`cargo build`, CI, `release.yml`) and `cargo xwin` builds alike, with no
change to `scripts/build-windows.sh`. No new dependency, and `build.rs`
resource embedding and `deny.toml` are unaffected.

## Alternatives
- Ship `vcruntime140.dll` next to the executables: the installer itself still
  could not start, the DLL needs a redistribution notice and updates, and
  every new binary must remember to use it.
- Require users to install the VC++ Redistributable: not acceptable for a
  shell replacement that must work out of the box.

## Consequences
- Binaries are somewhat larger (the CRT is linked in) and no longer share
  the system CRT, so CRT security fixes need a rebuild.
- Setting `RUSTFLAGS` (or `CARGO_ENCODED_RUSTFLAGS`) replaces the config
  value; if you do, include `-C target-feature=+crt-static`.
- Check a build: `dumpbin /dependents <exe>` must list no `VCRUNTIME*`,
  `MSVCP*` or `api-ms-win-crt-*` DLL.
