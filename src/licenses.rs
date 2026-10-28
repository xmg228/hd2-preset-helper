//! License text travels with the executable; no runtime files or network access.
pub fn text() -> String {
    format!(
        concat!(
            "HD2 Preset Helper {}\nCopyright (c) 2026 xmg and contributors\n\n",
            "The project's original source code is licensed under GPL-3.0-or-later.\n",
            "This executable includes Slint under GPL-3.0-only and is distributed under GPLv3.\n",
            "You may redistribute and modify it under the applicable license terms.\n",
            "This program comes with no warranty. See the full license below.\n\n",
            "Corresponding source: {}/archive/refs/tags/v{}.zip\n",
            "The archive includes Cargo.lock, build scripts, UI files and resources.\n",
            "With Rust and cargo-about 0.9.3 on PATH, build with:\n",
            "cargo build --release --locked\n",
            "Cargo retrieves the locked dependencies; their source links are listed below.\n\n",
            "The notices below also include build-time components not all linked into the executable.\n\n{}"
        ),
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_REPOSITORY"),
        env!("CARGO_PKG_VERSION"),
        include_str!(concat!(env!("OUT_DIR"), "/third-party-notices.txt")),
    )
}
