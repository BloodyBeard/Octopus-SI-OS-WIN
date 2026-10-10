import AppKit
import QuartzCore

final class HudOverlayController {
    private var panels: [NSPanel] = []
    private var views: [HudOverlayView] = []
    private var panelsInvalid = false
    private var visible = false

    init() {
        NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            self?.panelsInvalid = true
        }
    }

    func update(snapshot: EngineSnapshot?, enabled: Bool) {
        guard enabled, let snapshot, snapshot.mode != .stopped, snapshot.mode != .unavailable else {
            hide()
            return
        }
        ensurePanels()
        if !visible {
            panels.forEach { $0.orderFrontRegardless() }
            visible = true
        }
        views.forEach { $0.apply(snapshot: snapshot) }
    }

    func flash(at point: CGPoint) {
        views.forEach { $0.addRipple(at: point) }
    }

    private func hide() {
        guard visible else { return }
        panels.forEach { $0.orderOut(nil) }
        views.forEach { $0.clearTransient() }
        visible = false
    }

    private func ensurePanels() {
        if panelsInvalid {
            panels.forEach { $0.orderOut(nil) }
            panels = []
            views = []
            panelsInvalid = false
            visible = false
        }
        guard panels.isEmpty else { return }

        for screen in NSScreen.screens {
            let panel = NSPanel(
                contentRect: screen.frame,
                styleMask: [.borderless, .nonactivatingPanel],
                backing: .buffered,
                defer: false
            )
            panel.isOpaque = false
            panel.backgroundColor = .clear
            panel.hasShadow = false
            panel.level = .screenSaver
            panel.ignoresMouseEvents = true
            panel.isReleasedWhenClosed = false
            panel.hidesOnDeactivate = false
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
            let view = HudOverlayView(frame: NSRect(origin: .zero, size: screen.frame.size))
            panel.contentView = view
            panels.append(panel)
            views.append(view)
        }
    }
}

final class HudOverlayView: NSView {
    private struct Ripple {
        var center: CGPoint
        var start: CFTimeInterval
    }

    private var snapshot: EngineSnapshot?
    private var trail: [(point: CGPoint, time: CFTimeInterval)] = []
    private var ripples: [Ripple] = []
    private var lastDirtyRect: NSRect = .zero
    private var lastActiveCursor: CGPoint?
    private var lastActiveTime: CFTimeInterval = 0
    private let coastDuration: CFTimeInterval = 1.2

    func apply(snapshot: EngineSnapshot) {
        self.snapshot = snapshot
        let now = CACurrentMediaTime()
        if let cursor = snapshot.cursorPoint, snapshot.gesture != .none {
            if trail.last.map({ $0.point.distance(to: cursor) > 2.5 }) ?? true {
                trail.append((cursor, now))
            }
            lastActiveCursor = cursor
            lastActiveTime = now
        }
        trail.removeAll { now - $0.time > 0.4 }
        ripples.removeAll { now - $0.start > 0.4 }
        invalidate()
    }

    func addRipple(at point: CGPoint) {
        ripples.append(Ripple(center: point, start: CACurrentMediaTime()))
        invalidate()
    }

    func clearTransient() {
        snapshot = nil
        trail = []
        ripples = []
        needsDisplay = true
    }


    override func draw(_ dirtyRect: NSRect) {
        guard let snapshot else { return }
        let now = CACurrentMediaTime()

        drawTrail(now: now, color: ringColor(for: snapshot))
        drawMagnetTarget(snapshot)

        if let cursor = snapshot.cursorPoint, snapshot.gesture != .none, let local = local(cursor) {
            drawRing(at: local, snapshot: snapshot)
            if snapshot.mode == .scrolling, let offset = snapshot.scrollOffset {
                drawScrollGauge(at: local, offset: offset)
            }
            if snapshot.gesture == .missionControl {
                drawSpaceChevrons(at: local)
            }
        } else if let coastCursor = lastActiveCursor,
                  now - lastActiveTime < coastDuration,
                  let local = local(coastCursor) {
            drawCoastRing(at: local, age: now - lastActiveTime)
        }

        drawRipples(now: now)
    }

    private func ringColor(for snapshot: EngineSnapshot) -> NSColor {
        switch snapshot.gesture {
        case .aim:
            return .systemTeal
        case .leftClick:
            return snapshot.isDragging ? .systemYellow : .systemOrange
        case .fist:
            return .systemGreen
        case .missionControl:
            return .systemPurple
        case .openPalm:
            return .systemGray
        default:
            return .systemTeal
        }
    }

    private func drawRing(at point: CGPoint, snapshot: EngineSnapshot) {
        let color = ringColor(for: snapshot)
        let throttle = CGFloat(snapshot.speedFactor ?? 1)
        let radius: CGFloat = snapshot.gesture == .leftClick ? 15 : 13 + throttle * 6

        let glow = NSBezierPath(ovalIn: rect(center: point, radius: radius))
        glow.lineWidth = 7
        color.withAlphaComponent(0.18).setStroke()
        glow.stroke()

        let ring = NSBezierPath(ovalIn: rect(center: point, radius: radius))
        ring.lineWidth = 2.2
        color.withAlphaComponent(0.9).setStroke()
        ring.stroke()

        let dotRadius: CGFloat = snapshot.gesture == .leftClick ? 5 : 3
        let dot = NSBezierPath(ovalIn: rect(center: point, radius: dotRadius))
        color.withAlphaComponent(snapshot.gesture == .leftClick ? 0.95 : 0.7).setFill()
        dot.fill()

        if snapshot.gesture == .aim, let factor = snapshot.speedFactor, factor < 0.97 {
            let arc = NSBezierPath()
            let sweep = CGFloat(factor) * 360
            arc.appendArc(
                withCenter: point,
                radius: radius + 5,
                startAngle: 90,
                endAngle: 90 - sweep,
                clockwise: true
            )
            arc.lineWidth = 2
            arc.lineCapStyle = .round
            NSColor.systemYellow.withAlphaComponent(0.85).setStroke()
            arc.stroke()
        }
    }

    private func drawCoastRing(at point: CGPoint, age: CFTimeInterval) {
        let fade = CGFloat(max(0, 1 - age / coastDuration))
        let color = NSColor.systemGray

        let glow = NSBezierPath(ovalIn: rect(center: point, radius: 19))
        glow.lineWidth = 7
        color.withAlphaComponent(0.12 * fade).setStroke()
        glow.stroke()

        let ring = NSBezierPath(ovalIn: rect(center: point, radius: 19))
        ring.lineWidth = 2.2
        color.withAlphaComponent(0.6 * fade).setStroke()
        ring.stroke()

        let dot = NSBezierPath(ovalIn: rect(center: point, radius: 3))
        color.withAlphaComponent(0.5 * fade).setFill()
        dot.fill()
    }

    private func drawTrail(now: CFTimeInterval, color: NSColor) {
        guard trail.count >= 2 else { return }
        for index in 1..<trail.count {
            guard let a = local(trail[index - 1].point), let b = local(trail[index].point) else { continue }
            let age = now - trail[index].time
            let alpha = max(0, 1 - age / 0.4) * 0.3
            let path = NSBezierPath()
            path.move(to: a)
            path.line(to: b)
            path.lineWidth = 2.5
            path.lineCapStyle = .round
            color.withAlphaComponent(alpha).setStroke()
            path.stroke()
        }
    }

    private func drawMagnetTarget(_ snapshot: EngineSnapshot) {
        guard let target = snapshot.magnetTarget, let rect = localRect(target) else { return }
        let path = NSBezierPath(roundedRect: rect.insetBy(dx: -3, dy: -3), xRadius: 7, yRadius: 7)
        NSColor.systemTeal.withAlphaComponent(0.08).setFill()
        path.fill()
        path.lineWidth = 1.6
        NSColor.systemTeal.withAlphaComponent(0.65).setStroke()
        path.stroke()
    }

    private func drawScrollGauge(at point: CGPoint, offset: CGFloat) {
        let gaugeHeight: CGFloat = 88
        let gaugeRect = NSRect(x: point.x + 32, y: point.y - gaugeHeight / 2, width: 9, height: gaugeHeight)
        let track = NSBezierPath(roundedRect: gaugeRect, xRadius: 4.5, yRadius: 4.5)
        NSColor.systemGreen.withAlphaComponent(0.18).setFill()
        track.fill()
        track.lineWidth = 1
        NSColor.systemGreen.withAlphaComponent(0.45).setStroke()
        track.stroke()

        let notch = NSBezierPath()
        notch.move(to: CGPoint(x: gaugeRect.minX - 2, y: point.y))
        notch.line(to: CGPoint(x: gaugeRect.maxX + 2, y: point.y))
        notch.lineWidth = 1
        NSColor.systemGreen.withAlphaComponent(0.5).setStroke()
        notch.stroke()

        let dotY = point.y + offset * (gaugeHeight / 2 - 8)
        let dot = NSBezierPath(ovalIn: rect(center: CGPoint(x: gaugeRect.midX, y: dotY), radius: 5.5))
        NSColor.systemGreen.setFill()
        dot.fill()
    }

    private func drawSpaceChevrons(at point: CGPoint) {
        let color = NSColor.systemPurple.withAlphaComponent(0.8)
        for direction: CGFloat in [-1, 1] {
            let baseX = point.x + direction * 42
            let chevron = NSBezierPath()
            chevron.move(to: CGPoint(x: baseX - direction * 7, y: point.y + 10))
            chevron.line(to: CGPoint(x: baseX + direction * 4, y: point.y))
            chevron.line(to: CGPoint(x: baseX - direction * 7, y: point.y - 10))
            chevron.lineWidth = 3
            chevron.lineCapStyle = .round
            chevron.lineJoinStyle = .round
            color.setStroke()
            chevron.stroke()
        }
    }

    private func drawRipples(now: CFTimeInterval) {
        for ripple in ripples {
            guard let center = local(ripple.center) else { continue }
            let progress = min(max((now - ripple.start) / 0.35, 0), 1)
            let radius = 8 + CGFloat(progress) * 38
            let path = NSBezierPath(ovalIn: rect(center: center, radius: radius))
            path.lineWidth = 2.5 * (1 - CGFloat(progress)) + 0.5
            NSColor.systemOrange.withAlphaComponent((1 - progress) * 0.6).setStroke()
            path.stroke()
        }
    }


    private func local(_ cgPoint: CGPoint) -> CGPoint? {
        guard let window, let primary = NSScreen.screens.first else { return nil }
        let appKitY = primary.frame.maxY - cgPoint.y
        return CGPoint(x: cgPoint.x - window.frame.minX, y: appKitY - window.frame.minY)
    }

    private func localRect(_ cgRect: CGRect) -> NSRect? {
        guard let bottomLeft = local(CGPoint(x: cgRect.minX, y: cgRect.maxY)) else { return nil }
        let rect = NSRect(origin: bottomLeft, size: cgRect.size)
        return rect.intersects(bounds.insetBy(dx: -60, dy: -60)) ? rect : nil
    }

    private func rect(center: CGPoint, radius: CGFloat) -> NSRect {
        NSRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2)
    }

    private func invalidate() {
        var dirty = NSRect.null
        if let snapshot, let cursor = snapshot.cursorPoint, let local = local(cursor) {
            dirty = dirty.union(rect(center: local, radius: 110))
        }
        if let coast = lastActiveCursor, let local = local(coast) {
            dirty = dirty.union(rect(center: local, radius: 110))
        }
        if let target = snapshot?.magnetTarget, let rect = localRect(target) {
            dirty = dirty.union(rect.insetBy(dx: -12, dy: -12))
        }
        for entry in trail {
            if let local = local(entry.point) {
                dirty = dirty.union(rect(center: local, radius: 8))
            }
        }
        for ripple in ripples {
            if let local = local(ripple.center) {
                dirty = dirty.union(rect(center: local, radius: 52))
            }
        }
        guard !dirty.isNull else { return }
        setNeedsDisplay(dirty.union(lastDirtyRect))
        lastDirtyRect = dirty
    }
}
