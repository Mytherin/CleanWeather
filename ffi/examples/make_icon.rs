//! Draws the iPhone app's icon (1024 × 1024, opaque, as the App Store wants) into
//! `ios/CleanWeather/Assets.xcassets/AppIcon.appiconset/AppIcon.png`:
//!
//!   cargo run -p cleanweather-ffi --example make_icon
//!
//! A white cloud on a blue sky, with three raindrops in the radar's blues.

use std::io::Write;

const N: usize = 1024;

fn main() {
    let mut px = vec![[0u8; 3]; N * N];
    // Coverage of a shape at (x, y), antialiased over 4×4 samples.
    let cover = |f: &dyn Fn(f64, f64) -> bool, x: usize, y: usize| -> f64 {
        let mut n = 0;
        for sy in 0..4 {
            for sx in 0..4 {
                if f(x as f64 + (sx as f64 + 0.5) / 4.0, y as f64 + (sy as f64 + 0.5) / 4.0) {
                    n += 1;
                }
            }
        }
        n as f64 / 16.0
    };
    let circle = |cx: f64, cy: f64, r: f64| move |x: f64, y: f64| (x - cx).powi(2) + (y - cy).powi(2) <= r * r;
    let cloud = {
        let parts = [circle(380.0, 480.0, 150.0), circle(560.0, 400.0, 190.0), circle(720.0, 500.0, 130.0)];
        move |x: f64, y: f64| parts.iter().any(|c| c(x, y)) || (x >= 380.0 && x <= 720.0 && y >= 470.0 && y <= 630.0)
    };
    let cloud_bottom = |x: f64, y: f64| cloud(x, y) && y <= 630.0;
    // A raindrop: a circle with a point on top.
    let drop = |cx: f64, cy: f64, r: f64| {
        move |x: f64, y: f64| {
            let (dx, dy) = (x - cx, y - cy);
            // A circle, and a triangle from the tip (2r above the centre) to the points
            // where its sides touch the circle (30° either side of the top).
            let d = 2.0 * r;
            dx * dx + dy * dy <= r * r || (dy >= -d && dy <= -0.5 * r && dx.abs() <= 0.866 * r * (dy + d) / (d - 0.5 * r))
        }
    };
    let drops = [(drop(400.0, 790.0, 42.0), [0x9c, 0xd3, 0xff]), (drop(540.0, 850.0, 42.0), [0x4a, 0x9c, 0xf7]), (drop(680.0, 790.0, 42.0), [0x9c, 0xd3, 0xff])];
    for y in 0..N {
        // Sky: a vertical gradient.
        let t = y as f64 / N as f64;
        let sky = [(0x1d as f64 + (0x2f - 0x1d) as f64 * t), (0x4f as f64 + (0x7c - 0x4f) as f64 * t), (0xd8 as f64 + (0xf6 - 0xd8) as f64 * t)];
        for x in 0..N {
            let mut c = sky;
            let mut paint = |cov: f64, col: [f64; 3]| {
                for k in 0..3 {
                    c[k] = c[k] * (1.0 - cov) + col[k] * cov;
                }
            };
            for (d, col) in &drops {
                paint(cover(d, x, y), [col[0] as f64, col[1] as f64, col[2] as f64]);
            }
            paint(cover(&cloud_bottom, x, y), [255.0, 255.0, 255.0]);
            px[y * N + x] = [c[0].round() as u8, c[1].round() as u8, c[2].round() as u8];
        }
    }
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../ios/CleanWeather/Assets.xcassets/AppIcon.appiconset/AppIcon.png");
    std::fs::write(out, png(&px)).expect("writing the icon");
    println!("wrote {out}");
}

/// A minimal PNG encoder: 8-bit RGB, no filtering, zlib-compressed.
fn png(px: &[[u8; 3]]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(N * (N * 3 + 1));
    for row in px.chunks(N) {
        raw.push(0);
        row.iter().for_each(|p| raw.extend_from_slice(p));
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&raw).unwrap();
    let idat = z.finish().unwrap();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(N as u32).to_be_bytes());
    ihdr.extend_from_slice(&(N as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    for (kind, data) in [(b"IHDR", &ihdr), (b"IDAT", &idat), (b"IEND", &Vec::new())] {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
