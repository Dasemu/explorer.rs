// SPDX-License-Identifier: GPL-3.0-or-later
//! Embeds the icon, application manifest and version info (see
//! assets/build/resources.rs and docs/PACKAGING.md).

include!("../../assets/build/resources.rs");

fn main() {
    embed_resources(&[
        Bin::new("explorer-rs-indexd", "explorer.rs file index daemon"),
        Bin::new("explorer-rs-index-helper", "explorer.rs file index helper"),
    ]);
}
