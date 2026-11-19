//! Decoding KNMI radar reflectivity composites (HDF5, `RAD_NL25_PCP_NA_*.h5`).
//!
//! Each file is one 5-minute scan: a 700 × 765 grid of 1 km pixels on a polar
//! stereographic projection, holding reflectivity in dBZ (`dBZ = 0.5 · PV − 32`).

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use hdf5_reader::Hdf5File;
use hdf5_reader::group::Group;
use std::path::Path;

/// Below this reflectivity a pixel is treated as dry (≈ 0.1 mm/h).
pub const RAIN_DBZ: f32 = 7.0;

/// The grid's projection: `+proj=stere +lat_0=90 +lon_0=0 +lat_ts=60` on an ellipsoid,
/// with KNMI's pixel offsets. Coordinates in km.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    a: f64,
    e: f64,
    /// ρ = k · t(φ): the scale fixed by the true-scale latitude.
    k: f64,
    px_x: f64,
    px_y: f64,
    col_offset: f64,
    row_offset: f64,
}

impl Grid {
    fn new(width: usize, height: usize, a_km: f64, b_km: f64, lat_ts: f64, px: (f64, f64), off: (f64, f64)) -> Grid {
        let e = (1.0 - (b_km * b_km) / (a_km * a_km)).sqrt();
        let phi_c = lat_ts.to_radians();
        let (t_c, m_c) = (t(phi_c, e), m(phi_c, e));
        Grid {
            width,
            height,
            a: a_km,
            e,
            k: a_km * m_c / t_c,
            px_x: px.0,
            px_y: px.1,
            col_offset: off.0,
            row_offset: off.1,
        }
    }

    /// The grid of current KNMI composites, for when no file has been read yet.
    pub fn knmi() -> Grid {
        Grid::new(700, 765, 6378.137, 6356.752, 60.0, (1.0000026, -1.0000044), (0.0, 3649.9795))
    }

    /// Fractional pixel position (column, row) of a lat/lon; pixel centres are at +0.5.
    pub fn to_pixel(&self, lat: f64, lon: f64) -> (f64, f64) {
        let (phi, lam) = (lat.to_radians(), lon.to_radians());
        let rho = self.k * t(phi, self.e);
        let (x, y) = (rho * lam.sin(), -rho * lam.cos());
        (x / self.px_x - self.col_offset, y / self.px_y - self.row_offset)
    }

    /// Lat/lon of a fractional pixel position (inverse of [`Grid::to_pixel`]).
    pub fn to_latlon(&self, col: f64, row: f64) -> (f64, f64) {
        let x = (col + self.col_offset) * self.px_x;
        let y = (row + self.row_offset) * self.px_y;
        let rho = x.hypot(y);
        let tt = rho / self.k;
        // Iterate φ = π/2 − 2·atan(t · ((1 − e sinφ)/(1 + e sinφ))^(e/2)).
        let mut phi = std::f64::consts::FRAC_PI_2 - 2.0 * tt.atan();
        for _ in 0..8 {
            let es = self.e * phi.sin();
            phi = std::f64::consts::FRAC_PI_2 - 2.0 * (tt * ((1.0 - es) / (1.0 + es)).powf(self.e / 2.0)).atan();
        }
        let lam = x.atan2(-y);
        let _ = self.a;
        (phi.to_degrees(), lam.to_degrees())
    }
}

fn t(phi: f64, e: f64) -> f64 {
    let es = e * phi.sin();
    (std::f64::consts::FRAC_PI_4 - phi / 2.0).tan() / ((1.0 - es) / (1.0 + es)).powf(e / 2.0)
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
        let grid = Grid::new(
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
        let dbz = pv
            .iter()
            .map(|&v| if v == outside { f32::NAN } else { gain * v as f32 + offset })
            .collect();

        let (mut radars_up, mut radars_down) = (Vec::new(), Vec::new());
        let n = file.group("/overview").ok().and_then(|g| num(&g, "number_radar_groups").ok());
        for i in 1..=n.unwrap_or(0.0) as usize {
            let Ok(g) = file.group(&format!("/radar{i}")) else { continue };
            let name = g.attribute("radar_name").and_then(|a| a.read_string()).unwrap_or_default();
            match num(&g, "radar_operational") {
                Ok(v) if v != 0.0 => radars_up.push(name),
                _ => radars_down.push(name),
            }
        }
        Ok(Frame { time, grid, dbz, radars_up, radars_down })
    }

    pub fn get(&self, col: usize, row: usize) -> f32 {
        self.dbz[row * self.grid.width + col]
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
