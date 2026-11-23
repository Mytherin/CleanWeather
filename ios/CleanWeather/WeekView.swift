import SwiftUI

/// The next 7 days: weather, low and high, rain, and wind as direction and Beaufort
/// force, as Dutch forecasts give it.
struct WeekView: View {
    let week: WeekForecast

    /// The temperature range across the week, for the bars.
    private var range: (Float, Float) {
        let lo = week.days.map(\.min).min() ?? 0
        let hi = week.days.map(\.max).max() ?? 1
        return (lo, max(hi, lo + 1))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("7-DAY FORECAST").font(.caption.weight(.semibold)).foregroundStyle(.secondary).padding(.bottom, 6)
            ForEach(Array(week.days.enumerated()), id: \.offset) { i, d in
                if i > 0 { Divider() }
                row(d)
            }
        }
    }

    private func row(_ d: DayForecast) -> some View {
        HStack(spacing: 10) {
            Text(Format.dayName(d)).frame(width: 78, alignment: .leading).lineLimit(1)
            VStack(spacing: 0) {
                Image(systemName: Format.symbol(d.kind)).symbolRenderingMode(.multicolor).font(.title3)
                if let chance = d.rainChance, chance >= 20 {
                    Text("\(chance)%").font(.caption2.bold()).foregroundStyle(RainGraph.rain)
                }
            }
            .frame(width: 36)
            Text(Format.degrees(d.min)).foregroundStyle(.secondary).frame(width: 34, alignment: .trailing)
            TemperatureBar(min: d.min, max: d.max, range: range).frame(height: 5)
            Text(Format.degrees(d.max)).frame(width: 34, alignment: .leading)
            VStack(alignment: .trailing, spacing: 1) {
                if d.rainMm >= 0.1 {
                    Text(String(format: "%.1f mm", d.rainMm)).foregroundStyle(RainGraph.rain)
                }
                Text("\(d.windFrom) \(d.beaufort)").foregroundStyle(.secondary)
            }
            .font(.caption)
            .frame(width: 52, alignment: .trailing)
        }
        .padding(.vertical, 8)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(Format.dayName(d)): \(d.label), \(Format.degrees(d.min)) to \(Format.degrees(d.max)), rain \(String(format: "%.1f", d.rainMm)) mm, wind \(d.windFrom) force \(d.beaufort)")
    }
}

/// The day's low-to-high span on the week's scale, coloured from cold to warm.
private struct TemperatureBar: View {
    let min: Float
    let max: Float
    let range: (Float, Float)

    var body: some View {
        GeometryReader { g in
            let span = CGFloat(range.1 - range.0)
            let x0 = CGFloat(min - range.0) / span * g.size.width
            let x1 = CGFloat(max - range.0) / span * g.size.width
            ZStack(alignment: .leading) {
                Capsule().fill(.quaternary)
                Capsule()
                    .fill(LinearGradient(colors: [color(min), color(max)], startPoint: .leading, endPoint: .trailing))
                    .frame(width: Swift.max(x1 - x0, 5))
                    .offset(x: x0)
            }
        }
    }

    /// Blue when freezing, through green and yellow, to orange-red.
    private func color(_ t: Float) -> Color {
        switch t {
        case ..<0: return Color(red: 0.35, green: 0.6, blue: 1)
        case ..<10: return Color(red: 0.3, green: 0.8, blue: 0.75)
        case ..<18: return Color(red: 0.55, green: 0.85, blue: 0.35)
        case ..<25: return Color(red: 0.98, green: 0.8, blue: 0.2)
        default: return Color(red: 0.97, green: 0.45, blue: 0.2)
        }
    }
}
