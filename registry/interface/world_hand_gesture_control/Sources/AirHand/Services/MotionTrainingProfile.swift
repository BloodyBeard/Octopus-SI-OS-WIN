import CoreGraphics
import Foundation

enum MotionTrainingError: Error, CustomStringConvertible {
    case notEnoughExamples
    case notEnoughSamples(String)
    case movementTooSmall(String)

    var description: String {
        switch self {
        case .notEnoughExamples:
            return "training needs more examples"
        case .notEnoughSamples(let label):
            return "\(label) recording is too short"
        case .movementTooSmall(let label):
            return "\(label) movement is too small"
        }
    }
}

struct MotionTrainingExample: Codable, Equatable {
    var targetPath: [CGPoint]
    var handPath: [CGPoint]
}

private struct TrainingPoint: Codable, Equatable {
    var handX: Double
    var handY: Double
    var targetX: Double
    var targetY: Double

    init(hand: CGPoint, target: CGPoint) {
        handX = Double(hand.x)
        handY = Double(hand.y)
        targetX = Double(target.x)
        targetY = Double(target.y)
    }

    var hand: CGPoint {
        CGPoint(x: handX, y: handY)
    }

    var target: CGPoint {
        CGPoint(x: targetX, y: targetY)
    }
}

private struct TrainingSegment: Codable, Equatable {
    var handX: Double
    var handY: Double
    var handDX: Double
    var handDY: Double
    var targetDX: Double
    var targetDY: Double

    init(hand: CGPoint, handDelta: CGPoint, targetDelta: CGPoint) {
        handX = Double(hand.x)
        handY = Double(hand.y)
        handDX = Double(handDelta.x)
        handDY = Double(handDelta.y)
        targetDX = Double(targetDelta.x)
        targetDY = Double(targetDelta.y)
    }

    var hand: CGPoint {
        CGPoint(x: handX, y: handY)
    }

    var handDelta: CGPoint {
        CGPoint(x: handDX, y: handDY)
    }

    var targetDelta: CGPoint {
        CGPoint(x: targetDX, y: targetDY)
    }
}

private struct VelocityFit {
    var xx: Double
    var xy: Double
    var yx: Double
    var yy: Double
    var quality: Double
    var averageHandStep: Double
    var averageTargetStep: Double
}

struct MotionTrainingProfile: Codable, Equatable {
    private var points: [TrainingPoint]
    private var segments: [TrainingSegment]
    var exampleCount: Int
    var createdAt: TimeInterval
    var velocityXX: Double
    var velocityXY: Double
    var velocityYX: Double
    var velocityYY: Double
    var motionQuality: Double
    var averageHandStep: Double
    var averageTargetStep: Double

    fileprivate init(
        points: [(hand: CGPoint, target: CGPoint)],
        segments: [(hand: CGPoint, handDelta: CGPoint, targetDelta: CGPoint)],
        velocityFit: VelocityFit,
        exampleCount: Int,
        createdAt: TimeInterval
    ) {
        self.points = points.map { TrainingPoint(hand: $0.hand, target: $0.target) }
        self.segments = segments.map {
            TrainingSegment(hand: $0.hand, handDelta: $0.handDelta, targetDelta: $0.targetDelta)
        }
        self.exampleCount = exampleCount
        self.createdAt = createdAt
        velocityXX = velocityFit.xx
        velocityXY = velocityFit.xy
        velocityYX = velocityFit.yx
        velocityYY = velocityFit.yy
        motionQuality = velocityFit.quality
        averageHandStep = velocityFit.averageHandStep
        averageTargetStep = velocityFit.averageTargetStep
    }

    init?(encodedString: String) {
        guard !encodedString.isEmpty,
              let data = encodedString.data(using: .utf8),
              let decoded = try? JSONDecoder().decode(MotionTrainingProfile.self, from: data)
        else {
            return nil
        }
        self = decoded
    }

    func encodedString() throws -> String {
        let data = try JSONEncoder().encode(self)
        guard let string = String(data: data, encoding: .utf8) else {
            throw MotionTrainingError.notEnoughExamples
        }
        return string
    }

    func normalizedPoint(for hand: CGPoint) -> CGPoint {
        guard !points.isEmpty else { return .zero }

        let nearest = points
            .map { point -> (point: TrainingPoint, distance: CGFloat) in
                (point, max(hand.distance(to: point.hand), 0.000_001))
            }
            .sorted { $0.distance < $1.distance }
            .prefix(7)

        var weighted = CGPoint.zero
        var totalWeight = CGFloat(0)
        for item in nearest {
            let weight = 1 / (item.distance * item.distance + 0.000_01)
            weighted = weighted + item.point.target * weight
            totalWeight += weight
        }

        guard totalWeight > 0 else { return points[0].target }
        return (weighted / totalWeight).clampedToUnit()
    }

    func predictedTargetDelta(forHandDelta handDelta: CGPoint, fromHand hand: CGPoint? = nil) -> CGPoint {
        guard handDelta.length > 0.000_000_1 else { return .zero }

        let global = CGPoint(
            x: handDelta.x * CGFloat(velocityXX) + handDelta.y * CGFloat(velocityXY),
            y: handDelta.x * CGFloat(velocityYX) + handDelta.y * CGFloat(velocityYY)
        )

        guard let hand, !segments.isEmpty else {
            return global
        }

        let movementDirection = handDelta.normalized()
        let candidates = segments
            .compactMap { segment -> (prediction: CGPoint, weight: CGFloat)? in
                let segmentHandDelta = segment.handDelta
                let segmentDirection = segmentHandDelta.normalized()
                let alignment = max(min(movementDirection.dot(segmentDirection), 1), -1)
                guard alignment > 0.05 else { return nil }

                let denominator = max(segmentHandDelta.dot(segmentHandDelta), 0.000_000_000_1)
                let scale = handDelta.dot(segmentHandDelta) / denominator
                guard scale > 0 else { return nil }

                let positionDistance = hand.distance(to: segment.hand)
                let directionPenalty = CGFloat(1 - alignment) * 0.11
                let weight = 1 / (positionDistance * positionDistance + directionPenalty * directionPenalty + 0.000_002)
                return (segment.targetDelta * scale, weight)
            }
            .sorted { $0.weight > $1.weight }
            .prefix(12)

        var weighted = CGPoint.zero
        var totalWeight = CGFloat(0)
        for candidate in candidates {
            weighted = weighted + candidate.prediction * candidate.weight
            totalWeight += candidate.weight
        }

        guard totalWeight > 0 else {
            return global
        }

        let local = weighted / totalWeight
        let localTrust = CGFloat(min(max(motionQuality, 0.30), 0.88))
        return local * localTrust + global * (1 - localTrust)
    }
}

struct MotionTrainingProfileBuilder {
    var minimumExamples = 3
    var minimumSamples = 18
    var resampledPointCount = 72
    var minimumPathLength: CGFloat = 0.08

    func build(examples: [MotionTrainingExample], createdAt: TimeInterval = Date().timeIntervalSince1970) throws -> MotionTrainingProfile {
        guard examples.count >= minimumExamples else {
            throw MotionTrainingError.notEnoughExamples
        }

        var pairs: [(hand: CGPoint, target: CGPoint)] = []
        var segments: [(hand: CGPoint, handDelta: CGPoint, targetDelta: CGPoint)] = []
        for (index, example) in examples.enumerated() {
            let target = try resamplePath(example.targetPath, label: "target \(index)")
            let hand = try resamplePath(example.handPath, label: "hand \(index)")
            let count = min(target.count, hand.count)
            for pointIndex in 0..<count {
                pairs.append((hand: hand[pointIndex], target: target[pointIndex].clampedToUnit()))
            }

            guard count > 1 else { continue }
            for pointIndex in 0..<(count - 1) {
                let handStart = hand[pointIndex]
                let handEnd = hand[pointIndex + 1]
                let targetStart = target[pointIndex].clampedToUnit()
                let targetEnd = target[pointIndex + 1].clampedToUnit()
                let handDelta = handEnd - handStart
                let targetDelta = targetEnd - targetStart
                guard handDelta.length > 0.000_01, targetDelta.length > 0.000_01 else {
                    continue
                }

                segments.append((hand: handStart, handDelta: handDelta, targetDelta: targetDelta))
                segments.append((hand: handEnd, handDelta: handDelta * -1, targetDelta: targetDelta * -1))
            }
        }

        guard !segments.isEmpty else {
            throw MotionTrainingError.movementTooSmall("training velocity")
        }

        return MotionTrainingProfile(
            points: pairs,
            segments: segments,
            velocityFit: fitVelocity(segments: segments),
            exampleCount: examples.count,
            createdAt: createdAt
        )
    }

    private func resamplePath(_ path: [CGPoint], label: String) throws -> [CGPoint] {
        guard path.count >= minimumSamples else {
            throw MotionTrainingError.notEnoughSamples(label)
        }

        let length = pathLength(path)
        guard length >= minimumPathLength else {
            throw MotionTrainingError.movementTooSmall(label)
        }

        return resampleByArcLength(path, count: resampledPointCount)
    }

    private func pathLength(_ path: [CGPoint]) -> CGFloat {
        guard path.count > 1 else { return 0 }
        return (1..<path.count).reduce(CGFloat(0)) { total, index in
            total + path[index].distance(to: path[index - 1])
        }
    }

    private func resampleByArcLength(_ samples: [CGPoint], count: Int) -> [CGPoint] {
        guard let first = samples.first, samples.count > 1, count > 1 else { return samples }

        var distances = Array(repeating: CGFloat(0), count: samples.count)
        for index in 1..<samples.count {
            distances[index] = distances[index - 1] + samples[index].distance(to: samples[index - 1])
        }

        guard let total = distances.last, total > 0.000_001 else {
            return Array(repeating: first, count: count)
        }

        return (0..<count).map { outputIndex in
            let target = total * CGFloat(outputIndex) / CGFloat(count - 1)
            var upper = 1
            while upper < distances.count - 1, distances[upper] < target {
                upper += 1
            }

            let lower = max(upper - 1, 0)
            let span = max(distances[upper] - distances[lower], 0.000_001)
            let fraction = (target - distances[lower]) / span
            return samples[lower] + (samples[upper] - samples[lower]) * fraction
        }
    }

    private func fitVelocity(segments: [(hand: CGPoint, handDelta: CGPoint, targetDelta: CGPoint)]) -> VelocityFit {
        var sumXX = 0.0
        var sumXY = 0.0
        var sumYY = 0.0
        var rhsTargetXFromHandX = 0.0
        var rhsTargetXFromHandY = 0.0
        var rhsTargetYFromHandX = 0.0
        var rhsTargetYFromHandY = 0.0
        var totalHandStep = 0.0
        var totalTargetStep = 0.0

        for segment in segments {
            let hx = Double(segment.handDelta.x)
            let hy = Double(segment.handDelta.y)
            let tx = Double(segment.targetDelta.x)
            let ty = Double(segment.targetDelta.y)

            sumXX += hx * hx
            sumXY += hx * hy
            sumYY += hy * hy
            rhsTargetXFromHandX += hx * tx
            rhsTargetXFromHandY += hy * tx
            rhsTargetYFromHandX += hx * ty
            rhsTargetYFromHandY += hy * ty
            totalHandStep += hypot(hx, hy)
            totalTargetStep += hypot(tx, ty)
        }

        let regularization = max((sumXX + sumYY) * 0.000_1, 0.000_000_001)
        let a = sumXX + regularization
        let b = sumXY
        let d = sumYY + regularization
        let determinant = max(a * d - b * b, 0.000_000_000_001)

        func solve(rhsX: Double, rhsY: Double) -> (Double, Double) {
            (
                (rhsX * d - b * rhsY) / determinant,
                (a * rhsY - b * rhsX) / determinant
            )
        }

        let targetXFit = solve(rhsX: rhsTargetXFromHandX, rhsY: rhsTargetXFromHandY)
        let targetYFit = solve(rhsX: rhsTargetYFromHandX, rhsY: rhsTargetYFromHandY)
        let count = max(Double(segments.count), 1)
        let averageTargetStep = totalTargetStep / count
        let averageHandStep = totalHandStep / count

        var squaredError = 0.0
        for segment in segments {
            let hx = Double(segment.handDelta.x)
            let hy = Double(segment.handDelta.y)
            let predictedX = hx * targetXFit.0 + hy * targetXFit.1
            let predictedY = hx * targetYFit.0 + hy * targetYFit.1
            let errorX = predictedX - Double(segment.targetDelta.x)
            let errorY = predictedY - Double(segment.targetDelta.y)
            squaredError += errorX * errorX + errorY * errorY
        }

        let rmse = sqrt(squaredError / count)
        let quality = min(max(1 - rmse / max(averageTargetStep, 0.000_000_001), 0), 1)

        return VelocityFit(
            xx: targetXFit.0,
            xy: targetXFit.1,
            yx: targetYFit.0,
            yy: targetYFit.1,
            quality: quality,
            averageHandStep: averageHandStep,
            averageTargetStep: averageTargetStep
        )
    }
}

private extension CGPoint {
    func clampedToUnit() -> CGPoint {
        CGPoint(x: min(max(x, 0), 1), y: min(max(y, 0), 1))
    }
}
