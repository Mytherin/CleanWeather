import Foundation
import WidgetKit

extension RainSnapshot {
    /// The widget's view of a nowcast: the graph, and the headline at the newest scan
    /// and at every forecast step after it, as it will read then.
    init(location: WeatherLocation, report: RainReport) {
        let times = [report.issued] + report.samples.map(\.time).filter { $0 > report.issued }
        self.init(
            place: location.name,
            followsDevice: location.followsDevice,
            issued: Format.date(report.issued),
            points: report.samples.map { s in
                Point(time: Format.date(s.time), height: s.mmPerHour.map { Double(rainAxis(mmPerHour: $0)) }, observed: s.observed)
            },
            headlines: times.map { t in
                let outlook = outlookAt(samples: report.samples, now: t)
                return Headline(at: Format.date(t), text: Format.headline(outlook, now: Format.date(t)), kind: Kind(outlook))
            },
            message: nil,
            source: report.source
        )
    }

    /// No nowcast for the place (yet): say why.
    init(location: WeatherLocation, message: String) {
        self.init(place: location.name, followsDevice: location.followsDevice, issued: nil, points: [], headlines: [], message: message, source: nil)
    }

    /// Save it and have the widget show it.
    func publish() {
        if save() {
            WidgetCenter.shared.reloadTimelines(ofKind: Self.widgetKind)
        }
    }
}

extension RainSnapshot.Kind {
    init(_ outlook: RainOutlook) {
        switch outlook {
        case .dry: self = .dry
        case .rainFrom: self = .rainSoon
        case .raining: self = .raining
        case .noData: self = .noData
        }
    }
}
