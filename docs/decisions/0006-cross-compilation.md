# ADR 0006: Cross-compile with cargo-xwin (MSVC), GNU as fallback

- Status: accepted
- Date: 2026-11-02

## Context
Development happens in a Linux container; the product targets Windows.

## Decision
Build `x86_64-pc-windows-msvc` with `cargo xwin` (clang-cl/lld-link plus the
MSVC CRT and Windows SDK, downloaded on first link; this accepts Microsoft's
license for them — the maintainer agreed). Keep
`TARGET=x86_64-pc-windows-gnu` working in `scripts/build-windows.sh` as a
fallback. CI also builds natively on `windows-latest`.

## Consequences
- Same ABI as a native MSVC build; PDBs are produced for crash analysis.
- First link needs network access to Microsoft's download servers.
