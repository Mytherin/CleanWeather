//! Local builds embed the KNMI API key, so the app works without setup on your own
//! machine. The key is taken from `KNMI_API_KEY` or, if that isn't set, from the key
//! file the app saves (`%APPDATA%\CleanWeather\api_key.txt` on Windows).
//!
//! CI builds never embed a key: anything built for download would hand it to whoever
//! downloads it. They're recognised by the `CI` variable (set by GitHub Actions and
//! most CI services), or by `CLEANWEATHER_NO_EMBED=1`, which also turns embedding off
//! for a local build you want to share.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=KNMI_API_KEY");
    println!("cargo:rerun-if-env-changed=CI");
    println!("cargo:rerun-if-env-changed=CLEANWEATHER_NO_EMBED");

    let set = |v: &str| std::env::var(v).is_ok_and(|s| !s.is_empty() && s != "0" && s != "false");
    if set("CI") || set("CLEANWEATHER_NO_EMBED") {
        return;
    }

    let file = directories::BaseDirs::new().map(|d| d.config_dir().join("CleanWeather").join("api_key.txt"));
    if let Some(f) = &file {
        // Rebuild when the key file appears or changes.
        watch(f);
    }
    let key = std::env::var("KNMI_API_KEY")
        .ok()
        .or_else(|| file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()))
        .map(|k| k.trim().trim_start_matches('\u{feff}').trim_matches('"').trim().to_string())
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_graphic()));
    if let Some(key) = key {
        println!("cargo:rustc-env=CLEANWEATHER_EMBEDDED_KEY={key}");
        println!("cargo:warning=embedding your KNMI API key in this build: don't share these binaries (set CLEANWEATHER_NO_EMBED=1 to build without it)");
    }
}

/// Watch a file if it exists, else the nearest folder above it that does, so creating
/// the file later still triggers a rebuild (without re-running on every build).
fn watch(f: &PathBuf) {
    let mut p = f.as_path();
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
