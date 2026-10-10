import Foundation

enum MotionCalibrationPose: Equatable {
    case openPalm
    case closedPalm
    case other
}

struct MotionCalibrationPoseClassifier {
    static func pose(gesture: GestureName, curls: FingerCurls) -> MotionCalibrationPose {
        if gesture == .openPalm {
            return .openPalm
        }

        if gesture == .fist {
            return .closedPalm
        }

        let topAverage = curls.topFingerAverage
        let topClosedCount = [curls.index, curls.middle, curls.ring, curls.little].filter { $0 >= 0.58 }.count
        if topAverage >= 0.61 || topClosedCount >= 3 {
            return .closedPalm
        }

        return .other
    }
}
