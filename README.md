# CleanWeather

A rain nowcast for the Netherlands in Rust, in the spirit of Buienalarm and Buienradar: when will it rain here in the next two hours?
It reads KNMI's rain radar directly and makes its own forecast. There's no server in between.
The desktop app is built on [egui/eframe](https://github.com/emilk/egui), like [CleanNotes](../CleanNotes). The forecasting lives in `core/` so that an iPhone app can share it later.

## How it works

1. **Radar**: every 5 minutes KNMI publishes a reflectivity composite of its two radars (Den Helder and Herwijnen). It's a 700 × 765 grid of 1 km pixels on a polar stereographic projection (dataset `radar_reflectivity_composites` 2.0, HDF5). CleanWeather keeps the last 30 minutes of scans (7 files, ~100 KB each) and caches them on disk. After the first start it downloads one file per new scan.
2. **Motion**: the latest scan is compared with the one from 15 minutes earlier in 32 km blocks. A coarse search runs on a 4× downsampled grid, then a fine search with sub-pixel refinement. The block vectors are smoothed into one motion field, and areas without rain take the overall motion.
3. **Extrapolation**: for every pixel and every 5 minutes up to 2 hours ahead, the forecast traces back along the motion field and takes the latest scan's value from there (semi-Lagrangian advection).
4. **Your location**: reflectivity is converted to rain rate with Marshall–Palmer (Z = 200 R^1.6) and averaged around your location. The averaging area grows from 1.5 km now to ~6.5 km at 2 hours, because the position of rain gets less certain over time. The share of that area with rain is shown as a rough chance.

Growth and decay of showers isn't modelled, so the forecast is best for the first hour, as with every radar-extrapolation app.

### 7-day forecast

Current conditions and the week ahead come from [Open-Meteo](https://open-meteo.com). It's free and needs no key. For the Netherlands it uses KNMI's HARMONIE model for the first ~2.5 days and ECMWF and other global models after that. KNMI's own open data has no 7-day forecast for a single point. Each day shows the weather type, the night-time low and daytime high, rain amount and chance, and wind as direction and Beaufort force. The forecast refreshes every 30 minutes and whenever you move your location.

## Running

```sh
cargo run --release              # the desktop app
cargo run --release --bin rain   # the forecast in the terminal
cargo run --release --bin rain -- --lat 52.37 --lon 4.89
cargo run --release --bin rain -- verify [DIR]   # score the nowcast against what happened
cargo test --workspace
```

### KNMI API key

You need a free key for KNMI's **Open Data API**. Register at [developer.dataplatform.knmi.nl](https://developer.dataplatform.knmi.nl/) and click "Request an API key" under Open Data API. Then paste it into the app, or save it yourself:

- Windows: `%APPDATA%\CleanWeather\api_key.txt`
- macOS: `~/Library/Application Support/CleanWeather/api_key.txt`
- or set `KNMI_API_KEY`.

**Local builds embed the key.** When you build on your own machine, `build.rs` compiles the key from `KNMI_API_KEY` or the key file above into the binaries, so they work without setup. A key saved in the app still takes precedence. Cargo prints a warning when this happens: don't share those binaries. Set `CLEANWEATHER_NO_EMBED=1` to build without the key. CI builds (where `CI` is set, as on GitHub Actions) never embed it, so the downloads from GitHub contain no key.

The key is never stored in this repository. A registered key allows 1000 requests per hour. CleanWeather uses about 80: one listing every 90 seconds, plus one download URL per new scan.

### Network use

CleanWeather only makes requests for weather data: to KNMI for radar scans, and to Open-Meteo for the 7-day forecast. Open-Meteo receives your location rounded to 0.01° (~1 km). The map is baked into the app (`core/assets/map.json`, ~40 KB). It holds land, lakes, borders, rivers and towns from [Natural Earth](https://www.naturalearthdata.com) (public domain), clipped and simplified by `core/examples/make_map.rs`. To rebuild it, download the five `ne_10m_*` GeoJSON files listed at the top of that file and run `cargo run --release -p cleanweather-core --example make_map -- DIR`.

### Your location

Click the radar map to set your location, or enter it in Settings (⚙). It's saved in `settings.json` next to the API key.

## Verifying the forecast

`rain verify DIR` runs a hindcast: it starts a forecast from every scan in DIR that has 15 minutes of history, then compares it with what the radar actually saw 15–60 minutes later. Scores are given for both the nowcast and *persistence* (assuming the rain stays where it is).
On 3 hours of light, showery rain on 19 November 2026 (34 forecasts):

| Lead | CSI ≥ 1 mm/h (nowcast / persistence) | Mean abs. error mm/h |
|---|---|---|
| 15 min | **0.36** / 0.20 | **0.22** / 0.30 |
| 30 min | **0.22** / 0.10 | **0.28** / 0.34 |
| 60 min | **0.11** / 0.06 | **0.33** / 0.36 |

CSI is the critical success index: hits / (hits + misses + false alarms), so higher is better.
Scattered showers like these are the hardest case for any nowcast.

## Layout

- `core/` (`cleanweather-core`): `knmi.rs` (API client and file cache), `radar.rs` (HDF5 decoding and projection), `mercator.rs` and `basemap.rs` (the built-in map and the radar drawn onto it), `nowcast.rs` (motion, extrapolation, point forecast and skill scores), `forecast.rs` (7-day forecast), `service.rs` (keeps a nowcast up to date). It has no UI and makes no assumptions about where files live, so it's ready for an `ffi/` crate and a SwiftUI app, like CleanNotes.
- `src/`: the desktop app (`app.rs`, and `week.rs` for the 7-day list and weather icons), the `rain` tool (`bin/rain.rs`) and where settings are stored (`config.rs`).
- `core/examples/`: `make_map` builds the map data; `inspect` dumps an HDF5 file's structure; `histogram` shows a radar image's value distribution.
