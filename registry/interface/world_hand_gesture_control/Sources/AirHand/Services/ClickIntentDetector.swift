import Foundation

struct ClickIntentDetector {
    private let enterMargin = 0.24
    private let exitMargin = 0.14
    private let absoluteFloor = 0.55
    private let adaptRate = 0.22
    private let baselineCeiling = 0.55

    private var baseline = 0.25
    private var clicking = false
    private var primed = false

    mutating func reset() {
        baseline = 0.25
        clicking = false
        primed = false
    }

    mutating func update(indexCurl: Double) -> Bool {
        if !primed {
            baseline = indexCurl
            primed = true
        }

        let above = indexCurl - baseline
        if clicking {
            if above < exitMargin {
                clicking = false
            }
        } else if above >= enterMargin, indexCurl >= absoluteFloor {
            clicking = true
        }

        if !clicking {
            baseline += (indexCurl - baseline) * adaptRate
            baseline = min(baseline, baselineCeiling)
        }

        return clicking
    }
}
