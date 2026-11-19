//! Rain nowcasting by extrapolation, as Buienradar-style apps do for the next two hours:
//!
//! 1. **Motion**: compare the latest scan with one ~15 minutes older, block by block
//!    (32 km blocks), and find the shift that best lines the rain up (coarse search on
//!    a 4× downsampled grid, then a fine search with sub-pixel refinement).
//! 2. **Smooth** the block vectors into one motion field; where there's no rain to
//!    track, fall back to the average motion.
//! 3. **Advect**: for each pixel and lead time, trace backwards along the motion field
//!    and take the latest scan's value from there (semi-Lagrangian extrapolation).
//!    Each lead time samples the original scan, so the field doesn't blur step by step.
//!
//! Rain cells growing or dying isn't modelled; the point forecast widens the area it
//! averages over with lead time to reflect that the position gets less certain.

use crate::radar::{Frame, Grid, RAIN_DBZ, rain_rate};
use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};

pub const STEP_MINUTES: i64 = 5;
/// Two hours ahead.
pub const DEFAULT_STEPS: usize = 24;

const COARSE: usize = 4; // downsampling factor for the coarse motion search
const BLOCK: usize = 32; // block size in pixels (km)
const STRIDE: usize = 16;
const MAX_SPEED_KMH: f64 = 130.0;

/// A reflectivity field stored compactly: 0 = no data, else dBZ = code / 2 − 32.
#[derive(Clone)]
pub struct Field(pub Vec<u8>);

impl Field {
    pub fn from_dbz(dbz: &[f32]) -> Field {
        Field(dbz.iter().map(|&d| encode(d)).collect())
    }
    #[inline]
    pub fn dbz(&self, i: usize) -> f32 {
        decode(self.0[i])
    }
}

#[inline]
fn encode(d: f32) -> u8 {
    if d.is_nan() { 0 } else { ((d + 32.0) * 2.0).round().clamp(1.0, 255.0) as u8 }
}
#[inline]
fn decode(c: u8) -> f32 {
    if c == 0 { f32::NAN } else { c as f32 * 0.5 - 32.0 }
}

/// Motion on a coarse grid of block centres, in pixels per 5-minute step.
#[derive(Clone)]
pub struct Motion {
    pub nx: usize,
    pub ny: usize,
    pub vx: Vec<f32>,
    pub vy: Vec<f32>,
    /// Which blocks had enough rain to measure their own motion.
    pub measured: Vec<bool>,
    /// The grid's pixel size, to turn pixels per step into km/h.
    pixel_km: f32,
}

impl Motion {
    fn zero(grid: &Grid) -> Motion {
        let (nx, ny) = block_counts(grid);
        Motion { nx, ny, vx: vec![0.0; nx * ny], vy: vec![0.0; nx * ny], measured: vec![false; nx * ny], pixel_km: grid.pixel_km() as f32 }
    }

    /// Block centre of block (bx, by) in pixel coordinates.
    fn centre(b: usize) -> f32 {
        (b * STRIDE + BLOCK / 2) as f32
    }

    /// Velocity at a pixel position, bilinear between block centres.
    pub fn at(&self, x: f32, y: f32) -> (f32, f32) {
        let fx = ((x - Self::centre(0)) / STRIDE as f32).clamp(0.0, (self.nx - 1) as f32);
        let fy = ((y - Self::centre(0)) / STRIDE as f32).clamp(0.0, (self.ny - 1) as f32);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.nx - 1), (y0 + 1).min(self.ny - 1));
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let lerp = |v: &[f32]| {
            let a = v[y0 * self.nx + x0] * (1.0 - tx) + v[y0 * self.nx + x1] * tx;
            let b = v[y1 * self.nx + x0] * (1.0 - tx) + v[y1 * self.nx + x1] * tx;
            a * (1.0 - ty) + b * ty
        };
        (lerp(&self.vx), lerp(&self.vy))
    }

    /// Average speed (km/h) and direction the rain is moving towards (degrees, 0 = north).
    pub fn mean_motion(&self) -> Option<(f32, f32)> {
        let n = self.measured.iter().filter(|&&m| m).count();
        if n == 0 {
            return None;
        }
        let (mut sx, mut sy) = (0.0, 0.0);
        for i in 0..self.vx.len() {
            if self.measured[i] {
                sx += self.vx[i];
                sy += self.vy[i];
            }
        }
        let (vx, vy) = (sx / n as f32, sy / n as f32);
        let kmh = vx.hypot(vy) * self.pixel_km * (60 / STEP_MINUTES) as f32;
        let towards = vx.atan2(-vy).to_degrees().rem_euclid(360.0);
        Some((kmh, towards))
    }
}

fn block_counts(grid: &Grid) -> (usize, usize) {
    ((grid.width - BLOCK) / STRIDE + 1, (grid.height - BLOCK) / STRIDE + 1)
}

/// The signal used for matching: rain only, with weak echoes and noise flattened to 0.
fn match_field(f: &Frame) -> Vec<f32> {
    f.dbz.iter().map(|&d| if d >= RAIN_DBZ { d - RAIN_DBZ + 1.0 } else { 0.0 }).collect()
}

fn downsample(v: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let (cw, ch) = (w / COARSE, h / COARSE);
    let mut out = vec![0.0; cw * ch];
    for y in 0..ch * COARSE {
        for x in 0..cw * COARSE {
            out[(y / COARSE) * cw + x / COARSE] += v[y * w + x];
        }
    }
    let n = (COARSE * COARSE) as f32;
    out.iter_mut().for_each(|o| *o /= n);
    (out, cw, ch)
}

/// Mean squared difference between `cur` in block (x0, y0, size) and `prev` shifted by (dx, dy).
fn block_cost(cur: &[f32], prev: &[f32], w: usize, h: usize, x0: usize, y0: usize, size: usize, dx: i32, dy: i32) -> f32 {
    let (mut sum, mut n) = (0.0f32, 0u32);
    for y in y0..y0 + size {
        let py = y as i32 - dy;
        if py < 0 || py >= h as i32 {
            continue;
        }
        let (row, prow) = (y * w, py as usize * w);
        for x in x0..x0 + size {
            let px = x as i32 - dx;
            if px < 0 || px >= w as i32 {
                continue;
            }
            let d = cur[row + x] - prev[prow + px as usize];
            sum += d * d;
            n += 1;
        }
    }
    if n < (size * size / 2) as u32 { f32::INFINITY } else { sum / n as f32 }
}

/// Sub-pixel offset of a minimum from three costs around it.
fn parabola(cm: f32, c0: f32, cp: f32) -> f32 {
    let denom = cm - 2.0 * c0 + cp;
    if denom.is_finite() && denom > 1e-6 { (0.5 * (cm - cp) / denom).clamp(-0.5, 0.5) } else { 0.0 }
}

/// Estimate motion between `prev` and `cur`, `steps` 5-minute steps apart.
pub fn estimate_motion(prev: &Frame, cur: &Frame, steps: f32) -> Motion {
    let grid = &cur.grid;
    let (w, h) = (grid.width, grid.height);
    let (a, b) = (match_field(cur), match_field(prev));
    let (ca, cw, ch) = downsample(&a, w, h);
    let (cb, _, _) = downsample(&b, w, h);
    let max_px = (MAX_SPEED_KMH / 60.0 * STEP_MINUTES as f64 * steps as f64 / grid.pixel_km()).ceil() as i32;
    let cs = (max_px + COARSE as i32 - 1) / COARSE as i32;

    let mut m = Motion::zero(grid);
    let mut raw = Vec::new(); // (index, vx, vy)
    for by in 0..m.ny {
        for bx in 0..m.nx {
            let (x0, y0) = (bx * STRIDE, by * STRIDE);
            // Enough rain in the block to track? (≥ 8% of pixels, in both scans)
            let rainy = |f: &[f32]| (y0..y0 + BLOCK).flat_map(|y| (x0..x0 + BLOCK).map(move |x| (x, y))).filter(|&(x, y)| f[y * w + x] > 0.0).count();
            let need = BLOCK * BLOCK * 8 / 100;
            if rainy(&a) < need {
                continue;
            }
            // Coarse search.
            let (cx0, cy0, csz) = (x0 / COARSE, y0 / COARSE, BLOCK / COARSE);
            let mut best = (f32::INFINITY, 0, 0);
            for dy in -cs..=cs {
                for dx in -cs..=cs {
                    let c = block_cost(&ca, &cb, cw, ch, cx0, cy0, csz, dx, dy);
                    // Prefer smaller shifts on ties (still-standing clutter, flat fields).
                    let c = c * (1.0 + 0.002 * (dx * dx + dy * dy) as f32);
                    if c < best.0 {
                        best = (c, dx, dy);
                    }
                }
            }
            if !best.0.is_finite() {
                continue;
            }
            // Fine search around the coarse answer.
            let (fx, fy) = (best.1 * COARSE as i32, best.2 * COARSE as i32);
            let r = COARSE as i32;
            let mut fine = (f32::INFINITY, fx, fy);
            for dy in fy - r..=fy + r {
                for dx in fx - r..=fx + r {
                    let c = block_cost(&a, &b, w, h, x0, y0, BLOCK, dx, dy);
                    if c < fine.0 {
                        fine = (c, dx, dy);
                    }
                }
            }
            let (_, dx, dy) = fine;
            if prev_rainy_enough(&b, w, x0, y0, dx, dy, h) < need / 2 {
                continue;
            }
            let cost = |dx, dy| block_cost(&a, &b, w, h, x0, y0, BLOCK, dx, dy);
            let sx = dx as f32 + parabola(cost(dx - 1, dy), fine.0, cost(dx + 1, dy));
            let sy = dy as f32 + parabola(cost(dx, dy - 1), fine.0, cost(dx, dy + 1));
            raw.push((by * m.nx + bx, sx / steps, sy / steps));
        }
    }
    smooth(&mut m, &raw);
    m
}

fn prev_rainy_enough(b: &[f32], w: usize, x0: usize, y0: usize, dx: i32, dy: i32, h: usize) -> usize {
    let mut n = 0;
    for y in y0..y0 + BLOCK {
        for x in x0..x0 + BLOCK {
            let (px, py) = (x as i32 - dx, y as i32 - dy);
            if px >= 0 && py >= 0 && (px as usize) < w && (py as usize) < h && b[py as usize * w + px as usize] > 0.0 {
                n += 1;
            }
        }
    }
    n
}

/// Normalised Gaussian smoothing of the measured vectors over the block grid, with a
/// weak pull towards the robust (median) overall motion so empty areas get that.
fn smooth(m: &mut Motion, raw: &[(usize, f32, f32)]) {
    if raw.is_empty() {
        return;
    }
    let median = |mut v: Vec<f32>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    let gx = median(raw.iter().map(|r| r.1).collect());
    let gy = median(raw.iter().map(|r| r.2).collect());
    // Drop outliers: vectors far from the overall motion that no neighbour supports
    // are usually mismatches between different rain cells.
    let sigma = 2.0f32; // blocks (~32 km)
    let nx = m.nx;
    for i in 0..m.vx.len() {
        let (ix, iy) = ((i % nx) as f32, (i / nx) as f32);
        let (mut sx, mut sy, mut sw) = (gx * 0.05, gy * 0.05, 0.05);
        for &(j, vx, vy) in raw {
            let (jx, jy) = ((j % nx) as f32, (j / nx) as f32);
            let d2 = (ix - jx).powi(2) + (iy - jy).powi(2);
            if d2 > 9.0 * sigma * sigma {
                continue;
            }
            let wgt = (-d2 / (2.0 * sigma * sigma)).exp();
            sx += wgt * vx;
            sy += wgt * vy;
            sw += wgt;
        }
        m.vx[i] = sx / sw;
        m.vy[i] = sy / sw;
    }
    for &(j, _, _) in raw {
        m.measured[j] = true;
    }
}

/// Bilinear sample of a field at a fractional pixel position; NaN outside or near no-data.
fn sample(field: &Field, w: usize, h: usize, x: f32, y: f32) -> f32 {
    // Pixel centres are at +0.5.
    let (fx, fy) = (x - 0.5, y - 0.5);
    if fx < 0.0 || fy < 0.0 || fx > (w - 1) as f32 || fy > (h - 1) as f32 {
        return f32::NAN;
    }
    let (x0, y0) = (fx as usize, fy as usize);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let v = [field.dbz(y0 * w + x0), field.dbz(y0 * w + x1), field.dbz(y1 * w + x0), field.dbz(y1 * w + x1)];
    if v.iter().any(|d| d.is_nan()) {
        // Nearest neighbour at data edges.
        let (nx, ny) = (if tx < 0.5 { x0 } else { x1 }, if ty < 0.5 { y0 } else { y1 });
        return field.dbz(ny * w + nx);
    }
    // Interpolate rain-ness, not dBZ below the threshold (avoids smearing noise).
    let a = v[0] * (1.0 - tx) + v[1] * tx;
    let b = v[2] * (1.0 - tx) + v[3] * tx;
    a * (1.0 - ty) + b * ty
}

/// Extrapolate `latest` along `motion` for `steps` 5-minute steps.
pub fn extrapolate(latest: &Frame, motion: &Motion, steps: usize) -> Vec<Field> {
    let (w, h) = (latest.grid.width, latest.grid.height);
    let base = Field::from_dbz(&latest.dbz);
    let mut px: Vec<f32> = (0..w * h).map(|i| (i % w) as f32 + 0.5).collect();
    let mut py: Vec<f32> = (0..w * h).map(|i| (i / w) as f32 + 0.5).collect();
    let mut out = Vec::with_capacity(steps);
    for _ in 0..steps {
        let mut f = vec![0u8; w * h];
        for i in 0..w * h {
            // Midpoint rule: velocity half a step back, for curved trajectories.
            let (vx, vy) = motion.at(px[i], py[i]);
            let (mx, my) = motion.at(px[i] - vx * 0.5, py[i] - vy * 0.5);
            px[i] -= mx;
            py[i] -= my;
            f[i] = encode(sample(&base, w, h, px[i], py[i]));
        }
        out.push(Field(f));
    }
    out
}

/// A computed nowcast: the latest scan and the extrapolated fields after it.
pub struct Nowcast {
    pub grid: Grid,
    /// Time of the latest radar scan the forecast starts from.
    pub issued: DateTime<Utc>,
    /// Past scans, oldest first, ending with the latest (`issued`).
    pub observed: Vec<(DateTime<Utc>, Field)>,
    /// `forecast[k]` is valid at `issued + 5·(k+1)` minutes.
    pub forecast: Vec<Field>,
    pub motion: Motion,
    pub radars_down: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub time: DateTime<Utc>,
    /// Mean rain rate (mm/h) around the location; None if the radar doesn't cover it.
    pub mm_per_hour: Option<f32>,
    /// Share of the area around the location with rain (≥ 0.1 mm/h): a rough chance of rain.
    pub chance: f32,
    pub observed: bool,
}

/// One-line outlook for a location.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outlook {
    /// No rain expected within the forecast.
    Dry { until: DateTime<Utc> },
    /// Dry now, rain from `at`.
    RainFrom { at: DateTime<Utc>, peak: f32 },
    /// Raining now; stops at `until` if that's within the forecast.
    Raining { until: Option<DateTime<Utc>>, peak: f32 },
    /// The location is outside the radar's coverage.
    NoData,
}

/// Rain at a point at or above this rate (mm/h) counts as "rain" for the outlook.
pub const RAIN_MM_H: f32 = 0.1;

impl Nowcast {
    /// Compute a nowcast from scans (any order). Needs at least two, ideally covering
    /// 15 minutes or more.
    pub fn compute(frames: &[Frame], steps: usize) -> Result<Nowcast> {
        let mut fr: Vec<&Frame> = frames.iter().collect();
        fr.sort_by_key(|f| f.time);
        fr.dedup_by_key(|f| f.time);
        let Some(&latest) = fr.last() else { bail!("no radar scans") };
        // The scan closest to 15 minutes before the latest one, at least 5 minutes before.
        let prev = fr
            .iter()
            .filter(|f| f.time < latest.time && latest.time - f.time <= Duration::minutes(30))
            .min_by_key(|f| ((latest.time - f.time).num_minutes() - 15).abs());
        let motion = match prev {
            Some(p) => {
                let steps = (latest.time - p.time).num_seconds() as f32 / (STEP_MINUTES * 60) as f32;
                estimate_motion(p, latest, steps)
            }
            None => Motion::zero(&latest.grid),
        };
        let forecast = extrapolate(latest, &motion, steps);
        Ok(Nowcast {
            grid: latest.grid,
            issued: latest.time,
            observed: fr.iter().map(|f| (f.time, Field::from_dbz(&f.dbz))).collect(),
            forecast,
            motion,
            radars_down: latest.radars_down.clone(),
        })
    }

    pub fn forecast_time(&self, k: usize) -> DateTime<Utc> {
        self.issued + Duration::minutes(STEP_MINUTES * (k as i64 + 1))
    }

    /// Rain at a location: past scans, then the forecast.
    pub fn point(&self, lat: f64, lon: f64) -> Vec<Sample> {
        let (col, row) = self.grid.to_pixel(lat, lon);
        let mut out = Vec::new();
        // Radii in km, in pixels of this grid (at least about one pixel).
        let px = |km: f64| (km / self.grid.pixel_km()).max(0.75);
        for (t, f) in &self.observed {
            out.push(self.area_sample(f, col, row, px(1.5), *t, true));
        }
        for (k, f) in self.forecast.iter().enumerate() {
            let minutes = (k as f64 + 1.0) * STEP_MINUTES as f64;
            // Position uncertainty grows with lead time: ~1.5 km now, ~6.5 km at 2 h.
            let radius = px(1.5 + minutes / 24.0);
            out.push(self.area_sample(f, col, row, radius, self.forecast_time(k), false));
        }
        out
    }

    fn area_sample(&self, f: &Field, col: f64, row: f64, radius: f64, time: DateTime<Utc>, observed: bool) -> Sample {
        let (w, h) = (self.grid.width as i64, self.grid.height as i64);
        let r = radius.ceil() as i64;
        let (c0, r0) = (col.floor() as i64, row.floor() as i64);
        let (mut sum, mut n, mut wet) = (0.0f32, 0u32, 0u32);
        for y in r0 - r..=r0 + r {
            for x in c0 - r..=c0 + r {
                if x < 0 || y < 0 || x >= w || y >= h {
                    continue;
                }
                let (dx, dy) = (x as f64 + 0.5 - col, y as f64 + 0.5 - row);
                if dx * dx + dy * dy > radius * radius {
                    continue;
                }
                let d = f.dbz((y * w + x) as usize);
                if d.is_nan() {
                    continue;
                }
                let rate = rain_rate(d);
                sum += rate;
                n += 1;
                if rate >= RAIN_MM_H {
                    wet += 1;
                }
            }
        }
        if n == 0 {
            return Sample { time, mm_per_hour: None, chance: 0.0, observed };
        }
        Sample { time, mm_per_hour: Some(sum / n as f32), chance: wet as f32 / n as f32, observed }
    }
}

/// Summarise a point series (from [`Nowcast::point`]) from the latest observation on.
pub fn outlook(samples: &[Sample]) -> Outlook {
    let Some(start) = samples.iter().rposition(|s| s.observed) else { return Outlook::NoData };
    let ahead = &samples[start..];
    if ahead.iter().all(|s| s.mm_per_hour.is_none()) {
        return Outlook::NoData;
    }
    let wet = |s: &Sample| s.mm_per_hour.unwrap_or(0.0) >= RAIN_MM_H;
    let peak = ahead.iter().filter_map(|s| s.mm_per_hour).fold(0.0, f32::max);
    if wet(&ahead[0]) {
        // Raining now: when does it stop (two dry steps in a row)?
        let until = ahead.windows(2).find(|w| !wet(&w[0]) && !wet(&w[1])).map(|w| w[0].time);
        Outlook::Raining { until, peak }
    } else if let Some(s) = ahead.iter().find(|s| wet(s)) {
        Outlook::RainFrom { at: s.time, peak }
    } else {
        Outlook::Dry { until: ahead.last().unwrap().time }
    }
}

/// Words for a rain rate, as Dutch rain apps use them.
pub fn intensity_label(mm_h: f32) -> &'static str {
    match mm_h {
        r if r < RAIN_MM_H => "dry",
        r if r < 1.0 => "light rain",
        r if r < 5.0 => "moderate rain",
        r if r < 20.0 => "heavy rain",
        _ => "very heavy rain",
    }
}

/// Skill of a forecast against what was observed, over the whole radar area.
#[derive(Debug, Clone, Copy)]
pub struct Skill {
    /// Critical success index for rain ≥ 1 mm/h: hits / (hits + misses + false alarms).
    pub csi: f32,
    /// Mean absolute error of rain rate (mm/h) over pixels where either has rain.
    pub mae: f32,
}

pub fn skill(forecast: &Field, observed: &Field) -> Skill {
    let (mut hits, mut misses, mut fa) = (0u32, 0u32, 0u32);
    let (mut err, mut n) = (0.0f32, 0u32);
    for i in 0..forecast.0.len() {
        let (f, o) = (forecast.dbz(i), observed.dbz(i));
        if f.is_nan() || o.is_nan() {
            continue;
        }
        let (rf, ro) = (rain_rate(f), rain_rate(o));
        match (rf >= 1.0, ro >= 1.0) {
            (true, true) => hits += 1,
            (false, true) => misses += 1,
            (true, false) => fa += 1,
            _ => {}
        }
        if rf >= RAIN_MM_H || ro >= RAIN_MM_H {
            err += (rf - ro).abs();
            n += 1;
        }
    }
    let total = hits + misses + fa;
    Skill { csi: if total > 0 { hits as f32 / total as f32 } else { f32::NAN }, mae: if n > 0 { err / n as f32 } else { f32::NAN } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob_frame(cx: f32, cy: f32, minutes: i64) -> Frame {
        let grid = Grid::knmi();
        let (w, h) = (grid.width, grid.height);
        let mut dbz = vec![0.0f32; w * h];
        // A few rain cells of different sizes so block matching has texture.
        let cells = [(0.0, 0.0, 25.0, 40.0), (60.0, 30.0, 12.0, 35.0), (-50.0, 40.0, 18.0, 30.0), (20.0, -60.0, 9.0, 45.0)];
        for y in 0..h {
            for x in 0..w {
                let mut v: f32 = 0.0;
                for (ox, oy, r, peak) in cells {
                    let d2 = (x as f32 - cx - ox).powi(2) + (y as f32 - cy - oy).powi(2);
                    v = v.max(peak * (-d2 / (2.0 * r * r)).exp());
                }
                dbz[y * w + x] = v;
            }
        }
        Frame { time: DateTime::from_timestamp(1_800_000_000 + minutes * 60, 0).unwrap(), grid, dbz, radars_up: vec![], radars_down: vec![], source: "test" }
    }

    #[test]
    fn finds_motion_and_extrapolates() {
        // Rain moving 3 px east and 1.5 px south per 5 minutes (≈ 40 km/h).
        let (vx, vy) = (3.0, 1.5);
        let frames: Vec<Frame> = (0..4).map(|k| blob_frame(300.0 + vx * k as f32, 350.0 + vy * k as f32, 5 * k as i64)).collect();
        let nc = Nowcast::compute(&frames, 12).unwrap();
        let (mx, my) = nc.motion.at(300.0, 350.0);
        assert!((mx - vx).abs() < 0.3 && (my - vy).abs() < 0.3, "motion ({mx}, {my})");

        // Extrapolated 60 minutes should match the truth there.
        let truth = blob_frame(300.0 + vx * 15.0, 350.0 + vy * 15.0, 75);
        let s = skill(&nc.forecast[11], &Field::from_dbz(&truth.dbz));
        assert!(s.csi > 0.9, "csi {}", s.csi);
        // And beat persistence (no motion) clearly.
        let p = skill(&Field::from_dbz(&frames[3].dbz), &Field::from_dbz(&truth.dbz));
        assert!(s.csi > p.csi + 0.2, "csi {} vs persistence {}", s.csi, p.csi);
    }

    #[test]
    fn outlook_rain_coming() {
        let t = |m: i64| DateTime::from_timestamp(1_800_000_000 + m * 60, 0).unwrap();
        let s = |m, r: f32, observed| Sample { time: t(m), mm_per_hour: Some(r), chance: 0.0, observed };
        let series = [s(0, 0.0, true), s(5, 0.0, false), s(10, 0.6, false), s(15, 2.0, false)];
        assert_eq!(outlook(&series), Outlook::RainFrom { at: t(10), peak: 2.0 });
        let series = [s(0, 1.0, true), s(5, 0.5, false), s(10, 0.0, false), s(15, 0.0, false)];
        assert_eq!(outlook(&series), Outlook::Raining { until: Some(t(10)), peak: 1.0 });
    }
}
