import SwiftUI
import WidgetKit
import Charts

/// The Home Screen widget: will it rain here in the next two hours. It draws what the
/// app saved (`RainSnapshot`); a widget can't run the nowcast itself.
@main
struct RainWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: RainSnapshot.widgetKind, provider: Provider()) { entry in
            RainWidgetView(entry: entry)
                .containerBackground(.fill.tertiary, for: .widget)
        }
        .configurationDisplayName("Rain")
        .description("Rain in the next two hours, from radar.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

struct Entry: TimelineEntry {
    let date: Date
    let snapshot: RainSnapshot?
}

struct Provider: TimelineProvider {
    func placeholder(in context: Context) -> Entry {
        Entry(date: Date(), snapshot: .preview)
    }

    func getSnapshot(in context: Context, completion: @escaping (Entry) -> Void) {
        completion(Entry(date: Date(), snapshot: context.isPreview ? (RainSnapshot.load() ?? .preview) : RainSnapshot.load()))
    }

    /// An entry at every headline (every 5 minutes) until the forecast runs out, then
    /// one saying it's out of date. The app reloads the widget when it has a new one.
    func getTimeline(in context: Context, completion: @escaping (Timeline<Entry>) -> Void) {
        let now = Date()
        guard let snapshot = RainSnapshot.load(), let end = snapshot.end else {
            completion(Timeline(entries: [Entry(date: now, snapshot: RainSnapshot.load())], policy: .never))
            return
        }
        var dates = [now] + snapshot.headlines.map(\.at).filter { $0 > now }
        dates.append(end)
        completion(Timeline(entries: dates.map { Entry(date: $0, snapshot: snapshot) }, policy: .never))
    }
}

struct RainWidgetView: View {
    let entry: Entry
    @Environment(\.widgetFamily) private var family

    var body: some View {
        if let snapshot = entry.snapshot {
            content(snapshot)
        } else {
            message("Open CleanWeather to see the rain here.")
        }
    }

    @ViewBuilder private func content(_ s: RainSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 3) {
                if s.followsDevice {
                    Image(systemName: "location.fill").font(.caption2)
                }
                Text(s.place).font(.caption.weight(.semibold)).lineLimit(1)
            }
            if let end = s.end, entry.date < end, let headline = s.headline(at: entry.date) {
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: Self.symbol(headline.kind))
                        .symbolRenderingMode(.multicolor)
                        .font(family == .systemSmall ? .title3 : .title2)
                    Text(headline.text)
                        .font(family == .systemSmall ? .footnote.weight(.semibold) : .subheadline.weight(.semibold))
                        .lineLimit(family == .systemSmall ? 3 : 2)
                        .minimumScaleFactor(0.8)
                }
                Spacer(minLength: 2)
                WidgetRainChart(points: s.points, now: entry.date, axisLabels: family != .systemSmall)
                if family != .systemSmall, let issued = s.issued {
                    Text("Radar \(issued.formatted(date: .omitted, time: .shortened))\(s.source.map { " · \($0)" } ?? "")")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            } else {
                Spacer()
                Text(s.message ?? "Forecast out of date. Open CleanWeather to update it.")
                    .font(.footnote).foregroundStyle(.secondary)
                Spacer()
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func message(_ text: String) -> some View {
        Text(text).font(.footnote).foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
    }

    static func symbol(_ kind: RainSnapshot.Kind) -> String {
        switch kind {
        case .dry: return "umbrella"
        case .rainSoon: return "cloud.drizzle.fill"
        case .raining: return "cloud.rain.fill"
        case .noData: return "questionmark.circle"
        }
    }
}

/// The rain from now to the end of the forecast, on the app's logarithmic axis.
struct WidgetRainChart: View {
    let points: [RainSnapshot.Point]
    let now: Date
    let axisLabels: Bool

    static let rain = Color(red: 0x2f / 255, green: 0x7c / 255, blue: 0xf6 / 255)

    var body: some View {
        let end = points.last?.time ?? now
        // The step before now too, so the area starts at the left edge.
        let firstShown = points.lastIndex(where: { $0.time <= now }) ?? 0
        let shown = Array(points[firstShown...]).filter { $0.height != nil }
        Chart {
            ForEach(shown, id: \.time) { p in
                AreaMark(x: .value("Time", p.time), y: .value("Rain", p.height ?? 0))
                    .foregroundStyle(Self.rain.gradient)
                    .interpolationMethod(.monotone)
            }
            // A floor line, so a dry forecast still shows as a graph.
            RuleMark(y: .value("Dry", 0)).foregroundStyle(.secondary.opacity(0.5))
        }
        .chartXScale(domain: now...max(end, now.addingTimeInterval(60)))
        .chartYScale(domain: 0...1)
        .chartYAxis(.hidden)
        .chartXAxis {
            if axisLabels {
                AxisMarks(values: .stride(by: .minute, count: 30)) { _ in
                    AxisGridLine()
                    AxisValueLabel(format: .dateTime.hour().minute())
                }
            } else {
                AxisMarks(values: .stride(by: .hour, count: 1)) { _ in
                    AxisGridLine()
                }
            }
        }
        .clipped()
    }
}

extension RainSnapshot {
    /// For the widget gallery: rain coming in a quarter of an hour.
    static var preview: RainSnapshot {
        let now = Date()
        let heights: [Double] = [0, 0, 0, 0.2, 0.45, 0.6, 0.5, 0.35, 0.2, 0.1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        let points = heights.enumerated().map { k, h in Point(time: now.addingTimeInterval(Double(k) * 300), height: h, observed: k == 0) }
        let at = now.addingTimeInterval(900)
        return RainSnapshot(
            place: "Amsterdam", followsDevice: true, issued: now, points: points,
            headlines: [Headline(at: now, text: "Moderate rain in 15 min (\(at.formatted(date: .omitted, time: .shortened)))", kind: .rainSoon)],
            message: nil, source: "KNMI"
        )
    }
}
