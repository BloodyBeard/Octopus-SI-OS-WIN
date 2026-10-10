import CoreGraphics
import Foundation

enum MotionCalibrationError: Error, CustomStringConvertible {
    case notEnoughSamples(String)
    case movementTooSmall(String)
    case invalidAxis(String)

    var description: String {
        switch self {
        case .notEnoughSamples(let axis):
            return "\(axis) pass needs more hand samples"
        case .movementTooSmall(let axis):
            return "\(axis) pass is too short"
        case .invalidAxis(let axis):
            return "\(axis) pass could not be fitted"
        }
    }
}

struct MotionCalibrationAxisProfile: Codable, Equatable {
    var originX: Double
    var originY: Double
    var unitX: Double
    var unitY: Double
    var lowerProjection: Double
    var upperProjection: Double
    var noiseRadius: Double
    var quality: Double
    var sampleCount: Int

    var range: Double {
        max(upperProjection - lowerProjection, 0.000_001)
    }

    var normalizedNoiseRadius: Double {
        min(max(noiseRadius / range, 0.004), 0.08)
    }

    func normalizedValue(for point: CGPoint) -> CGFloat {
        let projection = (Double(point.x) - originX) * unitX + (Double(point.y) - originY) * unitY
        let value = (projection - lowerProjection) / range
        return CGFloat(min(max(value, 0), 1))
    }

    func relativeValue(for point: CGPoint) -> CGFloat {
        let projection = (Double(point.x) - originX) * unitX + (Double(point.y) - originY) * unitY
        return CGFloat((projection - lowerProjection) / range)
    }
}

struct MotionCalibrationRailPoint: Codable, Equatable {
    var x: Double
    var y: Double

    init(_ point: CGPoint) {
        x = Double(point.x)
        y = Double(point.y)
    }

    var point: CGPoint {
        CGPoint(x: x, y: y)
    }
}

struct MotionCalibrationProfile: Codable, Equatable {
    var horizontal: MotionCalibrationAxisProfile
    var vertical: MotionCalibrationAxisProfile
    var createdAt: TimeInterval
    var topRail: [MotionCalibrationRailPoint]?
    var middleRail: [MotionCalibrationRailPoint]?
    var bottomRail: [MotionCalibrationRailPoint]?

    init(horizontal: MotionCalibrationAxisProfile, vertical: MotionCalibrationAxisProfile, createdAt: TimeInterval) {
        self.horizontal = horizontal
        self.vertical = vertical
        self.createdAt = createdAt
    }

    init?(encodedString: String) {
        guard !encodedString.isEmpty,
              let data = encodedString.data(using: .utf8),
              let decoded = try? JSONDecoder().decode(MotionCalibrationProfile.self, from: data)
        else {
            return nil
        }

        self = decoded
    }

    func encodedString() throws -> String {
        let data = try JSONEncoder().encode(self)
        guard let string = String(data: data, encoding: .utf8) else {
            throw MotionCalibrationError.invalidAxis("profile")
        }
        return string
    }

    func normalizedPoint(for palm: CGPoint) -> CGPoint {
        if let surfacePoint = surfacePoint(for: palm, clamped: true) {
            return surfacePoint
        }

        return CGPoint(
            x: horizontal.normalizedValue(for: palm),
            y: vertical.normalizedValue(for: palm)
        )
    }

    func relativeControlPoint(for palm: CGPoint) -> CGPoint {
        if let surfacePoint = surfacePoint(for: palm, clamped: false) {
            return surfacePoint
        }

        return CGPoint(
            x: horizontal.relativeValue(for: palm),
            y: vertical.relativeValue(for: palm)
        )
    }

    var isSurfaceProfile: Bool {
        guard let topRail, let middleRail, let bottomRail else { return false }
        return topRail.count >= 2 && middleRail.count >= 2 && bottomRail.count >= 2
    }

    var normalizedNoiseRadius: Double {
        max(horizontal.normalizedNoiseRadius, vertical.normalizedNoiseRadius)
    }

    private func surfacePoint(for palm: CGPoint, clamped: Bool) -> CGPoint? {
        guard let topRail = topRail?.map(\.point),
              let middleRail = middleRail?.map(\.point),
              let bottomRail = bottomRail?.map(\.point),
              topRail.count >= 2,
              middleRail.count >= 2,
              bottomRail.count >= 2
        else {
            return nil
        }

        let u = projectedRailParameter(for: palm, rail: middleRail)
        let top = point(on: topRail, at: u)
        let bottom = point(on: bottomRail, at: u)
        let vertical = bottom - top
        let verticalRange = max(vertical.dot(vertical), 0.000_001)
        var v = (palm - top).dot(vertical) / verticalRange
        var x = u
        if clamped {
            x = min(max(x, 0), 1)
            v = min(max(v, 0), 1)
        }
        return CGPoint(x: x, y: v)
    }

    private func projectedRailParameter(for point: CGPoint, rail: [CGPoint]) -> CGFloat {
        guard rail.count > 1 else { return 0 }

        var bestDistance = CGFloat.greatestFiniteMagnitude
        var bestParameter: CGFloat = 0
        let lastSegmentIndex = max(rail.count - 1, 1)

        for index in 0..<(rail.count - 1) {
            let start = rail[index]
            let end = rail[index + 1]
            let segment = end - start
            let segmentLength = max(segment.dot(segment), 0.000_001)
            let local = min(max((point - start).dot(segment) / segmentLength, 0), 1)
            let closest = start + segment * local
            let distance = point.distance(to: closest)
            if distance < bestDistance {
                bestDistance = distance
                bestParameter = (CGFloat(index) + local) / CGFloat(lastSegmentIndex)
            }
        }

        return min(max(bestParameter, 0), 1)
    }

    private func point(on rail: [CGPoint], at parameter: CGFloat) -> CGPoint {
        guard let first = rail.first else { return .zero }
        guard rail.count > 1 else { return first }

        let scaled = min(max(parameter, 0), 1) * CGFloat(rail.count - 1)
        let lowerIndex = Int(floor(scaled))
        let upperIndex = min(lowerIndex + 1, rail.count - 1)
        let fraction = scaled - CGFloat(lowerIndex)
        return rail[lowerIndex] + (rail[upperIndex] - rail[lowerIndex]) * fraction
    }
}

struct MotionCalibrationProfileBuilder {
    var minimumSamples = 24
    var minimumAxisRange: Double = 0.08

    func build(
        horizontalSamples: [CGPoint],
        verticalSamples: [CGPoint],
        createdAt: TimeInterval = Date().timeIntervalSince1970
    ) throws -> MotionCalibrationProfile {
        let horizontal = try buildAxis(samples: horizontalSamples, label: "horizontal")
        let vertical = try buildAxis(samples: verticalSamples, label: "vertical")
        return MotionCalibrationProfile(horizontal: horizontal, vertical: vertical, createdAt: createdAt)
    }

    func buildSurface(
        topSamples: [CGPoint],
        middleSamples: [CGPoint],
        bottomSamples: [CGPoint],
        createdAt: TimeInterval = Date().timeIntervalSince1970
    ) throws -> MotionCalibrationProfile {
        let horizontal = try buildAxis(samples: middleSamples, label: "middle")
        let verticalSamples = topSamples + bottomSamples
        let vertical = try buildAxis(samples: verticalSamples, label: "vertical surface")
        let topRail = try buildRail(samples: topSamples, label: "top")
        let middleRail = try buildRail(samples: middleSamples, label: "middle")
        let bottomRail = try buildRail(samples: bottomSamples, label: "bottom")

        var profile = MotionCalibrationProfile(horizontal: horizontal, vertical: vertical, createdAt: createdAt)
        profile.topRail = topRail.map(MotionCalibrationRailPoint.init)
        profile.middleRail = middleRail.map(MotionCalibrationRailPoint.init)
        profile.bottomRail = bottomRail.map(MotionCalibrationRailPoint.init)
        return profile
    }

    private func buildAxis(samples: [CGPoint], label: String) throws -> MotionCalibrationAxisProfile {
        guard samples.count >= minimumSamples else {
            throw MotionCalibrationError.notEnoughSamples(label)
        }

        let origin = medianPoint(samples)
        var unit = dominantUnit(samples: samples)
        let early = medianPoint(Array(samples.prefix(max(6, samples.count / 8))))
        let late = medianPoint(Array(samples.suffix(max(6, samples.count / 8))))
        let intendedDirection = late - early

        guard intendedDirection.length >= CGFloat(minimumAxisRange) else {
            throw MotionCalibrationError.movementTooSmall(label)
        }

        if unit.dot(intendedDirection) < 0 {
            unit = unit * -1
        }

        guard unit.length > 0.000_001 else {
            throw MotionCalibrationError.invalidAxis(label)
        }

        let projections = samples.map { projection($0, origin: origin, unit: unit) }
        let lower = percentile(projections, 0.04)
        let upper = percentile(projections, 0.96)
        let axisRange = upper - lower

        guard axisRange >= minimumAxisRange else {
            throw MotionCalibrationError.movementTooSmall(label)
        }

        let perpendicular = CGPoint(x: -unit.y, y: unit.x)
        let residuals = samples.map { abs(projection($0, origin: origin, unit: perpendicular)) }
        let noiseRadius = max(percentile(residuals, 0.85), axisRange * 0.008)
        let quality = axisRange / (axisRange + noiseRadius * 7)

        return MotionCalibrationAxisProfile(
            originX: Double(origin.x),
            originY: Double(origin.y),
            unitX: Double(unit.x),
            unitY: Double(unit.y),
            lowerProjection: lower,
            upperProjection: upper,
            noiseRadius: noiseRadius,
            quality: min(max(quality, 0), 1),
            sampleCount: samples.count
        )
    }

    private func dominantUnit(samples: [CGPoint]) -> CGPoint {
        let mean = samples.reduce(.zero, +) / CGFloat(samples.count)
        var xx: CGFloat = 0
        var xy: CGFloat = 0
        var yy: CGFloat = 0

        for sample in samples {
            let centered = sample - mean
            xx += centered.x * centered.x
            xy += centered.x * centered.y
            yy += centered.y * centered.y
        }

        let angle = 0.5 * atan2(2 * xy, xx - yy)
        return CGPoint(x: cos(angle), y: sin(angle)).normalized()
    }

    private func buildRail(samples: [CGPoint], label: String) throws -> [CGPoint] {
        guard samples.count >= minimumSamples else {
            throw MotionCalibrationError.notEnoughSamples(label)
        }

        let early = medianPoint(Array(samples.prefix(max(6, samples.count / 8))))
        let late = medianPoint(Array(samples.suffix(max(6, samples.count / 8))))
        guard early.distance(to: late) >= CGFloat(minimumAxisRange) else {
            throw MotionCalibrationError.movementTooSmall(label)
        }

        return resampleByArcLength(samples, count: 64)
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

    private func medianPoint(_ points: [CGPoint]) -> CGPoint {
        guard !points.isEmpty else { return .zero }
        return CGPoint(
            x: CGFloat(percentile(points.map { Double($0.x) }, 0.5)),
            y: CGFloat(percentile(points.map { Double($0.y) }, 0.5))
        )
    }

    private func projection(_ point: CGPoint, origin: CGPoint, unit: CGPoint) -> Double {
        Double((point - origin).dot(unit))
    }

    private func percentile(_ values: [Double], _ quantile: Double) -> Double {
        guard !values.isEmpty else { return 0 }
        let sorted = values.sorted()
        guard sorted.count > 1 else { return sorted[0] }

        let position = min(max(quantile, 0), 1) * Double(sorted.count - 1)
        let lowerIndex = Int(floor(position))
        let upperIndex = Int(ceil(position))
        if lowerIndex == upperIndex {
            return sorted[lowerIndex]
        }

        let fraction = position - Double(lowerIndex)
        return sorted[lowerIndex] + (sorted[upperIndex] - sorted[lowerIndex]) * fraction
    }
}
