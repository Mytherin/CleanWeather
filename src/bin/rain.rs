//! `rain`: the CleanWeather forecast in the terminal.
//!
//!   rain                         forecast for the place set in the app
//!   rain --lat 52.37 --lon 4.89  forecast for a location
//!   rain verify [DIR]            score the nowcast against what happened, using the
//!                                KNMI radar files in DIR (default: the app's cache)
//!   rain gauges                  score KNMI and OPERA radar against KNMI's rain gauges
//!                                over the last 3 hours
//!   rain compare                 compare OPERA's and KNMI's latest common scan over the
//!                                Netherlands

#[path = "../config.rs"]
#[allow(dead_code)]
mod config;

use anyhow::{Context, Result, bail};
use chrono::Local;
use cleanweather_core::nowcast::{self, Field, Nowcast, Outlook, STEP_MINUTES, intensity_label, skill};
use cleanweather_core::opera::{Opera, Product};
use cleanweather_core::radar::{Frame, rain_rate};
use cleanweather_core::service::{self, RadarSource, Service};
use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("verify") {
        let dir = args.get(1).map(PathBuf::from).unwrap_or_else(config::cache_dir);
        return verify(&dir);
    }
    if args.first().map(String::as_str) == Some("gauges") {
        return gauges();
    }
    if args.first().map(String::as_str) == Some("compare") {
        return compare();
    }
    let mut settings = config::Settings::load();
    let mut i = 0;
    while i < args.len() {
        let val = || args.get(i + 1).context("missing value").and_then(|v| v.parse::<f64>().context("not a number"));
        match args[i].as_str() {
            "--lat" => settings.lat = val()?,
            "--lon" => settings.lon = val()?,
            a => bail!("unknown argument {a} (see the top of src/bin/rain.rs)"),
        }
        settings.place = format!("{:.3}, {:.3}", settings.lat, settings.lon);
        i += 2;
    }
    let mut service = Service::new(config::api_key(), config::meteogate_key(), config::cache_dir());
    let update = service.update(settings.lat, settings.lon)?.context("no nowcast")?;
    if let Some(why) = &update.fallback_reason {
        println!("note: using {} because {why}", update.source);
    }
    print_forecast(&update.nowcast, update.source, &settings);
    Ok(())
}

fn print_forecast(nc: &Nowcast, source: &str, s: &config::Settings) {
    let samples = nc.point(s.lat, s.lon);
    let issued = nc.issued.with_timezone(&Local);
    println!("{} — {source} radar {} ({} min ago)", s.place, issued.format("%H:%M"), (chrono::Utc::now() - nc.issued).num_minutes());
    if !nc.radars_down.is_empty() {
        println!("note: radar(s) not operational: {}", nc.radars_down.join(", "));
    }
    if let Some((kmh, dir)) = nc.motion.mean_motion() {
        println!("rain moving towards {:.0}° at {:.0} km/h", dir, kmh);
    }
    let t = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&Local).format("%H:%M").to_string();
    match nowcast::outlook(&samples) {
        Outlook::Dry { until } => println!("Dry until at least {}", t(until)),
        Outlook::RainFrom { at, peak } => println!("Rain from {} ({})", t(at), intensity_label(peak)),
        Outlook::Raining { until: Some(u), peak } => println!("Raining, stops around {} (up to {})", t(u), intensity_label(peak)),
        Outlook::Raining { until: None, peak } => println!("Raining for the next 2 hours (up to {})", intensity_label(peak)),
        Outlook::NoData => println!("No radar coverage at this location"),
    }
    println!();
    for smp in &samples {
        let mm = smp.mm_per_hour.unwrap_or(0.0);
        let bar = "█".repeat(((mm.max(0.0) + 1.0).log2() * 8.0).round() as usize);
        let kind = if smp.observed { "obs " } else { "fcst" };
        println!("{} {kind} {:>5.1} mm/h {:>3.0}%  {bar}", t(smp.time), mm, smp.chance * 100.0);
    }
}

/// Hindcast: start a forecast from each scan that has 15 minutes of history and an
/// hour of future in DIR, and compare with what the radar saw.
fn verify(dir: &PathBuf) -> Result<()> {
    let mut frames: Vec<Frame> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "h5"))
        .filter_map(|e| Frame::read(&e.path()).ok())
        .collect();
    frames.sort_by_key(|f| f.time);
    println!("{} scans from {} to {}", frames.len(), frames.first().map(|f| f.time.to_string()).unwrap_or_default(), frames.last().map(|f| f.time.to_string()).unwrap_or_default());
    let leads = [3usize, 6, 9, 12]; // 15, 30, 45, 60 minutes
    let mut rows = Vec::new();
    for i in 3..frames.len() {
        let start = &frames[i - 3..=i];
        if (start[3].time - start[0].time).num_minutes() != 15 {
            continue;
        }
        let max_lead = leads.iter().copied().filter(|&l| i + l < frames.len()).max();
        let Some(max_lead) = max_lead else { continue };
        let started = std::time::Instant::now();
        let nc = Nowcast::compute(start, max_lead)?;
        let took = started.elapsed();
        let persist = Field::from_dbz(&frames[i].dbz);
        for &l in leads.iter().filter(|&&l| l <= max_lead) {
            let obs = &frames[i + l];
            if obs.time != nc.forecast_time(l - 1) {
                continue; // a gap in the scans
            }
            let o = Field::from_dbz(&obs.dbz);
            rows.push((l, skill(&nc.forecast[l - 1], &o), skill(&persist, &o)));
        }
        let mm = nc.motion.mean_motion();
        println!("from {}: motion {:?} ({} ms)", frames[i].time.format("%H:%M"), mm.map(|(k, d)| format!("{k:.0} km/h towards {d:.0}°")), took.as_millis());
    }
    if rows.is_empty() {
        bail!("need at least 20 minutes of consecutive scans in {}", dir.display());
    }
    println!("\nlead   CSI≥1mm/h (nowcast / persistence)   MAE mm/h (nowcast / persistence)");
    for &l in &leads {
        let r: Vec<_> = rows.iter().filter(|r| r.0 == l && r.1.csi.is_finite() && r.2.csi.is_finite()).collect();
        if r.is_empty() {
            continue;
        }
        let n = r.len() as f32;
        let avg = |f: &dyn Fn(&&(usize, nowcast::Skill, nowcast::Skill)) -> f32| r.iter().map(f).sum::<f32>() / n;
        println!(
            "{:>3} min   {:.2} / {:.2}                        {:.2} / {:.2}     (n={})",
            l as i64 * STEP_MINUTES,
            avg(&|r| r.1.csi),
            avg(&|r| r.2.csi),
            avg(&|r| r.1.mae),
            avg(&|r| r.2.mae),
            r.len()
        );
    }
    Ok(())
}

/// Compare OPERA with KNMI over the Netherlands for the newest scan both have:
/// agreement on where it rains and on how hard.
fn compare() -> Result<()> {
    for product in [Product::MaxReflectivity, Product::RainRate] {
        println!("== OPERA {product:?}");
        if let Err(e) = compare_with(product) {
            println!("error: {e:#}");
        }
        println!();
    }
    Ok(())
}

fn compare_with(product: Product) -> Result<()> {
    let key = config::api_key().context("compare needs a KNMI key")?;
    let ams = (52.37, 4.89);
    let mut knmi = service::Knmi::new(key, config::cache_dir());
    let mut opera = Opera::new(config::meteogate_key(), config::cache_dir().join("opera")).with_product(product);
    let k = RadarSource::latest(&mut knmi, ams.0, ams.1, service::HISTORY)?;
    let o = RadarSource::latest(&mut opera, ams.0, ams.1, 3)?;
    let Some((kf, of)) = k.iter().rev().find_map(|kf| o.iter().find(|of| of.time == kf.time).map(|of| (kf, of))) else {
        bail!("no scan time in common (KNMI newest {:?}, OPERA newest {:?})", k.iter().map(|f| f.time).max(), o.iter().map(|f| f.time).max());
    };
    println!("scan {} UTC; KNMI vs OPERA on KNMI's grid where both have data", kf.time.format("%H:%M"));
    let g = &kf.grid;
    let (mut both, mut hits, mut misses, mut fa, mut wet_k, mut wet_o) = (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    let (mut diff, mut n_diff) = (0.0f64, 0u32);
    for row in 0..g.height {
        for col in 0..g.width {
            let dk = kf.get(col, row);
            let (lat, lon) = g.to_latlon(col as f64 + 0.5, row as f64 + 0.5);
            let (oc, or) = of.grid.to_pixel(lat, lon);
            if dk.is_nan() || oc < 0.0 || or < 0.0 || oc as usize >= of.grid.width || or as usize >= of.grid.height {
                continue;
            }
            let dop = of.get(oc as usize, or as usize);
            if dop.is_nan() {
                continue;
            }
            both += 1;
            let (rk, ro) = (rain_rate(dk) >= 1.0, rain_rate(dop) >= 1.0);
            match (rk, ro) {
                (true, true) => hits += 1,
                (true, false) => misses += 1,
                (false, true) => fa += 1,
                _ => {}
            }
            wet_k += (rain_rate(dk) >= 0.1) as u32;
            wet_o += (rain_rate(dop) >= 0.1) as u32;
            if dk >= 7.0 && dop >= 7.0 {
                diff += (dop - dk) as f64;
                n_diff += 1;
            }
        }
    }
    println!("pixels compared: {both}");
    println!("area with rain (>= 0.1 mm/h): KNMI {:.1}%, OPERA {:.1}%", 100.0 * wet_k as f64 / both as f64, 100.0 * wet_o as f64 / both as f64);
    let total = hits + misses + fa;
    if total > 0 {
        println!("agreement on rain >= 1 mm/h (CSI): {:.2}  (hits {hits}, only KNMI {misses}, only OPERA {fa})", hits as f64 / total as f64);
    }
    if n_diff > 0 {
        println!("OPERA minus KNMI where both see rain: {:+.1} dBZ on average", diff / n_diff as f64);
    }
    // Georeferencing check: agreement when OPERA is shifted by a few km. OPERA gets
    // the threshold that gives it the same rain area as KNMI at 0.1 mm/h, so this
    // compares where it rains, not how hard.
    let mut rates: Vec<f32> = Vec::new();
    for row in (0..g.height).step_by(2) {
        for col in (0..g.width).step_by(2) {
            let (lat, lon) = g.to_latlon(col as f64 + 0.5, row as f64 + 0.5);
            let (oc, or) = of.grid.to_pixel(lat, lon);
            if !kf.get(col, row).is_nan() && oc >= 0.0 && or >= 0.0 && (oc as usize) < of.grid.width && (or as usize) < of.grid.height {
                let v = of.get(oc as usize, or as usize);
                if !v.is_nan() {
                    rates.push(rain_rate(v));
                }
            }
        }
    }
    rates.sort_by(|a, b| a.total_cmp(b));
    let area_k = wet_k as f64 / both as f64;
    let t_o = rates[((1.0 - area_k) * rates.len() as f64) as usize].max(0.1);
    let mut best = (0.0, 0.0, 0.0);
    for dy in -6..=6 {
        for dx in -6..=6 {
            let c = shifted_csi(kf, of, dx as f64 * 2.0 / product.pixel_km() as f64, dy as f64 * 2.0 / product.pixel_km() as f64, 0.1, t_o);
            if c > best.0 {
                best = (c, dx as f64 * 2.0, dy as f64 * 2.0);
            }
        }
    }
    let at0 = shifted_csi(kf, of, 0.0, 0.0, 0.1, t_o);
    println!("same-area threshold for OPERA: {t_o:.1} mm/h");
    println!("CSI: {at0:.3} unshifted, best {:.3} with OPERA shifted {:+} km east, {:+} km south", best.0, best.1, best.2);
    Ok(())
}

/// Agreement (CSI at `threshold` mm/h) between two scans, with the second shifted by
/// (dx, dy) pixels of its own grid: a georeferencing check.
#[allow(dead_code)]
fn shifted_csi(kf: &Frame, of: &Frame, dx: f64, dy: f64, threshold: f32, other_threshold: f32) -> f64 {
    let g = &kf.grid;
    let (mut hits, mut other) = (0u32, 0u32);
    for row in (0..g.height).step_by(2) {
        for col in (0..g.width).step_by(2) {
            let dk = kf.get(col, row);
            if dk.is_nan() {
                continue;
            }
            let (lat, lon) = g.to_latlon(col as f64 + 0.5, row as f64 + 0.5);
            let (oc, or) = of.grid.to_pixel(lat, lon);
            let (oc, or) = (oc + dx, or + dy);
            if oc < 0.0 || or < 0.0 || oc as usize >= of.grid.width || or as usize >= of.grid.height {
                continue;
            }
            let dop = of.get(oc as usize, or as usize);
            if dop.is_nan() {
                continue;
            }
            match (rain_rate(dk) >= threshold, rain_rate(dop) >= other_threshold) {
                (true, true) => hits += 1,
                (false, false) => {}
                _ => other += 1,
            }
        }
    }
    hits as f64 / (hits + other).max(1) as f64
}

/// Score each radar against KNMI's rain gauges over the last ~3 hours: for every
/// station and 10-minute interval, the gauge's mean intensity against the radar's,
/// averaged over the scans in that interval at the station's pixel (3×3 mean).
fn gauges() -> Result<()> {
    use chrono::{DateTime, Duration, Utc};
    use cleanweather_core::gauges;
    use cleanweather_core::knmi::{Client, time_from_filename};
    use std::collections::BTreeMap;

    let key = config::api_key().context("gauges needs a KNMI key")?;
    let cache = config::cache_dir();
    let obs_client = Client::new(key.clone(), cache.with_file_name("obs")).with_dataset(gauges::DATASET.0, gauges::DATASET.1);
    let radar_client = Client::new(key, cache.clone());
    let opera = Opera::new(config::meteogate_key(), cache.join("opera"));
    let rate = Opera::new(config::meteogate_key(), cache.join("opera")).with_product(Product::RainRate);

    // Gauges: the last 18 ten-minute files.
    let mut obs: Vec<(DateTime<Utc>, Vec<gauges::Gauge>)> = Vec::new();
    for name in obs_client.latest_files(18)? {
        let t = time_from_filename(&name).context("obs file time")?;
        obs.push((t, gauges::read(&obs_client.fetch(&name)?).with_context(|| name.clone())?));
    }
    obs.sort_by_key(|o| o.0);
    let (start, end) = (obs[0].0 - Duration::minutes(10), obs.last().unwrap().0);
    println!("{} gauge files, {} to {} UTC, {} stations", obs.len(), start.format("%H:%M"), end.format("%H:%M"), obs[0].1.len());

    // KNMI radar scans covering the same period (start times).
    let mut knmi: BTreeMap<DateTime<Utc>, Frame> = BTreeMap::new();
    let names = radar_client.latest_files(48)?;
    let wanted: Vec<&String> = names.iter().filter(|n| time_from_filename(n).is_some_and(|t| t >= start && t < end)).collect();
    let loaded: Vec<Result<Frame>> = std::thread::scope(|s| {
        let hs: Vec<_> = wanted.iter().map(|n| s.spawn(|| radar_client.fetch(n).and_then(|p| Frame::read(&p)))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for f in loaded.into_iter().flatten() {
        knmi.insert(f.time, f);
    }

    // OPERA scans (nominal times) for the same period, around the Netherlands.
    let (bucket, _) = opera.newest()?;
    let fetch_opera = |o: &Opera, product: Product, step: i64| -> BTreeMap<DateTime<Utc>, Frame> {
        let crop = cleanweather_core::opera::crop_for(52.2, 5.3, product).unwrap();
        let mut times = Vec::new();
        let mut t = start;
        while t <= end {
            if t.timestamp() % (step * 60) == 0 {
                times.push(t);
            }
            t += Duration::minutes(5);
        }
        let got: Vec<Option<Frame>> = std::thread::scope(|s| {
            let hs: Vec<_> = times.iter().map(|&t| { let (b, c) = (&bucket, &crop); s.spawn(move || o.frame(b, t, c).ok()) }).collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        got.into_iter().flatten().map(|f| (f.time, f)).collect()
    };
    let opera_max = fetch_opera(&opera, Product::MaxReflectivity, 5);
    let opera_rate = fetch_opera(&rate, Product::RainRate, 15);
    println!("scans: KNMI {}, OPERA max reflectivity {}, OPERA rain rate {}", knmi.len(), opera_max.len(), opera_rate.len());

    // Rain rate (mm/h) of a scan around a location: mean of the 3×3 pixels.
    let at = |f: &Frame, lat: f64, lon: f64| -> Option<f64> {
        let (c, r) = f.grid.to_pixel(lat, lon);
        let (mut sum, mut n) = (0.0, 0);
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                let (x, y) = (c as i64 + dx, r as i64 + dy);
                if x < 0 || y < 0 || x as usize >= f.grid.width || y as usize >= f.grid.height {
                    continue;
                }
                let v = f.get(x as usize, y as usize);
                if !v.is_nan() {
                    sum += rain_rate(v) as f64;
                    n += 1;
                }
            }
        }
        (n > 0).then(|| sum / n as f64)
    };
    // The radar's mean over a gauge interval (t-10, t]: scans valid inside it.
    // KNMI names scans by their start; OPERA by their nominal (end) time.
    let mean_over = |scans: &BTreeMap<DateTime<Utc>, Frame>, from: DateTime<Utc>, to: DateTime<Utc>, lat: f64, lon: f64| -> Option<f64> {
        let v: Vec<f64> = scans.range(from..to).filter_map(|(_, f)| at(f, lat, lon)).collect();
        (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
    };

    #[derive(Default)]
    struct Score { n: u32, hits: u32, misses: u32, false_alarms: u32, radar_sum: f64, gauge_sum: f64, abs_err: f64 }
    let mut scores: Vec<(&str, Score)> = vec![("KNMI", Score::default()), ("OPERA max reflectivity", Score::default()), ("OPERA rain rate", Score::default())];
    let mut wet_gauges = 0;
    for (t, gs) in &obs {
        let from = *t - Duration::minutes(10);
        for g in gs {
            if g.mm_per_hour >= 0.1 {
                wet_gauges += 1;
            }
            let est = [
                mean_over(&knmi, from, *t, g.lat, g.lon),
                mean_over(&opera_max, from + Duration::minutes(1), *t + Duration::minutes(1), g.lat, g.lon),
                mean_over(&opera_rate, from + Duration::minutes(1), *t + Duration::minutes(1), g.lat, g.lon),
            ];
            for (k, e) in est.iter().enumerate() {
                let Some(r) = *e else { continue };
                let s = &mut scores[k].1;
                s.n += 1;
                match (r >= 0.1, g.mm_per_hour >= 0.1) {
                    (true, true) => s.hits += 1,
                    (false, true) => s.misses += 1,
                    (true, false) => s.false_alarms += 1,
                    _ => {}
                }
                s.radar_sum += r;
                s.gauge_sum += g.mm_per_hour;
                s.abs_err += (r - g.mm_per_hour).abs();
            }
        }
    }
    println!("station intervals with rain at the gauge (>= 0.1 mm/h): {wet_gauges}\n");
    println!("{:<24} {:>6} {:>5} {:>5} {:>5} {:>13} {:>10}", "radar", "pairs", "POD", "FAR", "CSI", "radar/gauge", "MAE mm/h");
    for (name, s) in &scores {
        let f = |a: u32, b: u32| if a + b > 0 { a as f64 / (a + b) as f64 } else { f64::NAN };
        println!(
            "{:<24} {:>6} {:>5.2} {:>5.2} {:>5.2} {:>13.2} {:>10.2}",
            name,
            s.n,
            f(s.hits, s.misses),
            f(s.false_alarms, s.hits),
            f(s.hits, s.misses + s.false_alarms),
            s.radar_sum / s.gauge_sum,
            s.abs_err / s.n.max(1) as f64
        );
    }
    println!("\nPOD: share of rainy gauge intervals the radar also saw. FAR: share of the radar's rain the gauge didn't get.");

    // Would a fixed correction (in dB) make a radar match the gauges?
    println!("\nwith a fixed reflectivity correction:");
    println!("{:<24} {:>6} {:>5} {:>5} {:>5} {:>13}", "radar", "dB", "POD", "FAR", "CSI", "radar/gauge");
    for (name, scans, shift) in [("KNMI", &knmi, 0i64), ("OPERA max reflectivity", &opera_max, 1)] {
        for db in [-12.0f32, -9.0, -6.0, -3.0, 0.0, 3.0, 6.0] {
            let (mut hits, mut misses, mut fa, mut rs, mut gs) = (0u32, 0u32, 0u32, 0.0f64, 0.0f64);
            for (t, gl) in &obs {
                let from = *t - Duration::minutes(10) + Duration::minutes(shift);
                for g in gl {
                    let v: Vec<f64> = scans
                        .range(from..*t + Duration::minutes(shift))
                        .filter_map(|(_, f)| {
                            let (c, r) = f.grid.to_pixel(g.lat, g.lon);
                            let (mut s, mut n) = (0.0, 0);
                            for dy in -1i64..=1 {
                                for dx in -1i64..=1 {
                                    let (x, y) = (c as i64 + dx, r as i64 + dy);
                                    if x >= 0 && y >= 0 && (x as usize) < f.grid.width && (y as usize) < f.grid.height {
                                        let d = f.get(x as usize, y as usize);
                                        if !d.is_nan() {
                                            s += rain_rate(d + db) as f64;
                                            n += 1;
                                        }
                                    }
                                }
                            }
                            (n > 0).then(|| s / n as f64)
                        })
                        .collect();
                    if v.is_empty() {
                        continue;
                    }
                    let r = v.iter().sum::<f64>() / v.len() as f64;
                    match (r >= 0.1, g.mm_per_hour >= 0.1) {
                        (true, true) => hits += 1,
                        (false, true) => misses += 1,
                        (true, false) => fa += 1,
                        _ => {}
                    }
                    rs += r;
                    gs += g.mm_per_hour;
                }
            }
            let ratio = |a: u32, b: u32| a as f64 / (a + b).max(1) as f64;
            println!("{:<24} {:>+6.0} {:>5.2} {:>5.2} {:>5.2} {:>13.2}", name, db, ratio(hits, misses), ratio(fa, hits), ratio(hits, misses + fa), rs / gs);
        }
    }
    Ok(())
}
