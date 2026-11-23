import SwiftUI

/// Choose where the forecast is for: the iPhone's location, a town from the map's
/// list (searched as you type, ignoring accents), or a spot picked on the map.
struct LocationPicker: View {
    @EnvironmentObject private var store: WeatherStore
    @Environment(\.dismiss) private var dismiss
    @State private var query = ""
    @State private var locating = false
    /// Close the picker and let the next tap on the map choose.
    let pickOnMap: () -> Void

    private var results: [Place] {
        searchPlaces(query: query, limit: 25)
    }

    var body: some View {
        NavigationStack {
            List {
                if query.trimmingCharacters(in: .whitespaces).isEmpty {
                    Section {
                        Button {
                            locating = true
                            Task {
                                await store.useDeviceLocation()
                                locating = false
                                if store.locationError == nil { dismiss() }
                            }
                        } label: {
                            HStack {
                                Label("Current Location", systemImage: "location.fill")
                                Spacer()
                                if locating { ProgressView() }
                            }
                        }
                        Button {
                            dismiss()
                            pickOnMap()
                        } label: {
                            Label("Pick on the Map", systemImage: "mappin.and.ellipse")
                        }
                    } footer: {
                        if let e = store.locationError {
                            Text(e)
                        } else {
                            Text("Rain radar covers most of Europe: the Netherlands from KNMI, the rest from EUMETNET OPERA.")
                        }
                    }
                } else if results.isEmpty {
                    ContentUnavailableView {
                        Label("No Town by That Name", systemImage: "magnifyingglass")
                    } description: {
                        Text("Only towns on the map can be found. Try “Pick on the Map” instead.")
                    }
                } else {
                    ForEach(results, id: \.self) { p in
                        Button {
                            store.setLocation(WeatherLocation(name: p.name, lat: p.lat, lon: p.lon, followsDevice: false))
                            dismiss()
                        } label: {
                            HStack {
                                Text(p.name).foregroundStyle(.primary)
                                Spacer()
                                Text(String(format: "%.2f° N, %.2f° E", p.lat, p.lon)).font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Search for a town or city")
            .autocorrectionDisabled()
            .navigationTitle("Location")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}
