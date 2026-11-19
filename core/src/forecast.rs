//! The 7-day forecast: current conditions and day/night temperatures, rain and wind
//! per day, from Open-Meteo (free, no key). For the Netherlands its default blends
//! KNMI's HARMONIE model for the first ~2.5 days with ECMWF and others after that.
//!
//! Only the location is sent, rounded to 0.01° (~1 km).

use anyhow::{Context, Result, anyhow};
use chrono::NaiveDate;
use serde::Deserialize;
use std::time::Duration;

/// Weather, grouped from WMO weather codes into what an icon can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
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

impl Condition {
    /// From a WMO weather interpretation code (as used by Open-Meteo).
    pub fn from_wmo(code: u8) -> Condition {
        match code {
            0 => Condition::Clear,
            1 | 2 => Condition::PartlyCloudy,
            3 => Condition::Cloudy,
            45 | 48 => Condition::Fog,
            51..=57 => Condition::Drizzle,
            61 | 63 | 66 | 80 | 81 => Condition::Rain,
            65 | 67 | 82 => Condition::HeavyRain,
            71..=77 | 85 | 86 => Condition::Snow,
            95..=99 => Condition::Thunder,
            _ => Condition::Cloudy,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Condition::Clear => "Clear",
            Condition::PartlyCloudy => "Partly cloudy",
            Condition::Cloudy => "Cloudy",
            Condition::Fog => "Fog",
            Condition::Drizzle => "Drizzle",
            Condition::Rain => "Rain",
            Condition::HeavyRain => "Heavy rain",
            Condition::Snow => "Snow",
            Condition::Thunder => "Thunderstorms",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Current {
    pub temperature: f32,
    pub condition: Condition,
    pub wind_kmh: f32,
    pub is_day: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    pub date: NaiveDate,
    pub condition: Condition,
    /// Daytime high and night-time low, °C.
    pub max: f32,
    pub min: f32,
    pub rain_mm: f32,
    /// Chance of rain, % (may be missing beyond the models that provide it).
    pub rain_chance: Option<u8>,
    pub wind_kmh: f32,
    /// Direction the wind comes from, degrees (0 = north).
    pub wind_from: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Forecast {
    pub lat: f64,
    pub lon: f64,
    pub current: Current,
    pub days: Vec<Day>,
}

#[derive(Deserialize)]
struct Response {
    current: RawCurrent,
    daily: RawDaily,
}

#[derive(Deserialize)]
struct RawCurrent {
    temperature_2m: f32,
    weather_code: u8,
    wind_speed_10m: f32,
    is_day: u8,
}

#[derive(Deserialize)]
struct RawDaily {
    time: Vec<String>,
    weather_code: Vec<Option<u8>>,
    temperature_2m_max: Vec<Option<f32>>,
    temperature_2m_min: Vec<Option<f32>>,
    precipitation_sum: Vec<Option<f32>>,
    precipitation_probability_max: Vec<Option<u8>>,
    wind_speed_10m_max: Vec<Option<f32>>,
    wind_direction_10m_dominant: Vec<Option<f32>>,
}

/// Round to 0.01° so the request doesn't carry a precise location.
fn coarse(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Fetch the forecast for a location (blocking; ~0.2 s).
pub fn fetch(lat: f64, lon: f64) -> Result<Forecast> {
    let (lat, lon) = (coarse(lat), coarse(lon));
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
         &current=temperature_2m,weather_code,wind_speed_10m,is_day\
         &daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,\
         precipitation_probability_max,wind_speed_10m_max,wind_direction_10m_dominant\
         &timezone=Europe%2FAmsterdam&forecast_days=7"
    );
    let resp = ureq::get(&url)
        .timeout(Duration::from_secs(20))
        .call()
        .map_err(|e| anyhow!("cannot get the 7-day forecast: {e}"))?;
    let body = resp.into_string()?;
    parse(&body, lat, lon)
}

pub fn parse(json: &str, lat: f64, lon: f64) -> Result<Forecast> {
    let r: Response = serde_json::from_str(json).context("parsing forecast")?;
    let c = r.current;
    let d = r.daily;
    let mut days = Vec::new();
    for i in 0..d.time.len() {
        // Skip days the models don't cover completely.
        let (Some(max), Some(min), Some(code)) = (
            d.temperature_2m_max.get(i).copied().flatten(),
            d.temperature_2m_min.get(i).copied().flatten(),
            d.weather_code.get(i).copied().flatten(),
        ) else {
            continue;
        };
        days.push(Day {
            date: NaiveDate::parse_from_str(&d.time[i], "%Y-%m-%d")?,
            condition: Condition::from_wmo(code),
            max,
            min,
            rain_mm: d.precipitation_sum.get(i).copied().flatten().unwrap_or(0.0),
            rain_chance: d.precipitation_probability_max.get(i).copied().flatten(),
            wind_kmh: d.wind_speed_10m_max.get(i).copied().flatten().unwrap_or(0.0),
            wind_from: d.wind_direction_10m_dominant.get(i).copied().flatten().unwrap_or(0.0),
        });
    }
    Ok(Forecast {
        lat,
        lon,
        current: Current { temperature: c.temperature_2m, condition: Condition::from_wmo(c.weather_code), wind_kmh: c.wind_speed_10m, is_day: c.is_day != 0 },
        days,
    })
}

/// Beaufort force for a wind speed in km/h, as Dutch forecasts give wind.
pub fn beaufort(kmh: f32) -> u8 {
    const LIMITS: [f32; 12] = [1.0, 6.0, 12.0, 20.0, 29.0, 39.0, 50.0, 62.0, 75.0, 89.0, 103.0, 118.0];
    LIMITS.iter().position(|&l| kmh < l).unwrap_or(12) as u8
}

/// Compass point for a direction in degrees.
pub fn compass(deg: f32) -> &'static str {
    ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][((deg.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_meteo() {
        let json = r#"{"current":{"time":"2026-11-19T19:45","interval":900,"temperature_2m":6.3,"weather_code":3,"wind_speed_10m":4.7,"is_day":0},
            "daily":{"time":["2026-11-19","2026-11-20"],"weather_code":[53,null],"temperature_2m_max":[10.8,6.9],"temperature_2m_min":[6.1,1.8],
            "precipitation_sum":[2.8,1.5],"precipitation_probability_max":[100,null],"wind_speed_10m_max":[32.0,14.4],"wind_direction_10m_dominant":[212,46]}}"#;
        let f = parse(json, 52.1, 5.18).unwrap();
        assert_eq!(f.current.condition, Condition::Cloudy);
        assert!(!f.current.is_day);
        assert_eq!(f.days.len(), 1, "a day without a weather code is skipped");
        assert_eq!(f.days[0].condition, Condition::Drizzle);
        assert_eq!(f.days[0].rain_chance, Some(100));
    }

    #[test]
    fn wind() {
        assert_eq!(beaufort(0.5), 0);
        assert_eq!(beaufort(32.0), 5);
        assert_eq!(compass(212.0), "SW");
        assert_eq!(compass(359.0), "N");
    }

    #[test]
    fn location_is_rounded() {
        assert_eq!(coarse(52.10168), 52.1);
        assert_eq!(coarse(5.17834), 5.18);
    }
}
