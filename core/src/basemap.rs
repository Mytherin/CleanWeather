//! The map under the radar, baked into the app: land, lakes, borders, rivers and towns
//! of Europe from Natural Earth (public domain), clipped and simplified by
//! `examples/make_map.rs` into `assets/map.json`. Nothing is downloaded.
//!
//! For each whole zoom level the geometry is projected to world pixels and simplified
//! to about a pixel ([`MapData::lod`], cached), so zoomed-out views stay fast. Land and
//! water are rasterised here into an image for a view, so the desktop and iPhone apps
//! draw the same map; lines and labels are left to the UI to draw crisply.

use crate::mercator::{self, View};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

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
    /// The Netherlands (European part), to choose the radar for a location.
    #[serde(default)]
    pub netherlands: Vec<Line>,
    /// Largest first.
    pub places: Vec<Place>,
    #[serde(skip)]
    lods: Mutex<HashMap<u32, Arc<Lod>>>,
}

/// A line or ring in world pixels at a [`Lod`]'s zoom, with its bounding box.
pub struct Shape {
    pub points: Vec<(f64, f64)>,
    pub min: (f64, f64),
    pub max: (f64, f64),
}

impl Shape {
    fn new(points: Vec<(f64, f64)>) -> Shape {
        let mut min = (f64::MAX, f64::MAX);
        let mut max = (f64::MIN, f64::MIN);
        for &(x, y) in &points {
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        }
        Shape { points, min, max }
    }

    /// Does it overlap a rectangle (in the same coordinates)?
    pub fn overlaps(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> bool {
        self.max.0 >= x0 && self.min.0 <= x1 && self.max.1 >= y0 && self.min.1 <= y1
    }
}

/// The map projected and simplified for one whole zoom level.
pub struct Lod {
    pub zoom: u32,
    pub land: Vec<Shape>,
    pub lakes: Vec<Shape>,
    pub borders: Vec<Shape>,
    pub rivers: Vec<Shape>,
}

/// The embedded map (parsed on first use).
pub fn map() -> &'static MapData {
    static MAP: OnceLock<MapData> = OnceLock::new();
    MAP.get_or_init(|| serde_json::from_slice(MAP_JSON).expect("embedded map.json is valid"))
}

impl MapData {
    /// The geometry for a whole zoom level (computed once, then cached).
    pub fn lod(&self, zoom: u32) -> Arc<Lod> {
        let zoom = zoom.min(12);
        if let Some(l) = self.lods.lock().unwrap().get(&zoom) {
            return l.clone();
        }
        let z = zoom as f64;
        // Simplify to ~half a pixel; drop shapes smaller than a pixel.
        let project = |lines: &[Line], closed: bool| -> Vec<Shape> {
            lines
                .iter()
                .map(|l| simplify(&l.iter().map(|p| mercator::to_world(p[1], p[0], z)).collect::<Vec<_>>(), 0.5))
                .filter(|pts| pts.len() >= if closed { 3 } else { 2 })
                .map(Shape::new)
                .filter(|s| !closed || (s.max.0 - s.min.0) + (s.max.1 - s.min.1) > 1.0)
                .collect()
        };
        let lod = Arc::new(Lod {
            zoom,
            land: project(&self.land, true),
            lakes: project(&self.lakes, true),
            borders: project(&self.borders, false),
            rivers: project(&self.rivers, false),
        });
        self.lods.lock().unwrap().insert(zoom, lod.clone());
        lod
    }

    /// An RGBA image (`w × h`, opaque) of land and water for `view`, antialiased.
    pub fn render(&self, view: &View, w: usize, h: usize, land: [u8; 3], water: [u8; 3]) -> Vec<u8> {
        // Geometry from the next zoom level up, so it's at least as detailed as the view.
        let lod = self.lod(view.zoom.ceil().max(0.0) as u32);
        let k = (view.zoom - lod.zoom as f64).exp2();
        // Coverage at 2×2 samples per pixel.
        const SS: usize = 2;
        let (mw, mh) = (w * SS, h * SS);
        let (sx, sy) = (mw as f64 / view.width(), mh as f64 / view.height());
        let visible = |s: &&Shape| s.overlaps(view.x0 / k, view.y0 / k, view.x1 / k, view.y1 / k);
        let to_px = |shapes: &[Shape]| -> Vec<Vec<(f64, f64)>> {
            shapes.iter().filter(visible).map(|s| s.points.iter().map(|&(x, y)| ((x * k - view.x0) * sx, (y * k - view.y0) * sy)).collect()).collect()
        };
        let mut mask = vec![false; mw * mh];
        fill_even_odd(&mut mask, mw, mh, &to_px(&lod.land), true);
        fill_even_odd(&mut mask, mw, mh, &to_px(&lod.lakes), false);

        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let mut n = 0;
                for sy in 0..SS {
                    let row = (y * SS + sy) * mw + x * SS;
                    n += mask[row..row + SS].iter().filter(|&&m| m).count();
                }
                let t = n as f32 / (SS * SS) as f32;
                for c in 0..3 {
                    out.push((water[c] as f32 + (land[c] as f32 - water[c] as f32) * t).round() as u8);
                }
                out.push(255);
            }
        }
        out
    }

    /// Is a location in the Netherlands, or within ~5 km of it (so the coast, the
    /// IJsselmeer and places right at the border count too)?
    pub fn in_netherlands(&self, lat: f64, lon: f64) -> bool {
        let inside = |lat: f64, lon: f64| {
            // Even–odd rule over all rings (the mainland and the islands).
            let mut odd = false;
            for ring in &self.netherlands {
                for i in 0..ring.len() {
                    let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                    if (a[1] > lat) != (b[1] > lat) && lon < a[0] + (lat - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
                        odd = !odd;
                    }
                }
            }
            odd
        };
        let (dlat, dlon) = (5.0 / 111.2, 5.0 / (111.2 * lat.to_radians().cos()));
        inside(lat, lon) || (0..8).any(|k| {
            let a = k as f64 * std::f64::consts::FRAC_PI_4;
            inside(lat + dlat * a.sin(), lon + dlon * a.cos())
        })
    }

    /// The nearest labelled place and its distance in km.
    pub fn nearest_place(&self, lat: f64, lon: f64) -> Option<(&Place, f64)> {
        self.places
            .iter()
            .map(|p| (p, ((p.lat - lat) * 111.2).hypot((p.lon - lon) * 111.2 * lat.to_radians().cos())))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// A name for a location: the town there, "Near" one within 25 km, or a generic one.
    pub fn name_for(&self, lat: f64, lon: f64) -> String {
        match self.nearest_place(lat, lon) {
            Some((p, d)) if d < 4.0 => p.name.clone(),
            Some((p, d)) if d < 25.0 => format!("Near {}", p.name),
            _ => "Selected location".into(),
        }
    }

    /// Towns whose name contains `query` (ignoring case and accents): names starting
    /// with it first, then bigger towns first; at most `limit`.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Place> {
        let q = fold(query.trim());
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(bool, &Place)> = self
            .places
            .iter()
            .filter_map(|p| {
                let name = fold(&p.name);
                name.contains(&q).then(|| (name.starts_with(&q), p))
            })
            .collect();
        hits.sort_by_key(|(starts, p)| (!starts, std::cmp::Reverse(p.population)));
        hits.into_iter().take(limit).map(|(_, p)| p).collect()
    }
}

/// Lowercase without accents, for matching typed place names ("dusseldorf", "zurich").
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        let base = match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ą' | 'ă' => "a",
            'ç' | 'ć' | 'č' => "c",
            'ď' => "d",
            'è' | 'é' | 'ê' | 'ë' | 'ę' | 'ě' => "e",
            'ğ' => "g",
            'ì' | 'í' | 'î' | 'ï' | 'ı' => "i",
            'ł' => "l",
            'ñ' | 'ń' | 'ň' => "n",
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ő' => "o",
            'ř' => "r",
            'ś' | 'š' | 'ș' | 'ş' => "s",
            'ß' => "ss",
            'ț' | 'ţ' | 'ť' => "t",
            'ù' | 'ú' | 'û' | 'ü' | 'ů' | 'ű' => "u",
            'ý' | 'ÿ' => "y",
            'ź' | 'ż' | 'ž' => "z",
            '\'' | '’' => "",
            _ => {
                out.push(c);
                continue;
            }
        };
        out.push_str(base);
    }
    out
}

/// Douglas–Peucker simplification in the points' own units.
fn simplify(pts: &[(f64, f64)], tolerance: f64) -> Vec<(f64, f64)> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (ax, ay) = pts[a];
        let (dx, dy) = (pts[b].0 - ax, pts[b].1 - ay);
        let len2 = dx * dx + dy * dy;
        let (mut best, mut idx) = (0.0, 0);
        for (i, &(px, py)) in pts.iter().enumerate().take(b).skip(a + 1) {
            let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let d = (px - ax - t * dx).hypot(py - ay - t * dy);
            if d > best {
                best = d;
                idx = i;
            }
        }
        if best > tolerance {
            keep[idx] = true;
            stack.push((a, idx));
            stack.push((idx, b));
        }
    }
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// Set (or clear) the pixels inside the rings, even–odd rule, sampling pixel centres.
/// Scanline with an active edge list, so each row only looks at the edges crossing it.
fn fill_even_odd(mask: &mut [bool], w: usize, h: usize, rings: &[Vec<(f64, f64)>], value: bool) {
    // Edges as (y_min, y_max, x at y_min, dx/dy), sorted by y_min.
    let mut edges = Vec::new();
    for r in rings {
        for i in 0..r.len() {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            if a.1 == b.1 {
                continue;
            }
            let (lo, hi) = if a.1 < b.1 { (a, b) } else { (b, a) };
            if hi.1 < 0.0 || lo.1 > h as f64 {
                continue;
            }
            edges.push((lo.1, hi.1, lo.0, (hi.0 - lo.0) / (hi.1 - lo.1)));
        }
    }
    edges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut next = 0;
    let mut active: Vec<usize> = Vec::new();
    let mut xs = Vec::new();
    for y in 0..h {
        let yc = y as f64 + 0.5;
        while next < edges.len() && edges[next].0 <= yc {
            active.push(next);
            next += 1;
        }
        active.retain(|&i| edges[i].1 > yc);
        xs.clear();
        for &i in &active {
            let e = edges[i];
            if e.0 <= yc {
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
    fn map_loads_and_knows_europe() {
        let m = map();
        assert!(m.land.len() > 100 && !m.lakes.is_empty() && !m.borders.is_empty() && !m.rivers.is_empty());
        for (lat, lon, name) in [(52.37, 4.90, "Amsterdam"), (48.86, 2.35, "Paris"), (59.33, 18.07, "Stockholm")] {
            let (p, d) = m.nearest_place(lat, lon).unwrap();
            assert_eq!(p.name, name);
            assert!(d < 5.0, "{name}: {d} km");
        }
    }

    #[test]
    fn searches_places_ignoring_case_and_accents() {
        let m = map();
        assert_eq!(fold("Düsseldorf"), "dusseldorf");
        assert_eq!(fold("'s-Hertogenbosch"), "s-hertogenbosch");
        let hits = m.search("dusseld", 5);
        assert!(hits.first().is_some_and(|p| p.name == "Düsseldorf"), "{:?}", hits.iter().map(|p| &p.name).collect::<Vec<_>>());
        let ams = m.search("AMSTER", 5);
        assert_eq!(ams[0].name, "Amsterdam");
        assert!(m.search("   ", 5).is_empty());
        assert_eq!(m.name_for(52.373, 4.893), "Amsterdam");
        assert_eq!(m.name_for(0.0, -30.0), "Selected location");
    }

    #[test]
    fn knows_where_the_netherlands_is() {
        let m = map();
        for (name, lat, lon) in [("Amsterdam", 52.37, 4.89), ("Maastricht", 50.85, 5.69), ("Groningen", 53.22, 6.57), ("Texel", 53.06, 4.80), ("IJsselmeer", 52.75, 5.30), ("Vlissingen", 51.44, 3.57)] {
            assert!(m.in_netherlands(lat, lon), "{name} is in the Netherlands");
        }
        for (name, lat, lon) in [("Antwerp", 51.22, 4.40), ("Brussels", 50.85, 4.35), ("Cologne", 50.94, 6.96), ("Bremen", 53.08, 8.80), ("Paris", 48.86, 2.35), ("North Sea", 52.6, 3.0)] {
            assert!(!m.in_netherlands(lat, lon), "{name} is not in the Netherlands");
        }
    }

    #[test]
    fn land_and_sea_in_the_right_places() {
        for zoom in [5.0, 8.0] {
            let view = View::around((50.55, 53.85), (2.3, 7.5), zoom);
            let (w, h) = (view.width() as usize, view.height() as usize);
            let img = map().render(&view, w, h, [255, 255, 255], [0, 0, 0]);
            let at = |lat: f64, lon: f64| {
                let (x, y) = mercator::to_world(lat, lon, zoom);
                let (i, j) = ((x - view.x0) as usize, (y - view.y0) as usize);
                img[(j * w + i) * 4]
            };
            assert_eq!(at(52.09, 5.12), 255, "Utrecht is land (zoom {zoom})");
            assert_eq!(at(52.5, 3.5), 0, "North Sea is water (zoom {zoom})");
            assert_eq!(at(51.0, 7.0), 255, "Germany is land (zoom {zoom})");
        }
        // The IJsselmeer is only big enough to show from zoom 8.
        let view = View::around((50.55, 53.85), (2.3, 7.5), 8.0);
        let (w, h) = (view.width() as usize, view.height() as usize);
        let img = map().render(&view, w, h, [255, 255, 255], [0, 0, 0]);
        let (x, y) = mercator::to_world(52.80, 5.25, 8.0);
        assert_eq!(img[(((y - view.y0) as usize) * w + (x - view.x0) as usize) * 4], 0, "IJsselmeer is water");
    }

    #[test]
    fn rendering_europe_is_quick() {
        let view = View::around((35.0, 71.0), (-25.0, 45.0), 4.5);
        let t = std::time::Instant::now();
        let _ = map().render(&view, view.width() as usize, view.height() as usize, [255; 3], [0; 3]);
        let _ = map().render(&view, view.width() as usize, view.height() as usize, [255; 3], [0; 3]);
        // Generous: debug builds are slow; release takes a few ms.
        assert!(t.elapsed().as_secs_f64() < 5.0, "{:?}", t.elapsed());
    }
}
