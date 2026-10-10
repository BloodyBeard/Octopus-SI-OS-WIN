import CoreGraphics
import Foundation

struct CalibratedCursorFilter {
    struct Settings {
        var smoothing: Double
        var sensitivity: Double
        var deadZonePixels: Double
        var stabilizationRadiusPixels: Double
        var noiseRadiusPixels: Double
        var maxStepPixels: Double
    }

    private var filter = OneEuroPointFilter()
    private var output: CGPoint?
    private var lastUpdateTime: TimeInterval?

    mutating func reset() {
        filter.reset()
        output = nil
        lastUpdateTime = nil
    }

    mutating func update(raw: CGPoint, now: TimeInterval, freeze: Bool, settings: Settings) -> CGPoint {
        guard let previous = output else {
            output = raw
            lastUpdateTime = now
            filter.reset()
            filter.prime(raw)
            return raw
        }

        if freeze {
            lastUpdateTime = now
            filter.prime(previous)
            return previous
        }

        let dt = min(max(now - (lastUpdateTime ?? now), 1.0 / 120.0), 1.0 / 15.0)
        lastUpdateTime = now

        let tuning = CursorFilterTuning.screenSpace(smoothing: settings.smoothing, sensitivity: settings.sensitivity)
        filter.configure(minCutoff: tuning.minCutoff, beta: tuning.beta)
        var smoothed = filter.filter(raw, dt: dt)

        let holdThreshold = max(0.4, CGFloat(settings.noiseRadiusPixels) * 0.02)
        if smoothed.distance(to: previous) < holdThreshold {
            return previous
        }

        let delta = smoothed - previous
        let maxStep = CGFloat(max(settings.maxStepPixels, 24))
        if delta.length > maxStep {
            smoothed = previous + delta.normalized() * maxStep
        }

        output = smoothed
        return smoothed
    }
}
