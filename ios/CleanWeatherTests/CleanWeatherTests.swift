import XCTest
@testable import CleanWeather

/// The app's own Swift (headlines, formatting, the map's images) against the Rust core.
final class CleanWeatherTests: XCTestCase {
    func testHeadlines() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let in20 = Int64(1_800_000_000 + 20 * 60)
        XCTAssertTrue(Format.headline(.rainFrom(at: in20, peak: 2), now: now).hasPrefix("Moderate rain in 20 min ("))
        XCTAssertEqual(Format.headline(.rainFrom(at: 1_800_000_000, peak: 0.5), now: now), "Light rain now")
        XCTAssertTrue(Format.headline(.dry(until: in20), now: now).hasPrefix("Dry until at least "))
        XCTAssertEqual(Format.headline(.raining(until: nil, peak: 25), now: now), "Rain for the next 2 hours (up to very heavy rain)")
        XCTAssertEqual(Format.headline(.noData, now: now), "No radar coverage here")
    }

    func testWidgetSnapshotHeadlinesAhead() {
        let t0: Int64 = 1_800_000_000
        // Dry at the newest scan, light rain from +10 to +20 min.
        let samples = (-1...4).map { k in
            RainSample(time: t0 + Int64(k) * 300, mmPerHour: (2...3).contains(k) ? Float(0.5) : 0, chance: 0, observed: k <= 0)
        }
        let report = RainReport(issued: t0, samples: samples, outlook: .rainFrom(at: t0 + 600, peak: 0.5), motion: nil,
                                source: "KNMI", attribution: "", fallbackReason: nil, radarsDown: [])
        let s = RainSnapshot(location: .amsterdam, report: report)
        XCTAssertEqual(s.points.count, 6)
        XCTAssertEqual(s.headlines.map(\.kind), [RainSnapshot.Kind]([.rainSoon, .rainSoon, .raining, .raining, .dry]))
        XCTAssertEqual(s.headline(at: Format.date(t0 + 400))?.text, "Light rain in 5 min (\(Format.time(t0 + 600)))")
        XCTAssertEqual(s.end, Format.date(t0 + 1200))
    }

    func testSavedRainRoundTripAndFreshness() throws {
        let t0: Int64 = 1_800_000_000
        let rates: [Float?] = [0, 0, 0, 1.5, nil]
        let samples = rates.enumerated().map { i, mm in
            RainSample(time: t0 + Int64(i - 1) * 300, mmPerHour: mm, chance: 0.25, observed: i <= 1)
        }
        let report = RainReport(issued: t0, samples: samples, outlook: .rainFrom(at: t0 + 600, peak: 1.5),
                                motion: RainMotion(kmh: 30, towards: "NE"), source: "KNMI", attribution: "KNMI (CC BY 4.0)",
                                fallbackReason: nil, radarsDown: ["Herwijnen"])
        let saved = try JSONDecoder().decode(SavedRain.self, from: JSONEncoder().encode(SavedRain(location: .amsterdam, report: report)))
        XCTAssertEqual(saved.report, report, "the outlook comes back from the samples")
        let at = { (minutes: Int64) in Format.date(t0 + minutes * 60) }
        XCTAssertTrue(saved.usable(for: .amsterdam, now: at(14)))
        XCTAssertFalse(saved.usable(for: .amsterdam, now: at(16)), "too old")
        XCTAssertFalse(saved.usable(for: WeatherLocation(name: "Utrecht", lat: 52.09, lon: 5.12, followsDevice: false), now: at(1)), "elsewhere")
    }

    func testDegreesAndDays() {
        XCTAssertEqual(Format.degrees(-0.4), "0°")
        XCTAssertEqual(Format.degrees(6.6), "7°")
        let cal = Calendar(identifier: .gregorian)
        let today = cal.date(from: DateComponents(year: 2026, month: 11, day: 23, hour: 12))!
        let day = { (d: UInt32) in
            DayForecast(year: 2026, month: 11, day: d, kind: .rain, label: "Rain", max: 9, min: 3, rainMm: 2.5,
                        rainChance: 80, windKmh: 20, beaufort: 3, windFrom: "SW")
        }
        XCTAssertEqual(Format.dayName(day(23), today: today, calendar: cal), "Today")
        XCTAssertEqual(Format.dayName(day(24), today: today, calendar: cal), "Tomorrow")
        XCTAssertFalse(Format.dayName(day(26), today: today, calendar: cal).isEmpty)
    }

    func testPlacesFromTheCore() {
        XCTAssertEqual(searchPlaces(query: "dusseldorf", limit: 3).first?.name, "Düsseldorf")
        XCTAssertEqual(placeName(lat: 52.09, lon: 5.12), "Utrecht")
    }

    func testMapImagesBecomeCGImages() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("cleanweather-tests").path
        let core = WeatherCore(cacheDir: dir, knmiKey: nil, meteogateKey: nil)
        XCTAssertNil(core.rain(lat: 52.1, lon: 5.1), "no nowcast before a refresh")
        let canvas = core.mapCanvas(centreLat: 52.1, centreLon: 5.1, zoom: 7, width: 120, height: 80, scale: 2)
        let base = try XCTUnwrap(canvas.baseImage(dark: false).cgImage())
        XCTAssertEqual(base.width, 240)
        XCTAssertEqual(base.height, 160)
        XCTAssertNotNil(canvas.radarImage(frame: 0, dark: true).cgImage())
        XCTAssertFalse(canvas.labels(pinLat: 52.1, pinLon: 5.1).isEmpty)
    }
}
