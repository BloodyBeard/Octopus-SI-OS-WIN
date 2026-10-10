import CoreGraphics
import Foundation

struct HandPoseMeasurement: Equatable {
    var controlPoint: CGPoint
    var palmWidth: CGFloat
    var palmLength: CGFloat
    var palmAngle: CGFloat
}

struct HandPoseStabilizer {
    private var lastRawPoint: CGPoint?
    private var lastUpdateTime: TimeInterval?
    private var jumpCandidate: CGPoint?
    private var jumpCandidateCount = 0
    private var filter = OneEuroPointFilter(minCutoff: 2.2, beta: 1.2)
    private var output: CGPoint?

    mutating func reset() {
        lastRawPoint = nil
        lastUpdateTime = nil
        jumpCandidate = nil
        jumpCandidateCount = 0
        filter.reset()
        output = nil
    }

    mutating func update(measurement: HandPoseMeasurement, now: TimeInterval, freeze: Bool) -> CGPoint {
        let raw = measurement.controlPoint

        guard let previous = output, let lastRaw = lastRawPoint else {
            output = raw
            lastRawPoint = raw
            lastUpdateTime = now
            filter.reset()
            filter.prime(raw)
            return raw
        }

        let dt = min(max(now - (lastUpdateTime ?? now), 1.0 / 120.0), 1.0 / 15.0)
        lastUpdateTime = now
        let rawDelta = raw - lastRaw

        if freeze {
            lastRawPoint = raw
            return previous
        }

        let rawJumpThreshold = max(CGFloat(dt) * 2.8, 0.14)
        let outputJumpThreshold: CGFloat = 0.11
        let distanceFromOutput = raw.distance(to: previous)
        if let candidate = jumpCandidate,
           candidate.distance(to: raw) < 0.045,
           distanceFromOutput > outputJumpThreshold {
            jumpCandidateCount += 1
            if jumpCandidateCount < 4 {
                lastRawPoint = raw
                return previous
            }
            jumpCandidate = nil
            jumpCandidateCount = 0
            filter.prime(raw)
            output = raw
            lastRawPoint = raw
            return raw
        } else if rawDelta.length > rawJumpThreshold, distanceFromOutput > outputJumpThreshold {
            jumpCandidate = raw
            jumpCandidateCount = 1
            lastRawPoint = raw
            return previous
        } else {
            jumpCandidate = nil
            jumpCandidateCount = 0
        }

        lastRawPoint = raw
        let smoothed = filter.filter(raw, dt: dt)
        output = smoothed
        return smoothed
    }
}
