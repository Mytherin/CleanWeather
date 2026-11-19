//! The map under the radar, baked into the app: land, lakes, borders, rivers and towns
//! from Natural Earth (public domain), clipped to the Netherlands and surroundings by
//! `examples/make_map.rs` into `assets/map.json`. Nothing is downloaded.
//!
//! Land and water are rasterised here into an image for a map view, so the desktop and
//! iPhone apps draw the same map. Lines and labels are left to the UI to draw crisply.

use crate::mercator::{self, View};
use serde::Deserialize;
use std::sync::OnceLock;

static MAP_JSON: &[u8] = include_bytes!("../assets/map.json");

#[derive(Deserialize)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub population: u64,
}

/// Lines and rings as (lon, lat) pairs, as in GeoJSON.
pub type Line = Vec<[f64; 2]>;

#[derive(Deserialize)]
pub struct MapData {
    pub land: Vec<Line>,
    pub lakes: Vec<Line>,
    pub borders: Vec<Line>,
    pub rivers: Vec<Line>,
    /// Largest first.
    pub places: Vec<Place>,
}

/// The embedded map (parsed on first use).
pub fn map() -> &'static MapData {
    static MAP: OnceLock<MapData> = OnceLock::new();
    MAP.get_or_init(|| serde_json::from_slice(MAP_JSON).expect("embedded map.json is valid"))
}

impl MapData {
    /// An RGBA image (`w × h`, opaque) of land and water for `view`, antialiased.
    pub fn render(&self, view: &View, w: usize, h: usize, land: [u8; 3], water: [u8; 3]) -> Vec<u8> {
        // Coverage at 3×3 samples per pixel.
        const SS: usize = 3;
        let (mw, mh) = (w * SS, h * SS);
        let to_px = |p: &[f64; 2]| {
            let (x, y) = mercator::to_world(p[1], p[0], view.zoom);
            ((x - view.x0) / view.width() * mw as f64, (y - view.y0) / view.height() * mh as f64)
        };
        let project = |rings: &[Line]| -> Vec<Vec<(f64, f64)>> { rings.iter().map(|r| r.iter().map(to_px).collect()).collect() };
        let mut mask = vec![false; mw * mh];
        fill_even_odd(&mut mask, mw, mh, &project(&self.land), true);
        fill_even_odd(&mut mask, mw, mh, &project(&self.lakes), false);

        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let mut n = 0;
                for sy in 0..SS {
                    let row = (y * SS + sy) * mw + x * SS;
                    n += mask[row..row + SS].iter().filter(|&&m| m).count();
                }
                let t = n as f32 / (SS * SS) as f32;
                for k in 0..3 {
                    out.push((water[k] as f32 + (land[k] as f32 - water[k] as f32) * t).round() as u8);
                }
                out.push(255);
            }
        }
        out
    }

    /// The nearest labelled place and its distance in km.
    pub fn nearest_place(&self, lat: f64, lon: f64) -> Option<(&Place, f64)> {
        self.places
            .iter()
            .map(|p| (p, ((p.lat - lat) * 111.2).hypot((p.lon - lon) * 111.2 * lat.to_radians().cos())))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }
}

/// Set (or clear) the pixels inside the rings, even–odd rule, sampling pixel centres.
fn fill_even_odd(mask: &mut [bool], w: usize, h: usize, rings: &[Vec<(f64, f64)>], value: bool) {
    // Edges as (y_min, y_max, x at y_min, dx/dy).
    let mut edges = Vec::new();
    for r in rings {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if a.1 == b.1 {
                continue;
            }
            let (lo, hi) = if a.1 < b.1 { (a, b) } else { (b, a) };
            edges.push((lo.1, hi.1, lo.0, (hi.0 - lo.0) / (hi.1 - lo.1)));
        }
    }
    edges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut xs = Vec::new();
    let mut first = 0;
    for y in 0..h {
        let yc = y as f64 + 0.5;
        while first < edges.len() && edges[first].1 <= yc && edges[first].0 <= yc {
            // Edges are sorted by y_min only, so this just skips a prefix cheaply.
            if edges[first].1 > yc {
                break;
            }
            first += 1;
        }
        xs.clear();
        for e in &edges[first..] {
            if e.0 > yc {
                break;
            }
            if yc < e.1 {
                xs.push(e.2 + (yc - e.0) * e.3);
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for pair in xs.chunks_exact(2) {
            let x0 = (pair[0] - 0.5).ceil().max(0.0) as usize;
            let x1 = ((pair[1] - 0.5).floor() + 1.0).clamp(0.0, w as f64) as usize;
            if x0 < x1 {
                mask[y * w + x0..y * w + x1].fill(value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_loads_and_knows_the_netherlands() {
        let m = map();
        assert!(m.land.len() > 5 && !m.lakes.is_empty() && !m.borders.is_empty() && !m.rivers.is_empty());
        let (p, d) = m.nearest_place(52.37, 4.90).unwrap();
        assert_eq!(p.name, "Amsterdam");
        assert!(d < 4.0, "{d} km");
    }

    #[test]
    fn land_and_sea_in_the_right_places() {
        let view = View::around((50.55, 53.85), (2.3, 7.5), 8);
        let (w, h) = (view.width() as usize, view.height() as usize);
        let img = map().render(&view, w, h, [255, 255, 255], [0, 0, 0]);
        let at = |lat: f64, lon: f64| {
            let (x, y) = mercator::to_world(lat, lon, 8);
            let (i, j) = ((x - view.x0) as usize, (y - view.y0) as usize);
            img[(j * w + i) * 4]
        };
        assert_eq!(at(52.09, 5.12), 255, "Utrecht is land");
        assert_eq!(at(52.5, 3.5), 0, "North Sea is water");
        assert_eq!(at(52.80, 5.25), 0, "IJsselmeer is water");
        assert_eq!(at(51.0, 7.0), 255, "Germany is land");
    }
}
