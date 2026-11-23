import SwiftUI
import Charts

/// Rain at the location: the last 30 minutes of radar (pale) and the next two hours of
/// forecast, on the desktop app's logarithmic axis so drizzle still shows. Touch and
/// drag to read a time.
struct RainGraph: View {
    let report: RainReport
    @State private var selected: Date?

    static let rain = Color(red: 0x2f / 255, green: 0x7c / 255, blue: 0xf6 / 255)
    static let now = Color(red: 0xf0 / 255, green: 0x5a / 255, blue: 0x28 / 255)

    private struct Point: Identifiable {
        let id: Int
        let date: Date
        let height: Double
        let series: String
    }

    /// The scans and the forecast as two areas; the newest scan starts both, so they meet.
    private var points: [Point] {
        var out: [Point] = []
        let samples = report.samples
        let lastObserved = samples.lastIndex(where: { $0.observed })
        for (i, s) in samples.enumerated() {
            let h = Double(rainAxis(mmPerHour: s.mmPerHour ?? 0))
            if s.observed {
                out.append(Point(id: out.count, date: Format.date(s.time), height: h, series: "Radar"))
            }
            if !s.observed || i == lastObserved {
                out.append(Point(id: out.count, date: Format.date(s.time), height: h, series: "Forecast"))
            }
        }
        return out
    }

    /// Lines at the intensity boundaries.
    private static let ticks: [(Double, String)] = [(1, "light"), (5, "moderate"), (20, "heavy")].map { (Double(rainAxis(mmPerHour: Float($0.0))), $0.1) }

    private var nearest: RainSample? {
        guard let selected else { return nil }
        return report.samples.min(by: { abs(Format.date($0.time).timeIntervalSince(selected)) < abs(Format.date($1.time).timeIntervalSince(selected)) })
    }

    var body: some View {
        Chart {
            ForEach(points) { p in
                AreaMark(x: .value("Time", p.date), y: .value("Rain", p.height), stacking: .unstacked)
                    .foregroundStyle(by: .value("Kind", p.series))
                    .interpolationMethod(.linear)
            }
            // No radar at the location then.
            ForEach(report.samples.filter { $0.mmPerHour == nil }, id: \.time) { s in
                RuleMark(x: .value("Time", Format.date(s.time)))
                    .lineStyle(StrokeStyle(lineWidth: 3))
                    .foregroundStyle(.gray.opacity(0.3))
            }
            RuleMark(x: .value("Now", Date()))
                .lineStyle(StrokeStyle(lineWidth: 1.5))
                .foregroundStyle(Self.now)
                .annotation(position: .top, spacing: 2) {
                    Text("now").font(.caption2).foregroundStyle(Self.now)
                }
            if let s = nearest {
                RuleMark(x: .value("Selected", Format.date(s.time)))
                    .foregroundStyle(.secondary)
                    .annotation(position: .top, spacing: 0, overflowResolution: .init(x: .fit(to: .chart), y: .disabled)) {
                        Text(Format.sample(s))
                            .font(.caption)
                            .padding(6)
                            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
                    }
            }
        }
        .chartForegroundStyleScale(["Radar": Self.rain.opacity(0.4), "Forecast": Self.rain])
        .chartLegend(.hidden)
        .chartYScale(domain: 0...1)
        .chartYAxis {
            AxisMarks(position: .leading, values: Self.ticks.map { $0.0 }) { value in
                AxisGridLine()
                AxisValueLabel {
                    if let v = value.as(Double.self), let t = Self.ticks.first(where: { abs($0.0 - v) < 1e-6 }) {
                        Text(t.1)
                    }
                }
            }
        }
        .chartXAxis {
            AxisMarks(values: .stride(by: .minute, count: 30)) { _ in
                AxisGridLine()
                AxisValueLabel(format: .dateTime.hour().minute())
            }
        }
        .chartXSelection(value: $selected)
        .frame(height: 190)
        .accessibilityLabel("Rain graph")
        .accessibilityValue(Format.headline(report.outlook))
    }
}
