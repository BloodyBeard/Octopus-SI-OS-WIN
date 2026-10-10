import CoreGraphics
import Foundation

struct RelativeCursorMotionController {
    struct Settings {
        var smoothing: Double
        var sensitivity: Double
        var deadZonePixels: Double
        var stabilizationRadiusPixels: Double
        var maxStepPixels: Double
        var speedScale: Double = 1
    }

    private var lastControlPoint: CGPoint?
    private var lastUpdateTime: TimeInterval?
    private var positionFilter = OneEuroPointFilter()
    private var rawCursor: CGPoint?

    mutating func reset() {
        lastControlPoint = nil
        lastUpdateTime = nil
        positionFilter.reset()
        rawCursor = nil
    }

    mutating func update(
        controlPoint: CGPoint,
        fallbackCursor: CGPoint,
        previousCursor: CGPoint?,
        desktop: CGRect,
        now: TimeInterval,
        freeze: Bool,
        settings: Settings
    ) -> CGPoint {
        guard !desktop.isEmpty else { return fallbackCursor }

        guard let previousCursor else {
            lastControlPoint = controlPoint
            lastUpdateTime = now
            positionFilter.reset()
            positionFilter.prime(fallbackCursor)
            rawCursor = fallbackCursor
            return fallbackCursor.clamped(to: desktop)
        }

        let dt = min(max(now - (lastUpdateTime ?? now), 1.0 / 120.0), 1.0 / 15.0)
        lastUpdateTime = now

        guard let previousControlPoint = lastControlPoint else {
            lastControlPoint = controlPoint
            positionFilter.prime(previousCursor)
            rawCursor = previousCursor
            return previousCursor.clamped(to: desktop)
        }

        if freeze {
            lastControlPoint = controlPoint
            positionFilter.prime(previousCursor)
            rawCursor = previousCursor
            return previousCursor.clamped(to: desktop)
        }

        let controlDelta = controlPoint - previousControlPoint
        lastControlPoint = controlPoint

        let screenDelta = CGPoint(
            x: controlDelta.x * desktop.width,
            y: controlDelta.y * desktop.height
        )

        let handSpeed = controlDelta.length / CGFloat(dt)
        let baseGain = CGFloat(0.55 + min(max(settings.sensitivity, 0.20), 1.25) * 0.95)
        let accel = 1 + 2.4 * smoothstep(1.2, 4.0, handSpeed)
        let speedScale = CGFloat(min(max(settings.speedScale, 0.05), 1))
        var step = screenDelta * (baseGain * accel * speedScale)

        let noiseFloor = max(CGFloat(settings.deadZonePixels) * 0.05, 0.35)
        if step.length < noiseFloor {
            step = .zero
        }

        let frameScale = min(max(CGFloat(dt / (1.0 / 60.0)), 0.75), 2.0)
        let maxStep = CGFloat(max(settings.maxStepPixels, 120)) * frameScale
        if step.length > maxStep {
            step = step.normalized() * maxStep
        }

        let raw = (rawCursor ?? previousCursor) + step
        rawCursor = raw.clamped(to: desktop)

        let tuning = CursorFilterTuning.screenSpace(smoothing: settings.smoothing, sensitivity: settings.sensitivity)
        positionFilter.configure(minCutoff: tuning.minCutoff, beta: tuning.beta)
        return positionFilter.filter(rawCursor ?? raw, dt: dt).clamped(to: desktop)
    }

    private func smoothstep(_ edge0: CGFloat, _ edge1: CGFloat, _ value: CGFloat) -> CGFloat {
        guard edge1 > edge0 else { return value >= edge1 ? 1 : 0 }
        let t = min(max((value - edge0) / (edge1 - edge0), 0), 1)
        return t * t * (3 - 2 * t)
    }
}
