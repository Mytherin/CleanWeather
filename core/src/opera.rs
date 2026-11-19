//! EUMETNET OPERA's European radar composite (CC BY 4.0): maximum reflectivity from
//! ~160 radars, Iceland to Romania, 1 km, every 5 minutes, ~5 minutes after the fact.
//!
//! The composites are cloud-optimised GeoTIFFs (3800 × 4400 pixels in 512-pixel
//! tiles, two float32 bands: dBZ and a quality index) in a public S3 bucket. Only the
//! tiles around the location are downloaded, with HTTP range requests (typically
//! 50–600 KB per scan instead of the whole 3.6 MB), and cached on disk.
//!
//! Finding the newest scan: the MeteoGate API (with an API key, if there is one: 2000
//! requests per hour instead of 200), else listing the S3 buckets. The documented
//! bucket and its backup are both tried, since the service has moved between them.

use crate::radar::{Frame, Grid, Laea, Projection};
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const API: &str = "https://api.meteogate.eu/eu-eumetnet-weather-radar/collections/observations/locations/0-20010-0-OPERA";
/// S3 buckets holding the last 24 hours, in order of preference.
pub const BUCKETS: &[&str] = &["https://s3.waw3-1.cloudferro.com/openradar-24h", "https://object-store.os-api.cci1.ecmwf.int/openradar-24h-backup"];
pub const ATTRIBUTION: &str = "EUMETNET OPERA (CC BY 4.0)";

/// The area covered: 3800 × 4400 km.
const EXTENT_KM: (usize, usize) = (3800, 4400);
/// Half the side of the square fetched around the location, in km. The nowcast needs
/// the rain that will arrive within two hours, and the map shows about this much.
pub const RADIUS_KM: usize = 400;
/// Snap the fetched square to this many km, so small moves of the location reuse it.
const SNAP: usize = 50;
const NODATA: f32 = -9_000_000.0;

/// The full OPERA grid for a product: `+proj=laea +lat_0=55 +lon_0=10 +x_0=1950000
/// +y_0=-2100000 +ellps=WGS84`, top-left corner at plane (0, 0).
pub fn grid(p: Product) -> Grid {
    grid_at(p.pixel_km())
}

/// The OPERA grid at a pixel size (km): 1 or 2 for the products, coarser for the
/// overviews in each file.
pub fn grid_at(km: usize) -> Grid {
    let laea = Laea::new(6_378_137.0, 298.257_223_563, 55.0, 10.0, 1_950_000.0, -2_100_000.0);
    let m = km as f64 * 1000.0;
    Grid::new(EXTENT_KM.0 / km, EXTENT_KM.1 / km, Projection::Laea(laea), (m, -m), (0.0, 0.0))
}

/// All of Europe at 4 km: the composite's overview, ~290 KB per scan.
pub fn europe_grid() -> Grid {
    grid_at(4)
}

/// The square of the grid fetched for a location, or None outside OPERA's grid.
pub fn crop_for(lat: f64, lon: f64, p: Product) -> Option<Grid> {
    let g = grid(p);
    if !g.contains(lat, lon) {
        return None;
    }
    let km = p.pixel_km();
    let (c, r) = g.to_pixel(lat, lon);
    let (radius, snap_px) = (RADIUS_KM / km, SNAP / km);
    let size = 2 * radius;
    let snap = |v: f64, max: usize| {
        let start = ((v - radius as f64) / snap_px as f64).round() as i64 * snap_px as i64;
        start.clamp(0, (max - size) as i64) as usize
    };
    Some(g.crop(snap(c, g.width), snap(r, g.height), size, size))
}

/// Which composite to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    /// Maximum reflectivity in the vertical column (dBZ), every 5 minutes.
    MaxReflectivity,
    /// Instantaneous surface rain rate (mm/h), every 15 minutes.
    RainRate,
}

impl Product {
    fn name(self) -> &'static str {
        match self {
            Product::MaxReflectivity => "DBZH",
            Product::RainRate => "RATE",
        }
    }

    /// Pixel size in km.
    pub fn pixel_km(self) -> usize {
        match self {
            Product::MaxReflectivity => 1,
            Product::RainRate => 2,
        }
    }

    pub fn interval_minutes(self) -> i64 {
        match self {
            Product::MaxReflectivity => 5,
            Product::RainRate => 15,
        }
    }
}

/// The object key of a composite: `2026/11/19/OPERA/COMP/OPERA@20261119T1955@0@DBZH.tiff`.
fn key(t: DateTime<Utc>, p: Product) -> String {
    format!("{}/OPERA/COMP/OPERA@{}@0@{}.tiff", t.format("%Y/%m/%d"), t.format("%Y%m%dT%H%M"), p.name())
}

/// The time in a composite's name (`…OPERA@20261119T1955@0@DBZH…`).
pub fn time_from_key(k: &str) -> Option<DateTime<Utc>> {
    let s = k.split("OPERA@").nth(1)?.get(..13)?;
    NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M").ok().map(|t| t.and_utc())
}

/// The TIFF index of one composite: where each tile's compressed bytes are.
struct Cog {
    width: usize,
    height: usize,
    tiles_across: usize,
    offsets: Vec<u64>,
    lengths: Vec<u64>,
    samples: usize,
}

pub struct Opera {
    agent: ureq::Agent,
    api_key: Option<String>,
    cache: PathBuf,
    headers: Mutex<HashMap<String, Arc<Vec<Cog>>>>,
    newest_cache: Mutex<Option<(std::time::Instant, (String, DateTime<Utc>))>>,
    product: Product,
}

impl Opera {
    pub fn new(api_key: Option<String>, cache: PathBuf) -> Opera {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("CleanWeather/", env!("CARGO_PKG_VERSION")))
            .build();
        Opera { agent, api_key, cache, headers: Mutex::new(HashMap::new()), newest_cache: Mutex::new(None), product: Product::MaxReflectivity }
    }

    pub fn with_product(mut self, product: Product) -> Opera {
        self.product = product;
        self
    }

    /// The newest scan: the bucket it's in and its time. Tries MeteoGate, then lists
    /// the buckets; errors only if every way failed.
    pub fn newest(&self) -> Result<(String, DateTime<Utc>)> {
        let mut errors = Vec::new();
        match self.newest_from_api() {
            // Good enough unless it's older than a scan or two.
            Ok(found) if Utc::now() - found.1 <= Duration::minutes(self.product.interval_minutes() + 8) => return Ok(found),
            Ok(found) => errors.push((Some(found), "MeteoGate: newest scan is old".to_string())),
            Err(e) => errors.push((None, format!("MeteoGate: {e:#}"))),
        }
        let mut best: Option<(String, DateTime<Utc>)> = errors[0].0.clone();
        for bucket in BUCKETS {
            match self.newest_in_bucket(bucket) {
                Ok(t) => {
                    if best.as_ref().is_none_or(|b| t > b.1) {
                        best = Some((bucket.to_string(), t));
                    }
                }
                Err(e) => errors.push((None, format!("{bucket}: {e:#}"))),
            }
        }
        best.ok_or_else(|| anyhow!("no OPERA scans found ({})", errors.iter().map(|e| e.1.as_str()).collect::<Vec<_>>().join("; ")))
    }

    fn newest_from_api(&self) -> Result<(String, DateTime<Utc>)> {
        let now = Utc::now();
        let from = now - Duration::minutes(30);
        let url = format!(
            "{API}?datetime={}/{}&standard_name={}&method=comp&format=ODIM",
            from.format("%Y-%m-%dT%H:%MZ"),
            now.format("%Y-%m-%dT%H:%MZ"),
            self.product.name()
        );
        let mut req = self.agent.get(&url);
        if let Some(k) = &self.api_key {
            req = req.set("apikey", k);
        }
        let json: serde_json::Value = req.call().map_err(http_error)?.into_json()?;
        // The response links to the composite file; its bucket is everything before
        // the date folders.
        let mut hrefs = Vec::new();
        collect_hrefs(&json, &mut hrefs);
        let href = hrefs
            .into_iter()
            .filter(|h| h.contains("/OPERA/COMP/") && h.contains(&format!("@{}", self.product.name())))
            .max_by_key(|h| time_from_key(h))
            .ok_or_else(|| anyhow!("no composite in the response"))?;
        let t = time_from_key(&href).ok_or_else(|| anyhow!("unexpected link {href}"))?;
        let idx = href.find(&format!("/{}/", t.format("%Y/%m/%d"))).ok_or_else(|| anyhow!("unexpected link {href}"))?;
        Ok((href[..idx].to_string(), t))
    }

    fn newest_in_bucket(&self, bucket: &str) -> Result<DateTime<Utc>> {
        let now = Utc::now();
        let mut newest = None;
        // This hour and the one before (one listing each, a dozen keys).
        for h in [now - Duration::hours(1), now] {
            let prefix = format!("{}/OPERA/COMP/OPERA@{}", h.format("%Y/%m/%d"), h.format("%Y%m%dT%H"));
            let url = format!("{bucket}?list-type=2&prefix={prefix}");
            let xml = self.agent.get(&url).call().map_err(http_error)?.into_string()?;
            for k in xml.split("<Key>").skip(1).filter_map(|s| s.split("</Key>").next()) {
                if k.ends_with(&format!("@{}.tiff", self.product.name())) {
                    newest = newest.max(time_from_key(k));
                }
            }
        }
        newest.ok_or_else(|| anyhow!("no recent scans"))
    }

    /// The `n` newest scans (oldest first) as frames of the square around a location.
    pub fn latest(&self, lat: f64, lon: f64, n: usize) -> Result<Vec<Frame>> {
        let crop = crop_for(lat, lon, self.product).ok_or_else(|| anyhow!("location is outside OPERA's coverage"))?;
        self.scans(&crop, n)
    }

    /// The `n` newest scans (oldest first) of all of Europe at 4 km, for the map.
    pub fn europe(&self, n: usize) -> Result<Vec<Frame>> {
        self.scans(&europe_grid(), n)
    }

    /// The newest scan, remembered for a minute so the local and Europe-wide fetches
    /// of one update share one lookup.
    fn newest_cached(&self) -> Result<(String, DateTime<Utc>)> {
        let mut cache = self.newest_cache.lock().unwrap();
        if let Some((at, found)) = cache.as_ref() {
            if at.elapsed() < std::time::Duration::from_secs(60) {
                return Ok(found.clone());
            }
        }
        let found = self.newest()?;
        *cache = Some((std::time::Instant::now(), found.clone()));
        Ok(found)
    }

    fn scans(&self, crop: &Grid, n: usize) -> Result<Vec<Frame>> {
        let crop = *crop;
        let (bucket, newest) = self.newest_cached()?;
        let times: Vec<DateTime<Utc>> = (0..n as i64).map(|k| newest - Duration::minutes(self.product.interval_minutes() * k)).collect();
        let results: Vec<(DateTime<Utc>, Result<Frame>)> = std::thread::scope(|s| {
            let handles: Vec<_> = times.iter().map(|&t| s.spawn({ let bucket = &bucket; move || (t, self.frame(bucket, t, &crop)) })).collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        // Scans that fail are left out: the newest is often announced (and its HDF5
        // version uploaded) a few seconds before its GeoTIFF exists. Whether what's
        // left is recent enough is the caller's call.
        let mut frames = Vec::new();
        let mut newest_error = None;
        for (t, r) in results {
            match r {
                Ok(f) => frames.push(f),
                Err(e) if t == newest => newest_error = Some(e.context(format!("OPERA scan {}", t.format("%H:%M")))),
                Err(_) => {}
            }
        }
        if frames.is_empty() {
            return Err(newest_error.unwrap_or_else(|| anyhow!("no OPERA scans could be read")));
        }
        frames.sort_by_key(|f| f.time);
        crate::knmi::prune(&self.cache, Duration::hours(3));
        Ok(frames)
    }

    fn header(&self, bucket: &str, key: &str) -> Result<Arc<Vec<Cog>>> {
        let id = format!("{bucket}/{key}");
        if let Some(h) = self.headers.lock().unwrap().get(&id) {
            return Ok(h.clone());
        }
        // The index is at the start of the file (GDAL's COG layout); 16 KB covers it.
        let mut bytes = self.range(&id, 0, 16 * 1024)?;
        let cog = match parse_cog(&bytes, self.product) {
            Ok(c) => c,
            Err(NeedMore(n)) => {
                bytes = self.range(&id, 0, n)?;
                parse_cog(&bytes, self.product).map_err(|_| anyhow!("unreadable TIFF index"))?
            }
        };
        let cog = Arc::new(cog);
        let mut map = self.headers.lock().unwrap();
        if map.len() > 64 {
            map.clear();
        }
        map.insert(id, cog.clone());
        Ok(cog)
    }

    fn range(&self, url: &str, start: u64, len: u64) -> Result<Vec<u8>> {
        let r = self
            .agent
            .get(url)
            .set("Range", &format!("bytes={start}-{}", start + len - 1))
            .call()
            .map_err(http_error)?;
        let mut b = Vec::with_capacity(len as usize);
        r.into_reader().take(len).read_to_end(&mut b)?;
        Ok(b)
    }

    /// The compressed bytes of tiles (tx, ty) for one scan, from the disk cache or the
    /// bucket. Tiles next to each other in a row are fetched with one request.
    fn tiles(&self, bucket: &str, t: DateTime<Utc>, cog: &Cog, wanted: &[(usize, usize)]) -> Result<HashMap<(usize, usize), Vec<u8>>> {
        let stamp = t.format("%Y%m%d%H%M").to_string();
        let path = |tx: usize, ty: usize| self.cache.join(format!("opera_{}_{stamp}_{}_{tx}_{ty}.z", self.product.name(), cog.width));
        let mut out = HashMap::new();
        let mut missing = Vec::new();
        for &(tx, ty) in wanted {
            match std::fs::read(path(tx, ty)) {
                Ok(b) => {
                    out.insert((tx, ty), b);
                }
                Err(_) => missing.push((tx, ty)),
            }
        }
        missing.sort_by_key(|&(tx, ty)| (ty, tx));
        let url = format!("{bucket}/{}", key(t, self.product));
        let _ = std::fs::create_dir_all(&self.cache);
        let mut i = 0;
        while i < missing.len() {
            // A run of tiles stored back to back.
            let mut j = i;
            let idx = |(tx, ty): (usize, usize)| ty * cog.tiles_across + tx;
            while j + 1 < missing.len() && {
                let (a, b) = (idx(missing[j]), idx(missing[j + 1]));
                b == a + 1 && cog.offsets[b] >= cog.offsets[a] + cog.lengths[a] && cog.offsets[b] - (cog.offsets[a] + cog.lengths[a]) < 64
            } {
                j += 1;
            }
            let (first, last) = (idx(missing[i]), idx(missing[j]));
            let start = cog.offsets[first];
            let end = cog.offsets[last] + cog.lengths[last];
            let bytes = self.range(&url, start, end - start)?;
            if (bytes.len() as u64) < end - start {
                bail!("short read from {url}");
            }
            for &(tx, ty) in &missing[i..=j] {
                let k = idx((tx, ty));
                let off = (cog.offsets[k] - start) as usize;
                let tile = bytes[off..off + cog.lengths[k] as usize].to_vec();
                let _ = std::fs::write(path(tx, ty), &tile);
                out.insert((tx, ty), tile);
            }
            i = j + 1;
        }
        Ok(out)
    }

    /// One scan of an area (a crop of the grid at the product's resolution, or of a
    /// coarser overview such as [`europe_grid`]), from a bucket.
    pub fn frame(&self, bucket: &str, t: DateTime<Utc>, crop: &Grid) -> Result<Frame> {
        let km = crop.pixel_km().round().max(1.0) as usize;
        let full = grid_at(km);
        // The crop's position on the full grid.
        let (c0f, r0f) = full.to_pixel(crop.to_latlon(0.5, 0.5).0, crop.to_latlon(0.5, 0.5).1);
        let (c0, r0) = ((c0f - 0.5).round() as usize, (r0f - 0.5).round() as usize);
        let (w, h) = (crop.width.min(full.width - c0), crop.height.min(full.height - r0));
        const T: usize = 512;

        let levels = self.header(bucket, &key(t, self.product))?;
        let cog = levels.iter().find(|c| c.width == full.width).ok_or_else(|| anyhow!("no {km} km level in the composite"))?;
        let wanted: Vec<(usize, usize)> = (r0 / T..=((r0 + h - 1) / T).min(cog.height.div_ceil(T) - 1))
            .flat_map(|ty| (c0 / T..=((c0 + w - 1) / T).min(cog.tiles_across - 1)).map(move |tx| (tx, ty)))
            .collect();
        let compressed = self.tiles(bucket, t, cog, &wanted)?;
        let (w, h) = (crop.width, crop.height);
        let mut dbz = vec![f32::NAN; w * h];
        for (&(tx, ty), z) in &compressed {
            let mut raw = Vec::with_capacity(T * T * cog.samples * 4);
            flate2::read::ZlibDecoder::new(&z[..]).read_to_end(&mut raw).context("inflating tile")?;
            if raw.len() < T * T * cog.samples * 4 {
                bail!("tile ({tx}, {ty}) is too short");
            }
            for y in 0..T {
                let row = ty * T + y;
                if row < r0 || row >= r0 + h {
                    continue;
                }
                for x in 0..T {
                    let col = tx * T + x;
                    if col < c0 || col >= c0 + w {
                        continue;
                    }
                    let o = ((y * T + x) * cog.samples) * 4;
                    let v = f32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
                    // NaN is "undetect": the radars see no echo (dry). The fill value
                    // means no radar covers the pixel.
                    dbz[(row - r0) * w + (col - c0)] = if v.is_nan() || (v < NODATA && v > -8_900_000.0) {
                        -32.0
                    } else if v <= NODATA {
                        f32::NAN
                    } else if self.product == Product::RainRate {
                        // mm/h back to reflectivity (Z = 200 R^1.6), so the nowcast and
                        // the display treat both products alike.
                        if v <= 0.0 { -32.0 } else { 10.0 * (200.0 * v.powf(1.6)).log10() }
                    } else {
                        v
                    };
                }
            }
        }
        Ok(Frame { time: t, grid: *crop, dbz, radars_up: vec![], radars_down: vec![], source: "OPERA" })
    }
}

fn http_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(429, _) => anyhow!("rate limit reached"),
        ureq::Error::Status(code, _) => anyhow!("HTTP {code}"),
        e => anyhow!("{e}"),
    }
}

fn collect_hrefs(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                match (k.as_str(), v) {
                    ("href", serde_json::Value::String(s)) => out.push(s.clone()),
                    _ => collect_hrefs(v, out),
                }
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| collect_hrefs(v, out)),
        _ => {}
    }
}

/// Not enough bytes were fetched to read the index; this many are needed.
struct NeedMore(u64);

/// Read all directories of a little-endian classic TIFF: the full image, then GDAL's
/// overviews (each half the size of the one before), largest first.
fn parse_cog(b: &[u8], p: Product) -> std::result::Result<Vec<Cog>, NeedMore> {
    if b.get(0..4) != Some(&[0x49, 0x49, 0x2a, 0x00]) {
        return Err(NeedMore(0));
    }
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let full = grid(p);
    let mut levels = Vec::new();
    let mut ifd = u32_at(4).ok_or(NeedMore(0))? as usize;
    while ifd != 0 && levels.len() < 8 {
        let (cog, next) = parse_ifd(b, ifd)?;
        // The first must be the full image; overviews must keep the aspect ratio.
        let expected_w = full.width >> levels.len();
        if cog.width != expected_w && cog.width != expected_w + 1 {
            return Err(NeedMore(0));
        }
        levels.push(cog);
        ifd = next;
    }
    if levels.is_empty() {
        return Err(NeedMore(0));
    }
    Ok(levels)
}

/// One TIFF directory: the tile layout and where the tiles are, checking it's the
/// format we decode (deflate, float32, no predictor). Also returns the next directory.
fn parse_ifd(b: &[u8], ifd: usize) -> std::result::Result<(Cog, usize), NeedMore> {
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let bad = NeedMore(0);
    let n = u16_at(ifd).ok_or(NeedMore(ifd as u64 + 2))? as usize;
    if b.len() < ifd + 2 + 12 * n + 4 {
        return Err(NeedMore((ifd + 2 + 12 * n + 4) as u64));
    }
    let next = u32_at(ifd + 2 + 12 * n).unwrap() as usize;
    let mut tags: HashMap<u16, (u16, u32, usize)> = HashMap::new();
    for i in 0..n {
        let e = ifd + 2 + 12 * i;
        tags.insert(u16_at(e).unwrap(), (u16_at(e + 2).unwrap(), u32_at(e + 4).unwrap(), e + 8));
    }
    // A tag's values (SHORT or LONG), stored inline or at an offset.
    let values = |tag: u16| -> std::result::Result<Vec<u64>, NeedMore> {
        let &(ty, count, at) = tags.get(&tag).ok_or(NeedMore(0))?;
        let size = match ty {
            3 => 2,
            4 => 4,
            _ => return Err(NeedMore(0)),
        };
        let total = size * count as usize;
        let start = if total <= 4 { at } else { u32_at(at).unwrap() as usize };
        if b.len() < start + total {
            return Err(NeedMore((start + total) as u64));
        }
        Ok((0..count as usize).map(|i| if size == 2 { u16_at(start + 2 * i).unwrap() as u64 } else { u32_at(start + 4 * i).unwrap() as u64 }).collect())
    };
    let one = |tag: u16, default: u64| values(tag).map(|v| v[0]).or_else(|e| if e.0 == 0 { Ok(default) } else { Err(e) });
    let (width, height) = (one(256, 0)? as usize, one(257, 0)? as usize);
    let (tw, th) = (one(322, 0)? as usize, one(323, 0)? as usize);
    let samples = one(277, 1)? as usize;
    let ok = width > 0 && height > 0 && tw == 512 && th == 512
        && one(259, 1)? == 8 // deflate
        && one(317, 1)? == 1 // no predictor
        && one(284, 1)? == 1 // samples interleaved
        && one(258, 0)? == 32
        && one(339, 1)? == 3; // IEEE float
    if !ok {
        return Err(bad);
    }
    let offsets = values(324)?;
    let lengths = values(325)?;
    let across = width.div_ceil(tw);
    if offsets.len() != across * height.div_ceil(th) || lengths.len() != offsets.len() {
        return Err(bad);
    }
    Ok((Cog { width, height, tiles_across: across, offsets, lengths, samples }, next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn grid_matches_operas_corners() {
        // Corner coordinates from a composite's metadata (pixel corners).
        let g = grid(Product::MaxReflectivity);
        let corners = [
            ((31.7462153182675, -10.4345768386404), (0.0, 4400.0)),
            ((67.0228327624372, -39.5357864125034), (0.0, 0.0)),
            ((67.6210371071631, 57.8119647501499), (3800.0, 0.0)),
            ((31.987650276733, 29.421038635578), (3800.0, 4400.0)),
        ];
        for ((lat, lon), (c, r)) in corners {
            let (col, row) = g.to_pixel(lat, lon);
            assert!((col - c).abs() < 0.05 && (row - r).abs() < 0.05, "({lat},{lon}) -> ({col},{row}), want ({c},{r})");
            let (la, lo) = g.to_latlon(c, r);
            assert!((la - lat).abs() < 1e-5 && (lo - lon).abs() < 1e-5, "inverse ({c},{r}) -> ({la},{lo})");
        }
    }

    #[test]
    fn crops_and_keys() {
        let crop = crop_for(52.37, 4.89, Product::MaxReflectivity).unwrap();
        assert_eq!((crop.width, crop.height), (800, 800));
        let (c, r) = crop.to_pixel(52.37, 4.89);
        assert!((c - 400.0).abs() < 60.0 && (r - 400.0).abs() < 60.0, "Amsterdam near the middle: ({c},{r})");
        assert!(crop_for(10.0, 10.0, Product::MaxReflectivity).is_none(), "Africa isn't covered");
        let t = time_from_key("2026/11/19/OPERA/COMP/OPERA@20261119T1955@0@DBZH.tiff").unwrap();
        assert_eq!(key(t, Product::MaxReflectivity), "2026/11/19/OPERA/COMP/OPERA@20261119T1955@0@DBZH.tiff");
        assert_eq!(t.minute(), 55);
    }

    #[test]
    fn rejects_non_tiff() {
        assert!(parse_cog(b"not a tiff at all", Product::MaxReflectivity).is_err());
    }
}
