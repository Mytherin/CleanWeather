//! `rain`: the CleanWeather forecast in the terminal.
//!
//!   rain                         forecast for the place set in the app
//!   rain --lat 52.37 --lon 4.89  forecast for a location
//!   rain verify [DIR]            score the nowcast against what happened, using the
//!                                radar files in DIR (default: the app's cache)

#[path = "../config.rs"]
#[allow(dead_code)]
mod config;

use anyhow::{Context, Result, bail};
use chrono::Local;
use cleanweather_core::nowcast::{self, Field, Nowcast, Outlook, STEP_MINUTES, intensity_label, skill};
use cleanweather_core::radar::Frame;
use cleanweather_core::service::Service;
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
    let key = config::api_key().context("no KNMI API key: set KNMI_API_KEY or save it from the app")?;
    let nc = Service::new(key, config::cache_dir()).update()?.context("no nowcast")?;
    print_forecast(&nc, &settings);
    Ok(())
}

fn print_forecast(nc: &Nowcast, s: &config::Settings) {
    let samples = nc.point(s.lat, s.lon);
    let issued = nc.issued.with_timezone(&Local);
    println!("{} — radar {} ({} min ago)", s.place, issued.format("%H:%M"), (chrono::Utc::now() - nc.issued).num_minutes());
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
