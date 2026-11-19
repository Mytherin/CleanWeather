//! Where CleanWeather keeps its files, the API key and the user's settings.
//!
//! - Settings and key: `%APPDATA%\CleanWeather` on Windows
//!   (`~/Library/Application Support/CleanWeather` on macOS, `~/.config/CleanWeather` on Linux).
//! - Downloaded radar files: `%LOCALAPPDATA%\CleanWeather\radar` (the OS cache folder).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const APP: &str = "CleanWeather";

pub fn config_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.config_dir().join(APP))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn cache_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|d| d.cache_dir().join(APP).join("radar"))
        .unwrap_or_else(|| PathBuf::from("radar-cache"))
}

fn key_path() -> PathBuf {
    config_dir().join("api_key.txt")
}

/// The key compiled into a local build (see build.rs); never set in CI builds.
pub const EMBEDDED_KEY: Option<&str> = option_env!("CLEANWEATHER_EMBEDDED_KEY");

/// The KNMI Open Data API key: `KNMI_API_KEY`, else `api_key.txt` in the config
/// folder, else the key embedded in a local build.
pub fn api_key() -> Option<String> {
    if let Ok(k) = std::env::var("KNMI_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    let from_file = std::fs::read_to_string(key_path()).ok().map(|k| {
        // Tolerate a BOM or quotes from however the file was written.
        k.trim().trim_start_matches('\u{feff}').trim_matches('"').trim().to_string()
    });
    from_file.filter(|k| !k.is_empty()).or_else(|| EMBEDDED_KEY.map(str::to_string))
}

pub fn save_api_key(key: &str) -> Result<()> {
    std::fs::create_dir_all(config_dir())?;
    std::fs::write(key_path(), key.trim()).context("saving API key")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    pub place: String,
    pub lat: f64,
    pub lon: f64,
}

impl Default for Settings {
    fn default() -> Self {
        // Amsterdam (Dam square), until the user picks a spot on the map.
        Settings { place: "Amsterdam".into(), lat: 52.373, lon: 4.893 }
    }
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(Self::path(), serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}
