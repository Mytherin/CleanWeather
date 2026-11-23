import SwiftUI

/// The one screen: the place and current weather, the rain headline and graph, the
/// radar map and the week ahead. Pull to check the radar again.
struct ContentView: View {
    @EnvironmentObject private var store: WeatherStore
    @State private var showPicker = false
    @State private var showSettings = false
    /// "Pick on the Map": the next tap on the map sets the location.
    @State private var pickingOnMap = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    header
                    rainSection
                    RadarMapView(pickingOnMap: $pickingOnMap)
                    if let week = store.week {
                        WeekView(week: week)
                    } else if let e = store.forecastError {
                        Label(e, systemImage: "exclamationmark.triangle").font(.footnote).foregroundStyle(.secondary)
                    }
                    StatusFooter()
                }
                .padding(.horizontal)
                .padding(.bottom, 24)
            }
            .refreshable { await store.refresh() }
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { showSettings = true } label: { Image(systemName: "gearshape") }
                        .accessibilityLabel("Settings")
                }
            }
            .sheet(isPresented: $showPicker) {
                LocationPicker(pickOnMap: { pickingOnMap = true })
            }
            .sheet(isPresented: $showSettings) {
                SettingsView()
            }
        }
    }

    private var header: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                Button { showPicker = true } label: {
                    HStack(spacing: 6) {
                        if store.location.followsDevice {
                            Image(systemName: "location.fill").font(.title3)
                        }
                        Text(store.location.name).font(.largeTitle.bold()).lineLimit(1).minimumScaleFactor(0.6)
                        Image(systemName: "chevron.down").font(.headline).foregroundStyle(.secondary)
                    }
                }
                .buttonStyle(.plain)
                .accessibilityHint("Change location")
                Text(String(format: "%.3f° N, %.3f° E", store.location.lat, store.location.lon))
                    .font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if let now = store.week?.current {
                VStack(alignment: .trailing, spacing: 0) {
                    HStack(spacing: 6) {
                        Image(systemName: Format.symbol(now.kind, isDay: now.isDay)).symbolRenderingMode(.multicolor).font(.title)
                        Text(Format.degrees(now.temperature)).font(.system(size: 34, weight: .regular))
                    }
                    Text(now.label).font(.caption).foregroundStyle(.secondary)
                }
                .accessibilityElement(children: .combine)
            }
        }
        .padding(.top, 4)
    }

    @ViewBuilder private var rainSection: some View {
        if let rain = store.rain {
            VStack(alignment: .leading, spacing: 8) {
                Text(Format.headline(rain.outlook)).font(.title3.weight(.semibold))
                RainGraph(report: rain)
            }
        } else if let e = store.radarError {
            Label(e, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
        } else {
            HStack(spacing: 8) {
                ProgressView()
                Text("Getting the radar…").foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, minHeight: 120)
        }
        if let e = store.locationError {
            Label(e, systemImage: "location.slash").font(.footnote).foregroundStyle(.secondary)
        }
    }
}

/// Where the radar is from, how old it is, and what's wrong if anything is.
struct StatusFooter: View {
    @EnvironmentObject private var store: WeatherStore
    /// Older than this, the forecast gets a warning.
    static let staleMinutes = 20

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            VStack(alignment: .leading, spacing: 4) {
                if let r = store.rain {
                    let age = Int(context.date.timeIntervalSince(Format.date(r.issued)) / 60)
                    let text = "Radar \(Format.time(r.issued)) (\(age) min ago) · \(r.source)"
                    if age > Self.staleMinutes {
                        Label("\(text): forecast may be outdated", systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                    } else {
                        Text(text)
                    }
                    if let why = r.fallbackReason {
                        Label("Using \(r.source) instead: \(why)", systemImage: "arrow.triangle.branch").foregroundStyle(.orange)
                    }
                    if !r.radarsDown.isEmpty {
                        Label("Radar down: \(r.radarsDown.joined(separator: ", "))", systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                    }
                }
                HStack(spacing: 6) {
                    if store.checking {
                        ProgressView().controlSize(.mini)
                        Text("Checking…")
                    } else if let t = store.lastCheck {
                        Text("Checked \(t.formatted(date: .omitted, time: .shortened))")
                    }
                }
                if store.rain != nil, let e = store.radarError {
                    Label(e, systemImage: "exclamationmark.triangle").foregroundStyle(.red)
                }
                Text("Forecast: Open-Meteo · Radar: \(store.rain?.attribution ?? "KNMI, EUMETNET OPERA")")
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
    }
}
