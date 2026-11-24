import SwiftUI
import BackgroundTasks

/// CleanWeather on the iPhone: when will it rain here in the next two hours, from
/// radar, plus the week ahead. The radar download, fallback between sources, the
/// nowcast and the map's images come from the Rust core shared with the desktop app
/// (`ffi/`, through the generated `cleanweather_ffi.swift`).
@main
struct CleanWeatherApp: App {
    @StateObject private var store = WeatherStore()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(store)
        }
        .onChange(of: scenePhase, initial: true) { _, phase in
            // Check the radar every 90 s while the app is on screen; in the background
            // only now and then, for the widget.
            if phase == .active {
                store.start()
            } else if phase == .background {
                store.stop()
                Self.scheduleRefresh()
            }
        }
        .backgroundTask(.appRefresh(Self.refreshTask)) {
            Self.scheduleRefresh()
            await store.backgroundRefresh()
        }
    }

    nonisolated static let refreshTask = "com.cleanweather.CleanWeather.refresh"

    /// Ask iOS for a background refresh in 15 minutes or later (it decides when, and
    /// not at all with Background App Refresh off).
    nonisolated static func scheduleRefresh() {
        let request = BGAppRefreshTaskRequest(identifier: refreshTask)
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
        try? BGTaskScheduler.shared.submit(request)
    }
}
