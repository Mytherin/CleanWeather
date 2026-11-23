import Foundation
import Security

/// The API keys, in the keychain (this iPhone only).
enum Keychain {
    enum Key: String {
        /// KNMI Open Data: adds the Dutch radar.
        case knmi = "knmi-api-key"
        /// MeteoGate: optional, for OPERA's (Europe's) radar.
        case meteogate = "meteogate-api-key"
    }

    private static let service = "com.cleanweather.CleanWeather"

    private static func query(_ key: Key) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: key.rawValue]
    }

    static func get(_ key: Key) -> String? {
        var q = query(key)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let data = out as? Data else { return nil }
        let s = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return s.isEmpty ? nil : s
    }

    /// Save a key; blank removes it.
    static func set(_ key: Key, _ value: String) {
        SecItemDelete(query(key) as CFDictionary)
        let v = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !v.isEmpty else { return }
        var q = query(key)
        q[kSecValueData as String] = Data(v.utf8)
        q[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(q as CFDictionary, nil)
    }
}
