import Foundation

struct GestureIntentClassifier {
    static func classify(
        curls: FingerCurls,
        indexPinchScore: Double,
        middlePinchScore: Double,
        previous: GestureName = .none
    ) -> GestureName {
        let topCurls = [curls.index, curls.middle, curls.ring, curls.little]
        let topOpen = topCurls.filter { $0 < 0.44 }.count
        let inPointerPose = previous == .aim || previous == .leftClick
        let thumbOpenLimit = inPointerPose ? 0.75 : 0.62
        let supportThreshold = inPointerPose ? 0.45 : 0.56
        let thumbOpen = curls.thumb < thumbOpenLimit

        if topOpen == 4 && curls.thumb < 0.62 {
            return .openPalm
        }

        if curls.index < 0.44, curls.middle < 0.44, curls.ring >= 0.56, curls.little >= 0.56 {
            return .missionControl
        }

        let supportFingersClosed = curls.middle >= supportThreshold
            && curls.ring >= supportThreshold
            && curls.little >= supportThreshold
        guard supportFingersClosed else { return .none }

        if curls.index >= 0.62 {
            return thumbOpen ? .leftClick : .fist
        }

        return thumbOpen ? .aim : .none
    }
}
