import Foundation
import CoreLocation

/// The place the forecast is for: a town, a spot picked on the map, or wherever the
/// iPhone is (`followsDevice`).
struct WeatherLocation: Codable, Equatable, Sendable {
    var name: String
    var lat: Double
    var lon: Double
    var followsDevice: Bool

    /// Until a location is chosen: Amsterdam (Dam square), as on the desktop.
    static let amsterdam = WeatherLocation(name: "Amsterdam", lat: 52.373, lon: 4.893, followsDevice: false)
}

/// The app's state: the location, the nowcast and the week ahead, kept fresh while
/// the app is on screen. The Rust core does the work, off the main thread.
@MainActor
final class WeatherStore: ObservableObject {
    @Published private(set) var location: WeatherLocation
    @Published private(set) var rain: RainReport?
    @Published private(set) var week: WeekForecast?
    @Published private(set) var radarError: String?
    @Published private(set) var forecastError: String?
    @Published private(set) var locationError: String?
    @Published private(set) var checking = false
    @Published private(set) var lastCheck: Date?
    /// Goes up with every new radar scan, so the map redraws.
    @Published private(set) var radarVersion = 0

    let core: WeatherCore
    private let locator = LocationProvider()
    private var loop: Task<Void, Never>?
    private var inFlight = 0
    /// When and where the 7-day forecast was last fetched.
    private var forecastFetched: (date: Date, lat: Double, lon: Double)?

    /// How often to look for a new scan (one comes every 5 minutes), and sooner after an error.
    static let checkEvery: Duration = .seconds(90)
    static let retryEvery: Duration = .seconds(45)
    /// The 7-day forecast's models update every few hours.
    static let forecastEvery: TimeInterval = 30 * 60

    init() {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0].appendingPathComponent("radar", isDirectory: true)
        try? FileManager.default.createDirectory(at: caches, withIntermediateDirectories: true)
        core = WeatherCore(cacheDir: caches.path, knmiKey: Keychain.get(.knmi), meteogateKey: Keychain.get(.meteogate))
        location = Self.savedLocation() ?? .amsterdam
    }

    // MARK: Refreshing

    /// Check now and then every 90 seconds, while the app is on screen.
    func start() {
        guard loop == nil else { return }
        loop = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.followDevice()
                await self.refresh()
                try? await Task.sleep(for: self.radarError == nil ? Self.checkEvery : Self.retryEvery)
            }
        }
    }

    func stop() {
        loop?.cancel()
        loop = nil
    }

    /// Check the radar for a new scan and, when it's due, the 7-day forecast.
    func refresh() async {
        inFlight += 1
        checking = true
        defer {
            inFlight -= 1
            checking = inFlight > 0
        }
        let loc = location
        let core = self.core
        let radar = await Task.detached(priority: .userInitiated) {
            Result(catching: { try core.refreshRadar(lat: loc.lat, lon: loc.lon) })
        }.value
        // The location may have changed meanwhile: show the rain where it is now.
        rain = core.rain(lat: location.lat, lon: location.lon)
        lastCheck = Date()
        switch radar {
        case .success(let changed):
            radarError = nil
            if changed { radarVersion += 1 }
        case .failure(let e):
            radarError = Self.message(e)
        }
        await refreshForecastIfDue()
    }

    private func refreshForecastIfDue() async {
        let loc = location
        if let f = forecastFetched, Date().timeIntervalSince(f.date) < Self.forecastEvery, !Self.moved(from: (f.lat, f.lon), to: loc) {
            return
        }
        let result = await Task.detached(priority: .userInitiated) {
            Result(catching: { try fetchForecast(lat: loc.lat, lon: loc.lon) })
        }.value
        // Drop an answer for a place we've since left.
        guard loc == location else { return }
        switch result {
        case .success(let w):
            week = w
            forecastError = nil
            forecastFetched = (Date(), loc.lat, loc.lon)
        case .failure(let e):
            forecastError = Self.message(e)
        }
    }

    // MARK: Location

    /// Forecast for another place, and remember it for next time.
    func setLocation(_ loc: WeatherLocation) {
        let moved = Self.moved(from: (location.lat, location.lon), to: loc)
        location = loc
        locationError = nil
        if let data = try? JSONEncoder().encode(loc) {
            UserDefaults.standard.set(data, forKey: "location")
        }
        guard moved else { return }
        // The current nowcast may already cover the new place; if it doesn't, show that
        // the radar is coming rather than "no coverage" until the refresh is done.
        let here = core.rain(lat: loc.lat, lon: loc.lon)
        rain = here?.outlook == .noData ? nil : here
        week = nil
        forecastFetched = nil
        Task { await refresh() }
    }

    /// Forecast for wherever the iPhone is (asks for permission the first time).
    func useDeviceLocation() async {
        do {
            let here = try await locator.current()
            let (lat, lon) = (here.coordinate.latitude, here.coordinate.longitude)
            setLocation(WeatherLocation(name: placeName(lat: lat, lon: lon), lat: lat, lon: lon, followsDevice: true))
        } catch {
            locationError = Self.message(error)
        }
    }

    /// With "Current Location" chosen, move along with the iPhone (quietly: a failure
    /// keeps the last known place).
    private func followDevice() async {
        guard location.followsDevice, let here = try? await locator.current() else { return }
        let (lat, lon) = (here.coordinate.latitude, here.coordinate.longitude)
        let loc = WeatherLocation(name: placeName(lat: lat, lon: lon), lat: lat, lon: lon, followsDevice: true)
        if Self.moved(from: (location.lat, location.lon), to: loc) {
            setLocation(loc)
        }
    }

    // MARK: Keys

    /// Save new API keys (blank removes one) and check again with them.
    func setKeys(knmi: String, meteogate: String) {
        Keychain.set(.knmi, knmi)
        Keychain.set(.meteogate, meteogate)
        core.setKeys(knmiKey: Keychain.get(.knmi), meteogateKey: Keychain.get(.meteogate))
        Task { await refresh() }
    }

    // MARK: Helpers

    private static func savedLocation() -> WeatherLocation? {
        guard let data = UserDefaults.standard.data(forKey: "location") else { return nil }
        return try? JSONDecoder().decode(WeatherLocation.self, from: data)
    }

    /// More than ~500 m apart (as the desktop app counts a move).
    static func moved(from a: (Double, Double), to b: WeatherLocation) -> Bool {
        abs(a.0 - b.lat) > 0.005 || abs(a.1 - b.lon) > 0.005
    }

    static func message(_ error: Error) -> String {
        if case WeatherError.Failed(let message) = error {
            return message
        }
        return error.localizedDescription
    }
}
