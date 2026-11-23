import Foundation

/// Words and numbers as the app shows them, in the iPhone's time zone and clock.
enum Format {
    static func date(_ secs: Int64) -> Date {
        Date(timeIntervalSince1970: TimeInterval(secs))
    }

    /// "14:35" (or "2:35 PM", as the iPhone is set).
    static func time(_ secs: Int64) -> String {
        date(secs).formatted(date: .omitted, time: .shortened)
    }

    /// The one-line answer: will it rain, and when.
    static func headline(_ outlook: RainOutlook, now: Date = Date()) -> String {
        switch outlook {
        case .dry(let until):
            return "Dry until at least \(time(until))"
        case .rainFrom(let at, let peak):
            let what = capitalised(intensityLabel(mmPerHour: peak))
            let minutes = Int((TimeInterval(at) - now.timeIntervalSince1970) / 60)
            return minutes <= 0 ? "\(what) now" : "\(what) in \(minutes) min (\(time(at)))"
        case .raining(let until, let peak):
            let up = intensityLabel(mmPerHour: peak)
            if let until {
                return "Rain stops around \(time(until)) (up to \(up))"
            }
            return "Rain for the next 2 hours (up to \(up))"
        case .noData:
            return "No radar coverage here"
        }
    }

    static func capitalised(_ s: String) -> String {
        s.prefix(1).uppercased() + s.dropFirst()
    }

    /// "7°", never "-0°".
    static func degrees(_ t: Float) -> String {
        let r = Int(t.rounded())
        return "\(r)°"
    }

    /// "Today", "Tomorrow", else "Mon 24".
    static func dayName(_ d: DayForecast, today: Date = Date(), calendar: Calendar = .current) -> String {
        guard let date = calendar.date(from: DateComponents(year: Int(d.year), month: Int(d.month), day: Int(d.day))) else { return "" }
        if calendar.isDate(date, inSameDayAs: today) { return "Today" }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: today), calendar.isDate(date, inSameDayAs: tomorrow) { return "Tomorrow" }
        return date.formatted(.dateTime.weekday(.abbreviated).day())
    }

    /// The SF Symbol for the weather (multicolour where it has colours).
    static func symbol(_ kind: WeatherKind, isDay: Bool = true) -> String {
        switch kind {
        case .clear: return isDay ? "sun.max.fill" : "moon.stars.fill"
        case .partlyCloudy: return isDay ? "cloud.sun.fill" : "cloud.moon.fill"
        case .cloudy: return "cloud.fill"
        case .fog: return "cloud.fog.fill"
        case .drizzle: return "cloud.drizzle.fill"
        case .rain: return "cloud.rain.fill"
        case .heavyRain: return "cloud.heavyrain.fill"
        case .snow: return "cloud.snow.fill"
        case .thunder: return "cloud.bolt.rain.fill"
        }
    }

    /// "1.2 mm/h · moderate rain" for a sample of the rain graph.
    static func sample(_ s: RainSample) -> String {
        let when = "\(time(s.time)) \(s.observed ? "radar" : "forecast")"
        guard let mm = s.mmPerHour else { return "\(when)\nno radar data" }
        return "\(when)\n\(String(format: "%.1f", mm)) mm/h · \(intensityLabel(mmPerHour: mm))\n\(Int((s.chance * 100).rounded()))% of the area wet"
    }

    /// The map's frame: "14:35 radar" or "15:10 forecast (+35 min)".
    static func frame(_ f: MapFrame) -> String {
        f.observed ? "\(time(f.time)) radar" : "\(time(f.time)) forecast (+\(f.minutesAhead) min)"
    }
}
