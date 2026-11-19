//! Web Mercator, the projection the map is drawn in.
//!
//! "World pixels" at zoom `z`: the whole world is `256 · 2^z` pixels square, x growing
//! east from 180° W and y growing south from ~85° N. Zoom may be fractional.

use std::f64::consts::PI;

pub const TILE: f64 = 256.0;

pub fn world_size(zoom: f64) -> f64 {
    TILE * zoom.exp2()
}

/// World pixel of a lat/lon.
pub fn to_world(lat: f64, lon: f64, zoom: f64) -> (f64, f64) {
    let s = world_size(zoom);
    let phi = lat.clamp(-85.0, 85.0).to_radians();
    let x = (lon + 180.0) / 360.0 * s;
    let y = (1.0 - (phi.tan() + 1.0 / phi.cos()).ln() / PI) / 2.0 * s;
    (x, y)
}

/// Lat/lon of a world pixel.
pub fn to_latlon(x: f64, y: f64, zoom: f64) -> (f64, f64) {
    let s = world_size(zoom);
    let lon = x / s * 360.0 - 180.0;
    let lat = (PI * (1.0 - 2.0 * y / s)).sinh().atan().to_degrees();
    (lat, lon)
}

/// A rectangle of the map in world pixels at a zoom level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub zoom: f64,
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl View {
    /// The view just covering a lat/lon box.
    pub fn around(lat: (f64, f64), lon: (f64, f64), zoom: f64) -> View {
        let (x0, y0) = to_world(lat.1, lon.0, zoom);
        let (x1, y1) = to_world(lat.0, lon.1, zoom);
        View { zoom, x0, y0, x1, y1 }
    }

    /// A view of `w × h` world pixels centred on a lat/lon.
    pub fn centred(lat: f64, lon: f64, zoom: f64, w: f64, h: f64) -> View {
        let (x, y) = to_world(lat, lon, zoom);
        View { zoom, x0: x - w / 2.0, y0: y - h / 2.0, x1: x + w / 2.0, y1: y + h / 2.0 }
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn centre(&self) -> (f64, f64) {
        to_latlon((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0, self.zoom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for (lat, lon) in [(52.1, 5.18), (50.0, 0.0), (55.9, 10.8), (64.1, -21.9)] {
            let (x, y) = to_world(lat, lon, 7.3);
            let (la, lo) = to_latlon(x, y, 7.3);
            assert!((la - lat).abs() < 1e-9 && (lo - lon).abs() < 1e-9);
        }
        // De Bilt is in tile (65, 42) at zoom 7.
        let (x, y) = to_world(52.1, 5.18, 7.0);
        assert_eq!(((x / TILE) as u32, (y / TILE) as u32), (65, 42));
        let v = View::centred(52.1, 5.18, 8.0, 400.0, 300.0);
        let (la, lo) = v.centre();
        assert!((la - 52.1).abs() < 1e-9 && (lo - 5.18).abs() < 1e-9);
    }
}
