import CoreGraphics
import Foundation

struct OneEuroScalarFilter {
    var minCutoff: Double
    var beta: Double
    var dCutoff: Double

    private var hasPrevious = false
    private var filteredValue = 0.0
    private var filteredDerivative = 0.0

    init(minCutoff: Double = 1.0, beta: Double = 0.0, dCutoff: Double = 1.0) {
        self.minCutoff = max(minCutoff, 0.000_1)
        self.beta = max(beta, 0)
        self.dCutoff = max(dCutoff, 0.000_1)
    }

    mutating func reset() {
        hasPrevious = false
        filteredValue = 0
        filteredDerivative = 0
    }

    mutating func prime(_ value: Double) {
        hasPrevious = true
        filteredValue = value
        filteredDerivative = 0
    }

    mutating func filter(_ value: Double, dt: TimeInterval) -> Double {
        let dt = max(dt, 1.0 / 1_000.0)
        guard hasPrevious else {
            hasPrevious = true
            filteredValue = value
            filteredDerivative = 0
            return value
        }

        let derivative = (value - filteredValue) / dt
        filteredDerivative = lowPass(derivative, previous: filteredDerivative, cutoff: dCutoff, dt: dt)
        let cutoff = minCutoff + beta * abs(filteredDerivative)
        filteredValue = lowPass(value, previous: filteredValue, cutoff: cutoff, dt: dt)
        return filteredValue
    }

    private func lowPass(_ value: Double, previous: Double, cutoff: Double, dt: TimeInterval) -> Double {
        let tau = 1.0 / (2.0 * Double.pi * max(cutoff, 0.000_1))
        let alpha = 1.0 / (1.0 + tau / dt)
        return previous + alpha * (value - previous)
    }
}

struct OneEuroPointFilter {
    private var xFilter: OneEuroScalarFilter
    private var yFilter: OneEuroScalarFilter

    init(minCutoff: Double = 1.0, beta: Double = 0.0, dCutoff: Double = 1.0) {
        xFilter = OneEuroScalarFilter(minCutoff: minCutoff, beta: beta, dCutoff: dCutoff)
        yFilter = OneEuroScalarFilter(minCutoff: minCutoff, beta: beta, dCutoff: dCutoff)
    }

    mutating func configure(minCutoff: Double, beta: Double, dCutoff: Double = 1.0) {
        xFilter.minCutoff = max(minCutoff, 0.000_1)
        xFilter.beta = max(beta, 0)
        xFilter.dCutoff = max(dCutoff, 0.000_1)
        yFilter.minCutoff = xFilter.minCutoff
        yFilter.beta = xFilter.beta
        yFilter.dCutoff = xFilter.dCutoff
    }

    mutating func reset() {
        xFilter.reset()
        yFilter.reset()
    }

    mutating func prime(_ point: CGPoint) {
        xFilter.prime(Double(point.x))
        yFilter.prime(Double(point.y))
    }

    mutating func filter(_ point: CGPoint, dt: TimeInterval) -> CGPoint {
        CGPoint(
            x: CGFloat(xFilter.filter(Double(point.x), dt: dt)),
            y: CGFloat(yFilter.filter(Double(point.y), dt: dt))
        )
    }
}

enum CursorFilterTuning {
    static func screenSpace(smoothing: Double, sensitivity: Double) -> (minCutoff: Double, beta: Double) {
        let steadiness = min(max(smoothing, 0), 1)
        let snappiness = min(max(sensitivity, 0.2), 1.25)
        let minCutoff = 0.8 + (1 - steadiness) * 2.8
        let beta = 0.006 + snappiness * 0.020
        return (minCutoff, beta)
    }
}
