//! Radar scans and their grids, and decoding KNMI radar reflectivity composites
//! (HDF5, `RAD_NL25_PCP_NA_*.h5`).
//!
//! A KNMI file is one 5-minute scan: a 700 × 765 grid of 1 km pixels on a polar
//! stereographic projection, holding reflectivity in dBZ (`dBZ = 0.5 · PV − 32`).
//! OPERA's European composite (see `opera.rs`) uses a Lambert azimuthal equal-area grid.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use hdf5_reader::Hdf5File;
use hdf5_reader::group::Group;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};
use std::path::Path;

/// Below this reflectivity a pixel is treated as dry (≈ 0.1 mm/h).
pub const RAIN_DBZ: f32 = 7.0;

/// A map projection from lat/lon to plane coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Projection {
    /// `+proj=stere +lat_0=90 +lon_0=0 +lat_ts=…` on an ellipsoid (KNMI), in the
    /// ellipsoid's units. ρ = k · t(φ).
    PolarStereo { e: f64, k: f64 },
    /// `+proj=laea` on an ellipsoid, oblique aspect (OPERA).
    Laea(Laea),
}

impl Projection {
    fn forward(&self, lat: f64, lon: f64) -> (f64, f64) {
        match self {
            Projection::PolarStereo { e, k } => {
                let (phi, lam) = (lat.to_radians(), lon.to_radians());
                let rho = k * t(phi, *e);
                (rho * lam.sin(), -rho * lam.cos())
            }
            Projection::Laea(p) => p.forward(lat, lon),
        }
    }

    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        match self {
            Projection::PolarStereo { e, k } => {
                let rho = x.hypot(y);
                let tt = rho / k;
                // Iterate φ = π/2 − 2·atan(t · ((1 − e sinφ)/(1 + e sinφ))^(e/2)).
                let mut phi = FRAC_PI_2 - 2.0 * tt.atan();
                for _ in 0..8 {
                    let es = e * phi.sin();
                    phi = FRAC_PI_2 - 2.0 * (tt * ((1.0 - es) / (1.0 + es)).powf(e / 2.0)).atan();
                }
                (phi.to_degrees(), x.atan2(-y).to_degrees())
            }
            Projection::Laea(p) => p.inverse(x, y),
        }
    }
}

/// Lambert azimuthal equal-area on an ellipsoid (Snyder, "Map Projections — A Working
/// Manual", oblique aspect), with false easting/northing. Plane units are metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Laea {
    e: f64,
    qp: f64,
    rq: f64,
    d: f64,
    sin_b1: f64,
    cos_b1: f64,
    lon0: f64,
    fe: f64,
    fn_: f64,
}

impl Laea {
    pub fn new(a: f64, inv_f: f64, lat0: f64, lon0: f64, fe: f64, fn_: f64) -> Laea {
        let f = 1.0 / inv_f;
        let e = (2.0 * f - f * f).sqrt();
        let qp = q(FRAC_PI_2, e);
        let rq = a * (qp / 2.0).sqrt();
        let phi1 = lat0.to_radians();
        let b1 = (q(phi1, e) / qp).asin();
        let m1 = phi1.cos() / (1.0 - e * e * phi1.sin().powi(2)).sqrt();
        let d = a * m1 / (rq * b1.cos());
        Laea { e, qp, rq, d, sin_b1: b1.sin(), cos_b1: b1.cos(), lon0: lon0.to_radians(), fe, fn_ }
    }

    fn forward(&self, lat: f64, lon: f64) -> (f64, f64) {
        let b = (q(lat.to_radians(), self.e) / self.qp).clamp(-1.0, 1.0).asin();
        let dl = lon.to_radians() - self.lon0;
        let bb = self.rq * (2.0 / (1.0 + self.sin_b1 * b.sin() + self.cos_b1 * b.cos() * dl.cos())).sqrt();
        let x = bb * self.d * b.cos() * dl.sin();
        let y = (bb / self.d) * (self.cos_b1 * b.sin() - self.sin_b1 * b.cos() * dl.cos());
        (x + self.fe, y + self.fn_)
    }

    fn inverse(&self, x: f64, y: f64) -> (f64, f64) {
        let (x, y) = (x - self.fe, y - self.fn_);
        let rho = ((x / self.d).powi(2) + (self.d * y).powi(2)).sqrt();
        if rho < 1e-9 {
            return (self.sin_b1.asin().to_degrees(), self.lon0.to_degrees());
        }
        let ce = 2.0 * (rho / (2.0 * self.rq)).clamp(-1.0, 1.0).asin();
        let b = (ce.cos() * self.sin_b1 + self.d * y * ce.sin() * self.cos_b1 / rho).clamp(-1.0, 1.0).asin();
        let lam = self.lon0 + (x * ce.sin()).atan2(self.d * rho * self.cos_b1 * ce.cos() - self.d * self.d * y * self.sin_b1 * ce.sin());
        let e2 = self.e * self.e;
        let (e4, e6) = (e2 * e2, e2 * e2 * e2);
        let phi = b
            + (e2 / 3.0 + 31.0 * e4 / 180.0 + 517.0 * e6 / 5040.0) * (2.0 * b).sin()
            + (23.0 * e4 / 360.0 + 251.0 * e6 / 3780.0) * (4.0 * b).sin()
            + (761.0 * e6 / 45360.0) * (6.0 * b).sin();
        (phi.to_degrees(), lam.to_degrees())
    }
}

/// Snyder's q(φ) for equal-area projections.
fn q(phi: f64, e: f64) -> f64 {
    let s = phi.sin();
    (1.0 - e * e) * (s / (1.0 - e * e * s * s) - (1.0 / (2.0 * e)) * ((1.0 - e * s) / (1.0 + e * s)).ln())
}

/// A raster of pixels on a projection: pixel (col, row) covers plane coordinates
/// `x = (col + col_offset) · px_x`, `y = (row + row_offset) · px_y` (its corner), so
/// pixel centres are at +0.5. Row 0 is the north edge (`px_y` is negative).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    proj: Projection,
    px_x: f64,
    px_y: f64,
    col_offset: f64,
    row_offset: f64,
}

impl Grid {
    pub fn new(width: usize, height: usize, proj: Projection, px: (f64, f64), off: (f64, f64)) -> Grid {
        Grid { width, height, proj, px_x: px.0, px_y: px.1, col_offset: off.0, row_offset: off.1 }
    }

    fn polar_stereo(width: usize, height: usize, a: f64, b: f64, lat_ts: f64, px: (f64, f64), off: (f64, f64)) -> Grid {
        let e = (1.0 - (b * b) / (a * a)).sqrt();
        let phi_c = lat_ts.to_radians();
        let k = a * m(phi_c, e) / t(phi_c, e);
        Grid::new(width, height, Projection::PolarStereo { e, k }, px, off)
    }

    /// The grid of current KNMI composites, for when no file has been read yet.
    pub fn knmi() -> Grid {
        Grid::polar_stereo(700, 765, 6378.137, 6356.752, 60.0, (1.0000026, -1.0000044), (0.0, 3649.9795))
    }

    /// A window of this grid: `width × height` pixels starting at (col, row).
    pub fn crop(&self, col: usize, row: usize, width: usize, height: usize) -> Grid {
        Grid { width, height, col_offset: self.col_offset + col as f64, row_offset: self.row_offset + row as f64, ..*self }
    }

    /// Fractional pixel position (column, row) of a lat/lon; pixel centres are at +0.5.
    pub fn to_pixel(&self, lat: f64, lon: f64) -> (f64, f64) {
        let (x, y) = self.proj.forward(lat, lon);
        (x / self.px_x - self.col_offset, y / self.px_y - self.row_offset)
    }

    /// Lat/lon of a fractional pixel position (inverse of [`Grid::to_pixel`]).
    pub fn to_latlon(&self, col: f64, row: f64) -> (f64, f64) {
        self.proj.inverse((col + self.col_offset) * self.px_x, (row + self.row_offset) * self.px_y)
    }

    /// The size of a pixel in km (KNMI's plane is in km, OPERA's in metres).
    pub fn pixel_km(&self) -> f64 {
        match self.proj {
            Projection::PolarStereo { .. } => self.px_x.abs(),
            Projection::Laea(_) => self.px_x.abs() / 1000.0,
        }
    }

    /// Whether a lat/lon falls on the grid.
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        let (c, r) = self.to_pixel(lat, lon);
        c >= 0.0 && r >= 0.0 && c < self.width as f64 && r < self.height as f64
    }
}

fn t(phi: f64, e: f64) -> f64 {
    let es = e * phi.sin();
    (FRAC_PI_4 - phi / 2.0).tan() / ((1.0 - es) / (1.0 + es)).powf(e / 2.0)
}

fn m(phi: f64, e: f64) -> f64 {
    let s = phi.sin();
    phi.cos() / (1.0 - e * e * s * s).sqrt()
}

/// One radar scan.
#[derive(Debug, Clone)]
pub struct Frame {
    pub time: DateTime<Utc>,
    pub grid: Grid,
    /// Reflectivity in dBZ, row-major (row 0 is the north edge). NaN where there's no data.
    pub dbz: Vec<f32>,
    /// Names of the radars that contributed, and of those that were down.
    pub radars_up: Vec<String>,
    pub radars_down: Vec<String>,
    /// Where the scan came from ("KNMI", "OPERA").
    pub source: &'static str,
}

impl Frame {
    pub fn read(path: &Path) -> Result<Frame> {
        let file = Hdf5File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let time = crate::knmi::time_from_filename(&name)
            .ok_or_else(|| anyhow!("no time in file name {name}"))?;
        Frame::decode(&file, time).with_context(|| format!("reading {name}"))
    }

    pub fn from_bytes(bytes: &[u8], time: DateTime<Utc>) -> Result<Frame> {
        Frame::decode(&Hdf5File::from_bytes(bytes)?, time)
    }

    fn decode(file: &Hdf5File, time: DateTime<Utc>) -> Result<Frame> {
        let geo = file.group("/geographic")?;
        let proj = file
            .group("/geographic/map_projection")?
            .attribute("projection_proj4_params")?
            .read_string()?;
        let p = |k: &str| proj4_param(&proj, k);
        let to_km = if proj.contains("+units=km") && p("a").unwrap_or(0.0) > 100_000.0 { 0.001 } else { 1.0 };
        let (a, b) = match (p("a"), p("b")) {
            (Some(a), Some(b)) => (a * to_km, b * to_km),
            _ => bail!("unexpected projection: {proj}"),
        };
        if p("lat_0") != Some(90.0) || p("lon_0").unwrap_or(0.0) != 0.0 || !proj.contains("+proj=stere") {
            bail!("unexpected projection: {proj}");
        }
        let width = num(&geo, "geo_number_columns")? as usize;
        let height = num(&geo, "geo_number_rows")? as usize;
        let grid = Grid::polar_stereo(
            width,
            height,
            a,
            b,
            p("lat_ts").unwrap_or(60.0),
            (num(&geo, "geo_pixel_size_x")?, num(&geo, "geo_pixel_size_y")?),
            (num(&geo, "geo_column_offset")?, num(&geo, "geo_row_offset")?),
        );

        let cal = file.group("/image1/calibration")?;
        let formula = cal.attribute("calibration_formulas")?.read_string()?;
        let (gain, offset) = parse_calibration(&formula)?;
        // `calibration_missing_data` is 0, but in practice 0 is what most pixels in clear
        // air hold (~⅔ of a typical scan), so it means "no echo" (dry), not "no data".
        // Only pixels outside the radars' range are really unknown.
        let outside = num(&cal, "calibration_out_of_image").unwrap_or(255.0) as u8;

        let data = file.dataset("/image1/image_data")?;
        let shape = data.shape().to_vec();
        if shape != [height as u64, width as u64] {
            bail!("image is {shape:?}, expected {height}×{width}");
        }
        let pv: Vec<u8> = data.read_array::<u8>()?.iter().copied().collect();
        let dbz: Vec<f32> = pv
            .iter()
            .map(|&v| if v == outside { f32::NAN } else { gain * v as f32 + offset })
            .collect();

        let (mut radars_up, mut radars_down) = (Vec::new(), Vec::new());
        let mut sites = Vec::new();
        let n = file.group("/overview").ok().and_then(|g| num(&g, "number_radar_groups").ok());
        for i in 1..=n.unwrap_or(0.0) as usize {
            let Ok(g) = file.group(&format!("/radar{i}")) else { continue };
            let name = g.attribute("radar_name").and_then(|a| a.read_string()).unwrap_or_default();
            match num(&g, "radar_operational") {
                Ok(v) if v != 0.0 => {
                    radars_up.push(name);
                    // [lon, lat]
                    if let Some(loc) = g.attribute("radar_location").ok().and_then(|a| a.read_1d::<f32>().ok()).filter(|l| l.len() == 2) {
                        sites.push((loc[1] as f64, loc[0] as f64));
                    }
                }
                _ => radars_down.push(name),
            }
        }
        let mut dbz = dbz;
        mask_beyond_range(&mut dbz, &grid, &sites);
        Ok(Frame { time, grid, dbz, radars_up, radars_down, source: "KNMI" })
    }

    pub fn get(&self, col: usize, row: usize) -> f32 {
        self.dbz[row * self.grid.width + col]
    }
}

/// How far from a radar KNMI's composite is trusted. The file reports "no echo" across
/// its whole grid, also far beyond where the radars see (and low rain is below a far
/// radar's beam), so pixels further than this from every operational radar are marked
/// as having no data.
pub const KNMI_RANGE_KM: f64 = 250.0;

fn mask_beyond_range(dbz: &mut [f32], grid: &Grid, sites: &[(f64, f64)]) {
    if sites.is_empty() {
        return; // unknown radar positions: keep everything
    }
    let centres: Vec<(f64, f64)> = sites.iter().map(|&(lat, lon)| grid.to_pixel(lat, lon)).collect();
    let r_px = KNMI_RANGE_KM / grid.pixel_km();
    for row in 0..grid.height {
        for col in 0..grid.width {
            let (x, y) = (col as f64 + 0.5, row as f64 + 0.5);
            if centres.iter().all(|&(cx, cy)| (x - cx).hypot(y - cy) > r_px) {
                dbz[row * grid.width + col] = f32::NAN;
            }
        }
    }
}

/// A numeric attribute, whatever its stored width.
fn num(g: &Group, name: &str) -> Result<f64> {
    let a = g.attribute(name)?;
    if let Ok(v) = a.read_1d::<f32>() {
        if let Some(&x) = v.first() {
            return Ok(x as f64);
        }
    }
    if let Ok(v) = a.read_1d::<i32>() {
        if let Some(&x) = v.first() {
            return Ok(x as f64);
        }
    }
    a.read_as_f64().with_context(|| format!("attribute {name}"))
}

fn proj4_param(proj: &str, key: &str) -> Option<f64> {
    proj.split_whitespace()
        .filter_map(|p| p.strip_prefix('+')?.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
}

/// "GEO = 0.500000 * PV + -32.000000" → (0.5, -32.0)
fn parse_calibration(s: &str) -> Result<(f32, f32)> {
    let rhs = s.split('=').nth(1).ok_or_else(|| anyhow!("calibration: {s}"))?;
    let (g, o) = rhs.split_once("* PV +").or_else(|| rhs.split_once("*PV+")).ok_or_else(|| anyhow!("calibration: {s}"))?;
    Ok((g.trim().parse()?, o.trim().parse()?))
}

/// Rain rate (mm/h) from reflectivity with Marshall–Palmer, Z = 200 R^1.6.
pub fn rain_rate(dbz: f32) -> f32 {
    if !(dbz >= RAIN_DBZ) {
        return 0.0;
    }
    // Hail and bright band give unrealistic rates; cap at 55 dBZ (~100 mm/h).
    let z = 10f32.powf(dbz.min(55.0) / 10.0);
    (z / 200.0).powf(1.0 / 1.6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_match_knmi() {
        // geo_product_corners from a real file: (lon, lat) of the lower-left,
        // upper-left, upper-right and lower-right image corners.
        let g = Grid::knmi();
        let corners = [((0.0, 49.362064), (0.0, 765.0)), ((0.0, 55.973602), (0.0, 0.0)), ((10.856454, 55.388973), (700.0, 0.0)), ((9.0093, 48.8953), (700.0, 765.0))];
        for ((lon, lat), (c, r)) in corners {
            let (col, row) = g.to_pixel(lat, lon);
            assert!((col - c).abs() < 0.05 && (row - r).abs() < 0.05, "({lat},{lon}) -> ({col},{row}), want ({c},{r})");
            let (la, lo) = g.to_latlon(c, r);
            assert!((la - lat).abs() < 1e-4 && (lo - lon).abs() < 1e-4, "inverse: ({la},{lo})");
        }
    }

    #[test]
    fn calibration() {
        assert_eq!(parse_calibration("GEO = 0.500000 * PV + -32.000000").unwrap(), (0.5, -32.0));
    }

    #[test]
    fn marshall_palmer() {
        assert_eq!(rain_rate(0.0), 0.0);
        assert_eq!(rain_rate(f32::NAN), 0.0);
        assert!((rain_rate(23.0) - 1.0).abs() < 0.1); // 23 dBZ ≈ 1 mm/h
        assert!((rain_rate(36.5) - 7.0).abs() < 0.7);
    }
}
