import CoreGraphics
import Foundation

enum HandJoint: String, CaseIterable, Identifiable {
    case wrist
    case thumbCMC
    case thumbMP
    case thumbIP
    case thumbTip
    case indexMCP
    case indexPIP
    case indexDIP
    case indexTip
    case middleMCP
    case middlePIP
    case middleDIP
    case middleTip
    case ringMCP
    case ringPIP
    case ringDIP
    case ringTip
    case littleMCP
    case littlePIP
    case littleDIP
    case littleTip

    var id: String { rawValue }
}

enum TrackingMode: String {
    case stopped = "Stopped"
    case searching = "Searching"
    case pointing = "Pointing"
    case scrolling = "Scrolling"
    case unavailable = "Unavailable"
}

enum GestureName: String {
    case none = "None"
    case aim = "Aim"
    case openPalm = "Open palm"
    case leftClick = "Left click"
    case rightClick = "Right click"
    case fist = "Fist"
    case missionControl = "Mission Control"
}

struct HandPoint: Equatable {
    var location: CGPoint
    var confidence: Float
}

struct HandPoseFrame: Equatable {
    var points: [HandJoint: HandPoint]
    var timestamp: TimeInterval

    func location(_ joint: HandJoint) -> CGPoint? {
        points[joint]?.location
    }

    func confidence(_ joint: HandJoint) -> Float {
        points[joint]?.confidence ?? 0
    }

    var averageConfidence: Float {
        guard !points.isEmpty else { return 0 }
        let total = points.values.reduce(Float(0)) { $0 + $1.confidence }
        return total / Float(points.count)
    }
}

struct FingerCurls: Equatable {
    var thumb: Double = 0
    var index: Double = 0
    var middle: Double = 0
    var ring: Double = 0
    var little: Double = 0

    var topFingerAverage: Double {
        (index + middle + ring + little) / 4
    }
}

struct EngineSnapshot: Equatable {
    var mode: TrackingMode = .stopped
    var gesture: GestureName = .none
    var confidence: Float = 0
    var palmCenter: CGPoint?
    var cursorPoint: CGPoint?
    var curls = FingerCurls()
    var landmarks: [HandJoint: CGPoint] = [:]
    var message: String = "Stopped"
    var desktopDescription: String = ""
    var lastAction: String = "None"
    var magnetTarget: CGRect?
    var scrollOffset: CGFloat?
    var isDragging: Bool = false
    var walkingHz: Double?
    var speedFactor: Double?

    static let stopped = EngineSnapshot()
}

enum PointerAction: Equatable {
    case none
    case move(CGPoint)
    case leftDown(CGPoint)
    case drag(CGPoint)
    case leftUp(CGPoint)
    case rightClick(CGPoint)
    case scroll(CGPoint, Int32)
    case switchSpaceLeft
    case switchSpaceRight
}

struct EngineOutput: Equatable {
    var snapshot: EngineSnapshot
    var action: PointerAction
}
