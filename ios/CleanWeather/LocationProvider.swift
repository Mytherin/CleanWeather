import CoreLocation

enum LocationProblem: LocalizedError {
    case denied
    case unavailable

    var errorDescription: String? {
        switch self {
        case .denied: return "Location access is off for CleanWeather. Turn it on in Settings → Privacy & Security → Location Services, or choose a place instead."
        case .unavailable: return "Your location isn't available right now."
        }
    }
}

/// The iPhone's location, once, to about a kilometre (all a rain forecast needs).
@MainActor
final class LocationProvider: NSObject, CLLocationManagerDelegate {
    private let manager = CLLocationManager()
    private var waiting: [CheckedContinuation<CLLocation, Error>] = []

    override init() {
        super.init()
        manager.delegate = self
        manager.desiredAccuracy = kCLLocationAccuracyKilometer
    }

    func current() async throws -> CLLocation {
        // A recent fix is good enough.
        if let last = manager.location, -last.timestamp.timeIntervalSinceNow < 120 {
            return last
        }
        return try await withCheckedThrowingContinuation { c in
            waiting.append(c)
            if waiting.count == 1 {
                ask()
            }
        }
    }

    private func ask() {
        switch manager.authorizationStatus {
        case .notDetermined:
            manager.requestWhenInUseAuthorization() // continues in locationManagerDidChangeAuthorization
        case .denied, .restricted:
            finish(.failure(LocationProblem.denied))
        default:
            manager.requestLocation()
        }
    }

    private func finish(_ result: Result<CLLocation, Error>) {
        let w = waiting
        waiting = []
        w.forEach { $0.resume(with: result) }
    }

    nonisolated func locationManagerDidChangeAuthorization(_ manager: CLLocationManager) {
        Task { @MainActor in
            if !self.waiting.isEmpty && self.manager.authorizationStatus != .notDetermined {
                self.ask()
            }
        }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let last = locations.last else { return }
        Task { @MainActor in self.finish(.success(last)) }
    }

    nonisolated func locationManager(_ manager: CLLocationManager, didFailWithError error: Error) {
        let denied = (error as? CLError)?.code == .denied
        Task { @MainActor in self.finish(.failure(denied ? LocationProblem.denied : LocationProblem.unavailable)) }
    }
}
