# ADR 0005: Win key via low-level keyboard hook + mask key

- Status: accepted
- Date: 2026-11-02

## Context
Pressing Win alone must open our launcher. `RegisterHotKey` cannot register a
bare modifier. In dev mode, explorer's Start menu would also open.

## Decision
- Install `WH_KEYBOARD_LL` (plus `WH_MOUSE_LL`, so Win+click cancels the tap)
  in the taskbar process. Feed a pure state machine
  (`explorer_core::hotkey`).
- On a lone tap, inject VK 0xE8 down/up (dev mode only) before the Win
  key-up continues, then post to the launcher. Never swallow keys.

## Consequences
- Win+E/Win+R/Win+L etc. are untouched.
- LL hooks are removed silently by Windows if the callback is slow, so the
  callback only does constant-time work.
- Behaviour on Windows is unverified, especially whether the mask key fully
  suppresses Start on Windows 11.
