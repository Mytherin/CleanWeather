import Foundation

/// The rain forecast as the widget shows it, written by the app (when it's open, and
/// in a background refresh) into the App Group container both can read.
///
/// A widget gets too little memory to run the radar nowcast, so the app works out
/// everything the widget needs ahead of time: the graph's heights and the headline as
/// it will read at every 5-minute step, so the widget only draws.
struct RainSnapshot: Codable, Equatable {
    /// One step of the rain graph.
    struct Point: Codable, Equatable {
        var time: Date
        /// Height on the rain graph's axis (0–1); nil where no radar covers the place.
        var height: Double?
        /// A radar scan (the past), not a forecast.
        var observed: Bool
    }

    enum Kind: String, Codable {
        case dry, rainSoon, raining, noData
    }

    /// The headline from `at` on (until the next one).
    struct Headline: Codable, Equatable {
        var at: Date
        var text: String
        var kind: Kind
    }

    var place: String
    var followsDevice: Bool
    /// Time of the newest radar scan; nil when there's no nowcast (see `message`).
    var issued: Date?
    var points: [Point]
    var headlines: [Headline]
    /// Why there's no nowcast, if there isn't.
    var message: String?
    var source: String?

    /// When the forecast runs out.
    var end: Date? { points.last?.time }

    /// The headline at a time: the newest one from then or before.
    func headline(at date: Date) -> Headline? {
        headlines.last(where: { $0.at <= date }) ?? headlines.first
    }

    // MARK: Storage

    static let appGroup = "group.com.cleanweather.CleanWeather"
    /// The widget's kind, for reloading it.
    static let widgetKind = "RainWidget"

    private static var url: URL? {
        FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup)?.appendingPathComponent("rain.json")
    }

    static func load() -> RainSnapshot? {
        guard let url, let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(RainSnapshot.self, from: data)
    }

    /// Saves it for the widget; false where there's no App Group (an unsigned build).
    @discardableResult
    func save() -> Bool {
        guard let url = Self.url, let data = try? JSONEncoder().encode(self) else { return false }
        return (try? data.write(to: url, options: .atomic)) != nil
    }
}
