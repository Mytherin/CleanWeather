# CleanWeather

A rain nowcast for the Netherlands and most of Europe in Rust, in the spirit of Buienalarm and Buienradar: when will it rain here in the next two hours?
It reads rain radar directly, KNMI's for the Netherlands and EUMETNET OPERA's for Europe, and makes its own forecast. There's no server in between.
The desktop app is built on [egui/eframe](https://github.com/emilk/egui), like [CleanNotes](../CleanNotes). The [iPhone app](#iphone-app) is SwiftUI on the same Rust core (`core/`), through UniFFI bindings (`ffi/`).

## How it works

1. **Radar**: every 5 minutes KNMI publishes a reflectivity composite of its two radars (Den Helder and Herwijnen). It's a 700 × 765 grid of 1 km pixels on a polar stereographic projection (dataset `radar_reflectivity_composites` 2.0, HDF5). CleanWeather keeps the last 30 minutes of scans (7 files, ~100 KB each) and caches them on disk. After the first start it downloads one file per new scan.
2. **Motion**: the latest scan is compared with the one from 15 minutes earlier in 32 km blocks. A coarse search runs on a 4× downsampled grid, then a fine search with sub-pixel refinement. The block vectors are smoothed into one motion field, and areas without rain take the overall motion.
3. **Extrapolation**: for every pixel and every 5 minutes up to 2 hours ahead, the forecast traces back along the motion field and takes the latest scan's value from there (semi-Lagrangian advection).
4. **Your location**: reflectivity is converted to rain rate with Marshall–Palmer (Z = 200 R^1.6) and averaged around your location. The averaging area grows from 1.5 km now to ~6.5 km at 2 hours, because the position of rain gets less certain over time. The share of that area with rain is shown as a rough chance.

Growth and decay of showers isn't modelled, so the forecast is best for the first hour, as with every radar-extrapolation app.

### Radar sources and fallback

| Source | Covers | Grid | Access |
|---|---|---|---|
| **KNMI** | The Netherlands and borders | 1 km, every 5 min, ~4 min late | Free key |
| **EUMETNET OPERA** | ~160 radars, Iceland to Romania (no Italy, Austria or Greece) | 1 km, every 5 min, ~5 min late | Public S3 bucket; MeteoGate key optional |

For OPERA, only the 512 km GeoTIFF tiles within 400 km of the location are downloaded, with HTTP range requests. That's typically 50–600 KB per scan instead of the full 3.6 MB file. The newest scan is found through the MeteoGate API, or by listing the documented bucket or its backup if that fails.

**Which source:**
- **In the Netherlands:** KNMI, with OPERA as fallback.
- **Elsewhere:** OPERA, with KNMI as fallback if the location is on KNMI's grid (Belgium, western Germany).

"In the Netherlands" is tested against the Dutch border baked into the map, with a 5 km margin so the coast and the IJsselmeer count. KNMI's data is only trusted within 250 km of its operational radars; its files report "no echo" far beyond that.

A source is passed over when:
- fetching fails,
- its newest scan is more than 15 minutes old,
- it has no data at the location, or
- it has fewer than two scans.

The next source is used instead. The app's status bar shows which source is in use and, on hover, why another one was skipped. A failed source is tried again after 5 minutes, or as soon as the location moves.

**The map** shows rain across Europe from OPERA's 4 km overview of each scan (~290 KB, extrapolated like the local forecast). The location's own radar is drawn on top at 1 km. Within the Netherlands that's KNMI, and OPERA is hidden where KNMI has data.

**Why KNMI in the Netherlands:** OPERA uses KNMI's own radars there, but processes them differently. KNMI's composite is reflectivity at ~1.5 km height. OPERA's 5-minute composite is the **maximum in the whole vertical column**, which includes rain aloft and the melting layer, so it reads much wetter. `rain gauges` scores both against KNMI's ~36 automatic rain gauges. Over 3 hours of winter rain on 19 November 2026 (594 station-intervals):

| Radar | Saw the rain (POD) | False alarms (FAR) | CSI | Radar ÷ gauge amount |
|---|---|---|---|---|
| KNMI | 0.68 | **0.20** | **0.58** | 0.34 |
| OPERA max reflectivity | **0.92** | 0.59 | 0.40 | 2.08 |
| OPERA rain rate (2 km, 15 min, ~12 min late) | 0.68 | 0.41 | 0.46 | 0.53 |

Lowering OPERA's reflectivity by ~10 dB brings it close (CSI 0.54) but not level. That gap depends on the weather (large with a melting layer, smaller in summer showers), so one evening isn't enough to set a correction. Expect OPERA to overstate light rain outside the Netherlands. KNMI shows about a third of the gauge amounts; +6 dB would fix that without hurting detection. Both corrections need more data. The georeferencing was checked with `rain compare`: OPERA and KNMI line up within 2 km.

**Speed:** KNMI publishes each scan ~3 min 50 s after its timestamp and OPERA ~4 min 25 s after. OPERA's timestamp marks the end of a 10-minute collection window, so its data can be a few minutes older than that suggests. A refresh takes ~1 s cold for KNMI and ~2 s for OPERA, which needs more round trips (API, file index, tiles). Both run in the background.

### 7-day forecast

Current conditions and the week ahead come from [Open-Meteo](https://open-meteo.com). It's free and needs no key. For the Netherlands it uses KNMI's HARMONIE model for the first ~2.5 days and ECMWF and other global models after that. KNMI's own open data has no 7-day forecast for a single point. Each day shows the weather type, the night-time low and daytime high, rain amount and chance, and wind as direction and Beaufort force. The forecast refreshes every 30 minutes and whenever you move your location.

## Running

```sh
cargo run --release              # the desktop app
cargo run --release --bin rain   # the forecast in the terminal
cargo run --release --bin rain -- --lat 52.37 --lon 4.89
cargo run --release --bin rain -- verify [DIR]   # score the nowcast against what happened
cargo run --release --bin rain -- compare        # OPERA vs KNMI over the Netherlands
cargo run --release --bin rain -- gauges         # KNMI and OPERA vs rain gauges, last 3 hours
cargo test --workspace
```

### KNMI API key

You need a free key for KNMI's **Open Data API**. Register at [developer.dataplatform.knmi.nl](https://developer.dataplatform.knmi.nl/) and click "Request an API key" under Open Data API. Then paste it into the app, or save it yourself:

- Windows: `%APPDATA%\CleanWeather\api_key.txt`
- macOS: `~/Library/Application Support/CleanWeather/api_key.txt`
- or set `KNMI_API_KEY`.

**Local builds embed the key.** When you build on your own machine, `build.rs` compiles the key from `KNMI_API_KEY` or the key file above into the binaries, so they work without setup. A key saved in the app still takes precedence. Cargo prints a warning when this happens: don't share those binaries. Set `CLEANWEATHER_NO_EMBED=1` to build without the key. CI builds (where `CI` is set, as on GitHub Actions) never embed it, so the downloads from GitHub contain no key.

The key is never stored in this repository. A registered key allows 1000 requests per hour. CleanWeather uses about 80: one listing every 90 seconds, plus one download URL per new scan.

Without a KNMI key the app still works: it uses OPERA everywhere.

### MeteoGate API key (optional)

OPERA works without a key. A free MeteoGate key from [devportal.meteogate.eu](https://devportal.meteogate.eu/) raises the API's limit from 200 to 2000 requests per hour. It's handled like the KNMI key:
- Settings (⚙), `meteogate_key.txt` next to `api_key.txt`, or `METEOGATE_API_KEY`.
- Embedded in local builds, never in CI builds.

### Network use

CleanWeather only makes requests for weather data:
- KNMI and OPERA (MeteoGate and its S3 buckets) for radar scans.
- Open-Meteo for the 7-day forecast, which receives your location rounded to 0.01° (~1 km).

The map of Europe is baked into the app (`core/assets/map.json`, ~1.8 MB), with pan and zoom from all of Europe down to a city. It holds land, lakes, borders, rivers and towns from [Natural Earth](https://www.naturalearthdata.com) (public domain), clipped and simplified by `core/examples/make_map.rs`. To rebuild it, download the five `ne_10m_*` GeoJSON files listed at the top of that file and run `cargo run --release -p cleanweather-core --example make_map -- DIR`.

### Your location

Click the place name at the top to change it. Type to search the towns on the map (accents optional: "zurich", "dusseldorf"), or choose "Pick on the map" and click a spot. You can also enter coordinates in Settings (⚙). The location is saved in `settings.json` next to the API key, so the app opens there next time.

Drag the map to move it; zoom with the + and − buttons, Ctrl+scroll, a pinch or a double-click; ⌖ goes back to your location. Clicking the map doesn't move your location unless you chose "Pick on the map".

For testing, `CLEANWEATHER_MAP_ZOOM=4.5` starts the map zoomed out to Europe, and `CLEANWEATHER_SEARCH=text` opens the location picker with a search.

## iPhone app

`ios/` is the iPhone app: native SwiftUI on the same Rust core as the desktop app, called through [UniFFI](https://mozilla.github.io/uniffi-rs/) bindings (`ffi/`). The radar download, fallback between KNMI and OPERA, the nowcast, the 7-day forecast, the place search and the map's land, water and rain images are all the core's; Swift draws the screen. One screen, top to bottom:

- **The place** (tap it to change: **Current Location**, a search of the map's towns, or **Pick on the Map** and tap a spot) and the current temperature and weather.
- **The headline**, as on the desktop ("Light rain in 20 min (14:35)"), and the **rain graph**: the last 30 minutes of radar and the next 2 hours, on the same logarithmic axis. Touch and drag to read a time's rain rate and how much of the area around you is wet.
- **The radar map**: Europe's rain under the location's own radar, with rivers, borders and towns. Drag to move, pinch or + and − to zoom, and the location button goes back. ▶ plays the last 30 minutes and the forecast; the slider picks a time and **Now** returns to the newest scan.
- **The week ahead**: weather, low and high (with a bar on the week's range), rain amount and chance, and wind as direction and Beaufort force.
- Where the radar is from and how old it is, with a warning when a source was skipped or a radar is down.

Pull down to check the radar again. While the app is open it checks every 90 seconds (45 after an error), and the 7-day forecast every 30 minutes or when you move. In the background it only checks the radar now and then (when iOS allows a Background App Refresh, every 15 minutes at most), for the widget. "Current Location" asks for location access once and follows you each time the app comes back. The location is only used on the iPhone, except for the 7-day forecast (rounded to ~1 km, as on the desktop).

**The widget** (Home Screen, small or medium) shows the headline and the rain graph for the next two hours, stepping its headline along every 5 minutes. A widget gets too little memory to run the nowcast, so the app saves what the widget shows (`ios/Shared/RainSnapshot.swift`) into an App Group, both while it's open and in its background refreshes. The app is called "Rain" on the Home Screen.

**Keys:** the app works without any, on OPERA's public radar. Settings (⚙) takes a KNMI key (the Dutch radar) and an optional MeteoGate key; they're kept in the iPhone's keychain. Unlike local desktop builds, the iPhone app never has keys built in.

### Building it

On a Mac with Xcode 15 or later, [rustup](https://rustup.rs) and [XcodeGen](https://github.com/yonaskolb/XcodeGen) (`brew install xcodegen`):

```sh
ios/build-rust.sh            # the Rust core as an XCFramework, and its Swift bindings
cd ios && xcodegen           # makes CleanWeather.xcodeproj from project.yml
open CleanWeather.xcodeproj  # choose your team under Signing, then Run
```

Run `build-rust.sh` once before `xcodegen`, since the project includes the generated `Generated/cleanweather_ffi.swift`. After that, Xcode runs the script before every build: a debug Rust build for Debug and a release build for Release.

**Tests:** ⌘U in Xcode, or `xcodebuild test -project CleanWeather.xcodeproj -scheme CleanWeather -destination 'platform=iOS Simulator,name=iPhone 16'`. The core's side (the map canvas, places, colours) is in `cargo test`.

**Downloads:** the Build workflow also builds `CleanWeather-unsigned.ipa`, to install with a tool that signs it with your Apple ID ([Sideloadly](https://sideloadly.io), [AltStore](https://altstore.io)), and `CleanWeather-Simulator.zip` for Xcode's iOS Simulator.

The app icon is drawn by `cargo run -p cleanweather-ffi --example make_icon`.

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

- `core/` (`cleanweather-core`): `knmi.rs` (KNMI API client and file cache), `opera.rs` (OPERA: finding scans, reading tiles and overviews of cloud-optimised GeoTIFFs), `gauges.rs` (KNMI rain gauge observations), `radar.rs` (scans, grids and projections; KNMI HDF5 decoding), `mercator.rs` and `basemap.rs` (the built-in map and the radar drawn onto it), `nowcast.rs` (motion, extrapolation, point forecast and skill scores), `forecast.rs` (7-day forecast), `service.rs` (picks a working radar source, keeps the local and Europe-wide nowcasts up to date). It has no UI and makes no assumptions about where files live, so the desktop and iPhone apps share it.
- `ffi/` (`cleanweather-ffi`): the core for Swift, through UniFFI: `WeatherCore` (radar refresh with fallback, the rain at a location), `MapCanvas` (`map.rs`: the map's images, lines and labels for an area), the 7-day forecast and place search. `examples/make_icon.rs` draws the app icon.
- `ios/`: the iPhone app: `CleanWeather/*.swift`, the widget (`CleanWeatherWidget/`, with `Shared/` between the two) (`WeatherStore.swift` drives the core; `RainGraph.swift`, `RadarMapView.swift`, `WeekView.swift` and `LocationPicker.swift` are the screen's parts), `CleanWeatherTests/`, `project.yml` (XcodeGen) and `build-rust.sh`.
- `src/`: the desktop app (`app.rs`, and `week.rs` for the 7-day list and weather icons), the `rain` tool (`bin/rain.rs`) and where settings are stored (`config.rs`).
- `core/examples/`: `make_map` builds the map data; `europe` times a Europe-wide update; `inspect` dumps an HDF5 file's structure; `histogram` shows a radar image's value distribution.
