//! KNMI's 10-minute station observations (dataset
//! `10-minute-in-situ-meteorological-observations`, NetCDF): rain gauge intensity at
//! ~30 automatic weather stations, the ground truth for checking the radars.

use anyhow::{Context, Result, anyhow};
use hdf5_reader::Hdf5File;
use std::path::Path;

pub const DATASET: (&str, &str) = ("10-minute-in-situ-meteorological-observations", "1.0");

#[derive(Debug, Clone, Copy)]
pub struct Gauge {
    pub lat: f64,
    pub lon: f64,
    /// Mean rain intensity over the 10 minutes ending at the file's time, mm/h.
    pub mm_per_hour: f64,
}

fn numbers(f: &Hdf5File, name: &str) -> Result<Vec<f64>> {
    let ds = f.dataset(name).with_context(|| format!("no variable {name}"))?;
    if let Ok(a) = ds.read_array::<f64>() {
        return Ok(a.iter().copied().collect());
    }
    let a = ds.read_array::<f32>().map_err(|e| anyhow!("{name}: {e}"))?;
    Ok(a.iter().map(|&v| v as f64).collect())
}

/// The stations with a valid rain gauge reading in one file.
pub fn read(path: &Path) -> Result<Vec<Gauge>> {
    let f = Hdf5File::open(path)?;
    let (lat, lon, rg) = (numbers(&f, "/lat")?, numbers(&f, "/lon")?, numbers(&f, "/rg")?);
    Ok((0..lat.len().min(rg.len()))
        .filter(|&i| rg[i].is_finite() && rg[i] >= 0.0 && rg[i] < 500.0)
        .map(|i| Gauge { lat: lat[i], lon: lon[i], mm_per_hour: rg[i] })
        .collect())
}
