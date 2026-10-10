import CoreGraphics
import Foundation

struct CoherentMicroMotionFilter {
    private var accumulatedIntent: CGPoint = .zero
    private var coherentFrames = 0

    mutating func reset() {
        accumulatedIntent = .zero
        coherentFrames = 0
    }

    mutating func update(step: CGPoint, lowGate: CGFloat, noiseGate: CGFloat, releaseFraction: CGFloat = 0.42) -> CGPoint? {
        let safeLowGate = max(lowGate, 0.1)
        let safeNoiseGate = max(noiseGate, safeLowGate * 2)
        guard step.length >= safeLowGate else {
            accumulatedIntent = accumulatedIntent * 0.62
            if accumulatedIntent.length < safeLowGate {
                reset()
            }
            return nil
        }

        if accumulatedIntent.length > safeLowGate {
            let alignment = step.normalized().dot(accumulatedIntent.normalized())
            if alignment < -0.18 {
                accumulatedIntent = accumulatedIntent * 0.28
                coherentFrames = 0
                return nil
            }
        }

        accumulatedIntent = accumulatedIntent + step
        coherentFrames += 1

        let cap = safeNoiseGate * 1.8
        if accumulatedIntent.length > cap {
            accumulatedIntent = accumulatedIntent.normalized() * cap
        }

        let release = max(safeNoiseGate * releaseFraction, safeLowGate * 2.2)
        guard coherentFrames >= 5, accumulatedIntent.length >= release else {
            return nil
        }

        let gain = min(max(accumulatedIntent.length / max(safeNoiseGate * 0.75, 1), 0.58), 1)
        return step * gain
    }
}
