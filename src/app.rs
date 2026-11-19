//! The desktop app: outlook, rain graph and radar map for one location.

use crate::config::{self, Settings};
use crate::week::{weather_icon, week};
use chrono::{DateTime, Local, Utc};
use cleanweather_core::forecast::{self, Forecast};
use cleanweather_core::nowcast::{self, Field, Nowcast, Outlook, Sample, intensity_label};
use cleanweather_core::{basemap, mercator};
use cleanweather_core::radar::{Grid, RAIN_DBZ};
use cleanweather_core::service::Service;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, pos2, vec2};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often to ask KNMI whether a new scan is out (one arrives every 5 minutes).
const CHECK_EVERY: Duration = Duration::from_secs(90);
/// After an error, try again sooner.
const RETRY_EVERY: Duration = Duration::from_secs(45);
/// Older than this, the forecast gets a warning.
const STALE_MINUTES: i64 = 20;

/// Map area shown: roughly the Netherlands plus some North Sea, where rain comes from.
const MAP_LAT: (f64, f64) = (50.55, 53.85);
const MAP_LON: (f64, f64) = (2.3, 7.5);
const MAP_ZOOM: u32 = 8;

/// Refresh the 7-day forecast this often (models update every few hours).
const FORECAST_EVERY: Duration = Duration::from_secs(30 * 60);

enum Command {
    SetKey(String),
    SetLocation(f64, f64),
    Refresh,
}

#[derive(Default)]
struct Shared {
    nowcast: Option<Arc<Nowcast>>,
    forecast: Option<Arc<Forecast>>,
    error: Option<String>,
    forecast_error: Option<String>,
    checking: bool,
    last_check: Option<DateTime<Utc>>,
}

pub struct App {
    shared: Arc<Mutex<Shared>>,
    commands: Sender<Command>,
    settings: Settings,
    has_key: bool,
    key_input: String,
    show_settings: bool,
    /// Map frame shown: index into observed scans followed by forecast steps.
    frame: usize,
    playing: bool,
    last_tick: Instant,
    /// Textures per map frame, for the nowcast issued at `textures_for`.
    textures: Vec<Option<TextureHandle>>,
    textures_for: Option<(DateTime<Utc>, bool)>,
    /// Radar grid cell under each pixel of the map image, for the grid it was made for.
    lookup: Option<(Grid, Vec<Option<u32>>)>,
    /// The land/water image, for dark mode or not.
    land: Option<(bool, TextureHandle)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = channel();
        let key = config::api_key();
        let settings = Settings::load();
        {
            let shared = shared.clone();
            let ctx = cc.egui_ctx.clone();
            let key = key.clone();
            let at = (settings.lat, settings.lon);
            std::thread::spawn(move || worker(key, at, rx, shared, ctx));
        }
        App {
            shared,
            commands: tx,
            settings,
            has_key: key.is_some(),
            key_input: String::new(),
            show_settings: false,
            frame: usize::MAX,
            playing: false,
            last_tick: Instant::now(),
            textures: Vec::new(),
            textures_for: None,
            lookup: None,
            land: None,
        }
    }
}

/// Background thread: checks KNMI for new scans and recomputes the nowcast, and keeps
/// the 7-day forecast for the current location fresh.
fn worker(key: Option<String>, mut at: (f64, f64), rx: Receiver<Command>, shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    let mut service = key.map(|k| Service::new(k, config::cache_dir()));
    // When and where the forecast was last fetched.
    let mut forecast_for: Option<(Instant, (f64, f64))> = None;
    loop {
        let mut wait = CHECK_EVERY;
        let moved = |f: (f64, f64)| (f.0 - at.0).abs() > 0.005 || (f.1 - at.1).abs() > 0.005;
        if forecast_for.is_none_or(|(t, f)| t.elapsed() >= FORECAST_EVERY || moved(f)) {
            let result = forecast::fetch(at.0, at.1);
            let mut sh = shared.lock().unwrap();
            match result {
                Ok(f) => {
                    sh.forecast = Some(Arc::new(f));
                    sh.forecast_error = None;
                    forecast_for = Some((Instant::now(), at));
                }
                Err(e) => {
                    sh.forecast_error = Some(format!("{e:#}"));
                    wait = RETRY_EVERY;
                }
            }
            drop(sh);
            ctx.request_repaint();
        }
        if let Some(s) = service.as_mut() {
            shared.lock().unwrap().checking = true;
            ctx.request_repaint();
            let result = s.update();
            let mut sh = shared.lock().unwrap();
            sh.checking = false;
            sh.last_check = Some(Utc::now());
            match result {
                Ok(Some(nc)) => {
                    sh.nowcast = Some(Arc::new(nc));
                    sh.error = None;
                }
                Ok(None) => sh.error = None,
                Err(e) => {
                    sh.error = Some(format!("{e:#}"));
                    wait = RETRY_EVERY;
                }
            }
            drop(sh);
            ctx.request_repaint();
        }
        match rx.recv_timeout(wait) {
            Ok(Command::SetKey(k)) => service = Some(Service::new(k, config::cache_dir())),
            Ok(Command::SetLocation(lat, lon)) => at = (lat, lon),
            Ok(Command::Refresh) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

/// Colours of the rain graph and radar map.
struct Palette {
    dark: bool,
    text: Color32,
    weak: Color32,
    grid: Color32,
    rain: Color32,
    rain_past: Color32,
    now: Color32,
    pin: Color32,
}

impl Palette {
    fn new(ui: &egui::Ui) -> Palette {
        let dark = ui.visuals().dark_mode;
        Palette {
            dark,
            text: ui.visuals().strong_text_color(),
            weak: ui.visuals().weak_text_color(),
            grid: if dark { Color32::from_gray(60) } else { Color32::from_gray(215) },
            rain: Color32::from_rgb(0x2f, 0x7c, 0xf6),
            rain_past: if dark { Color32::from_rgb(0x4a, 0x5a, 0x78) } else { Color32::from_rgb(0xa8, 0xbd, 0xe0) },
            now: Color32::from_rgb(0xf0, 0x5a, 0x28),
            pin: Color32::from_rgb(0xe5, 0x2d, 0x2d),
        }
    }
}

/// Radar colour for a reflectivity, Buienradar-like: blues for light rain up to red and
/// purple for downpours. Transparent when dry.
fn radar_color(dbz: f32, dark: bool) -> Color32 {
    if dbz.is_nan() {
        return if dark { Color32::from_rgba_unmultiplied(0, 0, 0, 120) } else { Color32::from_rgba_unmultiplied(120, 120, 120, 90) };
    }
    if dbz < RAIN_DBZ {
        return Color32::TRANSPARENT;
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
    Color32::from_rgba_unmultiplied(c(0), c(1), c(2), 230)
}

/// The rain-rate axis: logarithmic, so 0.1 mm/h drizzle is visible (~12% high) while
/// 1, 5 and 20 mm/h sit at ~40%, ~70% and ~95%.
fn rate_y(mm_h: f32) -> f32 {
    ((1.0 + mm_h.max(0.0) * 10.0).ln() / (1.0 + 30.0 * 10.0f32).ln()).min(1.0)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (nowcast, forecast, error, forecast_error, checking, last_check) = {
            let s = self.shared.lock().unwrap();
            (s.nowcast.clone(), s.forecast.clone(), s.error.clone(), s.forecast_error.clone(), s.checking, s.last_check)
        };

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                self.status_bar(ui, nowcast.as_deref(), error.as_deref(), checking, last_check);
                if let Some(e) = &forecast_error {
                    ui.colored_label(Color32::from_rgb(0xd0, 0x30, 0x30), e);
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.has_key {
                self.key_prompt(ui);
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.header(ui, nowcast.as_deref(), forecast.as_deref());
                match nowcast.as_deref() {
                    Some(nc) => {
                        let samples = nc.point(self.settings.lat, self.settings.lon);
                        ui.add_space(8.0);
                        self.rain_graph(ui, &samples);
                        if let Some(f) = forecast.as_deref() {
                            ui.add_space(12.0);
                            week(ui, f);
                        }
                        ui.add_space(16.0);
                        self.radar_map(ui, ctx, nc);
                    }
                    None => {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            if error.is_none() {
                                ui.spinner();
                                ui.label("Fetching radar from KNMI…");
                            }
                        });
                    }
                }
            });
        });

        if self.show_settings {
            self.settings_window(ctx);
        }

        // Keep "minutes ago" fresh; animate when playing.
        if self.playing {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else {
            ctx.request_repaint_after(Duration::from_secs(20));
        }
    }
}

impl App {
    fn key_prompt(&mut self, ui: &mut egui::Ui) {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.heading("Connect to KNMI");
            ui.add_space(8.0);
            ui.label("CleanWeather reads KNMI's rain radar directly.\nPaste your Open Data API key from developer.dataplatform.knmi.nl:");
            ui.add_space(8.0);
            ui.add(egui::TextEdit::singleline(&mut self.key_input).desired_width(320.0).hint_text("API key"));
            ui.add_space(8.0);
            if ui.button("Save").clicked() && !self.key_input.trim().is_empty() {
                self.save_key();
            }
        });
    }

    fn save_key(&mut self) {
        let key = self.key_input.trim().to_string();
        if let Err(e) = config::save_api_key(&key) {
            self.shared.lock().unwrap().error = Some(format!("{e:#}"));
            return;
        }
        let _ = self.commands.send(Command::SetKey(key));
        self.has_key = true;
        self.key_input.clear();
    }

    fn header(&mut self, ui: &mut egui::Ui, nc: Option<&Nowcast>, forecast: Option<&Forecast>) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&self.settings.place).size(26.0).strong());
                ui.label(egui::RichText::new(format!("{:.3}° N, {:.3}° E", self.settings.lat, self.settings.lon)).small().weak());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("⚙").on_hover_text("Settings").clicked() {
                    self.show_settings = true;
                }
                // Current conditions: temperature, icon and a word.
                if let Some(f) = forecast {
                    let c = &f.current;
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(crate::week::degrees(c.temperature)).size(30.0));
                            let (r, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                            weather_icon(ui, r.center(), 30.0, c.condition, c.is_day);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| ui.label(egui::RichText::new(c.condition.label()).small().weak()));
                    });
                }
            });
        });
        if let Some(nc) = nc {
            let samples = nc.point(self.settings.lat, self.settings.lon);
            let text = match nowcast::outlook(&samples) {
                Outlook::Dry { until } => format!("Dry until at least {}", local(until)),
                Outlook::RainFrom { at, peak } => {
                    let mins = (at - Utc::now()).num_minutes();
                    let when = if mins <= 0 { "now".to_string() } else { format!("in {mins} min ({})", local(at)) };
                    let what = intensity_label(peak);
                    format!("{}{} {when}", what[..1].to_uppercase(), &what[1..])
                }
                Outlook::Raining { until: Some(u), peak } => format!("Rain stops around {} (up to {})", local(u), intensity_label(peak)),
                Outlook::Raining { until: None, peak } => format!("Rain for the next 2 hours (up to {})", intensity_label(peak)),
                Outlook::NoData => "No radar coverage here".into(),
            };
            ui.add_space(6.0);
            ui.label(egui::RichText::new(text).size(18.0));
        }
    }

    fn rain_graph(&self, ui: &mut egui::Ui, samples: &[Sample]) {
        let pal = Palette::new(ui);
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(width, 190.0), Sense::hover());
        let painter = ui.painter_at(rect);
        let plot = Rect::from_min_max(rect.min + vec2(4.0, 6.0), rect.max - vec2(4.0, 22.0));
        let (Some(first), Some(last)) = (samples.first(), samples.last()) else { return };
        let (t0, t1) = (first.time, last.time);
        let span = (t1 - t0).num_seconds().max(1) as f32;
        let x_of = |t: DateTime<Utc>| plot.left() + plot.width() * ((t - t0).num_seconds() as f32 / span);
        let y_of = |mm: f32| plot.bottom() - plot.height() * rate_y(mm);

        // Grid lines at the intensity boundaries.
        for (mm, label) in [(1.0, "light"), (5.0, "moderate"), (20.0, "heavy")] {
            let y = y_of(mm);
            painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0_f32, pal.grid));
            painter.text(pos2(plot.left() + 2.0, y + 2.0), Align2::LEFT_TOP, label, FontId::proportional(10.0), pal.weak);
        }
        painter.line_segment([pos2(plot.left(), plot.bottom()), pos2(plot.right(), plot.bottom())], Stroke::new(1.0_f32, pal.grid));

        // Rain as a filled area, one trapezoid per 5-minute step.
        for w in samples.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            let (ra, rb) = (a.mm_per_hour.unwrap_or(0.0), b.mm_per_hour.unwrap_or(0.0));
            if ra <= 0.0 && rb <= 0.0 {
                continue;
            }
            let color = if b.observed { pal.rain_past } else { pal.rain };
            let (xa, xb) = (x_of(a.time), x_of(b.time));
            let pts = vec![pos2(xa, plot.bottom()), pos2(xa, y_of(ra)), pos2(xb, y_of(rb)), pos2(xb, plot.bottom())];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        // Coverage gaps.
        for s in samples.iter().filter(|s| s.mm_per_hour.is_none()) {
            let x = x_of(s.time);
            painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(3.0_f32, pal.grid));
        }

        // "Now" line and time labels every 30 minutes.
        let now = Utc::now().clamp(t0, t1);
        let xn = x_of(now);
        painter.line_segment([pos2(xn, plot.top()), pos2(xn, plot.bottom())], Stroke::new(1.5_f32, pal.now));
        painter.text(pos2(xn, rect.bottom()), Align2::CENTER_BOTTOM, "now", FontId::proportional(11.0), pal.now);
        let mut t = t0.with_timezone(&Local);
        let m = chrono::Timelike::minute(&t);
        t += chrono::Duration::minutes(((30 - m % 30) % 30) as i64);
        t = t.with_timezone(&Local);
        while t.with_timezone(&Utc) <= t1 {
            let x = x_of(t.with_timezone(&Utc));
            if (x - xn).abs() > 36.0 {
                painter.text(pos2(x, rect.bottom()), Align2::CENTER_BOTTOM, t.format("%H:%M").to_string(), FontId::proportional(11.0), pal.weak);
            }
            t += chrono::Duration::minutes(30);
        }

        // Hover: the nearest sample.
        if let Some(p) = resp.hover_pos() {
            let nearest = samples.iter().min_by(|a, b| (x_of(a.time) - p.x).abs().total_cmp(&(x_of(b.time) - p.x).abs()));
            if let Some(s) = nearest {
                let x = x_of(s.time);
                painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0_f32, pal.weak));
                let text = match s.mm_per_hour {
                    Some(mm) => format!(
                        "{}{}\n{:.1} mm/h · {}\n{:.0}% of area wet",
                        local(s.time),
                        if s.observed { " (radar)" } else { " (forecast)" },
                        mm,
                        intensity_label(mm),
                        s.chance * 100.0
                    ),
                    None => format!("{}\nno radar data", local(s.time)),
                };
                resp.on_hover_text_at_pointer(text);
            }
        }
    }

    fn map_frames(nc: &Nowcast) -> Vec<(DateTime<Utc>, &Field, bool)> {
        let mut v: Vec<_> = nc.observed.iter().map(|(t, f)| (*t, f, true)).collect();
        v.extend(nc.forecast.iter().enumerate().map(|(k, f)| (nc.forecast_time(k), f, false)));
        v
    }

    fn radar_map(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, nc: &Nowcast) {
        let pal = Palette::new(ui);
        let frames = Self::map_frames(nc);
        let latest = nc.observed.len() - 1;
        if self.textures_for != Some((nc.issued, pal.dark)) {
            self.textures = vec![None; frames.len()];
            if self.textures_for.map(|t| t.0) != Some(nc.issued) {
                self.frame = latest;
            }
            self.textures_for = Some((nc.issued, pal.dark));
        }
        self.frame = self.frame.min(frames.len() - 1);

        // The map and the radar reprojected onto it, in Web Mercator at zoom 8
        // (~0.4 km per pixel; the radar itself is 1 km).
        let view = mercator::View::around(MAP_LAT, MAP_LON, MAP_ZOOM);
        let (rw, rh) = (view.width() as usize, view.height() as usize);
        if self.land.as_ref().map(|l| l.0) != Some(pal.dark) {
            let (land, water) = if pal.dark { ([44, 46, 50], [24, 28, 35]) } else { ([246, 245, 240], [212, 225, 238]) };
            // Twice the resolution, so coastlines stay sharp on high-DPI screens.
            let img = basemap::map().render(&view, rw * 2, rh * 2, land, water);
            let img = egui::ColorImage::from_rgba_unmultiplied([rw * 2, rh * 2], &img);
            self.land = Some((pal.dark, ctx.load_texture("land", img, egui::TextureOptions::LINEAR)));
        }
        if self.lookup.as_ref().map(|l| l.0) != Some(nc.grid) {
            self.lookup = Some((nc.grid, view.radar_lookup(&nc.grid, rw, rh)));
        }
        let i = self.frame;
        if self.textures[i].is_none() {
            let f = frames[i].1;
            let lookup = &self.lookup.as_ref().unwrap().1;
            let mut rgba = Vec::with_capacity(rw * rh * 4);
            for idx in lookup {
                let dbz = idx.map_or(f32::NAN, |k| f.dbz(k as usize));
                rgba.extend_from_slice(&radar_color(dbz, pal.dark).to_array());
            }
            let img = egui::ColorImage::from_rgba_premultiplied([rw, rh], &rgba);
            self.textures[i] = Some(ctx.load_texture(format!("radar{i}"), img, egui::TextureOptions::LINEAR));
        }

        let width = ui.available_width();
        let size = vec2(width, width * (view.height() / view.width()) as f32);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, if pal.dark { Color32::from_gray(28) } else { Color32::from_gray(238) });
        let scale = rect.width() / view.width() as f32;
        let to_screen = |x: f64, y: f64| pos2(rect.left() + (x - view.x0) as f32 * scale, rect.top() + (y - view.y0) as f32 * scale);
        let full_uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let geo = |lat: f64, lon: f64| {
            let (x, y) = mercator::to_world(lat, lon, view.zoom);
            to_screen(x, y)
        };
        let map = basemap::map();

        // Land and water, then rivers and borders, then the rain, then place names on top
        // so they stay readable.
        painter.image(self.land.as_ref().unwrap().1.id(), rect, full_uv, Color32::WHITE);
        let (river, border) = if pal.dark {
            (Color32::from_rgb(40, 52, 68), Color32::from_gray(95))
        } else {
            (Color32::from_rgb(180, 202, 226), Color32::from_gray(150))
        };
        for line in &map.rivers {
            painter.add(egui::Shape::line(line.iter().map(|p| geo(p[1], p[0])).collect(), Stroke::new(1.0_f32, river)));
        }
        for line in &map.borders {
            painter.add(egui::Shape::dashed_line(&line.iter().map(|p| geo(p[1], p[0])).collect::<Vec<_>>(), Stroke::new(1.0_f32, border), 4.0, 3.0));
        }
        painter.image(self.textures[i].as_ref().unwrap().id(), rect, full_uv, Color32::WHITE);

        let (mx, my) = mercator::to_world(self.settings.lat, self.settings.lon, view.zoom);
        let me = to_screen(mx, my);
        // Labels, biggest towns first, skipping any that would overlap one already placed
        // (or the pin). More fit when the window is wider.
        let mut taken = vec![Rect::from_center_size(me, vec2(16.0, 16.0))];
        let halo = if pal.dark { Color32::from_black_alpha(200) } else { Color32::from_white_alpha(220) };
        for place in &map.places {
            let p = geo(place.lat, place.lon);
            if !rect.shrink(4.0).contains(p) {
                continue;
            }
            let size = if place.population >= 300_000 { 12.0 } else { 10.5 };
            let galley = painter.layout_no_wrap(place.name.clone(), FontId::proportional(size), pal.text);
            // Label to the right of the dot, else to the left, else not at all.
            let dot = Rect::from_center_size(p, vec2(5.0, 5.0));
            let options = [Align2::LEFT_CENTER.anchor_size(p + vec2(5.0, 0.0), galley.size()), Align2::RIGHT_CENTER.anchor_size(p - vec2(5.0, 0.0), galley.size())];
            let fits = |r: &Rect| {
                let footprint = r.union(dot).expand(2.0);
                rect.contains_rect(*r) && !taken.iter().any(|t| t.intersects(footprint))
            };
            let Some(text_rect) = options.into_iter().find(fits) else { continue };
            let footprint = text_rect.union(dot).expand(2.0);
            taken.push(footprint);
            painter.circle_filled(p, 2.5, pal.text);
            for d in [vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(0.0, -1.0), vec2(0.0, 1.0)] {
                painter.galley(text_rect.min + d, painter.layout_no_wrap(place.name.clone(), FontId::proportional(size), halo), halo);
            }
            painter.galley(text_rect.min, galley, pal.text);
        }
        painter.circle_filled(me, 6.0, pal.pin);
        painter.circle_stroke(me, 6.0, Stroke::new(2.0_f32, Color32::WHITE));

        // Time of the frame shown, and the map's attribution, on small backgrounds.
        let (t, _, observed) = frames[i];
        let label = if observed { format!("{} radar", local(t)) } else { format!("{} forecast (+{} min)", local(t), (t - nc.issued).num_minutes()) };
        let backdrop = if pal.dark { Color32::from_black_alpha(170) } else { Color32::from_white_alpha(200) };
        let tag = |pos: Pos2, align: Align2, text: String, size: f32| {
            let galley = painter.layout_no_wrap(text, FontId::proportional(size), pal.text);
            let r = align.anchor_size(pos, galley.size()).expand2(vec2(5.0, 2.0));
            painter.rect_filled(r, 4.0, backdrop);
            painter.galley(r.min + vec2(5.0, 2.0), galley, pal.text);
        };
        tag(rect.left_top() + vec2(8.0, 8.0), Align2::LEFT_TOP, label, 13.0);
        tag(rect.right_bottom() - vec2(6.0, 4.0), Align2::RIGHT_BOTTOM, "Map: Natural Earth · Radar: KNMI".into(), 9.5);

        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.clicked()) {
            let x = view.x0 + ((p.x - rect.left()) / scale) as f64;
            let y = view.y0 + ((p.y - rect.top()) / scale) as f64;
            let (lat, lon) = mercator::to_latlon(x, y, view.zoom);
            self.set_location(lat, lon);
        }
        resp.on_hover_text("Click to set your location");

        // Timeline controls.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button(if self.playing { "⏸" } else { "▶" }).clicked() {
                self.playing = !self.playing;
                self.last_tick = Instant::now();
            }
            ui.spacing_mut().slider_width = ui.available_width() - 70.0;
            let mut f = self.frame;
            if ui.add(egui::Slider::new(&mut f, 0..=frames.len() - 1).show_value(false)).changed() {
                self.frame = f;
                self.playing = false;
            }
            if ui.button("Now").clicked() {
                self.frame = latest;
                self.playing = false;
            }
        });
        if self.playing && self.last_tick.elapsed() >= Duration::from_millis(350) {
            self.last_tick = Instant::now();
            self.frame = (self.frame + 1) % frames.len();
        }

        // Legend.
        ui.horizontal(|ui| {
            for (dbz, name) in [(14.0, "light"), (26.0, ""), (33.0, "moderate"), (40.0, ""), (45.0, "heavy"), (52.0, "")] {
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, radar_color(dbz, pal.dark));
                if !name.is_empty() {
                    ui.label(egui::RichText::new(name).small().weak());
                }
            }
        });
        if let Some((kmh, dir)) = nc.motion.mean_motion() {
            let compass = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][((dir + 22.5) / 45.0) as usize % 8];
            ui.label(egui::RichText::new(format!("Rain moving {compass} at about {kmh:.0} km/h")).small().weak());
        }
    }

    fn set_location(&mut self, lat: f64, lon: f64) {
        self.settings.place = match basemap::map().nearest_place(lat, lon) {
            Some((p, d)) if d < 4.0 => p.name.clone(),
            Some((p, d)) if d < 25.0 => format!("Near {}", p.name),
            _ => "Selected location".into(),
        };
        self.settings.lat = lat;
        self.settings.lon = lon;
        let _ = self.commands.send(Command::SetLocation(lat, lon));
        let _ = self.settings.save();
    }

    fn status_bar(&self, ui: &mut egui::Ui, nc: Option<&Nowcast>, error: Option<&str>, checking: bool, last_check: Option<DateTime<Utc>>) {
        if let Some(nc) = nc {
            let age = (Utc::now() - nc.issued).num_minutes();
            let text = format!("Radar {} ({age} min ago)", local(nc.issued));
            if age > STALE_MINUTES {
                ui.colored_label(Color32::from_rgb(0xe0, 0x8a, 0x00), format!("⚠ {text}: forecast may be outdated"));
            } else {
                ui.label(egui::RichText::new(text).small().weak());
            }
            if !nc.radars_down.is_empty() {
                // De Bilt's old radar is listed but no longer used; only note real gaps.
                let down: Vec<&str> = nc.radars_down.iter().map(String::as_str).filter(|r| *r != "DeBilt").collect();
                if !down.is_empty() {
                    ui.colored_label(Color32::from_rgb(0xe0, 0x8a, 0x00), format!("⚠ radar down: {}", down.join(", ")));
                }
            }
        }
        if checking {
            ui.spinner();
        } else if let Some(t) = last_check {
            ui.label(egui::RichText::new(format!("checked {}", local(t))).small().weak());
        }
        if let Some(e) = error {
            ui.colored_label(Color32::from_rgb(0xd0, 0x30, 0x30), e);
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("Settings").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Place name");
                ui.text_edit_singleline(&mut self.settings.place);
                ui.end_row();
                ui.label("Latitude");
                ui.add(egui::DragValue::new(&mut self.settings.lat).speed(0.01).range(48.9..=56.0).max_decimals(4));
                ui.end_row();
                ui.label("Longitude");
                ui.add(egui::DragValue::new(&mut self.settings.lon).speed(0.01).range(0.0..=10.8).max_decimals(4));
                ui.end_row();
                ui.label("KNMI API key");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.key_input).password(true).hint_text("replace key").desired_width(180.0));
                    if ui.button("Save").clicked() && !self.key_input.trim().is_empty() {
                        self.save_key();
                    }
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            if config::EMBEDDED_KEY.is_some() {
                ui.label(egui::RichText::new("This build has a KNMI key built in; a key saved here takes precedence.").small().weak());
            }
            ui.label(egui::RichText::new("Tip: click the map to set your location.").small().weak());
            if ui.button("Refresh now").clicked() {
                let _ = self.commands.send(Command::Refresh);
            }
        });
        if !open {
            self.show_settings = false;
            let _ = self.settings.save();
            let _ = self.commands.send(Command::SetLocation(self.settings.lat, self.settings.lon));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rain_axis_is_monotonic() {
        let mut last = -1.0;
        for mm in [0.0, 0.1, 0.5, 1.0, 5.0, 20.0, 30.0] {
            let y = rate_y(mm);
            assert!(y > last);
            last = y;
        }
        assert_eq!(rate_y(100.0), 1.0);
    }
}
