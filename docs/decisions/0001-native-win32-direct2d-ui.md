# ADR 0001: Native Win32 + Direct2D/DirectWrite for UI

- Status: accepted
- Date: 2026-11-02

## Context
The shell must be reliable, fast and WebView-free. It needs non-standard
windows: AppBars, tool windows, `MA_NOACTIVATE`, topmost popups, and precise
control over activation and focus. Candidates were raw Win32 + Direct2D,
Slint, and iced.

## Decision
Use raw Win32 windows created through the `windows` crate. Draw with
Direct2D (rectangles) and DirectWrite (text). Use native controls where they
give us something for free: an EDIT control for the launcher query (IME,
clipboard, accessibility).

## Consequences
- No toolkit fighting AppBar or activation semantics; every window flag is
  under our control.
- Very small footprint (~2 MB binaries), no GPU-framework runtime.
- More code for layout and hit-testing. That code lives in `explorer-core`
  (`layout.rs`) and is unit-tested.
- Accessibility of custom-drawn lists is limited (TODO: UI Automation
  provider, or a native list control).
- Slint or iced can be reconsidered for rich surfaces (quick settings, a
  future file manager) via a new ADR.
