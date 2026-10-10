import CoreGraphics
import Foundation

struct LearnedMotionController {
    struct Settings {
        var smoothing: Double
        var sensitivity: Double
        var deadZonePixels: Double
        var maxStepPixels: Double
    }

    private var lastHand: CGPoint?
    private var cursor: CGPoint?
    private var filteredStep: CGPoint = .zero
    private var lastUpdateTime: TimeInterval?
    private var microMotionFilter = CoherentMicroMotionFilter()

    mutating func reset() {
        lastHand = nil
        cursor = nil
        filteredStep = .zero
        lastUpdateTime = nil
        microMotionFilter.reset()
    }

    mutating func update(
        hand: CGPoint,
        profile: MotionTrainingProfile,
        previousCursor: CGPoint?,
        desktop: CGRect,
        now: TimeInterval,
        freeze: Bool,
        settings: Settings
    ) -> CGPoint {
        guard !desktop.isEmpty else { return .zero }

        let anchor = map(profile.normalizedPoint(for: hand), to: desktop)
        let current = cursor ?? previousCursor ?? anchor

        guard let previousHand = lastHand else {
            lastHand = hand
            cursor = current.clamped(to: desktop)
            lastUpdateTime = now
            return cursor ?? anchor
        }

        let dt = min(max(now - (lastUpdateTime ?? now), 1.0 / 120.0), 1.0 / 15.0)
        lastUpdateTime = now

        if freeze {
            lastHand = hand
            filteredStep = .zero
            microMotionFilter.reset()
            cursor = current.clamped(to: desktop)
            return cursor ?? anchor
        }

        let handDelta = hand - previousHand
        lastHand = hand

        let targetDelta = profile.predictedTargetDelta(forHandDelta: handDelta, fromHand: previousHand)
        let sensitivity = CGFloat(min(max(settings.sensitivity, 0.20), 1.25))
        let sensitivityScale = CGFloat(0.78) + sensitivity * CGFloat(0.44)
        let learnedStep = CGPoint(
            x: targetDelta.x * desktop.width,
            y: targetDelta.y * desktop.height
        ) * sensitivityScale

        let handStationaryGate = max(
            min(CGFloat(profile.averageHandStep) * 0.16, 0.0014),
            0.00028
        )
        let stationaryPixelGate = max(CGFloat(settings.deadZonePixels) * 0.65, 2.5)
        if handDelta.length < handStationaryGate, learnedStep.length < stationaryPixelGate {
            let lowGate = max(stationaryPixelGate * 0.14, 0.55)
            if let coherentStep = microMotionFilter.update(
                step: learnedStep,
                lowGate: lowGate,
                noiseGate: stationaryPixelGate * 2.8,
                releaseFraction: 0.50
            ) {
                let next = (current + coherentStep).clamped(to: desktop)
                cursor = next
                return next
            }

            filteredStep = .zero
            cursor = current.clamped(to: desktop)
            return cursor ?? anchor
        }
        microMotionFilter.reset()

        let predictedCursor = current + learnedStep
        let anchorAlpha = CGFloat(min(max(0.04 + (1 - profile.motionQuality) * 0.12, 0.04), 0.18))
        let correctedCursor = predictedCursor + (anchor - predictedCursor) * anchorAlpha
        let targetStep = correctedCursor - current

        guard targetStep.length >= stationaryPixelGate * 0.22 else {
            filteredStep = .zero
            cursor = current.clamped(to: desktop)
            return cursor ?? anchor
        }

        let smoothness = CGFloat(min(max(settings.smoothing, 0.05), 0.92))
        let speedRatio = min(max((targetStep.length - stationaryPixelGate) / max(stationaryPixelGate * 10, 1), 0), 1)
        let alpha = min(max(0.82 + speedRatio * 0.12 - smoothness * 0.05, 0.76), 0.96)
        filteredStep = filteredStep * (1 - alpha) + targetStep * alpha

        let frameScale = min(max(CGFloat(dt / (1.0 / 60.0)), 0.75), 2.0)
        let maxStep = CGFloat(max(settings.maxStepPixels, 160)) * frameScale
        let cappedStep = filteredStep.length > maxStep
            ? filteredStep.normalized() * maxStep
            : filteredStep

        let next = (current + cappedStep).clamped(to: desktop)
        if next == current {
            filteredStep = .zero
        }
        cursor = next
        return next
    }

    private func map(_ normalized: CGPoint, to rect: CGRect) -> CGPoint {
        CGPoint(
            x: rect.minX + rect.width * normalized.x,
            y: rect.minY + rect.height * normalized.y
        )
        .clamped(to: rect)
    }
}
