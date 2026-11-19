//! Time a Europe-wide update: `cargo run --release -p cleanweather-core --example europe`
use cleanweather_core::service::Europe;
fn main() {
    let key = std::env::var("METEOGATE_API_KEY").ok();
    let cache = std::env::temp_dir().join("cleanweather-europe-test");
    let mut e = Europe::new(key, cache);
    for run in ["cold", "warm"] {
        let t = std::time::Instant::now();
        let nc = e.update().unwrap();
        println!("{run}: {:?}, new nowcast: {}", t.elapsed(), nc.is_some());
        if let Some(nc) = nc {
            println!("  grid {}x{}, {} scans, motion {:?}", nc.grid.width, nc.grid.height, nc.observed.len(), nc.motion.mean_motion());
            let wet = nc.observed.last().unwrap().1 .0.iter().filter(|&&c| c >= 78).count();
            println!("  pixels with rain in the newest scan: {wet}");
        }
    }
}
