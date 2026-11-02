#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Cross-compile explorer.rs release binaries for Windows from Linux and
# package them into dist/explorer-rs-<version>-x86_64-pc-windows-msvc.zip.
#
# Requirements: rustup target x86_64-pc-windows-msvc, cargo-xwin
# (`cargo install cargo-xwin --locked`), clang/lld, zip.
# cargo-xwin downloads the MSVC CRT and Windows SDK on first use; doing so
# accepts Microsoft's license terms for those components.
#
# Usage:
#   scripts/build-windows.sh                 # default features
#   FEATURES="everything" scripts/build-windows.sh
#   FEATURES="all" scripts/build-windows.sh  # every optional feature
#   TARGET=x86_64-pc-windows-gnu scripts/build-windows.sh   # MinGW fallback
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET="${TARGET:-x86_64-pc-windows-msvc}"
FEATURES="${FEATURES:-}"
VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')"
DIST="dist/explorer-rs-${VERSION}-${TARGET}"

feature_args=()
if [[ "$FEATURES" == "all" ]]; then
    feature_args=(--all-features)
elif [[ -n "$FEATURES" ]]; then
    feature_args=(--features "$FEATURES")
fi

rustup target add "$TARGET" >/dev/null

if [[ "$TARGET" == *-msvc ]]; then
    cargo xwin build --release --target "$TARGET" --workspace "${feature_args[@]}"
else
    cargo build --release --target "$TARGET" --workspace "${feature_args[@]}"
fi

rm -rf "$DIST" "$DIST.zip"
mkdir -p "$DIST/scripts"
for exe in target/"$TARGET"/release/explorer-rs*.exe; do
    cp "$exe" "$DIST/"
done
for pdb in target/"$TARGET"/release/explorer_rs*.pdb target/"$TARGET"/release/explorer-rs*.pdb; do
    [[ -f "$pdb" ]] && cp "$pdb" "$DIST/"
done
cp scripts/*.ps1 "$DIST/scripts/"
cp config/default.toml "$DIST/config.example.toml"
cp LICENSE "$DIST/"
cp docs/INSTALL.md docs/RECOVERY.md docs/TESTING-ON-WINDOWS.md "$DIST/"

(cd dist && zip -qr "$(basename "$DIST").zip" "$(basename "$DIST")")
echo "Built $DIST.zip"
ls -l "$DIST"
sha256sum "$DIST.zip"
