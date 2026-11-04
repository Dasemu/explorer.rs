# ADR 0002: One process per component, supervised; window-message IPC

- Status: accepted
- Date: 2026-11-02

## Context
A crash in the launcher must never take down the taskbar. Shell processes run
for days and touch flaky OS surfaces (COM shell namespace, hooks).

## Decision
- The supervisor (`explorer-rs.exe`) spawns `explorer-rs-taskbar.exe` and
  `explorer-rs-launcher.exe` as separate processes. Restart policy is a pure
  function (`explorer_core::supervisor`): exponential backoff, healthy-uptime
  reset, crash-loop give-up per component.
- Exit codes carry intent: 0 stop, 77 quit all, 78 restart.
- Children go into a kill-on-close job object.
- IPC is a registered window message posted to a window found by class name.
  No pipes or shared memory, so there is nothing a dead peer can wedge.

## Consequences
- Fault isolation at OS level; trivially restartable components.
- The supervisor contains no UI code and polls children every 200 ms.
- The launcher pays its own process startup cost once, then stays resident
  (hidden) so opening it is instant.
- Richer IPC (e.g. the file index) uses its own protocol with explicit
  versioning (see the indexer ADRs).
