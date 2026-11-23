import SwiftUI

/// The API keys, and where the data comes from.
struct SettingsView: View {
    @EnvironmentObject private var store: WeatherStore
    @Environment(\.dismiss) private var dismiss
    @State private var knmi = Keychain.get(.knmi) ?? ""
    @State private var meteogate = Keychain.get(.meteogate) ?? ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    SecureField("KNMI API key", text: $knmi)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                } header: {
                    Text("Dutch radar")
                } footer: {
                    Text("With a free key from KNMI's Open Data portal (developer.dataplatform.knmi.nl), the Netherlands gets KNMI's own radar, which matches rain gauges best. Without one, OPERA's is used.")
                }
                Section {
                    SecureField("MeteoGate API key (optional)", text: $meteogate)
                        .textInputAutocapitalization(.never).autocorrectionDisabled()
                } header: {
                    Text("European radar")
                } footer: {
                    Text("EUMETNET OPERA's radar works without a key. A MeteoGate key finds new scans faster.")
                }
                Section {
                    Button("Check the Radar Now") {
                        Task { await store.refresh() }
                    }
                }
                Section("About") {
                    LabeledContent("Radar", value: "KNMI, EUMETNET OPERA (CC BY 4.0)")
                    LabeledContent("7-day forecast", value: "Open-Meteo")
                    LabeledContent("Map", value: "Natural Earth")
                    Text("The rain forecast is made on this iPhone from the last radar scans: the rain is moved along the way it's been moving. Growing and dying showers aren't foreseen, so it's best for the first hour.")
                        .font(.footnote).foregroundStyle(.secondary)
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Save") {
                        store.setKeys(knmi: knmi, meteogate: meteogate)
                        dismiss()
                    }
                }
            }
        }
    }
}
