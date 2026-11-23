//! CleanWeather's core for Swift: the iPhone app runs the same radar download,
//! fallback between sources, nowcast and 7-day forecast as the desktop app,
//! through these bindings (made with UniFFI; `ios/build-rust.sh` builds the
//! library and generates the Swift).
//!
//! - [`WeatherCore`]: keeps the nowcast up to date from the best working radar
//!   ([`cleanweather_core::service::Service`]) and the Europe-wide rain for the map,
//!   and answers "when will it rain here" ([`WeatherCore::rain`]).
//! - [`MapCanvas`]: the radar map for an area, as images (land and water, the rain
//!   of each frame) and lines and labels for Swift to draw.
//! - [`fetch_forecast`], [`search_places`] and small helpers shared with the
//!   desktop app (radar colours, the rain graph's axis, intensity words).
//!
//! The network calls block: call them off the main thread. Times are Unix
//! seconds (UTC); Swift formats them in the phone's own time zone and locale.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Datelike, Utc};
use cleanweather_core::nowcast::{self, Nowcast, Outlook};
use cleanweather_core::service::{Europe, Service};
use cleanweather_core::{basemap, forecast, opera};

mod map;
pub use map::{LatLon, LineKind, MapCanvas, MapFrame, MapImage, MapLabel, MapLine, MapPoint};

uniffi::setup_scaffolding!();

// ---- Errors -------------------------------------------------------------------

/// Something that couldn't be done, with a message to show.
#[derive(Debug, uniffi::Error)]
pub enum WeatherError {
    Failed { message: String },
}

impl std::fmt::Display for WeatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WeatherError::Failed { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for WeatherError {}

fn failed(e: anyhow::Error) -> WeatherError {
    WeatherError::Failed { message: format!("{e:#}") }
}

/// A poisoned lock only means a panic elsewhere: the data is still usable.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn secs(t: DateTime<Utc>) -> i64 {
    t.timestamp()
}

/// An empty or blank key is no key.
fn key(k: Option<String>) -> Option<String> {
    k.map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

// ---- Rain at a location -------------------------------------------------------

/// Rain at the location at one time: a past scan or a forecast step.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct RainSample {
    pub time: i64,
    /// Mean rain rate (mm/h) around the location; none where no radar covers it.
    pub mm_per_hour: Option<f32>,
    /// Share of the area around the location with rain: a rough chance of rain.
    pub chance: f32,
    /// A radar scan (the past), not a forecast.
    pub observed: bool,
}

/// What the next two hours look like, for the headline.
#[derive(Clone, Copy, Debug, PartialEq, uniffi::Enum)]
pub enum RainOutlook {
    /// No rain within the forecast, which runs until `until`.
    Dry { until: i64 },
    /// Dry now, rain from `at`; `peak` is the heaviest rate (mm/h).
    RainFrom { at: i64, peak: f32 },
    /// Raining now; stops at `until` if that's within the forecast.
    Raining { until: Option<i64>, peak: f32 },
    /// No radar covers the location.
    NoData,
}

impl From<Outlook> for RainOutlook {
    fn from(o: Outlook) -> RainOutlook {
        match o {
            Outlook::Dry { until } => RainOutlook::Dry { until: secs(until) },
            Outlook::RainFrom { at, peak } => RainOutlook::RainFrom { at: secs(at), peak },
            Outlook::Raining { until, peak } => RainOutlook::Raining { until: until.map(secs), peak },
            Outlook::NoData => RainOutlook::NoData,
        }
    }
}

/// How the rain is moving, on average.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct RainMotion {
    pub kmh: f32,
    /// Compass point it's moving towards ("NE").
    pub towards: String,
}

/// The nowcast at a location.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct RainReport {
    /// Time of the newest radar scan.
    pub issued: i64,
    /// The last 30 minutes of scans, then the forecast every 5 minutes for 2 hours.
    pub samples: Vec<RainSample>,
    pub outlook: RainOutlook,
    pub motion: Option<RainMotion>,
    /// Which radar it's from ("KNMI", "OPERA"), and its credit line.
    pub source: String,
    pub attribution: String,
    /// Why a preferred radar wasn't used, if it wasn't.
    pub fallback_reason: Option<String>,
    /// Radars that were down for the newest scan.
    pub radars_down: Vec<String>,
}

#[derive(Default)]
struct Latest {
    nowcast: Option<Arc<Nowcast>>,
    source: Option<&'static str>,
    fallback: Option<String>,
    europe: Option<Arc<Nowcast>>,
}

fn attribution(source: &str) -> String {
    match source {
        "OPERA" => opera::ATTRIBUTION.to_string(),
        "KNMI" => "KNMI (CC BY 4.0)".to_string(),
        s => s.to_string(),
    }
}

#[derive(uniffi::Object)]
pub struct WeatherCore {
    cache_dir: PathBuf,
    service: Mutex<Service>,
    europe: Mutex<Europe>,
    latest: Mutex<Latest>,
}

#[uniffi::export]
impl WeatherCore {
    /// Radar files are cached in `cache_dir` (the app's Caches folder). A KNMI key
    /// adds the Dutch radar; a MeteoGate key is optional for OPERA's (Europe's).
    #[uniffi::constructor]
    pub fn new(cache_dir: String, knmi_key: Option<String>, meteogate_key: Option<String>) -> Arc<WeatherCore> {
        let cache_dir = PathBuf::from(cache_dir);
        let (knmi, mg) = (key(knmi_key), key(meteogate_key));
        Arc::new(WeatherCore {
            service: Mutex::new(Service::new(knmi, mg.clone(), cache_dir.clone())),
            europe: Mutex::new(Europe::new(mg, cache_dir.clone())),
            cache_dir,
            latest: Mutex::new(Latest::default()),
        })
    }

    /// Use new API keys (from the next refresh on).
    pub fn set_keys(&self, knmi_key: Option<String>, meteogate_key: Option<String>) {
        let (knmi, mg) = (key(knmi_key), key(meteogate_key));
        *lock(&self.service) = Service::new(knmi, mg.clone(), self.cache_dir.clone());
        *lock(&self.europe) = Europe::new(mg, self.cache_dir.clone());
    }

    /// Check the radars for new scans at a location, and Europe's for the map
    /// (blocking, ~1–3 s). True if there's something new to show. Fails only when
    /// no radar works for the location; the map's Europe layer failing is ignored.
    pub fn refresh_radar(&self, lat: f64, lon: f64) -> Result<bool, WeatherError> {
        let local = lock(&self.service).update(lat, lon);
        let europe = lock(&self.europe).update();
        let mut latest = lock(&self.latest);
        let mut changed = false;
        if let Ok(Some(nc)) = europe {
            latest.europe = Some(Arc::new(nc));
            changed = true;
        }
        match local {
            Ok(Some(u)) => {
                latest.nowcast = Some(Arc::new(u.nowcast));
                latest.source = Some(u.source);
                latest.fallback = u.fallback_reason;
                Ok(true)
            }
            Ok(None) => Ok(changed),
            Err(e) => Err(failed(e)),
        }
    }

    /// The nowcast at a location, once a refresh has made one.
    pub fn rain(&self, lat: f64, lon: f64) -> Option<RainReport> {
        let latest = lock(&self.latest);
        let nc = latest.nowcast.as_ref()?;
        let samples = nc.point(lat, lon);
        let source = latest.source.unwrap_or("radar");
        Some(RainReport {
            issued: secs(nc.issued),
            outlook: nowcast::outlook(&samples).into(),
            samples: samples
                .iter()
                .map(|s| RainSample { time: secs(s.time), mm_per_hour: s.mm_per_hour, chance: s.chance, observed: s.observed })
                .collect(),
            motion: nc.motion.mean_motion().map(|(kmh, deg)| RainMotion { kmh, towards: forecast::compass(deg).to_string() }),
            source: source.to_string(),
            attribution: attribution(source),
            fallback_reason: latest.fallback.clone(),
            // De Bilt's old radar is listed but no longer used; only real gaps.
            radars_down: nc.radars_down.iter().filter(|r| *r != "DeBilt").cloned().collect(),
        })
    }

    /// The radar map for a canvas of `width × height` points centred on a location
    /// at a zoom level (Web Mercator, 1 world pixel per point), drawn at `scale`
    /// pixels per point, with the rain the last refresh brought.
    pub fn map_canvas(&self, centre_lat: f64, centre_lon: f64, zoom: f64, width: u32, height: u32, scale: f64) -> Arc<MapCanvas> {
        let latest = lock(&self.latest);
        Arc::new(MapCanvas::new(
            centre_lat,
            centre_lon,
            zoom,
            width,
            height,
            scale,
            latest.nowcast.clone(),
            latest.europe.clone(),
            latest.source.map(attribution),
        ))
    }
}

// ---- The 7-day forecast -------------------------------------------------------

/// Weather grouped into what an icon can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum WeatherKind {
    Clear,
    PartlyCloudy,
    Cloudy,
    Fog,
    Drizzle,
    Rain,
    HeavyRain,
    Snow,
    Thunder,
}

impl From<forecast::Condition> for WeatherKind {
    fn from(c: forecast::Condition) -> WeatherKind {
        use forecast::Condition as C;
        match c {
            C::Clear => WeatherKind::Clear,
            C::PartlyCloudy => WeatherKind::PartlyCloudy,
            C::Cloudy => WeatherKind::Cloudy,
            C::Fog => WeatherKind::Fog,
            C::Drizzle => WeatherKind::Drizzle,
            C::Rain => WeatherKind::Rain,
            C::HeavyRain => WeatherKind::HeavyRain,
            C::Snow => WeatherKind::Snow,
            C::Thunder => WeatherKind::Thunder,
        }
    }
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct WeatherNow {
    pub temperature: f32,
    pub kind: WeatherKind,
    /// "Partly cloudy".
    pub label: String,
    pub wind_kmh: f32,
    pub is_day: bool,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct DayForecast {
    /// The local date at the location.
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub kind: WeatherKind,
    pub label: String,
    /// Daytime high and night-time low, °C.
    pub max: f32,
    pub min: f32,
    pub rain_mm: f32,
    /// Chance of rain, %, where the models give one.
    pub rain_chance: Option<u8>,
    pub wind_kmh: f32,
    pub beaufort: u8,
    /// Compass point the wind comes from ("SW").
    pub wind_from: String,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct WeekForecast {
    pub current: WeatherNow,
    pub days: Vec<DayForecast>,
}

impl From<forecast::Forecast> for WeekForecast {
    fn from(f: forecast::Forecast) -> WeekForecast {
        let c = f.current;
        WeekForecast {
            current: WeatherNow { temperature: c.temperature, kind: c.condition.into(), label: c.condition.label().into(), wind_kmh: c.wind_kmh, is_day: c.is_day },
            days: f
                .days
                .into_iter()
                .map(|d| DayForecast {
                    year: d.date.year(),
                    month: d.date.month(),
                    day: d.date.day(),
                    kind: d.condition.into(),
                    label: d.condition.label().into(),
                    max: d.max,
                    min: d.min,
                    rain_mm: d.rain_mm,
                    rain_chance: d.rain_chance,
                    wind_kmh: d.wind_kmh,
                    beaufort: forecast::beaufort(d.wind_kmh),
                    wind_from: forecast::compass(d.wind_from).into(),
                })
                .collect(),
        }
    }
}

/// Current conditions and the next 7 days from Open-Meteo (blocking, ~0.2 s). Only
/// the location rounded to ~1 km is sent.
#[uniffi::export]
pub fn fetch_forecast(lat: f64, lon: f64) -> Result<WeekForecast, WeatherError> {
    forecast::fetch(lat, lon).map(Into::into).map_err(failed)
}

// ---- Places -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// Towns on the map whose name contains `query`, ignoring case and accents; names
/// starting with it first, then the biggest.
#[uniffi::export]
pub fn search_places(query: String, limit: u32) -> Vec<Place> {
    basemap::map().search(&query, limit as usize).into_iter().map(|p| Place { name: p.name.clone(), lat: p.lat, lon: p.lon }).collect()
}

/// A name for a location: the town there, or "Near" the nearest one.
#[uniffi::export]
pub fn place_name(lat: f64, lon: f64) -> String {
    basemap::map().name_for(lat, lon)
}

// ---- Shared with the desktop app's drawing ------------------------------------

/// Words for a rain rate, as Dutch rain apps use them ("light rain").
#[uniffi::export]
pub fn intensity_label(mm_per_hour: f32) -> String {
    nowcast::intensity_label(mm_per_hour).to_string()
}

/// The rain graph's height for a rate (0–1): logarithmic, so 0.1 mm/h drizzle shows
/// (~12%) while 1, 5 and 20 mm/h sit at ~40%, ~70% and ~95%.
#[uniffi::export]
pub fn rain_axis(mm_per_hour: f32) -> f32 {
    ((1.0 + mm_per_hour.max(0.0) * 10.0).ln() / (1.0 + 30.0 * 10.0f32).ln()).min(1.0)
}

/// The radar colour for a reflectivity (dBZ), as 0xRRGGBBAA (not premultiplied),
/// for the map's legend.
#[uniffi::export]
pub fn radar_color(dbz: f32, dark: bool) -> u32 {
    u32::from_be_bytes(map::radar_rgba(dbz, dark))
}

/// The reflectivities the legend shows, with a word under some.
#[uniffi::export]
pub fn legend_steps() -> Vec<LegendStep> {
    [(14.0, "light"), (26.0, ""), (33.0, "moderate"), (40.0, ""), (45.0, "heavy"), (52.0, "")]
        .into_iter()
        .map(|(dbz, label)| LegendStep { dbz, label: label.into() })
        .collect()
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct LegendStep {
    pub dbz: f32,
    pub label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlook_converts_times() {
        let t = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        assert_eq!(RainOutlook::from(Outlook::RainFrom { at: t, peak: 2.0 }), RainOutlook::RainFrom { at: 1_800_000_000, peak: 2.0 });
        assert_eq!(RainOutlook::from(Outlook::Raining { until: None, peak: 1.0 }), RainOutlook::Raining { until: None, peak: 1.0 });
    }

    #[test]
    fn blank_keys_are_no_keys() {
        assert_eq!(key(Some("  ".into())), None);
        assert_eq!(key(Some(" abc \n".into())), Some("abc".into()));
        assert_eq!(key(None), None);
    }

    #[test]
    fn axis_and_colours_match_the_desktop() {
        assert!(rain_axis(0.0) == 0.0 && rain_axis(1000.0) == 1.0);
        assert!(rain_axis(0.1) < rain_axis(1.0) && rain_axis(1.0) < rain_axis(5.0));
        assert_eq!(radar_color(0.0, false) & 0xff, 0, "dry is transparent");
        assert_eq!(radar_color(30.0, false) & 0xff, 230);
        assert_eq!(legend_steps().len(), 6);
    }

    #[test]
    fn places() {
        assert_eq!(search_places("utrecht".into(), 3)[0].name, "Utrecht");
        assert_eq!(place_name(52.09, 5.12), "Utrecht");
    }

    #[test]
    fn no_rain_before_the_first_refresh() {
        let dir = std::env::temp_dir().join(format!("cleanweather-ffi-{}", std::process::id()));
        let core = WeatherCore::new(dir.to_string_lossy().into(), None, Some(" ".into()));
        assert!(core.rain(52.1, 5.1).is_none());
        let canvas = core.map_canvas(52.1, 5.1, 7.0, 200, 100, 2.0);
        assert_eq!(canvas.frames().len(), 0);
        let base = canvas.base_image(false);
        assert_eq!((base.width, base.height, base.rgba.len()), (400, 200, 400 * 200 * 4));
    }
}
