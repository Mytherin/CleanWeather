//! Web Mercator, the projection the map is drawn in, and drawing the radar grid onto it.
//!
//! "World pixels" at zoom `z`: the whole world is `256 · 2^z` pixels square, x growing
//! east from 180° W and y growing south from ~85° N.

use crate::radar::Grid;
use std::f64::consts::PI;

pub const TILE: f64 = 256.0;

fn world_size(zoom: u32) -> f64 {
    TILE * (1u64 << zoom) as f64
}

/// World pixel of a lat/lon.
pub fn to_world(lat: f64, lon: f64, zoom: u32) -> (f64, f64) {
    let s = world_size(zoom);
    let phi = lat.to_radians();
    let x = (lon + 180.0) / 360.0 * s;
    let y = (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / PI) / 2.0 * s;
    (x, y)
}

/// Lat/lon of a world pixel.
pub fn to_latlon(x: f64, y: f64, zoom: u32) -> (f64, f64) {
    let s = world_size(zoom);
    let lon = x / s * 360.0 - 180.0;
    let lat = (PI * (1.0 - 2.0 * y / s)).sinh().atan().to_degrees();
    (lat, lon)
}

/// A rectangle of the map in world pixels at a zoom level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub zoom: u32,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl View {
    /// The view just covering a lat/lon box.
    pub fn around(lat: (f64, f64), lon: (f64, f64), zoom: u32) -> View {
        let (x0, y0) = to_world(lat.1, lon.0, zoom);
        let (x1, y1) = to_world(lat.0, lon.1, zoom);
        View { zoom, x0, y0, x1, y1 }
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    /// For each pixel of a `w × h` image of this view, the index of the radar grid cell
    /// under it, or `None` outside the grid. Compute once, then colour every frame with it.
    pub fn radar_lookup(&self, grid: &Grid, w: usize, h: usize) -> Vec<Option<u32>> {
        let mut out = Vec::with_capacity(w * h);
        for j in 0..h {
            let y = self.y0 + (j as f64 + 0.5) / h as f64 * self.height();
            for i in 0..w {
                let x = self.x0 + (i as f64 + 0.5) / w as f64 * self.width();
                let (lat, lon) = to_latlon(x, y, self.zoom);
                let (c, r) = grid.to_pixel(lat, lon);
                out.push(if c >= 0.0 && r >= 0.0 && (c as usize) < grid.width && (r as usize) < grid.height {
                    Some((r as usize * grid.width + c as usize) as u32)
                } else {
                    None
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for (lat, lon) in [(52.1, 5.18), (50.0, 0.0), (55.9, 10.8)] {
            let (x, y) = to_world(lat, lon, 7);
            let (la, lo) = to_latlon(x, y, 7);
            assert!((la - lat).abs() < 1e-9 && (lo - lon).abs() < 1e-9);
        }
        // De Bilt is in tile (65, 42) at zoom 7.
        let (x, y) = to_world(52.1, 5.18, 7);
        assert_eq!(((x / TILE) as u32, (y / TILE) as u32), (65, 42));
    }
}
