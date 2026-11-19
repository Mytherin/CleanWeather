//! Local builds embed the API keys, so the app works without setup on your own machine:
//!
//! - KNMI (`KNMI_API_KEY`, or the app's `api_key.txt`)
//! - MeteoGate, for OPERA (`METEOGATE_API_KEY`, or the app's `meteogate_key.txt`)
//!
//! The key files are the ones the app saves (`%APPDATA%\CleanWeather\` on Windows).
//!
//! CI builds never embed keys: anything built for download would hand them to whoever
//! downloads it. They're recognised by the `CI` variable (set by GitHub Actions and
//! most CI services), or by `CLEANWEATHER_NO_EMBED=1`, which also turns embedding off
//! for a local build you want to share.

use std::path::{Path, PathBuf};

const KEYS: &[(&str, &str, &str)] = &[
    // (variable read at build time, key file, variable the code reads)
    ("KNMI_API_KEY", "api_key.txt", "CLEANWEATHER_EMBEDDED_KEY"),
    ("METEOGATE_API_KEY", "meteogate_key.txt", "CLEANWEATHER_EMBEDDED_METEOGATE_KEY"),
];

fn main() {
    println!("cargo:rerun-if-env-changed=CI");
    println!("cargo:rerun-if-env-changed=CLEANWEATHER_NO_EMBED");
    let set = |v: &str| std::env::var(v).is_ok_and(|s| !s.is_empty() && s != "0" && s != "false");
    if set("CI") || set("CLEANWEATHER_NO_EMBED") {
        return;
    }
    let dir = directories::BaseDirs::new().map(|d| d.config_dir().join("CleanWeather"));
    let mut embedded = Vec::new();
    for &(var, file, out) in KEYS {
        println!("cargo:rerun-if-env-changed={var}");
        let file = dir.as_ref().map(|d| d.join(file));
        if let Some(f) = &file {
            // Rebuild when the key file appears or changes.
            watch(f);
        }
        let key = std::env::var(var)
            .ok()
            .or_else(|| file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()))
            .map(|k| k.trim().trim_start_matches('\u{feff}').trim_matches('"').trim().to_string())
            .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_graphic()));
        if let Some(key) = key {
            println!("cargo:rustc-env={out}={key}");
            embedded.push(var.trim_end_matches("_API_KEY"));
        }
    }
    if !embedded.is_empty() {
        println!(
            "cargo:warning=embedding your {} API key(s) in this build: don't share these binaries (set CLEANWEATHER_NO_EMBED=1 to build without)",
            embedded.join(" and ")
        );
    }
}

/// Watch a file if it exists, else the nearest folder above it that does, so creating
/// the file later still triggers a rebuild (without re-running on every build).
fn watch(f: &PathBuf) {
    let mut p: &Path = f.as_path();
    loop {
        if p.exists() {
            println!("cargo:rerun-if-changed={}", p.display());
            return;
        }
        match p.parent() {
            Some(parent) => p = parent,
            None => return,
        }
    }
}
