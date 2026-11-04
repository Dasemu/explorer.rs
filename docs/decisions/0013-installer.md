# 0013. A Rust installer (`explorer-rs-setup.exe`)

## Status
Accepted. Unverified on real Windows.

## Context
Users need a normal installer: one `.exe`, administrator rights, Start menu
entries, an "Installed apps" entry, and a clean uninstall. Installation
mixes machine-wide steps (files in `%ProgramFiles%`, the elevated index
helper task, HKLM) with per-user steps (sign-in entry or shell registration
for the *signed-in* user, who may differ from the account that answered
UAC). The UI must be English/Spanish like the rest of explorer.rs, and the
logic must be testable on Linux where we build.

NSIS / WiX / Inno Setup were rejected: their scripting languages cannot share
our tested Rust code (`explorer_core::shellreg`, the shell backup rules,
`explorer_win::shellreg::interactive_user_sid`), they add a non-Rust build
dependency to the Linux pipeline, and per-user-from-elevated logic would be
duplicated in a second language.

## Decision
- New crate `explorer-setup` builds `explorer-rs-setup.exe`
  (`requireAdministrator`). Portable modules decide everything and are unit
  tested: `payload` (archive format with per-file SHA-256), `cli`, `plan`
  (ordered install/uninstall steps, rollback, process stop order, hive
  cleanup, ARP values, shortcut set), `task_xml`. The `win` module only
  executes steps and shows a plain Win32 wizard (comctl32 v6).
- **Fixed install folder** `%ProgramFiles%\explorer-rs`: the elevated index
  helper must only run binaries non-administrators cannot replace (ADR 0010).
- **Per-user state goes to `HKEY_USERS\<interactive SID>`**, never to the
  elevated process's HKCU. Machine state (choices, version) goes to
  `HKLM\Software\explorer-rs`; the supervisor reads `FileSearch` to enable
  `indexd` (`explorer_core::components`, `supervisor.file_search = "auto"`)
  because the installer cannot edit each user's config file.
- **Shell registration is the last fatal step**, and any fatal failure after
  it rolls it back (`plan::rollback`): setup never leaves a half-registered
  shell.
- **Never start the shell elevated.** Programs are started through the
  running Windows Explorer (`IShellWindows` -> desktop view ->
  `IShellDispatch2::ShellExecute`, the documented technique Raymond Chen
  describes); if Explorer does not run, through an on-demand scheduled task
  `\explorer-rs\Launch` for the interactive SID with `LeastPrivilege`.
- **Scheduled tasks** are registered with `schtasks.exe /Create /XML`
  (System32 path, UTF-16 file) from XML produced by tested code.
- **Uninstall from a copy**: `uninstall.exe` copies itself into
  `%ProgramData%\explorer-rs` (ACL: administrators/SYSTEM full, users read;
  re-created if it is a junction) rather than `%TEMP%`, so nothing unprivileged
  can swap the copy or plant DLLs next to it before it runs elevated. The
  copy deletes itself at reboot.
- **Settings shortcut** runs `explorer-rs.exe --edit-config` (asInvoker,
  as the signed-in user), not a mode of the elevated setup, which would
  prompt UAC on each click and edit the wrong user's file after
  over-the-shoulder elevation. (An earlier separate `explorer-rs-settings.exe`
  was folded into the supervisor's `--edit-config`.)
- **Resources** for all binaries come from one shared `build.rs` include
  (`assets/build/resources.rs`) that calls `llvm-rc` directly; no
  `embed-resource` dependency is needed.

## Consequences
- One more crate and two small dependencies (`sha2`, `miniz_oxide`; MIT/Apache).
- Only loaded user hives (signed-in users) are cleaned on uninstall; signed-out
  users keep a stale shell value (documented in INSTALL.md / RECOVERY.md).
- The Launch task stays registered after an install that used it (it is
  replaced on the next use and deleted on uninstall); whether deleting a task
  stops its running instance must be verified on Windows.
- The `DarkMode_Explorer` theme name for dark buttons/scroll bars is
  undocumented; failure only leaves light controls (ADR 0007).
