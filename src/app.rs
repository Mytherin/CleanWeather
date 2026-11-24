//! The desktop app: outlook, rain graph and radar map for one location.

use crate::config::{self, Settings};
use crate::week::{weather_icon, week};
use chrono::{DateTime, Local, Utc};
use cleanweather_core::forecast::{self, Forecast};
use cleanweather_core::nowcast::{self, Field, Nowcast, Outlook, Sample, intensity_label};
use cleanweather_core::{basemap, mercator};
use cleanweather_core::radar::RAIN_DBZ;
use cleanweather_core::service::{Europe, Service};
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
/// Starting zoom (about the Netherlands across) and the zoom range (Europe to a city).
const MAP_START_ZOOM: f64 = 7.0;
const MAP_ZOOM_RANGE: (f64, f64) = (3.5, 10.0);

/// The land/water image for an area somewhat larger than the map view.
struct LandCanvas {
    dark: bool,
    canvas: mercator::View,
    texture: TextureHandle,
}

/// Refresh the 7-day forecast this often (models update every few hours).
const FORECAST_EVERY: Duration = Duration::from_secs(30 * 60);

enum Command {
    /// A key was saved: rebuild the radar sources with the current keys.
    KeysChanged,
    SetLocation(f64, f64),
    Refresh,
}

#[derive(Default)]
struct Shared {
    nowcast: Option<Arc<Nowcast>>,
    forecast: Option<Arc<Forecast>>,
    error: Option<String>,
    forecast_error: Option<String>,
    /// Which radar the nowcast is from, and why a preferred one wasn't used.
    source: Option<&'static str>,
    fallback: Option<String>,
    checking: bool,
    last_check: Option<DateTime<Utc>>,
    /// Rain across Europe (OPERA, 4 km) for the map, and which of its pixels the local
    /// nowcast covers (shown from the local radar instead).
    europe: Option<Arc<Nowcast>>,
    europe_mask: Option<Arc<Vec<bool>>>,
}

pub struct App {
    shared: Arc<Mutex<Shared>>,
    commands: Sender<Command>,
    settings: Settings,
    key_input: String,
    meteogate_input: String,
    show_settings: bool,
    /// The location picker, and whether the next map click sets the location.
    picker_open: bool,
    picker_query: String,
    picking_on_map: bool,
    /// Map frame shown: index into observed scans followed by forecast steps.
    frame: usize,
    playing: bool,
    last_tick: Instant,
    /// Textures per map frame, for the nowcast issued at `textures_for`.
    textures: Vec<Option<TextureHandle>>,
    textures_for: Option<TextureKey>,
    /// Textures of the Europe-wide frames, for (nowcast, dark mode, coverage mask).
    europe_textures: Vec<Option<TextureHandle>>,
    europe_textures_for: Option<(DateTime<Utc>, bool, usize)>,
    /// The map's centre and zoom (the centre starts at the location).
    map_centre: Option<(f64, f64)>,
    map_zoom: f64,
    last_zoom_input: Instant,
    land: Option<LandCanvas>,
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
            key_input: String::new(),
            meteogate_input: String::new(),
            show_settings: false,
            // CLEANWEATHER_SEARCH opens the location picker with this text (for testing).
            picker_open: std::env::var("CLEANWEATHER_SEARCH").is_ok(),
            picker_query: std::env::var("CLEANWEATHER_SEARCH").unwrap_or_default(),
            picking_on_map: false,
            frame: usize::MAX,
            playing: false,
            last_tick: Instant::now(),
            textures: Vec::new(),
            textures_for: None,
            europe_textures: Vec::new(),
            europe_textures_for: None,
            map_centre: None,
            // CLEANWEATHER_MAP_ZOOM overrides the starting zoom (e.g. 4.5 for all of Europe).
            map_zoom: std::env::var("CLEANWEATHER_MAP_ZOOM").ok().and_then(|z| z.parse().ok()).unwrap_or(MAP_START_ZOOM),
            last_zoom_input: Instant::now(),
            land: None,
        }
    }
}

/// Background thread: checks the radar sources for new scans and recomputes the
/// nowcast, and keeps the 7-day forecast for the current location fresh.
fn worker(key: Option<String>, mut at: (f64, f64), rx: Receiver<Command>, shared: Arc<Mutex<Shared>>, ctx: egui::Context) {
    let new_service = |knmi: Option<String>| Service::new(knmi, config::meteogate_key(), config::cache_dir());
    let mut service = Some(new_service(key));
    let mut europe = Europe::new(config::meteogate_key(), config::cache_dir());
    let mut mask_for: Option<(DateTime<Utc>, cleanweather_core::radar::Grid, DateTime<Utc>)> = None;
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
            let result = s.update(at.0, at.1);
            let mut sh = shared.lock().unwrap();
            sh.checking = false;
            sh.last_check = Some(Utc::now());
            match result {
                Ok(Some(u)) => {
                    sh.nowcast = Some(Arc::new(u.nowcast));
                    sh.source = Some(u.source);
                    sh.fallback = u.fallback_reason;
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
        // Rain across Europe for the map; failures here only leave the map local.
        let europe_new = match europe.update() {
            Ok(Some(nc)) => Some(Arc::new(nc)),
            _ => None,
        };
        let (local, europe_now) = {
            let mut sh = shared.lock().unwrap();
            if let Some(e) = europe_new {
                sh.europe = Some(e);
            }
            (sh.nowcast.clone(), sh.europe.clone())
        };
        // Which Europe pixels the local radar covers, when either has changed.
        if let (Some(l), Some(e)) = (&local, &europe_now) {
            let id = (l.issued, l.grid, e.issued);
            if mask_for != Some(id) {
                let mask = Arc::new(cleanweather_core::service::covered(&e.grid, l));
                shared.lock().unwrap().europe_mask = Some(mask);
                mask_for = Some(id);
                ctx.request_repaint();
            }
        }
        match rx.recv_timeout(wait) {
            Ok(Command::KeysChanged) => {
                service = Some(new_service(config::api_key()));
                europe = Europe::new(config::meteogate_key(), config::cache_dir());
            }
            Ok(Command::SetLocation(lat, lon)) => at = (lat, lon),
            Ok(Command::Refresh) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Lowercase without accents, for matching typed place names ("dusseldorf", "zurich").
fn fold(s: &str) -> String {
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

/// What the map's radar textures were made for: the textures are rebuilt when it changes.
/// A move can give a nowcast for the same scan on another grid (another OPERA square,
/// or KNMI ↔ OPERA), with another number of scans.
type TextureKey = (DateTime<Utc>, cleanweather_core::radar::Grid, usize, bool);

fn texture_key(nc: &Nowcast, dark: bool) -> TextureKey {
    (nc.issued, nc.grid, nc.observed.len() + nc.forecast.len(), dark)
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
        let (nowcast, forecast, error, forecast_error, checking, last_check, source, fallback, europe, europe_mask) = {
            let s = self.shared.lock().unwrap();
            (s.nowcast.clone(), s.forecast.clone(), s.error.clone(), s.forecast_error.clone(), s.checking, s.last_check, s.source, s.fallback.clone(), s.europe.clone(), s.europe_mask.clone())
        };

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                self.status_bar(ui, nowcast.as_deref(), error.as_deref(), checking, last_check);
                if let Some(src) = source.filter(|_| nowcast.is_some()) {
                    let label = ui.label(egui::RichText::new(format!("· {src}")).small().weak());
                    if let Some(why) = &fallback {
                        label.on_hover_text(format!("Using {src} because {why}"));
                    }
                }
                if let Some(why) = &fallback {
                    ui.colored_label(Color32::from_rgb(0xe0, 0x8a, 0x00), "⚠ fallback").on_hover_text(why);
                }
                if let Some(e) = &forecast_error {
                    ui.colored_label(Color32::from_rgb(0xd0, 0x30, 0x30), e);
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
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
                        let europe = europe.as_deref().zip(europe_mask.as_ref());
                        self.radar_map(ui, ctx, nc, source, europe);
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
        if self.picker_open {
            self.location_picker(ctx);
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
    /// Save a key typed in Settings (KNMI or MeteoGate) and rebuild the sources.
    fn save_key(&mut self, meteogate: bool) {
        let input = if meteogate { &mut self.meteogate_input } else { &mut self.key_input };
        let key = input.trim().to_string();
        input.clear();
        let saved = if meteogate { config::save_meteogate_key(&key) } else { config::save_api_key(&key) };
        if let Err(e) = saved {
            self.shared.lock().unwrap().error = Some(format!("{e:#}"));
            return;
        }
        let _ = self.commands.send(Command::KeysChanged);
    }

    fn header(&mut self, ui: &mut egui::Ui, nc: Option<&Nowcast>, forecast: Option<&Forecast>) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                // The place name, with a small arrow, opens the location picker.
                let name = ui
                    .horizontal(|ui| {
                        let label = ui.add(egui::Label::new(egui::RichText::new(&self.settings.place).size(26.0).strong()).sense(Sense::click()));
                        let (r, arrow) = ui.allocate_exact_size(vec2(18.0, 26.0), Sense::click());
                        let c = r.center() + vec2(0.0, 2.0);
                        let color = if label.hovered() || arrow.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                        ui.painter().add(egui::Shape::convex_polygon(vec![c + vec2(-6.0, -3.0), c + vec2(6.0, -3.0), c + vec2(0.0, 4.0)], color, Stroke::NONE));
                        label.union(arrow)
                    })
                    .inner
                    .on_hover_text("Change location")
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if name.clicked() {
                    self.picker_open = !self.picker_open;
                    self.picker_query.clear();
                }
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

    fn radar_map(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, nc: &Nowcast, source: Option<&str>, europe: Option<(&Nowcast, &Arc<Vec<bool>>)>) {
        let pal = Palette::new(ui);
        let frames = Self::map_frames(nc);
        let latest = nc.observed.len() - 1;
        if self.textures_for != Some(texture_key(nc, pal.dark)) {
            self.textures = vec![None; frames.len()];
            if self.textures_for.map(|t| t.0) != Some(nc.issued) {
                self.frame = latest;
            }
            self.textures_for = Some(texture_key(nc, pal.dark));
        }
        self.frame = self.frame.min(frames.len() - 1);

        // The radar frame, as an image of its own grid (drawn warped onto the map below).
        // With the Europe layer underneath, areas without local data are left clear.
        let g = nc.grid;
        let i = self.frame;
        if self.textures[i].is_none() {
            let f = frames[i].1;
            let mut rgba = Vec::with_capacity(g.width * g.height * 4);
            for k in 0..g.width * g.height {
                let d = f.dbz(k);
                let c = if d.is_nan() && europe.is_some() { Color32::TRANSPARENT } else { radar_color(d, pal.dark) };
                rgba.extend_from_slice(&c.to_array());
            }
            let img = egui::ColorImage::from_rgba_premultiplied([g.width, g.height], &rgba);
            self.textures[i] = Some(ctx.load_texture(format!("radar{i}"), img, egui::TextureOptions::LINEAR));
        }

        // The Europe-wide frame for the same time (nearest within 5 minutes), without the
        // pixels the local radar covers.
        let (t_shown, _, _) = frames[i];
        let europe_frame = europe.and_then(|(e, mask)| {
            let ef = Self::map_frames(e);
            let key = (e.issued, pal.dark, Arc::as_ptr(mask) as usize);
            if self.europe_textures_for != Some(key) {
                self.europe_textures = vec![None; ef.len()];
                self.europe_textures_for = Some(key);
            }
            let j = (0..ef.len()).min_by_key(|&j| (ef[j].0 - t_shown).num_seconds().abs())?;
            if (ef[j].0 - t_shown).num_minutes().abs() > 5 {
                return None;
            }
            if self.europe_textures[j].is_none() {
                let (eg, f) = (e.grid, ef[j].1);
                let mut rgba = Vec::with_capacity(eg.width * eg.height * 4);
                for k in 0..eg.width * eg.height {
                    let c = if mask.get(k).copied().unwrap_or(false) { Color32::TRANSPARENT } else { radar_color(f.dbz(k), pal.dark) };
                    rgba.extend_from_slice(&c.to_array());
                }
                let img = egui::ColorImage::from_rgba_premultiplied([eg.width, eg.height], &rgba);
                self.europe_textures[j] = Some(ctx.load_texture(format!("europe{j}"), img, egui::TextureOptions::LINEAR));
            }
            Some((e.grid, self.europe_textures[j].clone().unwrap()))
        });

        // The view: one world pixel per point at the current zoom, centred on `map_centre`.
        let width = ui.available_width();
        let size = vec2(width, (width * 0.9).min(720.0));
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let centre = *self.map_centre.get_or_insert((self.settings.lat, self.settings.lon));
        let view = mercator::View::centred(centre.0, centre.1, self.map_zoom, rect.width() as f64, rect.height() as f64);
        let to_screen = |x: f64, y: f64| pos2(rect.left() + (x - view.x0) as f32, rect.top() + (y - view.y0) as f32);
        let geo = |lat: f64, lon: f64| {
            let (x, y) = mercator::to_world(lat, lon, view.zoom);
            to_screen(x, y)
        };
        let painter = ui.painter_at(rect);
        let (land_rgb, water_rgb) = if pal.dark { ([44, 46, 50], [24, 28, 35]) } else { ([246, 245, 240], [212, 225, 238]) };
        painter.rect_filled(rect, 0.0, Color32::from_rgb(water_rgb[0], water_rgb[1], water_rgb[2]));

        // Land and water: rendered for a canvas half a view larger on every side, so
        // panning doesn't need a new one; while zooming the old canvas is scaled until
        // the zoom has settled.
        let ppp = ctx.pixels_per_point().min(2.0) as f64;
        let stale = match &self.land {
            None => true,
            Some(l) => l.dark != pal.dark || {
                let k = (view.zoom - l.canvas.zoom).exp2();
                let inside = view.x0 >= l.canvas.x0 * k && view.y0 >= l.canvas.y0 * k && view.x1 <= l.canvas.x1 * k && view.y1 <= l.canvas.y1 * k;
                !inside || (view.zoom != l.canvas.zoom && self.last_zoom_input.elapsed() > Duration::from_millis(250))
            },
        };
        if stale {
            let (mw, mh) = (view.width() / 2.0, view.height() / 2.0);
            let canvas = mercator::View { zoom: view.zoom, x0: view.x0 - mw, y0: view.y0 - mh, x1: view.x1 + mw, y1: view.y1 + mh };
            let (w, h) = ((canvas.width() * ppp) as usize, (canvas.height() * ppp) as usize);
            let img = basemap::map().render(&canvas, w, h, land_rgb, water_rgb);
            let tex = ctx.load_texture("land", egui::ColorImage::from_rgba_unmultiplied([w, h], &img), egui::TextureOptions::LINEAR);
            self.land = Some(LandCanvas { dark: pal.dark, canvas, texture: tex });
        }
        if let Some(l) = &self.land {
            let k = (view.zoom - l.canvas.zoom).exp2();
            let r = Rect::from_min_max(to_screen(l.canvas.x0 * k, l.canvas.y0 * k), to_screen(l.canvas.x1 * k, l.canvas.y1 * k));
            painter.image(l.texture.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        if view.zoom != self.land.as_ref().map_or(view.zoom, |l| l.canvas.zoom) {
            ctx.request_repaint_after(Duration::from_millis(300));
        }

        // Rivers and borders, simplified for the zoom level.
        let map = basemap::map();
        let lod = map.lod(view.zoom.ceil() as u32);
        let k = (view.zoom - lod.zoom as f64).exp2();
        let (river, border) = if pal.dark { (Color32::from_rgb(40, 52, 68), Color32::from_gray(95)) } else { (Color32::from_rgb(180, 202, 226), Color32::from_gray(150)) };
        let visible = |s: &&basemap::Shape| s.overlaps(view.x0 / k, view.y0 / k, view.x1 / k, view.y1 / k);
        let pts = |s: &basemap::Shape| -> Vec<Pos2> { s.points.iter().map(|&(x, y)| to_screen(x * k, y * k)).collect() };
        if view.zoom >= 5.0 {
            for s in lod.rivers.iter().filter(visible) {
                painter.add(egui::Shape::line(pts(s), Stroke::new(1.0_f32, river)));
            }
        }
        for s in lod.borders.iter().filter(visible) {
            painter.add(egui::Shape::dashed_line(&pts(s), Stroke::new(1.0_f32, border), 4.0, 3.0));
        }

        // The rain: each radar grid's image on a mesh whose vertices are grid points
        // projected onto the map, so it lines up at any zoom whatever the radar's
        // projection. Europe underneath, the local radar on top.
        let grid_mesh = |g: &cleanweather_core::radar::Grid, tex: egui::TextureId, n: usize| {
            let mut mesh = egui::epaint::Mesh::with_texture(tex);
            for j in 0..=n {
                for ii in 0..=n {
                    let (u, v) = (ii as f32 / n as f32, j as f32 / n as f32);
                    let (lat, lon) = g.to_latlon(u as f64 * g.width as f64, v as f64 * g.height as f64);
                    mesh.vertices.push(egui::epaint::Vertex { pos: geo(lat, lon), uv: pos2(u, v), color: Color32::WHITE });
                }
            }
            for j in 0..n as u32 {
                for ii in 0..n as u32 {
                    let a = j * (n as u32 + 1) + ii;
                    let b = a + n as u32 + 1;
                    mesh.add_triangle(a, a + 1, b);
                    mesh.add_triangle(a + 1, b + 1, b);
                }
            }
            egui::Shape::mesh(mesh)
        };
        if let Some((eg, tex)) = &europe_frame {
            painter.add(grid_mesh(eg, tex.id(), 96));
        }
        if let Some(tex) = &self.textures[i] {
            painter.add(grid_mesh(&g, tex.id(), 48));
        }

        let me = geo(self.settings.lat, self.settings.lon);
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
        let radar = match source {
            Some("OPERA") => cleanweather_core::opera::ATTRIBUTION,
            Some(s) => s,
            None => "",
        };
        tag(rect.right_bottom() - vec2(6.0, 4.0), Align2::RIGHT_BOTTOM, format!("Map: Natural Earth · Radar: {radar}"), 9.5);

        // Zoom buttons and "back to my location".
        let mut zoom_to = None;
        // In a child Ui, so placing them doesn't move the parent's layout cursor.
        let button = |ui: &mut egui::Ui, at: Pos2, text: &str, tip: &str| {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(at, vec2(26.0, 26.0))));
            child.add_sized([26.0, 26.0], egui::Button::new(egui::RichText::new(text).size(15.0))).on_hover_text(tip).clicked()
        };
        let x = rect.right() - 34.0;
        if button(ui, pos2(x, rect.top() + 8.0), "+", "Zoom in") {
            zoom_to = Some((self.map_zoom + 1.0, rect.center()));
        }
        if button(ui, pos2(x, rect.top() + 38.0), "−", "Zoom out") {
            zoom_to = Some((self.map_zoom - 1.0, rect.center()));
        }
        if button(ui, pos2(x, rect.top() + 72.0), "⌖", "Back to my location") {
            self.map_centre = Some((self.settings.lat, self.settings.lon));
        }

        // Drag to pan; pinch or Ctrl+scroll (or double-click) to zoom around the pointer;
        // click to set the location.
        let to_latlon = |p: Pos2, v: &mercator::View| mercator::to_latlon(v.x0 + (p.x - rect.left()) as f64, v.y0 + (p.y - rect.top()) as f64, v.zoom);
        if resp.dragged() {
            let d = resp.drag_delta();
            let (cx, cy) = mercator::to_world(centre.0, centre.1, view.zoom);
            self.map_centre = Some(mercator::to_latlon(cx - d.x as f64, cy - d.y as f64, view.zoom));
        }
        if resp.hovered() {
            let factor = ui.input(|i| i.zoom_delta());
            if factor != 1.0 {
                zoom_to = Some((self.map_zoom + (factor as f64).log2(), ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center())));
            }
        }
        if resp.double_clicked() {
            zoom_to = Some((self.map_zoom + 1.0, resp.interact_pointer_pos().unwrap_or(rect.center())));
        } else if resp.clicked() && self.picking_on_map {
            // Only in "pick on the map" mode, so exploring the map never moves the location.
            if let Some(p) = resp.interact_pointer_pos() {
                let (lat, lon) = to_latlon(p, &view);
                self.set_location(lat, lon, None);
                self.picking_on_map = false;
            }
        }
        if self.picking_on_map {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.picking_on_map = false;
            }
            let banner = Rect::from_center_size(pos2(rect.center().x, rect.top() + 22.0), vec2(270.0, 26.0));
            painter.rect_filled(banner, 6.0, pal.pin);
            painter.text(banner.center(), Align2::CENTER_CENTER, "Click the map to set your location (Esc cancels)", FontId::proportional(12.0), Color32::WHITE);
        }
        if let Some((z, anchor)) = zoom_to {
            // Keep the point under the anchor where it is.
            let z = z.clamp(MAP_ZOOM_RANGE.0, MAP_ZOOM_RANGE.1);
            let (alat, alon) = to_latlon(anchor, &view);
            let (ax, ay) = mercator::to_world(alat, alon, z);
            let (dx, dy) = ((anchor.x - rect.center().x) as f64, (anchor.y - rect.center().y) as f64);
            self.map_centre = Some(mercator::to_latlon(ax - dx, ay - dy, z));
            self.map_zoom = z;
            self.last_zoom_input = Instant::now();
        }
        resp.on_hover_text(if self.picking_on_map { "Click to set your location" } else { "Drag to move · Ctrl+scroll, pinch or double-click to zoom" });

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

    /// Move the location (named, or named after the nearest town) and save it, so the
    /// app opens there next time.
    fn set_location(&mut self, lat: f64, lon: f64, name: Option<String>) {
        self.settings.place = name.unwrap_or_else(|| match basemap::map().nearest_place(lat, lon) {
            Some((p, d)) if d < 4.0 => p.name.clone(),
            Some((p, d)) if d < 25.0 => format!("Near {}", p.name),
            _ => "Selected location".into(),
        });
        self.settings.lat = lat;
        self.settings.lon = lon;
        let _ = self.commands.send(Command::SetLocation(lat, lon));
        if let Err(e) = self.settings.save() {
            self.shared.lock().unwrap().error = Some(format!("couldn't save the location: {e:#}"));
        }
    }

    /// The location picker: search the towns on the map, or pick a spot on the map.
    fn location_picker(&mut self, ctx: &egui::Context) {
        let mut open = self.picker_open;
        let mut chosen = None;
        egui::Window::new("Choose location").open(&mut open).collapsible(false).resizable(false).default_width(300.0).anchor(Align2::LEFT_TOP, vec2(12.0, 70.0)).show(ctx, |ui| {
            let field = ui.add(egui::TextEdit::singleline(&mut self.picker_query).hint_text("Search for a town or city").desired_width(f32::INFINITY));
            if !field.has_focus() && self.picker_query.is_empty() {
                field.request_focus();
            }
            let q = fold(self.picker_query.trim());
            if !q.is_empty() {
                // Names starting with the text first, then containing it; bigger towns first.
                let mut hits: Vec<&basemap::Place> = basemap::map().places.iter().filter(|p| fold(&p.name).contains(&q)).collect();
                hits.sort_by_key(|p| (!fold(&p.name).starts_with(&q), std::cmp::Reverse(p.population)));
                ui.add_space(4.0);
                if hits.is_empty() {
                    ui.label(egui::RichText::new("No town by that name on the map. Try “Pick on the map”.").weak());
                }
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                for (k, p) in hits.iter().take(10).enumerate() {
                    let label = format!("{}   {:.2}° N, {:.2}° E", p.name, p.lat, p.lon);
                    if ui.selectable_label(false, label).clicked() || (k == 0 && enter) {
                        chosen = Some((p.lat, p.lon, p.name.clone()));
                    }
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("📍 Pick on the map").clicked() {
                    self.picking_on_map = true;
                    self.picker_open = false;
                }
                ui.label(egui::RichText::new("then click a spot").small().weak());
            });
        });
        if let Some((lat, lon, name)) = chosen {
            self.set_location(lat, lon, Some(name));
            self.map_centre = Some((lat, lon));
            open = false;
        }
        if self.picker_open {
            self.picker_open = open;
        }
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
                ui.add(egui::DragValue::new(&mut self.settings.lat).speed(0.01).range(30.0..=72.0).max_decimals(4));
                ui.end_row();
                ui.label("Longitude");
                ui.add(egui::DragValue::new(&mut self.settings.lon).speed(0.01).range(-30.0..=45.0).max_decimals(4));
                ui.end_row();
                ui.label("KNMI API key");
                ui.horizontal(|ui| {
                    let hint = if config::api_key().is_some() { "replace key" } else { "for the Dutch radar" };
                    ui.add(egui::TextEdit::singleline(&mut self.key_input).password(true).hint_text(hint).desired_width(180.0));
                    if ui.button("Save").clicked() && !self.key_input.trim().is_empty() {
                        self.save_key(false);
                    }
                });
                ui.end_row();
                ui.label("MeteoGate API key");
                ui.horizontal(|ui| {
                    let hint = if config::meteogate_key().is_some() { "replace key" } else { "optional, for OPERA" };
                    ui.add(egui::TextEdit::singleline(&mut self.meteogate_input).password(true).hint_text(hint).desired_width(180.0));
                    if ui.button("Save").clicked() && !self.meteogate_input.trim().is_empty() {
                        self.save_key(true);
                    }
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            if config::EMBEDDED_KEY.is_some() || config::EMBEDDED_METEOGATE_KEY.is_some() {
                ui.label(egui::RichText::new("This build has API keys built in; keys saved here take precedence.").small().weak());
            }
            ui.label(egui::RichText::new("Tip: click the place name at the top to search for a town, or pick a spot on the map.").small().weak());
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
    fn place_search_ignores_case_and_accents() {
        assert_eq!(fold("Düsseldorf"), "dusseldorf");
        assert_eq!(fold("ZÜRICH"), "zurich");
        assert_eq!(fold("'s-Hertogenbosch"), "s-hertogenbosch");
        assert_eq!(fold("Kraków"), "krakow");
    }

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

    fn nowcast_at(lat: f64, lon: f64, scans: usize) -> Nowcast {
        use cleanweather_core::opera::{Product, crop_for};
        use cleanweather_core::radar::Frame;
        let grid = crop_for(lat, lon, Product::MaxReflectivity).unwrap();
        let issued = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let frames: Vec<Frame> = (0..scans)
            .map(|k| Frame { time: issued - chrono::Duration::minutes(5 * k as i64), grid, dbz: vec![0.0; grid.width * grid.height], radars_up: vec![], radars_down: vec![], source: "OPERA" })
            .collect();
        Nowcast::compute(&frames, 1).unwrap()
    }

    #[test]
    fn map_textures_follow_a_move_within_one_scan() {
        // Moving from Paris to Berlin (or Amsterdam to Brussels, KNMI to OPERA) usually
        // gives a nowcast for the same scan time on another grid, sometimes with another
        // number of scans. The cached textures must not be reused for it: they'd show
        // Paris's rain over Berlin, or index past the end of the texture list.
        let paris = nowcast_at(48.86, 2.35, 7);
        let berlin = nowcast_at(52.52, 13.40, 7);
        let fewer = nowcast_at(48.86, 2.35, 6);
        assert_eq!(paris.issued, berlin.issued);
        assert_ne!(texture_key(&paris, false), texture_key(&berlin, false));
        assert_ne!(texture_key(&paris, false), texture_key(&fewer, false));
        assert_eq!(texture_key(&paris, true), texture_key(&nowcast_at(48.86, 2.35, 7), true));
    }
}
