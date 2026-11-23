import SwiftUI

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
            // Check the radar while the app is on screen; nothing runs in the background.
            if phase == .active {
                store.start()
            } else if phase == .background {
                store.stop()
            }
        }
    }
}
