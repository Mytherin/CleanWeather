import SwiftUI

/// The radar map, drawn like the desktop app's: land and water and the rain are images
/// from the Rust core (`MapCanvas`); rivers, borders and towns are drawn here. Drag to
/// move, pinch to zoom, and play the last 30 minutes and the next two hours.
///
/// The canvas covers more than the view (`margin` beyond each edge), so a drag only
/// moves it; when a gesture ends outside it, or at another zoom, a new one is made in
/// the background while the old one stays on screen, scaled to fit.
@MainActor
struct RadarMapView: View {
    @EnvironmentObject private var store: WeatherStore
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.displayScale) private var displayScale
    @Binding var pickingOnMap: Bool

    /// The map's centre (none: the location) and zoom (Europe ≈ 4, the Netherlands ≈ 7).
    @State private var centre: LatLon?
    @State private var zoom: Double = 7
    @State private var drawn: Drawn?
    /// The rain of each frame of `drawn`'s canvas, filled in the background.
    @State private var radar: [UInt32: CGImage] = [:]
    @State private var frame: UInt32 = 0
    /// Show the newest scan when new data comes (until the slider is moved).
    @State private var followLatest = true
    @State private var playing = false
    @State private var drag: CGSize = .zero
    @State private var pinch: CGFloat = 1
    @State private var pinchShift: CGSize = .zero
    @State private var viewSize: CGSize = .zero
    /// Bumped for every canvas requested, so a slow one doesn't replace a newer one.
    @State private var generation = 0

    static let zoomRange: ClosedRange<Double> = 3.5...10
    /// The canvas reaches this share of the view beyond each edge.
    static let margin: CGFloat = 0.25
    static let pinColor = Color(red: 0xe5 / 255, green: 0x2d / 255, blue: 0x2d / 255)

    /// A canvas and what doesn't change while it's shown.
    struct Drawn {
        let canvas: MapCanvas
        let base: CGImage?
        let lines: [MapLine]
        let labels: [MapLabel]
        let frames: [MapFrame]
        let zoom: Double
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            GeometryReader { g in
                map(size: g.size)
            }
            .aspectRatio(1 / 0.9, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            timeline
            legend
            if let m = store.rain?.motion {
                Text("Rain moving \(m.towards) at about \(Int(m.kmh.rounded())) km/h").font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    private var here: LatLon {
        centre ?? LatLon(lat: store.location.lat, lon: store.location.lon)
    }

    // MARK: The map

    private func map(size: CGSize) -> some View {
        let t = transform(size: size)
        return ZStack(alignment: .topLeading) {
            colorScheme == .dark ? Color(red: 24 / 255, green: 28 / 255, blue: 35 / 255) : Color(red: 212 / 255, green: 225 / 255, blue: 238 / 255)
            if let d = drawn, let t {
                let (w, h) = (CGFloat(d.canvas.width()) * t.k, CGFloat(d.canvas.height()) * t.k)
                if let base = d.base {
                    layer(base, w, h, t)
                }
                Canvas { ctx, _ in drawLines(ctx, d, t) }
                if let rain = radar[frame] {
                    layer(rain, w, h, t)
                }
                Canvas { ctx, _ in drawLabels(ctx, d, t) }
            }
        }
        .frame(width: size.width, height: size.height)
        .contentShape(Rectangle())
        .gesture(dragGesture.simultaneously(with: pinchGesture))
        .simultaneousGesture(SpatialTapGesture().onEnded { tap in pick(at: tap.location) })
        // Outside the map's gestures, so tapping a button never picks the spot under it.
        .overlay { overlays }
        .onChange(of: store.location) { _, _ in
            centre = nil // a new place: show it
        }
        .task(id: Request(size: size, dark: colorScheme == .dark, version: store.radarVersion, location: store.location)) {
            viewSize = size
            await redraw()
        }
        .task(id: playing) {
            while playing && !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(350))
                guard playing, let n = drawn?.frames.count, n > 0 else { break }
                frame = (frame + 1) % UInt32(n)
                followLatest = false
            }
        }
    }

    /// An image of the canvas, placed by the transform; it doesn't change the layout.
    private func layer(_ image: CGImage, _ w: CGFloat, _ h: CGFloat, _ t: Transform) -> some View {
        Color.clear.overlay(alignment: .topLeading) {
            Image(decorative: image, scale: 1).resizable().interpolation(.medium)
                .frame(width: w, height: h)
                .offset(x: t.origin.x, y: t.origin.y)
        }
        .allowsHitTesting(false)
    }

    /// What a redraw depends on, besides gestures.
    private struct Request: Equatable {
        let size: CGSize
        let dark: Bool
        let version: Int
        let location: WeatherLocation
    }

    /// Where the canvas goes on screen: canvas point p is at `origin + p · k`.
    struct Transform {
        let origin: CGPoint
        let k: CGFloat

        func point(_ x: Float, _ y: Float) -> CGPoint {
            CGPoint(x: origin.x + CGFloat(x) * k, y: origin.y + CGFloat(y) * k)
        }
    }

    /// The scale from the canvas's zoom to the view's, with a pinch in progress.
    private func scale(_ d: Drawn) -> CGFloat {
        CGFloat(pow(2, zoom - d.zoom)) * pinch
    }

    /// How far a drag, and a pinch keeping the spot between the fingers in place, move the map.
    private var shift: CGSize {
        CGSize(width: drag.width + pinchShift.width, height: drag.height + pinchShift.height)
    }

    private func transform(size: CGSize) -> Transform? {
        guard let d = drawn else { return nil }
        // The view's centre on the canvas lands in the middle of the view (moved by gestures).
        let c = d.canvas.project(lat: here.lat, lon: here.lon)
        let k = scale(d)
        let mid = CGPoint(x: size.width / 2 + shift.width, y: size.height / 2 + shift.height)
        return Transform(origin: CGPoint(x: mid.x - CGFloat(c.x) * k, y: mid.y - CGFloat(c.y) * k), k: k)
    }

    /// The location at a point of the view.
    private func location(at point: CGPoint) -> LatLon? {
        guard let d = drawn else { return nil }
        let k = scale(d)
        let c = d.canvas.project(lat: here.lat, lon: here.lon)
        return d.canvas.location(x: c.x + Double((point.x - viewSize.width / 2 - shift.width) / k),
                                 y: c.y + Double((point.y - viewSize.height / 2 - shift.height) / k))
    }

    /// Make what the gestures did (moved, zoomed) the map's centre and zoom.
    private func settleGestures() {
        if let c = location(at: CGPoint(x: viewSize.width / 2, y: viewSize.height / 2)) {
            centre = c
        }
        zoom = clampZoom(zoom + log2(Double(pinch)))
        drag = .zero
        pinch = 1
        pinchShift = .zero
        Task { await redrawIfNeeded() }
    }

    private func drawLines(_ ctx: GraphicsContext, _ d: Drawn, _ t: Transform) {
        let dark = colorScheme == .dark
        let river = dark ? Color(red: 40 / 255, green: 52 / 255, blue: 68 / 255) : Color(red: 180 / 255, green: 202 / 255, blue: 226 / 255)
        let border = Color(white: dark ? 95 / 255 : 150 / 255)
        var rivers = Path()
        var borders = Path()
        for line in d.lines {
            let pts = line.points
            guard pts.count >= 4 else { continue }
            var p = Path()
            p.move(to: t.point(pts[0], pts[1]))
            for i in stride(from: 2, to: pts.count - 1, by: 2) {
                p.addLine(to: t.point(pts[i], pts[i + 1]))
            }
            if line.kind == .river { rivers.addPath(p) } else { borders.addPath(p) }
        }
        ctx.stroke(rivers, with: .color(river), lineWidth: 1)
        ctx.stroke(borders, with: .color(border), style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
    }

    private func drawLabels(_ ctx: GraphicsContext, _ d: Drawn, _ t: Transform) {
        let dark = colorScheme == .dark
        let ink = dark ? Color.white : Color.black
        let halo = dark ? Color.black.opacity(0.8) : Color.white.opacity(0.85)
        for l in d.labels {
            let p = t.point(l.x, l.y)
            let font = Font.system(size: l.big ? 12 : 10.5, weight: l.big ? .semibold : .regular)
            ctx.fill(Path(ellipseIn: CGRect(x: p.x - 2.5, y: p.y - 2.5, width: 5, height: 5)), with: .color(ink))
            let at = CGPoint(x: p.x + (l.textOnRight ? 5 : -5), y: p.y)
            let anchor: UnitPoint = l.textOnRight ? .leading : .trailing
            let outline = ctx.resolve(Text(l.name).font(font).foregroundColor(halo))
            for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                ctx.draw(outline, at: CGPoint(x: at.x + dx, y: at.y + dy), anchor: anchor)
            }
            ctx.draw(ctx.resolve(Text(l.name).font(font).foregroundColor(ink)), at: at, anchor: anchor)
        }
        // The location's pin.
        let pin = d.canvas.project(lat: store.location.lat, lon: store.location.lon)
        let p = t.point(Float(pin.x), Float(pin.y))
        let dot = Path(ellipseIn: CGRect(x: p.x - 6, y: p.y - 6, width: 12, height: 12))
        ctx.fill(dot, with: .color(Self.pinColor))
        ctx.stroke(dot, with: .color(.white), lineWidth: 2)
    }

    private var overlays: some View {
        ZStack {
            if let d = drawn, d.frames.indices.contains(Int(frame)) {
                tag(Format.frame(d.frames[Int(frame)]), size: 13)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding(8)
                tag(d.canvas.attribution(), size: 9)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomTrailing).padding(5)
            } else if drawn == nil {
                ProgressView()
            }
            VStack(spacing: 6) {
                mapButton("plus", "Zoom in") { zoomBy(1) }
                mapButton("minus", "Zoom out") { zoomBy(-1) }
                mapButton("location", "Back to your location") {
                    centre = nil
                    Task { await redrawIfNeeded() }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topTrailing).padding(8)
            if pickingOnMap {
                HStack {
                    Text("Tap the map to set your location")
                    Button("Cancel") { pickingOnMap = false }.bold().foregroundStyle(.white)
                }
                .font(.footnote).foregroundStyle(.white)
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Self.pinColor, in: Capsule())
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom).padding(.bottom, 24)
            }
        }
    }

    private func tag(_ text: String, size: CGFloat) -> some View {
        Text(text).font(.system(size: size)).padding(.horizontal, 5).padding(.vertical, 2)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 4))
    }

    private func mapButton(_ symbol: String, _ label: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 14, weight: .semibold)).frame(width: 32, height: 32)
        }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
        .accessibilityLabel(label)
    }

    // MARK: Gestures

    private var dragGesture: some Gesture {
        DragGesture(minimumDistance: 4)
            .onChanged { drag = $0.translation }
            .onEnded { _ in
                // While a pinch goes on, it settles both when it ends.
                if pinch == 1 { settleGestures() }
            }
    }

    private var pinchGesture: some Gesture {
        MagnifyGesture()
            .onChanged { v in
                let z = clampZoom(zoom + log2(Double(v.magnification)))
                pinch = CGFloat(pow(2, z - zoom))
                // Keep the spot where the pinch started under the fingers.
                let ax = v.startLocation.x - viewSize.width / 2 - drag.width
                let ay = v.startLocation.y - viewSize.height / 2 - drag.height
                pinchShift = CGSize(width: ax * (1 - pinch), height: ay * (1 - pinch))
            }
            .onEnded { _ in settleGestures() }
    }

    private func clampZoom(_ z: Double) -> Double {
        min(max(z, Self.zoomRange.lowerBound), Self.zoomRange.upperBound)
    }

    private func zoomBy(_ step: Double) {
        centre = here
        zoom = clampZoom(zoom + step)
        Task { await redrawIfNeeded() }
    }

    /// In "Pick on the Map" mode, a tap sets the location there.
    private func pick(at point: CGPoint) {
        guard pickingOnMap, let at = location(at: point) else { return }
        pickingOnMap = false
        store.setLocation(WeatherLocation(name: placeName(lat: at.lat, lon: at.lon), lat: at.lat, lon: at.lon, followsDevice: false))
    }

    // MARK: Drawing

    /// A new canvas if the view has left the old one or the zoom has changed.
    private func redrawIfNeeded() async {
        guard let d = drawn, viewSize != .zero else { return }
        let c = d.canvas.project(lat: here.lat, lon: here.lon)
        let k = pow(2, zoom - d.zoom)
        let (hw, hh) = (Double(viewSize.width) / 2 / k, Double(viewSize.height) / 2 / k)
        let inside = c.x - hw >= 0 && c.y - hh >= 0 && c.x + hw <= Double(d.canvas.width()) && c.y + hh <= Double(d.canvas.height())
        if !inside || d.zoom != zoom {
            await redraw()
        }
    }

    /// Make a canvas around the centre and the rain of all its frames (in the background;
    /// after the first, each frame is ~1 ms), then show them together, so playing or
    /// scrubbing never reaches a frame that isn't drawn yet.
    private func redraw() async {
        let size = viewSize
        guard size.width > 0, size.height > 0 else { return }
        generation += 1
        let gen = generation
        let core = store.core
        let dark = colorScheme == .dark
        let pixels = Double(min(displayScale, 3))
        let (w, h) = (UInt32(size.width * (1 + 2 * Self.margin)), UInt32(size.height * (1 + 2 * Self.margin)))
        let (at, z) = (here, zoom)
        let pin = (store.location.lat, store.location.lon)
        // A frame chosen on the slider stays at its time when new scans come.
        let keepTime: Int64? = followLatest ? nil : drawn.flatMap { $0.frames.indices.contains(Int(frame)) ? $0.frames[Int(frame)].time : nil }
        let (next, shown, images): (Drawn, UInt32, [UInt32: CGImage]) = await Task.detached(priority: .userInitiated) {
            let canvas = core.mapCanvas(centreLat: at.lat, centreLon: at.lon, zoom: z, width: w, height: h, scale: pixels)
            let frames = canvas.frames()
            var shown = canvas.latestFrame()
            if let t = keepTime, let i = frames.indices.min(by: { abs(frames[$0].time - t) < abs(frames[$1].time - t) }) {
                shown = UInt32(i)
            }
            let d = Drawn(canvas: canvas, base: canvas.baseImage(dark: dark).cgImage(), lines: canvas.lines(),
                          labels: canvas.labels(pinLat: pin.0, pinLon: pin.1), frames: frames, zoom: z)
            var images: [UInt32: CGImage] = [:]
            for i in frames.indices.map({ UInt32($0) }) {
                if let img = canvas.radarImage(frame: i, dark: dark).cgImage() {
                    images[i] = img
                }
            }
            return (d, shown, images)
        }.value
        guard gen == generation, !Task.isCancelled else { return }
        drawn = next
        radar = images
        frame = shown
        followLatest = shown == next.canvas.latestFrame()
        // A zoom or move made while this was drawing (e.g. before the first map came).
        if next.zoom != zoom || (centre != nil && centre != at) {
            await redrawIfNeeded()
        }
    }

    // MARK: Timeline and legend

    private var timeline: some View {
        HStack(spacing: 12) {
            Button { playing.toggle() } label: {
                Image(systemName: playing ? "pause.fill" : "play.fill").frame(width: 24)
            }
            .accessibilityLabel(playing ? "Pause" : "Play")
            Slider(value: Binding(get: { Double(frame) }, set: { v in
                frame = UInt32(max(v.rounded(), 0))
                playing = false
                followLatest = frame == drawn?.canvas.latestFrame()
            }), in: 0...Double(max((drawn?.frames.count ?? 1) - 1, 1)), step: 1)
            .accessibilityLabel("Time")
            Button("Now") {
                playing = false
                frame = drawn?.canvas.latestFrame() ?? 0
                followLatest = true
            }
        }
        .disabled(drawn?.frames.isEmpty ?? true)
    }

    private var legend: some View {
        HStack(spacing: 4) {
            ForEach(legendSteps(), id: \.dbz) { step in
                RoundedRectangle(cornerRadius: 2)
                    .fill(Self.color(radarColor(dbz: step.dbz, dark: colorScheme == .dark)))
                    .frame(width: 18, height: 10)
                if !step.label.isEmpty {
                    Text(step.label).font(.caption2).foregroundStyle(.secondary)
                }
            }
        }
        .accessibilityHidden(true)
    }

    /// A colour from the core's 0xRRGGBBAA.
    static func color(_ rgba: UInt32) -> Color {
        let r: Double = Double((rgba >> 24) & 0xff) / 255
        let g: Double = Double((rgba >> 16) & 0xff) / 255
        let b: Double = Double((rgba >> 8) & 0xff) / 255
        let a: Double = Double(rgba & 0xff) / 255
        return Color(.sRGB, red: r, green: g, blue: b, opacity: a)
    }
}

extension MapImage {
    /// As a CGImage (premultiplied RGBA, as the core makes it).
    func cgImage() -> CGImage? {
        guard width > 0, height > 0, let provider = CGDataProvider(data: rgba as CFData),
              let space = CGColorSpace(name: CGColorSpace.sRGB) else { return nil }
        return CGImage(width: Int(width), height: Int(height), bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: Int(width) * 4,
                       space: space, bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
                       provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }
}
