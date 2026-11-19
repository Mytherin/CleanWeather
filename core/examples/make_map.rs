//! Builds `core/assets/map.json`, the map baked into the app, from Natural Earth
//! (public domain, https://www.naturalearthdata.com) 1:10m GeoJSON files:
//!
//!   ne_10m_land, ne_10m_lakes, ne_10m_admin_0_boundary_lines_land,
//!   ne_10m_rivers_lake_centerlines, ne_10m_populated_places
//!
//! from https://github.com/nvkelso/natural-earth-vector/tree/master/geojson
//!
//!   cargo run --release -p cleanweather-core --example make_map -- DIR_WITH_GEOJSON
//!
//! Only needed to change the map; the app never downloads map data.

use serde_json::{Value, json};
use std::path::Path;

/// Area kept: the map view (50.55–53.85 N, 2.3–7.5 E) with a margin.
const LAT: (f64, f64) = (49.8, 54.6);
const LON: (f64, f64) = (1.0, 8.8);
/// Simplification tolerance in degrees (~200 m; a map pixel is ~400 m).
const TOLERANCE: f64 = 0.002;
/// Towns smaller than this aren't labelled.
const MIN_POPULATION: f64 = 45_000.0;

type Ring = Vec<(f64, f64)>; // (lon, lat)

const EXTRA_PLACES: &[(&str, f64, f64, u64)] = &[
    ("Tilburg", 51.556, 5.091, 225_000),
    ("Almere", 52.371, 5.222, 220_000),
    ("Breda", 51.589, 4.776, 185_000),
    ("Nijmegen", 51.842, 5.853, 180_000),
    ("Apeldoorn", 52.211, 5.969, 165_000),
    ("Enschede", 52.221, 6.894, 160_000),
    ("Amersfoort", 52.156, 5.387, 160_000),
    ("Leiden", 52.160, 4.497, 125_000),
    ("Dordrecht", 51.813, 4.690, 120_000),
    ("Zoetermeer", 52.057, 4.494, 125_000),
    ("Alkmaar", 52.632, 4.748, 110_000),
    ("Deventer", 52.255, 6.163, 100_000),
    ("Venlo", 51.370, 6.172, 100_000),
    ("Delft", 52.012, 4.357, 105_000),
    ("Lelystad", 52.518, 5.471, 80_000),
    ("Hoorn", 52.643, 5.060, 75_000),
    ("Den Helder", 52.959, 4.760, 56_000),
    ("Vlissingen", 51.442, 3.573, 45_000),
    ("Heerlen", 50.888, 5.979, 87_000),
    ("Roermond", 51.194, 5.987, 58_000),
    ("Harderwijk", 52.342, 5.620, 48_000),
    ("Ede", 52.040, 5.665, 120_000),
    ("Emmen", 52.779, 6.906, 107_000),
    ("Sneek", 53.033, 5.659, 34_000),
    ("De Bilt", 52.101, 5.177, 43_000),
];

fn main() {
    let dir = std::env::args().nth(1).expect("usage: make_map DIR_WITH_GEOJSON");
    let dir = Path::new(&dir);
    let load = |name: &str| -> Value {
        let s = std::fs::read_to_string(dir.join(format!("{name}.geojson"))).unwrap_or_else(|e| panic!("{name}: {e}"));
        serde_json::from_str(&s).unwrap()
    };

    let land = polygons(&load("ne_10m_land"));
    let lakes = polygons(&load("ne_10m_lakes"));
    let borders = lines(&load("ne_10m_admin_0_boundary_lines_land"));
    let rivers = lines(&load("ne_10m_rivers_lake_centerlines"));

    let mut places = Vec::new();
    for f in load("ne_10m_populated_places")["features"].as_array().unwrap() {
        let p = &f["properties"];
        let (lat, lon) = (p["LATITUDE"].as_f64().unwrap_or(0.0), p["LONGITUDE"].as_f64().unwrap_or(0.0));
        let pop = p["POP_MAX"].as_f64().unwrap_or(0.0);
        if !inside(lon, lat) || pop < MIN_POPULATION {
            continue;
        }
        let name = p["NAME_NL"].as_str().filter(|s| !s.is_empty()).or(p["NAME"].as_str()).unwrap_or("?");
        places.push(json!({ "name": name, "lat": round(lat), "lon": round(lon), "population": pop as u64 }));
    }
    // Natural Earth leaves out many Dutch towns; add them (approximate populations,
    // used only to decide which labels win when they'd overlap).
    for &(name, lat, lon, pop) in EXTRA_PLACES {
        let dup = places.iter().any(|p| ((p["lat"].as_f64().unwrap() - lat).abs() < 0.05) && (p["lon"].as_f64().unwrap() - lon).abs() < 0.08);
        if !dup {
            places.push(json!({ "name": name, "lat": lat, "lon": lon, "population": pop }));
        }
    }
    places.sort_by_key(|p| std::cmp::Reverse(p["population"].as_u64().unwrap()));

    let enc = |rings: &[Ring]| -> Value { rings.iter().map(|r| r.iter().map(|&(x, y)| json!([round(x), round(y)])).collect::<Vec<_>>()).collect() };
    let out = json!({
        "source": "Natural Earth 1:10m (public domain), clipped and simplified by core/examples/make_map.rs",
        "land": enc(&land),
        "lakes": enc(&lakes),
        "borders": enc(&borders),
        "rivers": enc(&rivers),
        "places": places,
    });
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("map.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_string(&out).unwrap()).unwrap();
    let pts = |r: &[Ring]| r.iter().map(Vec::len).sum::<usize>();
    println!(
        "wrote {} ({} KB): land {} rings/{} pts, lakes {}/{}, borders {}/{}, rivers {}/{}, {} places",
        path.display(),
        std::fs::metadata(&path).unwrap().len() / 1024,
        land.len(),
        pts(&land),
        lakes.len(),
        pts(&lakes),
        borders.len(),
        pts(&borders),
        rivers.len(),
        pts(&rivers),
        places.len()
    );
}

fn round(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

fn inside(lon: f64, lat: f64) -> bool {
    lon >= LON.0 && lon <= LON.1 && lat >= LAT.0 && lat <= LAT.1
}

fn coords(v: &Value) -> Ring {
    v.as_array().unwrap().iter().map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap())).collect()
}

/// Outer and inner rings of all polygons, clipped to the area.
fn polygons(fc: &Value) -> Vec<Ring> {
    let mut out = Vec::new();
    for f in fc["features"].as_array().unwrap() {
        let g = &f["geometry"];
        let polys: Vec<&Value> = match g["type"].as_str() {
            Some("Polygon") => vec![&g["coordinates"]],
            Some("MultiPolygon") => g["coordinates"].as_array().unwrap().iter().collect(),
            _ => continue,
        };
        for poly in polys {
            for ring in poly.as_array().unwrap() {
                let clipped = clip_polygon(&coords(ring));
                let simple = simplify(&clipped);
                if simple.len() >= 3 {
                    out.push(simple);
                }
            }
        }
    }
    out
}

/// Polylines, split where they leave the area.
fn lines(fc: &Value) -> Vec<Ring> {
    let mut out = Vec::new();
    for f in fc["features"].as_array().unwrap() {
        let g = &f["geometry"];
        let parts: Vec<&Value> = match g["type"].as_str() {
            Some("LineString") => vec![&g["coordinates"]],
            Some("MultiLineString") => g["coordinates"].as_array().unwrap().iter().collect(),
            _ => continue,
        };
        for part in parts {
            let mut run: Ring = Vec::new();
            for p in coords(part) {
                if inside(p.0, p.1) {
                    run.push(p);
                } else {
                    if run.len() >= 2 {
                        out.push(simplify(&run));
                    }
                    run.clear();
                }
            }
            if run.len() >= 2 {
                out.push(simplify(&run));
            }
        }
    }
    out
}

/// Sutherland–Hodgman clipping of a polygon ring to the area rectangle.
fn clip_polygon(ring: &Ring) -> Ring {
    let mut poly = ring.clone();
    // (is the point inside this edge?, intersection with the edge line)
    let edges: [(Box<dyn Fn(&(f64, f64)) -> bool>, Box<dyn Fn(&(f64, f64), &(f64, f64)) -> (f64, f64)>); 4] = [
        (Box::new(|p| p.0 >= LON.0), Box::new(|a, b| at_x(a, b, LON.0))),
        (Box::new(|p| p.0 <= LON.1), Box::new(|a, b| at_x(a, b, LON.1))),
        (Box::new(|p| p.1 >= LAT.0), Box::new(|a, b| at_y(a, b, LAT.0))),
        (Box::new(|p| p.1 <= LAT.1), Box::new(|a, b| at_y(a, b, LAT.1))),
    ];
    for (is_in, cross) in &edges {
        if poly.is_empty() {
            break;
        }
        let mut next = Vec::with_capacity(poly.len());
        for i in 0..poly.len() {
            let (cur, prev) = (poly[i], poly[(i + poly.len() - 1) % poly.len()]);
            match (is_in(&cur), is_in(&prev)) {
                (true, true) => next.push(cur),
                (true, false) => {
                    next.push(cross(&prev, &cur));
                    next.push(cur);
                }
                (false, true) => next.push(cross(&prev, &cur)),
                (false, false) => {}
            }
        }
        poly = next;
    }
    poly
}

fn at_x(a: &(f64, f64), b: &(f64, f64), x: f64) -> (f64, f64) {
    let t = (x - a.0) / (b.0 - a.0);
    (x, a.1 + t * (b.1 - a.1))
}

fn at_y(a: &(f64, f64), b: &(f64, f64), y: f64) -> (f64, f64) {
    let t = (y - a.1) / (b.1 - a.1);
    (a.0 + t * (b.0 - a.0), y)
}

/// Douglas–Peucker simplification.
fn simplify(pts: &Ring) -> Ring {
    if pts.len() < 3 {
        return pts.clone();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (mut best, mut idx) = (0.0, 0);
        for i in a + 1..b {
            let d = seg_dist(pts[i], pts[a], pts[b]);
            if d > best {
                best = d;
                idx = i;
            }
        }
        if best > TOLERANCE {
            keep[idx] = true;
            stack.push((a, idx));
            stack.push((idx, b));
        }
    }
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

fn seg_dist(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    // Degrees of longitude are ~0.62 of latitude here.
    let k = 0.62;
    let (px, py, ax, ay, bx, by) = (p.0 * k, p.1, a.0 * k, a.1, b.0 * k, b.1);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    ((px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2)).sqrt()
}
