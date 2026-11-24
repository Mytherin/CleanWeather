//! Keeping a nowcast up to date from the best working radar source, with fallback.
//! Shared by the desktop app, the `rain` tool and (later) the iPhone app.
//!
//! For a location in the Netherlands KNMI comes first, then OPERA; elsewhere OPERA comes
//! first, then KNMI if the location is on KNMI's grid (Belgium, western Germany).
//! KNMI's composite measures near the ground; OPERA's column-maximum composite reads
//! much wetter (see `rain gauges`). A source is passed over when:
//! - fetching fails (network, API or format errors),
//! - its newest scan is older than [`STALE_MINUTES`],
//! - it has no radar data at the location (outside its coverage, or radars down), or
//! - it has fewer than two scans (no motion to estimate).
//!
//! A source that failed is skipped for [`RETRY_MINUTES`] (or until the location moves),
//! then tried again, so the app returns to the preferred source by itself once it works
//! again.

use crate::knmi::Client;
use crate::nowcast::{DEFAULT_STEPS, Nowcast};
use crate::opera::Opera;
use crate::radar::Frame;
use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How many scans to keep: the last 30 minutes for the "past" part of the graph,
/// which also covers the 15 minutes the motion estimate needs.
pub const HISTORY: usize = 7;
/// A source whose newest scan is older than this is treated as not working.
pub const STALE_MINUTES: i64 = 15;
/// After a source fails, use the next one for this long before trying it again.
pub const RETRY_MINUTES: i64 = 5;

/// A provider of radar scans.
pub trait RadarSource: Send {
    fn name(&self) -> &'static str;
    /// Whether the source can have data for a location at all.
    fn covers(&self, _lat: f64, _lon: f64) -> bool {
        true
    }
    /// Whether the source should come first for a location.
    fn preferred(&self, _lat: f64, _lon: f64) -> bool {
        false
    }
    /// The newest scans around a location (up to `n`, any order).
    fn latest(&mut self, lat: f64, lon: f64, n: usize) -> Result<Vec<Frame>>;
}

/// KNMI's Dutch composite.
pub struct Knmi {
    client: Client,
    frames: BTreeMap<String, Frame>,
}

impl Knmi {
    pub fn new(api_key: String, cache_dir: PathBuf) -> Knmi {
        Knmi { client: Client::new(api_key, cache_dir), frames: BTreeMap::new() }
    }
}

impl RadarSource for Knmi {
    fn name(&self) -> &'static str {
        "KNMI"
    }

    fn covers(&self, lat: f64, lon: f64) -> bool {
        crate::radar::Grid::knmi().contains(lat, lon)
    }

    /// KNMI first for locations in the Netherlands; OPERA (Europe-wide) elsewhere.
    fn preferred(&self, lat: f64, lon: f64) -> bool {
        crate::basemap::map().in_netherlands(lat, lon)
    }

    fn latest(&mut self, _lat: f64, _lon: f64, n: usize) -> Result<Vec<Frame>> {
        let names = self.client.latest_files(n)?;
        let Some(newest) = names.first().cloned() else { bail!("KNMI listed no radar files") };
        // Download and decode the missing scans in parallel: after a day away that's all
        // of them, and one by one is dominated by round-trips (~2 s vs ~0.6 s).
        let missing: Vec<&String> = names.iter().filter(|n| !self.frames.contains_key(*n)).collect();
        let client = &self.client;
        let results: Vec<(String, Result<Frame>)> = std::thread::scope(|s| {
            let handles: Vec<_> = missing.iter().map(|&name| s.spawn(move || (name.clone(), client.fetch(name).and_then(|p| Frame::read(&p))))).collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (name, result) in results {
            match result {
                Ok(f) => {
                    self.frames.insert(name, f);
                }
                // A single bad or missing older file shouldn't stop the forecast.
                Err(e) if name == newest => return Err(e),
                Err(_) => {}
            }
        }
        self.frames.retain(|k, _| names.contains(k));
        self.client.prune_cache(Duration::hours(3));
        Ok(self.frames.values().cloned().collect())
    }
}

impl RadarSource for Opera {
    fn name(&self) -> &'static str {
        "OPERA"
    }

    fn covers(&self, lat: f64, lon: f64) -> bool {
        crate::opera::crop_for(lat, lon, crate::opera::Product::MaxReflectivity).is_some()
    }

    fn latest(&mut self, lat: f64, lon: f64, n: usize) -> Result<Vec<Frame>> {
        Opera::latest(self, lat, lon, n)
    }
}

/// A new nowcast, and where it came from.
pub struct Update {
    pub nowcast: Nowcast,
    pub source: &'static str,
    /// Why preferred sources weren't used, if they weren't.
    pub fallback_reason: Option<String>,
}

pub struct Service {
    sources: Vec<Box<dyn RadarSource>>,
    /// Per source: don't try before this time (after a failure), why it failed, and at
    /// which location (a failure such as "no radar data here" says nothing about another).
    skip_until: Vec<Option<(DateTime<Utc>, String, (f64, f64))>>,
    /// What the last nowcast was made from: source, newest scan, location.
    last: Option<(&'static str, DateTime<Utc>, (f64, f64))>,
}

impl Service {
    /// OPERA (with a MeteoGate key if there is one) and KNMI (if there's a key); which
    /// comes first depends on the location (see the module docs).
    pub fn new(knmi_key: Option<String>, meteogate_key: Option<String>, cache_dir: PathBuf) -> Service {
        let mut sources: Vec<Box<dyn RadarSource>> = vec![Box::new(Opera::new(meteogate_key, cache_dir.join("opera")))];
        if let Some(k) = knmi_key {
            sources.push(Box::new(Knmi::new(k, cache_dir)));
        }
        Service::with_sources(sources)
    }

    pub fn with_sources(sources: Vec<Box<dyn RadarSource>>) -> Service {
        let n = sources.len();
        Service { sources, skip_until: vec![None; n], last: None }
    }

    /// Check for new scans at a location. Returns a new nowcast if there is one (new
    /// scan, other source or moved location), `None` if nothing changed.
    pub fn update(&mut self, lat: f64, lon: f64) -> Result<Option<Update>> {
        self.update_at(lat, lon, Utc::now())
    }

    fn update_at(&mut self, lat: f64, lon: f64, now: DateTime<Utc>) -> Result<Option<Update>> {
        let mut reasons = Vec::new();
        // Sources preferred for this location first, then the rest in their order.
        let mut order: Vec<usize> = (0..self.sources.len()).filter(|&i| self.sources[i].covers(lat, lon)).collect();
        order.sort_by_key(|&i| !self.sources[i].preferred(lat, lon));
        for i in order {
            let name = self.sources[i].name();
            if let Some((until, why, at)) = &self.skip_until[i] {
                if now < *until && *at == (lat, lon) {
                    reasons.push(format!("{name}: {why}"));
                    continue;
                }
            }
            let checked = self.sources[i].latest(lat, lon, HISTORY).and_then(|frames| check(frames, lat, lon, now));
            let frames = match checked {
                Ok(f) => f,
                Err(e) => {
                    let why = format!("{e:#}");
                    reasons.push(format!("{name}: {why}"));
                    self.skip_until[i] = Some((now + Duration::minutes(RETRY_MINUTES), why, (lat, lon)));
                    continue;
                }
            };
            self.skip_until[i] = None;
            let newest = frames.iter().map(|f| f.time).max().unwrap();
            let id = (name, newest, (lat, lon));
            if self.last == Some(id) {
                return Ok(None);
            }
            let nowcast = Nowcast::compute(&frames, DEFAULT_STEPS)?;
            self.last = Some(id);
            let fallback_reason = (!reasons.is_empty()).then(|| reasons.join("; "));
            return Ok(Some(Update { nowcast, source: name, fallback_reason }));
        }
        if reasons.is_empty() {
            bail!("no radar covers this location");
        }
        Err(anyhow!("no radar source is working ({})", reasons.join("; ")))
    }
}

/// Is this a usable set of scans for a nowcast at the location?
fn check(frames: Vec<Frame>, lat: f64, lon: f64, now: DateTime<Utc>) -> Result<Vec<Frame>> {
    let Some(newest) = frames.iter().max_by_key(|f| f.time) else { bail!("no scans") };
    let age = (now - newest.time).num_minutes();
    if age > STALE_MINUTES {
        bail!("newest scan is {age} min old");
    }
    if frames.len() < 2 {
        bail!("only one scan available");
    }
    // Radar data at (or right around) the location in the newest scan.
    let g = &newest.grid;
    let (c, r) = g.to_pixel(lat, lon);
    let mut seen = false;
    for dy in -2i64..=2 {
        for dx in -2i64..=2 {
            let (x, y) = (c as i64 + dx, r as i64 + dy);
            if x >= 0 && y >= 0 && (x as usize) < g.width && (y as usize) < g.height && !newest.get(x as usize, y as usize).is_nan() {
                seen = true;
            }
        }
    }
    if !seen {
        bail!("no radar data at this location");
    }
    Ok(frames)
}

/// The pixels of a Europe-wide grid where a local nowcast has radar data (in its latest
/// scan), so a map can show the local radar there and the Europe-wide one elsewhere.
pub fn covered(europe: &crate::radar::Grid, local: &Nowcast) -> Vec<bool> {
    let mut out = vec![false; europe.width * europe.height];
    let Some((_, latest)) = local.observed.last() else { return out };
    let lg = &local.grid;
    // The local grid's outline in Europe pixels (sampled along its edges).
    let (mut c0, mut r0, mut c1, mut r1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let (w, h) = (lg.width as f64, lg.height as f64);
    for k in 0..=20 {
        let s = k as f64 / 20.0;
        for (c, r) in [(s * w, 0.0), (s * w, h), (0.0, s * h), (w, s * h)] {
            let (lat, lon) = lg.to_latlon(c, r);
            let (ec, er) = europe.to_pixel(lat, lon);
            (c0, r0, c1, r1) = (c0.min(ec), r0.min(er), c1.max(ec), r1.max(er));
        }
    }
    let clamp = |v: f64, max: usize| (v.max(0.0) as usize).min(max);
    for r in clamp(r0 - 1.0, europe.height)..clamp(r1 + 2.0, europe.height) {
        for c in clamp(c0 - 1.0, europe.width)..clamp(c1 + 2.0, europe.width) {
            let (lat, lon) = europe.to_latlon(c as f64 + 0.5, r as f64 + 0.5);
            let (lc, lr) = lg.to_pixel(lat, lon);
            if lc >= 0.0 && lr >= 0.0 && (lc as usize) < lg.width && (lr as usize) < lg.height && !latest.dbz(lr as usize * lg.width + lc as usize).is_nan() {
                out[r * europe.width + c] = true;
            }
        }
    }
    out
}

/// Rain across Europe for the map: OPERA's 4 km overview of each scan, extrapolated
/// like the local nowcast. Independent of which source the location's forecast uses.
pub struct Europe {
    opera: Opera,
    last: Option<DateTime<Utc>>,
}

impl Europe {
    pub fn new(meteogate_key: Option<String>, cache_dir: PathBuf) -> Europe {
        Europe { opera: Opera::new(meteogate_key, cache_dir.join("opera")), last: None }
    }

    /// A new Europe-wide nowcast if there's a new scan, else `None`.
    pub fn update(&mut self) -> Result<Option<Nowcast>> {
        let frames = self.opera.europe(HISTORY)?;
        let newest = frames.iter().map(|f| f.time).max().ok_or_else(|| anyhow!("no scans"))?;
        if self.last == Some(newest) {
            return Ok(None);
        }
        let nowcast = Nowcast::compute(&frames, DEFAULT_STEPS)?;
        self.last = Some(newest);
        Ok(Some(nowcast))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::radar::Grid;
    use std::sync::{Arc, Mutex};

    /// A source that returns scans of a given age, or an error, and counts its calls.
    struct Fake {
        name: &'static str,
        age_minutes: Arc<Mutex<Option<i64>>>,
        covered: bool,
        calls: Arc<Mutex<usize>>,
        now: DateTime<Utc>,
    }

    impl RadarSource for Fake {
        fn name(&self) -> &'static str {
            self.name
        }
        fn latest(&mut self, _: f64, _: f64, _: usize) -> Result<Vec<Frame>> {
            *self.calls.lock().unwrap() += 1;
            let Some(age) = *self.age_minutes.lock().unwrap() else { bail!("connection refused") };
            let grid = Grid::knmi();
            let value = if self.covered { 0.0 } else { f32::NAN };
            Ok((0..3)
                .map(|k| Frame {
                    time: self.now - Duration::minutes(age + 5 * k),
                    grid,
                    dbz: vec![value; grid.width * grid.height],
                    radars_up: vec![],
                    radars_down: vec![],
                    source: self.name,
                })
                .collect())
        }
    }

    fn fake(name: &'static str, age: Option<i64>, covered: bool, now: DateTime<Utc>) -> (Box<dyn RadarSource>, Arc<Mutex<Option<i64>>>, Arc<Mutex<usize>>) {
        let (a, c) = (Arc::new(Mutex::new(age)), Arc::new(Mutex::new(0)));
        (Box::new(Fake { name, age_minutes: a.clone(), covered, calls: c.clone(), now }), a, c)
    }

    const AMS: (f64, f64) = (52.37, 4.89);

    #[test]
    fn uses_the_first_working_source() {
        let now = Utc::now();
        let (opera, _, _) = fake("OPERA", Some(5), true, now);
        let (knmi, _, knmi_calls) = fake("KNMI", Some(5), true, now);
        let mut s = Service::with_sources(vec![opera, knmi]);
        let u = s.update_at(AMS.0, AMS.1, now).unwrap().unwrap();
        assert_eq!(u.source, "OPERA");
        assert!(u.fallback_reason.is_none());
        assert_eq!(*knmi_calls.lock().unwrap(), 0);
        // Same scan again: nothing new.
        assert!(s.update_at(AMS.0, AMS.1, now).unwrap().is_none());
    }

    #[test]
    fn falls_back_when_failing_stale_or_without_data() {
        let now = Utc::now();
        for (age, covered, why) in [(None, true, "connection refused"), (Some(25), true, "25 min old"), (Some(5), false, "no radar data")] {
            let (opera, _, _) = fake("OPERA", age, covered, now);
            let (knmi, _, _) = fake("KNMI", Some(5), true, now);
            let mut s = Service::with_sources(vec![opera, knmi]);
            let u = s.update_at(AMS.0, AMS.1, now).unwrap().unwrap();
            assert_eq!(u.source, "KNMI");
            let reason = u.fallback_reason.unwrap();
            assert!(reason.starts_with("OPERA: ") && reason.contains(why), "{reason}");
        }
    }

    #[test]
    fn retries_the_preferred_source_later() {
        let now = Utc::now();
        let (opera, opera_age, opera_calls) = fake("OPERA", None, true, now);
        let (knmi, _, _) = fake("KNMI", Some(5), true, now);
        let mut s = Service::with_sources(vec![opera, knmi]);
        assert_eq!(s.update_at(AMS.0, AMS.1, now).unwrap().unwrap().source, "KNMI");
        // OPERA recovers, but isn't asked again until the retry time.
        *opera_age.lock().unwrap() = Some(5);
        let _ = s.update_at(AMS.0, AMS.1, now + Duration::minutes(1)).unwrap();
        assert_eq!(*opera_calls.lock().unwrap(), 1);
        let u = s.update_at(AMS.0, AMS.1, now + Duration::minutes(RETRY_MINUTES + 1)).unwrap().unwrap();
        assert_eq!(u.source, "OPERA");
        assert!(u.fallback_reason.is_none());
    }

    /// A source with data only south of a latitude (like OPERA, which has no Italian radars
    /// but covers France).
    struct GapSource {
        now: DateTime<Utc>,
        data_north_of: f64,
    }

    impl RadarSource for GapSource {
        fn name(&self) -> &'static str {
            "OPERA"
        }
        fn latest(&mut self, lat: f64, _: f64, _: usize) -> Result<Vec<Frame>> {
            let grid = Grid::knmi();
            let value = if lat > self.data_north_of { 0.0 } else { f32::NAN };
            Ok((0..3)
                .map(|k| Frame { time: self.now - Duration::minutes(5 + 5 * k), grid, dbz: vec![value; grid.width * grid.height], radars_up: vec![], radars_down: vec![], source: "OPERA" })
                .collect())
        }
    }

    #[test]
    fn moving_away_from_a_gap_uses_the_source_again() {
        // No data at the first location, so the only source is set aside; after moving to
        // a location it does cover, it must be asked again rather than skipped for
        // RETRY_MINUTES with the old location's reason.
        let now = Utc::now();
        let mut s = Service::with_sources(vec![Box::new(GapSource { now, data_north_of: 52.0 })]);
        assert!(s.update_at(51.0, 5.0, now).is_err(), "no data at the first location");
        let u = s.update_at(AMS.0, AMS.1, now + Duration::minutes(1));
        assert!(u.is_ok(), "the source has data at the new location: {:#}", u.err().unwrap());
    }

    #[test]
    fn errors_when_nothing_works() {
        let now = Utc::now();
        let (opera, _, _) = fake("OPERA", None, true, now);
        let (knmi, _, _) = fake("KNMI", Some(40), true, now);
        let mut s = Service::with_sources(vec![opera, knmi]);
        let e = s.update_at(AMS.0, AMS.1, now).err().unwrap().to_string();
        assert!(e.contains("OPERA: connection refused") && e.contains("KNMI: newest scan is 40 min old"), "{e}");
    }
}
