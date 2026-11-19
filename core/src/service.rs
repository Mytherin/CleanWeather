//! Keeping a nowcast up to date: fetch the newest scans, decode them, recompute.
//! Shared by the desktop app, the `rain` tool and (later) the iPhone app.

use crate::knmi::Client;
use crate::nowcast::{DEFAULT_STEPS, Nowcast};
use crate::radar::Frame;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How many scans to keep: the last 30 minutes for the "past" part of the graph,
/// which also covers the 15 minutes the motion estimate needs.
pub const HISTORY: usize = 7;

pub struct Service {
    client: Client,
    frames: BTreeMap<String, Frame>,
    latest_name: Option<String>,
}

impl Service {
    pub fn new(api_key: String, cache_dir: PathBuf) -> Service {
        Service { client: Client::new(api_key, cache_dir), frames: BTreeMap::new(), latest_name: None }
    }

    /// Check KNMI for new scans. Returns a new nowcast if there was a new scan (or none
    /// had been computed yet), `None` if nothing changed.
    pub fn update(&mut self) -> Result<Option<Nowcast>> {
        let names = self.client.latest_files(HISTORY)?;
        let Some(newest) = names.first() else { bail!("KNMI listed no radar files") };
        if self.latest_name.as_ref() == Some(newest) {
            return Ok(None);
        }
        // Download and decode the missing scans in parallel: after a day away that's all
        // seven, and doing them one by one is dominated by round-trips (~2 s vs ~0.6 s).
        let missing: Vec<&String> = names.iter().filter(|n| !self.frames.contains_key(*n)).collect();
        let client = &self.client;
        let results: Vec<(String, Result<Frame>)> = std::thread::scope(|s| {
            let handles: Vec<_> = missing
                .iter()
                .map(|&name| s.spawn(move || (name.clone(), client.fetch(name).and_then(|p| Frame::read(&p)))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (name, result) in results {
            match result {
                Ok(f) => {
                    self.frames.insert(name, f);
                }
                // A single bad or missing older file shouldn't stop the forecast.
                Err(e) if &name == newest => return Err(e),
                Err(_) => {}
            }
        }
        self.frames.retain(|k, _| names.contains(k));
        self.client.prune_cache(chrono::Duration::hours(3));
        let frames: Vec<Frame> = self.frames.values().cloned().collect();
        let nc = Nowcast::compute(&frames, DEFAULT_STEPS)?;
        self.latest_name = Some(newest.clone());
        Ok(Some(nc))
    }
}
