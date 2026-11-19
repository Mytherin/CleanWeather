//! The 7-day forecast list and the weather icons, drawn as shapes so they need no
//! image files and follow light and dark mode.

use chrono::Local;
use cleanweather_core::forecast::{Condition, Forecast, beaufort, compass};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, epaint::Mesh, pos2, vec2};

const ROW: f32 = 38.0;

/// A temperature as whole degrees, without a "-0°".
pub fn degrees(t: f32) -> String {
    let r = t.round();
    format!("{}°", if r == 0.0 { 0.0 } else { r })
}

/// Colour for a temperature: blue when freezing, through green and yellow, to orange-red.
fn temp_color(t: f32) -> Color32 {
    let stops: [(f32, [u8; 3]); 5] = [(-5.0, [0x4a, 0x7d, 0xf0]), (5.0, [0x5a, 0xb8, 0xf0]), (12.0, [0x6c, 0xd4, 0xa8]), (20.0, [0xf5, 0xc5, 0x42]), (28.0, [0xf0, 0x6a, 0x3a])];
    let t = t.clamp(stops[0].0, stops[4].0);
    let i = stops.iter().rposition(|s| t >= s.0).unwrap_or(0).min(3);
    let (lo, hi) = (stops[i], stops[i + 1]);
    let f = (t - lo.0) / (hi.0 - lo.0);
    let c = |k: usize| (lo.1[k] as f32 + (hi.1[k] as f32 - lo.1[k] as f32) * f) as u8;
    Color32::from_rgb(c(0), c(1), c(2))
}

/// The week: one row per day with icon, night/day temperatures on a shared scale,
/// rain and wind.
pub fn week(ui: &mut egui::Ui, f: &Forecast) {
    let visuals = ui.visuals().clone();
    let (text, weak) = (visuals.strong_text_color(), visuals.weak_text_color());
    let rain_color = Color32::from_rgb(0x2f, 0x7c, 0xf6);
    let track = if visuals.dark_mode { Color32::from_gray(55) } else { Color32::from_gray(225) };

    ui.label(egui::RichText::new("7-day forecast").strong());
    ui.add_space(2.0);
    let lo = f.days.iter().map(|d| d.min).fold(f32::MAX, f32::min);
    let hi = f.days.iter().map(|d| d.max).fold(f32::MIN, f32::max);
    let today = Local::now().date_naive();

    for (i, d) in f.days.iter().enumerate() {
        let width = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW), Sense::hover());
        let p = ui.painter_at(rect);
        if i > 0 {
            p.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0_f32, track));
        }
        let y = rect.center().y;
        let font = FontId::proportional(14.0);
        let small = FontId::proportional(12.0);

        // Day name.
        let name = if d.date == today {
            "Today".to_string()
        } else if d.date == today.succ_opt().unwrap_or(today) {
            "Tomorrow".to_string()
        } else {
            d.date.format("%a %-d").to_string()
        };
        p.text(pos2(rect.left() + 2.0, y), Align2::LEFT_CENTER, name, font.clone(), text);

        // Icon, with the chance of rain under it when there is some.
        let icon_x = rect.left() + 100.0;
        let icon_y = if d.rain_chance.is_some_and(|c| c >= 20) { y - 5.0 } else { y };
        draw_icon(&p, pos2(icon_x, icon_y), 24.0, d.condition, true, visuals.panel_fill);
        if let Some(c) = d.rain_chance.filter(|&c| c >= 20) {
            p.text(pos2(icon_x, y + 13.0), Align2::CENTER_CENTER, format!("{c}%"), FontId::proportional(10.0), rain_color);
        }

        // Night low and day high, with a bar showing the day's range within the week's.
        let right = rect.right() - 2.0;
        let wind_w = 64.0;
        let rain_w = 58.0;
        let temps_r = right - wind_w - rain_w;
        let temps_l = icon_x + 26.0;
        p.text(pos2(temps_l, y), Align2::LEFT_CENTER, degrees(d.min), font.clone(), weak);
        p.text(pos2(temps_r, y), Align2::RIGHT_CENTER, degrees(d.max), font.clone(), text);
        let (bar_l, bar_r) = (temps_l + 34.0, temps_r - 34.0);
        if bar_r > bar_l + 10.0 {
            let track_rect = Rect::from_min_max(pos2(bar_l, y - 2.5), pos2(bar_r, y + 2.5));
            p.rect_filled(track_rect, 2.5, track);
            let x_of = |t: f32| bar_l + (bar_r - bar_l) * if hi > lo { (t - lo) / (hi - lo) } else { 0.5 };
            let (x0, x1) = (x_of(d.min), x_of(d.max).max(x_of(d.min) + 5.0));
            let mut mesh = Mesh::default();
            let (c0, c1) = (temp_color(d.min), temp_color(d.max));
            let base = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(x0, y - 2.5), c0);
            mesh.colored_vertex(pos2(x1, y - 2.5), c1);
            mesh.colored_vertex(pos2(x1, y + 2.5), c1);
            mesh.colored_vertex(pos2(x0, y + 2.5), c0);
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base, base + 2, base + 3);
            p.add(Shape::mesh(mesh));
            p.circle_filled(pos2(x0, y), 2.5, c0);
            p.circle_filled(pos2(x1, y), 2.5, c1);
        }

        // Rain amount.
        let rain_x = right - wind_w;
        if d.rain_mm >= 0.1 {
            p.text(pos2(rain_x, y), Align2::RIGHT_CENTER, format!("{:.1} mm", d.rain_mm), small.clone(), rain_color);
        }

        // Wind: direction it comes from and Beaufort force, with an arrow pointing downwind.
        let bft = beaufort(d.wind_kmh);
        p.text(pos2(right, y), Align2::RIGHT_CENTER, format!("{} {bft}", compass(d.wind_from)), small.clone(), weak);
        let arrow_c = pos2(right - 48.0, y);
        let a = (d.wind_from + 180.0).to_radians();
        let dir = vec2(a.sin(), -a.cos());
        let (tip, tail) = (arrow_c + dir * 6.0, arrow_c - dir * 6.0);
        let side = vec2(-dir.y, dir.x) * 3.5;
        p.line_segment([tail, tip], Stroke::new(1.5_f32, weak));
        p.add(Shape::convex_polygon(vec![tip, tip - dir * 4.5 + side, tip - dir * 4.5 - side], weak, Stroke::NONE));

        resp.on_hover_text(format!(
            "{}\n{}: {:.0}° / {:.0}°\nRain {:.1} mm{}\nWind {} {:.0} km/h (Bft {bft})",
            d.date.format("%A %-d %B"),
            d.condition.label(),
            d.max,
            d.min,
            d.rain_mm,
            d.rain_chance.map(|c| format!(", {c}% chance")).unwrap_or_default(),
            compass(d.wind_from),
            d.wind_kmh,
        ));
    }
}

/// A weather icon in `ui` at `center`, `size` points across.
pub fn weather_icon(ui: &egui::Ui, center: Pos2, size: f32, c: Condition, is_day: bool) {
    draw_icon(ui.painter(), center, size, c, is_day, ui.visuals().panel_fill);
}

fn draw_icon(p: &egui::Painter, c: Pos2, size: f32, cond: Condition, is_day: bool, bg: Color32) {
    let s = size;
    let sun = Color32::from_rgb(0xf6, 0xb9, 0x2b);
    let moon = Color32::from_rgb(0xd8, 0xdc, 0xe6);
    let cloud = if bg.r() < 128 { Color32::from_gray(200) } else { Color32::from_gray(150) };
    let dark_cloud = if bg.r() < 128 { Color32::from_gray(150) } else { Color32::from_gray(110) };
    let rain = Color32::from_rgb(0x3b, 0x86, 0xf7);
    let bolt = Color32::from_rgb(0xf6, 0xc3, 0x2b);

    let draw_sun = |center: Pos2, r: f32| {
        if is_day {
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let d = vec2(a.cos(), a.sin());
                p.line_segment([center + d * r * 1.35, center + d * r * 1.75], Stroke::new(s * 0.06, sun));
            }
            p.circle_filled(center, r, sun);
        } else {
            // Crescent: a disc with a background-coloured disc over it.
            p.circle_filled(center, r * 1.1, moon);
            p.circle_filled(center + vec2(r * 0.55, -r * 0.4), r * 0.95, bg);
        }
    };
    let draw_cloud = |center: Pos2, scale: f32, color: Color32| {
        let k = s * scale;
        p.circle_filled(center + vec2(-0.24 * k, 0.06 * k), 0.2 * k, color);
        p.circle_filled(center + vec2(0.0, -0.08 * k), 0.27 * k, color);
        p.circle_filled(center + vec2(0.24 * k, 0.07 * k), 0.19 * k, color);
        p.rect_filled(Rect::from_min_max(center + vec2(-0.24 * k, 0.04 * k), center + vec2(0.24 * k, 0.26 * k)), 0.1 * k, color);
    };
    let drops = |n: usize, len: f32, width: f32| {
        for i in 0..n {
            let x = c.x + (i as f32 - (n as f32 - 1.0) / 2.0) * s * 0.18;
            let top = pos2(x, c.y + s * 0.3);
            p.line_segment([top, top + vec2(-s * 0.06, s * len)], Stroke::new(s * width, rain));
        }
    };

    match cond {
        Condition::Clear => draw_sun(c, s * 0.24),
        Condition::PartlyCloudy => {
            draw_sun(c + vec2(-s * 0.14, -s * 0.12), s * 0.18);
            draw_cloud(c + vec2(s * 0.08, s * 0.06), 0.75, cloud);
        }
        Condition::Cloudy => draw_cloud(c, 1.0, cloud),
        Condition::Fog => {
            for (i, w) in [0.7, 0.9, 0.6].iter().enumerate() {
                let y = c.y + (i as f32 - 1.0) * s * 0.2;
                p.line_segment([pos2(c.x - s * w / 2.0, y), pos2(c.x + s * w / 2.0, y)], Stroke::new(s * 0.08, cloud));
            }
        }
        Condition::Drizzle => {
            draw_cloud(c - vec2(0.0, s * 0.1), 0.9, cloud);
            for i in 0..3 {
                p.circle_filled(pos2(c.x + (i as f32 - 1.0) * s * 0.18, c.y + s * 0.36), s * 0.04, rain);
            }
        }
        Condition::Rain => {
            draw_cloud(c - vec2(0.0, s * 0.1), 0.9, cloud);
            drops(3, 0.16, 0.06);
        }
        Condition::HeavyRain => {
            draw_cloud(c - vec2(0.0, s * 0.1), 0.9, dark_cloud);
            drops(4, 0.2, 0.07);
        }
        Condition::Snow => {
            draw_cloud(c - vec2(0.0, s * 0.1), 0.9, cloud);
            for i in 0..3 {
                let flake = pos2(c.x + (i as f32 - 1.0) * s * 0.2, c.y + s * 0.38);
                for k in 0..3 {
                    let a = k as f32 * std::f32::consts::PI / 3.0;
                    let d = vec2(a.cos(), a.sin()) * s * 0.06;
                    p.line_segment([flake - d, flake + d], Stroke::new(s * 0.035, moon));
                }
            }
        }
        Condition::Thunder => {
            draw_cloud(c - vec2(0.0, s * 0.1), 0.9, dark_cloud);
            let pts = [(0.04, 0.18), (-0.08, 0.36), (0.02, 0.36), (-0.06, 0.52)];
            let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| c + vec2(x * s, y * s - s * 0.08)).collect();
            p.add(Shape::line(pts, Stroke::new(s * 0.07, bolt)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temperature_colours() {
        assert_eq!(temp_color(-20.0), temp_color(-5.0));
        assert_ne!(temp_color(0.0), temp_color(25.0));
    }

    #[test]
    fn no_negative_zero() {
        assert_eq!(degrees(-0.4), "0°");
        assert_eq!(degrees(-0.6), "-1°");
        assert_eq!(degrees(6.3), "6°");
    }
}
