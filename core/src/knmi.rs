//! Client for the KNMI Data Platform Open Data API (radar reflectivity composites).
//!
//! Each radar file is fetched in two steps: ask the API for a temporary download URL,
//! then download from that URL. Files are cached on disk by name, so after the first
//! run only the newest file (one every 5 minutes) is downloaded.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DATASET: &str = "radar_reflectivity_composites";
const VERSION: &str = "2.0";

#[derive(Deserialize)]
struct FileList {
    files: Vec<FileEntry>,
}

#[derive(Deserialize)]
struct FileEntry {
    filename: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UrlResponse {
    temporary_download_url: String,
}

pub struct Client {
    dataset: (&'static str, &'static str),
    agent: ureq::Agent,
    key: String,
    cache: PathBuf,
}

/// The valid time encoded in a radar file name (`..._YYYYMMDDHHMM.h5`).
pub fn time_from_filename(name: &str) -> Option<DateTime<Utc>> {
    let digits: Vec<&str> = name
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() == 12)
        .collect();
    let s = digits.last()?;
    NaiveDateTime::parse_from_str(s, "%Y%m%d%H%M").ok().map(|t| t.and_utc())
}

impl Client {
    pub fn new(key: String, cache: PathBuf) -> Client {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("CleanWeather/", env!("CARGO_PKG_VERSION")))
            .build();
        Client { agent, key, cache, dataset: (DATASET, VERSION) }
    }

    /// The same client for another KNMI dataset (e.g. the 10-minute station observations).
    pub fn with_dataset(mut self, name: &'static str, version: &'static str) -> Client {
        self.dataset = (name, version);
        self
    }

    fn api(&self, url: &str) -> Result<ureq::Response> {
        match self.agent.get(url).set("Authorization", &self.key).call() {
            Ok(r) => Ok(r),
            Err(ureq::Error::Status(code, r)) => {
                let body = r.into_string().unwrap_or_default();
                match code {
                    401 | 403 => bail!("KNMI rejected the API key ({code}). Check the key in Settings."),
                    429 => bail!("KNMI rate limit reached; will retry later."),
                    _ => bail!("KNMI API error {code}: {}", body.trim()),
                }
            }
            Err(e) => Err(anyhow!("cannot reach KNMI: {e}")),
        }
    }

    /// Names of the newest `n` radar files, newest first.
    pub fn latest_files(&self, n: usize) -> Result<Vec<String>> {
        let url = format!(
            "https://api.dataplatform.knmi.nl/open-data/v1/datasets/{}/versions/{}/files?maxKeys={n}&orderBy=created&sorting=desc",
            self.dataset.0,
            self.dataset.1
        );
        let list: FileList = self.api(&url)?.into_json().context("parsing file list")?;
        let mut names: Vec<String> = list.files.into_iter().map(|f| f.filename).collect();
        // Sort by the time in the name, in case "created" order differs (re-deliveries).
        names.sort_by_key(|n| std::cmp::Reverse(time_from_filename(n)));
        Ok(names)
    }

    /// The local path of a radar file, downloading it if it isn't cached yet.
    pub fn fetch(&self, filename: &str) -> Result<PathBuf> {
        if filename.contains(['/', '\\']) || filename.contains("..") {
            bail!("unexpected file name from KNMI: {filename}");
        }
        let path = self.cache.join(filename);
        if path.exists() {
            return Ok(path);
        }
        std::fs::create_dir_all(&self.cache)?;
        let url = format!(
            "https://api.dataplatform.knmi.nl/open-data/v1/datasets/{}/versions/{}/files/{filename}/url",
            self.dataset.0,
            self.dataset.1
        );
        let resp: UrlResponse = self.api(&url)?.into_json().context("parsing download URL")?;
        // The temporary URL is pre-signed; it must not get the Authorization header.
        let r = self
            .agent
            .get(&resp.temporary_download_url)
            .call()
            .map_err(|e| anyhow!("downloading {filename}: {e}"))?;
        let mut bytes = Vec::new();
        r.into_reader().take(64 << 20).read_to_end(&mut bytes)?;
        // Write to a temp name first so an interrupted download never looks complete.
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Delete cached files older than `keep` (by the time in their name).
    pub fn prune_cache(&self, keep: chrono::Duration) {
        prune(&self.cache, keep);
    }
}

pub(crate) fn prune(dir: &Path, keep: chrono::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let cutoff = Utc::now() - keep;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let old = match time_from_filename(&name) {
            Some(t) => t < cutoff,
            None => name.ends_with(".part"),
        };
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_time_from_name() {
        let t = time_from_filename("RAD_NL25_PCP_NA_202611191235.h5").unwrap();
        assert_eq!(t.to_rfc3339(), "2026-11-19T12:35:00+00:00");
        assert!(time_from_filename("nothing.h5").is_none());
    }
}
