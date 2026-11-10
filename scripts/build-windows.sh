#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Cross-compile explorer.rs release binaries for Windows from Linux and build
#   dist/explorer-rs-setup-<version>-x64.exe                    (installer)
#   dist/explorer-rs-<version>-x86_64-pc-windows-msvc.zip       (portable)
#
# Pipeline (docs/PACKAGING.md):
#   1. cargo xwin build --release (launcher: file-index + everything);
#      every binary gets its icon, manifest and version info from build.rs.
#   2. stage the payload: explorer-rs*.exe (not the setup itself), LICENSE,
#      config.example.toml, INSTALL.md, RECOVERY.md, scripts/*.ps1.
#   3. pack it with the host tool explorer-setup-pack (per-file SHA-256).
#   4. rebuild explorer-setup with EXPLORER_RS_PAYLOAD=<payload> so the
#      payload is embedded in explorer-rs-setup.exe.
#
# Requirements: rustup target x86_64-pc-windows-msvc, cargo-xwin
# (`cargo install cargo-xwin --locked`), clang/lld, llvm-rc, zip.
# cargo-xwin downloads the MSVC CRT and Windows SDK on first use; doing so
# accepts Microsoft's license terms for those components.
#
# Usage:
#   scripts/build-windows.sh
#   TASKBAR_FEATURES="quick-settings" scripts/build-windows.sh
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET=x86_64-pc-windows-msvc
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
LAUNCHER_FEATURES="${LAUNCHER_FEATURES:-file-index,everything}"
TASKBAR_FEATURES="${TASKBAR_FEATURES:-}"
REL="target/$TARGET/release"
STAGE="dist/payload-$VERSION"
PORTABLE="dist/explorer-rs-${VERSION}-${TARGET}"
PAYLOAD="dist/explorer-rs-${VERSION}-payload.bin"
SETUP_OUT="dist/explorer-rs-setup-${VERSION}-x64.exe"

rustup target add "$TARGET" >/dev/null

echo "==> Building release binaries ($TARGET)"
taskbar_args=()
[[ -n "$TASKBAR_FEATURES" ]] && taskbar_args=(--features "$TASKBAR_FEATURES")
cargo xwin build --release --target "$TARGET" -p explorer-supervisor -p explorer-indexd -p explorer-desktop
cargo xwin build --release --target "$TARGET" -p explorer-taskbar "${taskbar_args[@]}"
cargo xwin build --release --target "$TARGET" -p explorer-launcher --features "$LAUNCHER_FEATURES"
# Binaries removed in earlier versions must not be picked up by the glob below.
rm -f "$REL/explorer-rs-settings.exe"

echo "==> Staging payload"
rm -rf "$STAGE" "$PORTABLE" "$PORTABLE.zip"
mkdir -p "$STAGE/scripts"
found=0
for exe in "$REL"/explorer-rs*.exe; do
    [[ "$(basename "$exe")" == explorer-rs-setup.exe ]] && continue
    cp "$exe" "$STAGE/"
    found=$((found + 1))
done
if [[ $found -lt 5 ]]; then
    echo "expected at least 5 explorer-rs*.exe in $REL, found $found" >&2
    exit 1
fi
cp LICENSE "$STAGE/"
cp config/default.toml "$STAGE/config.example.toml"
cp docs/INSTALL.md docs/RECOVERY.md "$STAGE/"
cp scripts/*.ps1 "$STAGE/scripts/"

echo "==> Packing payload"
cargo run --quiet --release -p explorer-setup --bin explorer-setup-pack -- \
    "$PAYLOAD" "$VERSION" "$STAGE"

echo "==> Building installer"
EXPLORER_RS_PAYLOAD="$(pwd)/$PAYLOAD" \
    cargo xwin build --release --target "$TARGET" -p explorer-setup --bin explorer-rs-setup
cp "$REL/explorer-rs-setup.exe" "$SETUP_OUT"

echo "==> Portable zip"
mkdir -p "$PORTABLE"
cp -r "$STAGE/." "$PORTABLE/"
for pdb in "$REL"/explorer_rs*.pdb "$REL"/explorer-rs*.pdb; do
    [[ -f "$pdb" ]] && [[ "$(basename "$pdb")" != explorer_rs_setup.pdb ]] && cp "$pdb" "$PORTABLE/"
done
cp docs/TESTING-ON-WINDOWS.md "$PORTABLE/"
(cd dist && zip -qr "$(basename "$PORTABLE").zip" "$(basename "$PORTABLE")")

echo
ls -l "$SETUP_OUT" "$PORTABLE.zip"
sha256sum "$SETUP_OUT" "$PORTABLE.zip"
