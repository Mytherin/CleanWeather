//! The radar map for Swift, drawn like the desktop app's: land and water and the rain
//! as images, rivers, borders and town labels as geometry for SwiftUI to draw crisply.
//!
//! A [`MapCanvas`] is one area at one zoom (Web Mercator, one world pixel per point).
//! The app makes one somewhat larger than the screen, so panning just moves it, and
//! asks for a new one when the gesture ends. Each map pixel's position in the radar
//! grids is worked out once per canvas, so stepping through the frames (playing the
//! animation) is only a lookup per pixel.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use cleanweather_core::basemap::{self, Shape};
use cleanweather_core::mercator::{self, View};
use cleanweather_core::nowcast::{Field, Nowcast};
use cleanweather_core::radar::RAIN_DBZ;

use crate::secs;

/// An image, premultiplied RGBA, row by row from the top.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One step of the map's timeline.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapFrame {
    pub time: i64,
    /// A radar scan (the past), not a forecast.
    pub observed: bool,
    /// Minutes after the newest scan (0 or less for scans).
    pub minutes_ahead: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LineKind {
    River,
    Border,
}

/// A line in canvas points: x0, y0, x1, y1, …
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapLine {
    pub kind: LineKind,
    pub points: Vec<f32>,
}

/// A town's dot at (x, y) in canvas points, its name to the right of it or, where
/// that doesn't fit, to the left.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct MapLabel {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub big: bool,
    pub text_on_right: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct MapPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, uniffi::Record)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

/// Radar colour for a reflectivity, Buienradar-like: blues for light rain up to red and
/// purple for downpours; transparent when dry, grey where there's no radar. Straight
/// (not premultiplied) RGBA, the same as the desktop app's.
pub(crate) fn radar_rgba(dbz: f32, dark: bool) -> [u8; 4] {
    if dbz.is_nan() {
        return if dark { [0, 0, 0, 120] } else { [120, 120, 120, 90] };
    }
    if dbz < RAIN_DBZ {
        return [0; 4];
    }
    let stops: [(f32, [u8; 3]); 7] = [
        (RAIN_DBZ, [0x9c, 0xd3, 0xff]),
        (20.0, [0x4a, 0x9c, 0xf7]),
        (28.0, [0x1d, 0x4f, 0xd8]),
        (36.0, [0x0b, 0x2a, 0x8a]),
        (42.0, [0xf2, 0xc2, 0x1b]),
        (48.0, [0xe5, 0x30, 0x2d]),
        (55.0, [0xb0, 0x2a, 0xc8]),
    ];
    let i = stops.iter().rposition(|s| dbz >= s.0).unwrap_or(0);
    let (lo, hi) = (stops[i], stops[(i + 1).min(stops.len() - 1)]);
    let t = if hi.0 > lo.0 { ((dbz - lo.0) / (hi.0 - lo.0)).clamp(0.0, 1.0) } else { 0.0 };
    let c = |k: usize| (lo.1[k] as f32 + (hi.1[k] as f32 - lo.1[k] as f32) * t) as u8;
    [c(0), c(1), c(2), 230]
}

fn premultiplied(c: [u8; 4]) -> [u8; 4] {
    let a = c[3] as u16;
    let m = |v: u8| ((v as u16 * a + 127) / 255) as u8;
    [m(c[0]), m(c[1]), m(c[2]), c[3]]
}

/// No radar pixel under this map pixel.
const NONE: u32 = u32::MAX;

/// A nowcast shown on the map, and where each map pixel falls in its grid.
struct Layer {
    nowcast: Arc<Nowcast>,
    lookup: OnceLock<Vec<u32>>,
}

impl Layer {
    fn frames(&self) -> usize {
        self.nowcast.observed.len() + self.nowcast.forecast.len()
    }

    fn field(&self, i: usize) -> &Field {
        let nc = &self.nowcast;
        if i < nc.observed.len() { &nc.observed[i].1 } else { &nc.forecast[i - nc.observed.len()] }
    }

    fn time(&self, i: usize) -> DateTime<Utc> {
        let nc = &self.nowcast;
        if i < nc.observed.len() { nc.observed[i].0 } else { nc.forecast_time(i - nc.observed.len()) }
    }
}

/// A step of the timeline: its time, scan (true) or forecast, and each layer's frame then.
type Step = (DateTime<Utc>, bool, Option<usize>, Option<usize>);

#[derive(uniffi::Object)]
pub struct MapCanvas {
    view: View,
    width: u32,
    height: u32,
    scale: f64,
    /// The location's own radar (on top) and Europe's (underneath).
    local: Option<Layer>,
    europe: Option<Layer>,
    timeline: Vec<Step>,
    issued: Option<DateTime<Utc>>,
    observed: usize,
    attribution: Option<String>,
}

impl MapCanvas {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        lat: f64,
        lon: f64,
        zoom: f64,
        width: u32,
        height: u32,
        scale: f64,
        local: Option<Arc<Nowcast>>,
        europe: Option<Arc<Nowcast>>,
        attribution: Option<String>,
    ) -> MapCanvas {
        let (width, height) = (width.clamp(1, 8192), height.clamp(1, 8192));
        let view = View::centred(lat, lon, zoom, width as f64, height as f64);
        let layer = |nowcast: Arc<Nowcast>| Layer { nowcast, lookup: OnceLock::new() };
        let (local, europe) = (local.map(layer), europe.map(layer));
        // The timeline follows the local radar's frames, else Europe's.
        let mut timeline = Vec::new();
        let base = local.as_ref().or(europe.as_ref());
        if let Some(b) = base {
            for i in 0..b.frames() {
                let t = b.time(i);
                let observed = i < b.nowcast.observed.len();
                let local_i = local.as_ref().map(|_| i);
                // Europe's frame nearest in time, within 5 minutes.
                let europe_i = europe.as_ref().and_then(|e| {
                    let j = (0..e.frames()).min_by_key(|&j| (e.time(j) - t).num_seconds().abs())?;
                    ((e.time(j) - t).num_minutes().abs() <= 5).then_some(j)
                });
                timeline.push((t, observed, local_i, europe_i));
            }
        }
        MapCanvas {
            view,
            width,
            height,
            scale: scale.clamp(0.5, 4.0),
            issued: base.map(|b| b.nowcast.issued),
            observed: base.map_or(0, |b| b.nowcast.observed.len()),
            local,
            europe,
            timeline,
            attribution,
        }
    }

    /// Where each map pixel (one per point) is in a layer's grid.
    fn lookup<'a>(&self, layer: &'a Layer) -> &'a [u32] {
        layer.lookup.get_or_init(|| {
            let (w, h) = (self.width as usize, self.height as usize);
            let g = layer.nowcast.grid;
            // Web Mercator is separable: latitude depends only on y, longitude on x.
            let lats: Vec<f64> = (0..h).map(|y| mercator::to_latlon(0.0, self.view.y0 + y as f64 + 0.5, self.view.zoom).0).collect();
            let lons: Vec<f64> = (0..w).map(|x| mercator::to_latlon(self.view.x0 + x as f64 + 0.5, 0.0, self.view.zoom).1).collect();
            let mut out = Vec::with_capacity(w * h);
            for &lat in &lats {
                for &lon in &lons {
                    let (c, r) = g.to_pixel(lat, lon);
                    out.push(if c >= 0.0 && r >= 0.0 && (c as usize) < g.width && (r as usize) < g.height { (r as usize * g.width + c as usize) as u32 } else { NONE });
                }
            }
            out
        })
    }
}

#[uniffi::export]
impl MapCanvas {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn zoom(&self) -> f64 {
        self.view.zoom
    }

    /// The map's timeline: the last 30 minutes of scans, then 2 hours of forecast.
    pub fn frames(&self) -> Vec<MapFrame> {
        self.timeline
            .iter()
            .map(|&(t, observed, _, _)| MapFrame {
                time: secs(t),
                observed,
                minutes_ahead: self.issued.map_or(0, |i| (t - i).num_minutes() as i32),
            })
            .collect()
    }

    /// The frame of the newest scan.
    pub fn latest_frame(&self) -> u32 {
        self.observed.saturating_sub(1) as u32
    }

    /// Land and water, at `scale` pixels per point.
    pub fn base_image(&self, dark: bool) -> MapImage {
        let (land, water) = if dark { ([44, 46, 50], [24, 28, 35]) } else { ([246, 245, 240], [212, 225, 238]) };
        let (w, h) = (((self.width as f64 * self.scale).round() as usize).max(1), ((self.height as f64 * self.scale).round() as usize).max(1));
        let rgba = basemap::map().render(&self.view, w, h, land, water);
        MapImage { width: w as u32, height: h as u32, rgba }
    }

    /// The rain in a frame of the timeline, one pixel per point (transparent where
    /// dry): the location's radar where it has data, Europe's elsewhere.
    pub fn radar_image(&self, frame: u32, dark: bool) -> MapImage {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut rgba = vec![0u8; w * h * 4];
        if let Some(&(_, _, local_i, europe_i)) = self.timeline.get(frame as usize) {
            let local = self.local.as_ref().zip(local_i).map(|(l, i)| (self.lookup(l), l.field(i)));
            let europe = self.europe.as_ref().zip(europe_i).map(|(e, j)| (self.lookup(e), e.field(j)));
            let no_data = premultiplied(radar_rgba(f32::NAN, dark));
            for k in 0..w * h {
                let mut c = [0u8; 4];
                let mut on_local = false;
                if let Some((lut, f)) = &local
                    && lut[k] != NONE
                {
                    on_local = true;
                    let d = f.dbz(lut[k] as usize);
                    if !d.is_nan() {
                        rgba[k * 4..k * 4 + 4].copy_from_slice(&premultiplied(radar_rgba(d, dark)));
                        continue;
                    }
                }
                if let Some((lut, f)) = &europe {
                    if lut[k] != NONE {
                        c = premultiplied(radar_rgba(f.dbz(lut[k] as usize), dark));
                    }
                } else if on_local {
                    c = no_data;
                }
                rgba[k * 4..k * 4 + 4].copy_from_slice(&c);
            }
        }
        MapImage { width: w as u32, height: h as u32, rgba }
    }

    /// Rivers (from zoom 5) and borders on the canvas, simplified for the zoom.
    pub fn lines(&self) -> Vec<MapLine> {
        let v = &self.view;
        let lod = basemap::map().lod(v.zoom.ceil().max(0.0) as u32);
        let k = (v.zoom - lod.zoom as f64).exp2();
        let visible = |s: &&Shape| s.overlaps(v.x0 / k, v.y0 / k, v.x1 / k, v.y1 / k);
        let line = |kind: LineKind, s: &Shape| MapLine {
            kind,
            points: s.points.iter().flat_map(|&(x, y)| [(x * k - v.x0) as f32, (y * k - v.y0) as f32]).collect(),
        };
        let mut out = Vec::new();
        if v.zoom >= 5.0 {
            out.extend(lod.rivers.iter().filter(visible).map(|s| line(LineKind::River, s)));
        }
        out.extend(lod.borders.iter().filter(visible).map(|s| line(LineKind::Border, s)));
        out
    }

    /// Town labels, biggest first, leaving out any that would overlap one already
    /// placed or the pin at (`pin_lat`, `pin_lon`).
    pub fn labels(&self, pin_lat: f64, pin_lon: f64) -> Vec<MapLabel> {
        // Rectangles as (x0, y0, x1, y1). Text widths are estimated: ~0.56 em per letter.
        type R = (f32, f32, f32, f32);
        let overlaps = |a: &R, b: &R| a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3;
        let pin = self.project(pin_lat, pin_lon);
        let (px, py) = (pin.x as f32, pin.y as f32);
        let mut taken: Vec<R> = vec![(px - 9.0, py - 9.0, px + 9.0, py + 9.0)];
        let (w, h) = (self.width as f32, self.height as f32);
        let mut out = Vec::new();
        for place in &basemap::map().places {
            let p = self.project(place.lat, place.lon);
            let (x, y) = (p.x as f32, p.y as f32);
            if x < 4.0 || y < 4.0 || x > w - 4.0 || y > h - 4.0 {
                continue;
            }
            let big = place.population >= 300_000;
            let size = if big { 12.0 } else { 10.5 };
            let (tw, th) = (place.name.chars().count() as f32 * size * 0.56, size * 1.25);
            let right: R = (x + 5.0, y - th / 2.0, x + 5.0 + tw, y + th / 2.0);
            let left: R = (x - 5.0 - tw, y - th / 2.0, x - 5.0, y + th / 2.0);
            let fits = |r: &R| {
                let f = (r.0.min(x - 2.5) - 2.0, r.1.min(y - 2.5) - 2.0, r.2.max(x + 2.5) + 2.0, r.3.max(y + 2.5) + 2.0);
                r.0 >= 0.0 && r.1 >= 0.0 && r.2 <= w && r.3 <= h && !taken.iter().any(|t| overlaps(t, &f))
            };
            let Some((r, on_right)) = [(right, true), (left, false)].into_iter().find(|(r, _)| fits(r)) else { continue };
            taken.push((r.0.min(x - 2.5) - 2.0, r.1.min(y - 2.5) - 2.0, r.2.max(x + 2.5) + 2.0, r.3.max(y + 2.5) + 2.0));
            out.push(MapLabel { name: place.name.clone(), x, y, big, text_on_right: on_right });
        }
        out
    }

    /// Where a location is on the canvas, in points.
    pub fn project(&self, lat: f64, lon: f64) -> MapPoint {
        let (x, y) = mercator::to_world(lat, lon, self.view.zoom);
        MapPoint { x: x - self.view.x0, y: y - self.view.y0 }
    }

    /// The location at a point of the canvas.
    pub fn location(&self, x: f64, y: f64) -> LatLon {
        let (lat, lon) = mercator::to_latlon(self.view.x0 + x, self.view.y0 + y, self.view.zoom);
        LatLon { lat, lon }
    }

    /// The credit line: the map's and the radar's.
    pub fn attribution(&self) -> String {
        let radar = match (&self.attribution, &self.europe) {
            (Some(a), _) => a.clone(),
            (None, Some(_)) => cleanweather_core::opera::ATTRIBUTION.to_string(),
            (None, None) => return "Map: Natural Earth".into(),
        };
        format!("Map: Natural Earth · Radar: {radar}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use cleanweather_core::radar::{Frame, Grid};

    /// A KNMI-grid nowcast with rain (40 dBZ) in a box around a location.
    fn nowcast_with_rain_at(lat: f64, lon: f64) -> Nowcast {
        let grid = Grid::knmi();
        let (c, r) = grid.to_pixel(lat, lon);
        let now = Utc::now();
        let frames: Vec<Frame> = (0..3)
            .map(|k| {
                let mut dbz = vec![0.0f32; grid.width * grid.height];
                for y in (r as usize - 10)..(r as usize + 10) {
                    for x in (c as usize - 10)..(c as usize + 10) {
                        dbz[y * grid.width + x] = 40.0;
                    }
                }
                Frame { time: now - Duration::minutes(5 * k), grid, dbz, radars_up: vec![], radars_down: vec![], source: "KNMI" }
            })
            .collect();
        Nowcast::compute(&frames, 4).unwrap()
    }

    #[test]
    fn rain_is_drawn_where_it_is() {
        let (lat, lon) = (52.09, 5.12);
        let nc = Arc::new(nowcast_with_rain_at(lat, lon));
        let canvas = MapCanvas::new(lat, lon, 8.0, 300, 200, 2.0, Some(nc), None, Some("KNMI".into()));
        assert_eq!(canvas.frames().len(), 3 + 4);
        assert_eq!(canvas.latest_frame(), 2);
        assert!(canvas.frames()[3].minutes_ahead == 5 && !canvas.frames()[3].observed);
        let img = canvas.radar_image(canvas.latest_frame(), false);
        let px = |x: usize, y: usize| &img.rgba[(y * 300 + x) * 4..(y * 300 + x) * 4 + 4];
        let expected = premultiplied(radar_rgba(40.0, false));
        assert_eq!(px(150, 100), expected, "rain at the centre");
        assert_eq!(px(5, 5)[3], 0, "dry far away");
        // The location projects to the centre and back.
        let p = canvas.project(lat, lon);
        assert!((p.x - 150.0).abs() < 1e-6 && (p.y - 100.0).abs() < 1e-6);
        let back = canvas.location(p.x, p.y);
        assert!((back.lat - lat).abs() < 1e-9 && (back.lon - lon).abs() < 1e-9);
        assert_eq!(canvas.attribution(), "Map: Natural Earth · Radar: KNMI");
    }

    #[test]
    fn labels_dont_overlap_and_lines_are_on_the_canvas() {
        let canvas = MapCanvas::new(52.2, 5.3, 7.0, 400, 400, 1.0, None, None, None);
        let labels = canvas.labels(52.2, 5.3);
        assert!(labels.iter().any(|l| l.name == "Amsterdam"), "{:?}", labels.iter().map(|l| &l.name).collect::<Vec<_>>());
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert!(a.name != b.name || (a.x, a.y) != (b.x, b.y));
            }
        }
        let lines = canvas.lines();
        assert!(lines.iter().any(|l| l.kind == LineKind::Border), "the Dutch–German border");
        assert!(lines.iter().all(|l| l.points.len() >= 4 && l.points.len() % 2 == 0));
        assert_eq!(canvas.radar_image(0, false).rgba.len(), 400 * 400 * 4);
        assert_eq!(canvas.attribution(), "Map: Natural Earth");
    }

    #[test]
    fn colours() {
        assert_eq!(radar_rgba(0.0, false), [0; 4]);
        assert_eq!(premultiplied([255, 0, 128, 255]), [255, 0, 128, 255]);
        assert_eq!(premultiplied([255, 255, 255, 0]), [0, 0, 0, 0]);
    }
}
