import Foundation

/// The last nowcast, saved so the app shows it at once when it's opened again soon
/// after, while it checks the radar.
struct SavedRain: Codable, Equatable {
    struct Sample: Codable, Equatable {
        var time: Int64
        var mmPerHour: Float?
        var chance: Float
        var observed: Bool
    }

    struct Motion: Codable, Equatable {
        var kmh: Float
        var towards: String
    }

    var lat: Double
    var lon: Double
    var issued: Int64
    var samples: [Sample]
    var motion: Motion?
    var source: String
    var attribution: String
    var fallbackReason: String?
    var radarsDown: [String]

    /// Shown on opening only while it's this fresh: the age the core still uses a
    /// scan for (older, and the screen would show a forecast that's gone stale).
    static let freshMinutes = 15

    init(location: WeatherLocation, report: RainReport) {
        lat = location.lat
        lon = location.lon
        issued = report.issued
        samples = report.samples.map { Sample(time: $0.time, mmPerHour: $0.mmPerHour, chance: $0.chance, observed: $0.observed) }
        motion = report.motion.map { Motion(kmh: $0.kmh, towards: $0.towards) }
        source = report.source
        attribution = report.attribution
        fallbackReason = report.fallbackReason
        radarsDown = report.radarsDown
    }

    var report: RainReport {
        let samples = self.samples.map { RainSample(time: $0.time, mmPerHour: $0.mmPerHour, chance: $0.chance, observed: $0.observed) }
        return RainReport(
            issued: issued, samples: samples, outlook: outlookAt(samples: samples, now: issued),
            motion: motion.map { RainMotion(kmh: $0.kmh, towards: $0.towards) },
            source: source, attribution: attribution, fallbackReason: fallbackReason, radarsDown: radarsDown
        )
    }

    // MARK: Storage

    private static var url: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("last-rain.json")
    }

    /// The saved nowcast, if it's for this place and fresh enough to show.
    static func load(for location: WeatherLocation, now: Date = Date()) -> RainReport? {
        guard let data = try? Data(contentsOf: url), let saved = try? JSONDecoder().decode(SavedRain.self, from: data) else { return nil }
        return saved.usable(for: location, now: now) ? saved.report : nil
    }

    func usable(for location: WeatherLocation, now: Date) -> Bool {
        let age = now.timeIntervalSince1970 - TimeInterval(issued)
        return !WeatherStore.moved(from: (lat, lon), to: location) && age >= 0 && age < TimeInterval(Self.freshMinutes * 60)
    }

    func save() {
        if let data = try? JSONEncoder().encode(self) {
            try? data.write(to: Self.url, options: .atomic)
        }
    }
}
