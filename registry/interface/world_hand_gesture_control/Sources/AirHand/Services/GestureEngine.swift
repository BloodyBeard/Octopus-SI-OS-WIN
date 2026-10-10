import CoreGraphics
import Foundation

final class GestureEngine {
    private let displayMapper = DisplayMapper()
    private var smoothedCursor: CGPoint?
    private var lastRawCursor: CGPoint?
    private var lastFilterTime: TimeInterval?
    private var lastControlMode: ControlMode?
    private var lastCalibrationActive = false
    private var lastMotionTrainingActive = false
    private var calibratedCursorFilter = CalibratedCursorFilter()
    private var handPoseStabilizer = HandPoseStabilizer()
    private var relativeMotionController = RelativeCursorMotionController()
    private var learnedMotionController = LearnedMotionController()
    private var lastScrollPalmY: CGFloat?
    private var smoothedScroll: Double = 0
    private var scrollAnchorY: CGFloat?
    private var spaceSwipeAnchorX: CGFloat?
    private var lastSpaceSwipeTime: TimeInterval = 0
    private var clickIntentDetector = ClickIntentDetector()
    private var autoGait = AutoGaitCanceller()
    private var prevPalm: CGPoint?
    private var prevPalm2: CGPoint?
    private var jitterEMA: CGFloat = 0
    private var smoothingBoost: Double = 0
    private var pointerGraceUntil: TimeInterval = 0
    private var throttleEMA: Double = 1
    private var clickAnchorCursor: CGPoint?
    private var mouseDownAnchor: CGPoint?
    private var isDragging = false
    private var lastMagnetTarget: CGRect?
    private var lastScrollOffset: CGFloat = 0
    private var lastClickTime: TimeInterval = 0
    private var candidateGesture: GestureName = .none
    private var candidateSince: TimeInterval = 0
    private var firedCandidate: GestureName = .none
    private var mode: TrackingMode = .searching
    private var isControlActive = false
    private var lastActionText = "None"
    weak var targetLocator: CursorTargetLocator?
    private var lastMagnetCursor: CGPoint?
    private var lastMagnetTime: TimeInterval?

    func reset() {
        smoothedCursor = nil
        lastRawCursor = nil
        lastFilterTime = nil
        lastControlMode = nil
        lastCalibrationActive = false
        lastMotionTrainingActive = false
        calibratedCursorFilter.reset()
        handPoseStabilizer.reset()
        autoGait.reset()
        prevPalm = nil
        prevPalm2 = nil
        jitterEMA = 0
        smoothingBoost = 0
        pointerGraceUntil = 0
        throttleEMA = 1
        relativeMotionController.reset()
        learnedMotionController.reset()
        lastScrollPalmY = nil
        smoothedScroll = 0
        scrollAnchorY = nil
        spaceSwipeAnchorX = nil
        clickIntentDetector.reset()
        clickAnchorCursor = nil
        mouseDownAnchor = nil
        isDragging = false
        lastMagnetTarget = nil
        lastScrollOffset = 0
        lastClickTime = 0
        candidateGesture = .none
        candidateSince = 0
        firedCandidate = .none
        mode = .searching
        isControlActive = false
        lastActionText = "None"
        lastMagnetCursor = nil
        lastMagnetTime = nil
    }

    func forceReleasePointer() -> PointerAction {
        guard let anchor = mouseDownAnchor else { return .none }
        mouseDownAnchor = nil
        isDragging = false
        return .leftUp(smoothedCursor ?? anchor)
    }

    func lostHand(now: TimeInterval) -> EngineOutput {
        let release = forceReleasePointer()
        candidateGesture = .none
        candidateSince = 0
        lastScrollPalmY = nil
        smoothedScroll = 0
        scrollAnchorY = nil
        spaceSwipeAnchorX = nil
        clickIntentDetector.reset()
        clickAnchorCursor = nil
        lastMagnetTarget = nil
        lastRawCursor = nil
        lastFilterTime = nil
        relativeMotionController.reset()
        learnedMotionController.reset()
        calibratedCursorFilter.reset()
        handPoseStabilizer.reset()
        prevPalm = nil
        prevPalm2 = nil
        isControlActive = false
        mode = .searching
        let snapshot = EngineSnapshot(
            mode: .searching,
            gesture: .none,
            confidence: 0,
            palmCenter: nil,
            cursorPoint: smoothedCursor,
            curls: FingerCurls(),
            landmarks: [:],
            message: "Fold middle, ring, and little fingers to aim",
            desktopDescription: displayMapper.desktopDescription,
            lastAction: lastActionText
        )
        return EngineOutput(snapshot: snapshot, action: release)
    }

    func process(frame: HandPoseFrame, settings: AppSettings) -> EngineOutput {
        let now = frame.timestamp
        guard let classification = classify(frame, settings: settings),
              let handMeasurement = handPoseMeasurement(frame)
        else {
            return lostHand(now: now)
        }

        var gesture = classification.gesture
        var coasting = false
        if gesture == .aim || gesture == .leftClick {
            pointerGraceUntil = now + 0.30
        } else if gesture == .none, now < pointerGraceUntil {
            gesture = .aim
            coasting = true
        }
        if coasting {
        } else if gesture == .aim || gesture == .leftClick {
            gesture = clickIntentDetector.update(indexCurl: classification.curls.index) ? .leftClick : .aim
        } else {
            clickIntentDetector.reset()
        }

        if gesture == .aim, !coasting, settings.fingerThrottle {
            let target = FingerThrottle.speedFactor(indexCurl: classification.curls.index)
            throttleEMA += (target - throttleEMA) * 0.3
        } else if !settings.fingerThrottle {
            throttleEMA = 1
        }
        if gesture != .fist {
            scrollAnchorY = nil
        }
        if gesture != .missionControl {
            spaceSwipeAnchorX = nil
        }
        updateCandidate(gesture: gesture, now: now)

        if mouseDownAnchor != nil, gesture != .aim, gesture != .leftClick {
            let release = forceReleasePointer()
            clickAnchorCursor = nil
            return EngineOutput(
                snapshot: EngineSnapshot(
                    mode: .searching,
                    gesture: gesture,
                    confidence: frame.averageConfidence,
                    palmCenter: handMeasurement.controlPoint,
                    cursorPoint: smoothedCursor,
                    curls: classification.curls,
                    landmarks: frame.points.mapValues(\.location),
                    message: "Released",
                    desktopDescription: displayMapper.desktopDescription,
                    lastAction: lastActionText
                ),
                action: release
            )
        }

        if gesture == .openPalm {
            deactivateControl()
            return EngineOutput(
                snapshot: EngineSnapshot(
                    mode: .searching,
                    gesture: gesture,
                    confidence: frame.averageConfidence,
                    palmCenter: handMeasurement.controlPoint,
                    cursorPoint: smoothedCursor,
                    curls: classification.curls,
                    landmarks: frame.points.mapValues(\.location),
                    message: "Control paused",
                    desktopDescription: displayMapper.desktopDescription,
                    lastAction: lastActionText,
                    walkingHz: detectedWalkingHz
                ),
                action: .none
            )
        }

        if gesture == .missionControl {
            isControlActive = false
            clickAnchorCursor = nil
            mode = .searching
            let palm = stabilizedControlPoint(handMeasurement, now: now, freeze: false, settings: settings)
            let action = spaceSwipeAction(palmX: palm.x, now: now, settings: settings)
            return EngineOutput(
                snapshot: EngineSnapshot(
                    mode: .searching,
                    gesture: gesture,
                    confidence: frame.averageConfidence,
                    palmCenter: palm,
                    cursorPoint: smoothedCursor,
                    curls: classification.curls,
                    landmarks: frame.points.mapValues(\.location),
                    message: "Move ✌ left / right to switch spaces",
                    desktopDescription: displayMapper.desktopDescription,
                    lastAction: lastActionText,
                    walkingHz: detectedWalkingHz
                ),
                action: action
            )
        }

        if gesture == .fist {
            isControlActive = false
            clickAnchorCursor = nil
            mode = .scrolling
            let palm = stabilizedControlPoint(handMeasurement, now: now, freeze: false, settings: settings)
            let delta = scrollVelocity(from: palm, settings: settings)
            let scrollPoint = smoothedCursor ?? displayMapper.mapPalmToDesktop(palm, settings: settings)
            let action: PointerAction = delta != 0 ? .scroll(scrollPoint, delta) : .none
            if delta != 0 {
                lastActionText = "Scroll"
            }
            return EngineOutput(
                snapshot: EngineSnapshot(
                    mode: .scrolling,
                    gesture: gesture,
                    confidence: frame.averageConfidence,
                    palmCenter: palm,
                    cursorPoint: smoothedCursor,
                    curls: classification.curls,
                    landmarks: frame.points.mapValues(\.location),
                    message: "Scroll · move fist up / down",
                    desktopDescription: displayMapper.desktopDescription,
                    lastAction: lastActionText,
                    scrollOffset: scrollAnchorY != nil ? lastScrollOffset : nil,
                    walkingHz: detectedWalkingHz
                ),
                action: action
            )
        }

        let canControlPointer = gesture == .aim || gesture == .leftClick
        guard canControlPointer else {
            clickAnchorCursor = nil
            lastScrollPalmY = nil
            mode = .searching
            return EngineOutput(
                snapshot: EngineSnapshot(
                    mode: .searching,
                    gesture: gesture,
                    confidence: frame.averageConfidence,
                    palmCenter: handMeasurement.controlPoint,
                    cursorPoint: smoothedCursor,
                    curls: classification.curls,
                    landmarks: frame.points.mapValues(\.location),
                    message: "Fold middle, ring, and little fingers to aim",
                    desktopDescription: displayMapper.desktopDescription,
                    lastAction: lastActionText
                ),
                action: .none
            )
        }

        if !isControlActive {
            isControlActive = true
            mode = .pointing
            handPoseStabilizer.reset()
            relativeMotionController.reset()
            learnedMotionController.reset()
            calibratedCursorFilter.reset()
            lastRawCursor = nil
            lastFilterTime = nil
        }

        let displayedBefore = smoothedCursor
        let palm = stabilizedControlPoint(handMeasurement, now: now, freeze: false, settings: settings)
        var cursor = pointerCursor(from: palm, now: now, settings: settings, freeze: false)
        if gesture == .aim, mouseDownAnchor == nil, settings.magneticTargeting {
            cursor = magnetizedCursor(cursor, now: now, settings: settings)
            smoothedCursor = cursor
        } else if gesture == .aim {
            lastMagnetTarget = nil
        }
        var action: PointerAction = .move(cursor)
        var message = controlMode(settings).message
        let dragThreshold: CGFloat = 16

        mode = .pointing
        lastScrollPalmY = nil

        if gesture == .leftClick {
            if let anchor = mouseDownAnchor {
                if isDragging {
                    action = .drag(cursor)
                    message = "Dragging"
                } else if cursor.distance(to: anchor) > dragThreshold {
                    isDragging = true
                    action = .drag(cursor)
                    lastActionText = "Drag"
                    message = "Dragging"
                } else {
                    cursor = anchor
                    action = .none
                    message = "Hold · move to drag"
                }
            } else if clickAnchorCursor == nil {
                clickAnchorCursor = displayedBefore ?? cursor
                cursor = clickAnchorCursor ?? cursor
                action = .none
                message = "Pressing"
            } else if canClick(now, settings: settings), held(gesture, now: now, duration: settings.clickHoldSeconds) {
                let anchor = clickAnchorCursor ?? cursor
                mouseDownAnchor = anchor
                isDragging = false
                lastClickTime = now
                cursor = anchor
                action = .leftDown(anchor)
                lastActionText = "Click"
                message = "Click"
            } else {
                cursor = clickAnchorCursor ?? cursor
                action = .none
                message = "Pressing"
            }
        } else {
            if let anchor = mouseDownAnchor {
                let wasDragging = isDragging
                mouseDownAnchor = nil
                isDragging = false
                if wasDragging {
                    action = .leftUp(cursor)
                    lastActionText = "Drop"
                    message = "Dropped"
                } else {
                    cursor = anchor
                    smoothedCursor = anchor
                    relativeMotionController.reset()
                    action = .leftUp(anchor)
                    message = controlMode(settings).message
                }
            } else {
                action = .move(cursor)
            }
            clickAnchorCursor = nil
        }

        let snapshot = EngineSnapshot(
            mode: mode,
            gesture: gesture,
            confidence: frame.averageConfidence,
            palmCenter: palm,
            cursorPoint: cursor,
            curls: classification.curls,
            landmarks: frame.points.mapValues(\.location),
            message: message,
            desktopDescription: displayMapper.desktopDescription,
            lastAction: lastActionText,
            magnetTarget: lastMagnetTarget,
            isDragging: isDragging,
            walkingHz: detectedWalkingHz,
            speedFactor: settings.fingerThrottle ? throttleEMA : nil
        )

        return EngineOutput(snapshot: snapshot, action: action)
    }

    private func deactivateControl() {
        isControlActive = false
        clickAnchorCursor = nil
        lastMagnetTarget = nil
        lastScrollPalmY = nil
        lastRawCursor = nil
        lastFilterTime = nil
        relativeMotionController.reset()
        learnedMotionController.reset()
        calibratedCursorFilter.reset()
        handPoseStabilizer.reset()
        lastMagnetCursor = nil
        lastMagnetTime = nil
        mode = .searching
    }

    private func magnetizedCursor(_ cursor: CGPoint, now: TimeInterval, settings: AppSettings) -> CGPoint {
        guard let locator = targetLocator else { return cursor }

        let dt = max(now - (lastMagnetTime ?? now), 1.0 / 120.0)
        let speed = lastMagnetCursor.map { cursor.distance(to: $0) / CGFloat(dt) } ?? 0
        lastMagnetTime = now
        lastMagnetCursor = cursor

        let slowFactor = 1 - smoothstep(300, 1_300, speed)
        guard slowFactor > 0.02 else {
            lastMagnetTarget = nil
            return cursor
        }

        let captureRadius: CGFloat = 60 + 45 * slowFactor
        guard let frame = locator.magneticTarget(near: cursor, captureRadius: captureRadius) else {
            lastMagnetTarget = nil
            return cursor
        }
        lastMagnetTarget = frame

        let center = CGPoint(x: frame.midX, y: frame.midY)
        let inside = frame.insetBy(dx: -3, dy: -3).contains(cursor)
        let proximity = inside ? 1 : max(0, 1 - cursor.distance(to: frame) / captureRadius)
        let strength = CGFloat(min(max(settings.magneticStrength, 0), 1))
        let pull = min(0.45, strength * slowFactor * proximity)
        return cursor + (center - cursor) * pull
    }

    private func smoothstep(_ edge0: CGFloat, _ edge1: CGFloat, _ value: CGFloat) -> CGFloat {
        guard edge1 > edge0 else { return value >= edge1 ? 1 : 0 }
        let t = min(max((value - edge0) / (edge1 - edge0), 0), 1)
        return t * t * (3 - 2 * t)
    }

    private func updateCandidate(gesture: GestureName, now: TimeInterval) {
        if gesture != candidateGesture {
            candidateGesture = gesture
            candidateSince = now
            firedCandidate = .none
        }
    }

    private func held(_ gesture: GestureName, now: TimeInterval, duration: TimeInterval) -> Bool {
        candidateGesture == gesture && now - candidateSince >= duration
    }

    private func canClick(_ now: TimeInterval, settings: AppSettings) -> Bool {
        now - lastClickTime > settings.clickCooldownSeconds
    }

    private func pointerCursor(from palm: CGPoint, now: TimeInterval, settings: AppSettings, freeze: Bool) -> CGPoint {
        let nextMode = controlMode(settings)
        let calibrationActive = settings.shouldUseCalibration
        let motionTrainingActive = settings.shouldUseMotionTraining && settings.motionTrainingProfile != nil
        if lastControlMode != nextMode || lastCalibrationActive != calibrationActive || lastMotionTrainingActive != motionTrainingActive {
            lastRawCursor = nil
            lastFilterTime = nil
            relativeMotionController.reset()
            learnedMotionController.reset()
            calibratedCursorFilter.reset()
            lastControlMode = nextMode
            lastCalibrationActive = calibrationActive
            lastMotionTrainingActive = motionTrainingActive
        }

        if motionTrainingActive,
           let profile = settings.motionTrainingProfile {
            return learnedCursor(from: palm, profile: profile, now: now, settings: settings, freeze: freeze)
        }

        if calibrationActive {
            return calibratedCursor(from: palm, now: now, settings: settings, freeze: freeze)
        }

        switch nextMode {
        case .direct:
            return directCursor(from: palm, now: now, settings: settings, freeze: freeze)
        case .smartWalk:
            return smartWalkCursor(from: palm, now: now, settings: settings, freeze: freeze)
        }
    }

    private func directCursor(from palm: CGPoint, now: TimeInterval, settings: AppSettings, freeze: Bool) -> CGPoint {
        let rawCursor = displayMapper.mapPalmToDesktop(palm, settings: settings)
        let desktop = displayMapper.desktopRect
        let desktopScale = max(desktop.width, desktop.height)
        let maxStepPixels = desktop.isEmpty ? 260 : min(max(Double(desktopScale) * 0.055, 180), 360)
        let next = calibratedCursorFilter.update(
            raw: rawCursor,
            now: now,
            freeze: freeze,
            settings: CalibratedCursorFilter.Settings(
                smoothing: effectiveSmoothing(settings),
                sensitivity: settings.pointerSensitivity,
                deadZonePixels: settings.deadZonePixels,
                stabilizationRadiusPixels: settings.stabilizationRadiusPixels,
                noiseRadiusPixels: 0,
                maxStepPixels: maxStepPixels
            )
        )
        smoothedCursor = next
        return next
    }

    private func calibratedCursor(from palm: CGPoint, now: TimeInterval, settings: AppSettings, freeze: Bool) -> CGPoint {
        let rawCursor = displayMapper.mapPalmToDesktop(palm, settings: settings)
        let desktop = displayMapper.desktopRect
        let noiseRadiusPixels: Double
        if let profile = settings.motionCalibrationProfile, !desktop.isEmpty {
            noiseRadiusPixels = profile.normalizedNoiseRadius * Double(max(desktop.width, desktop.height)) * 1.45
        } else {
            noiseRadiusPixels = 0
        }
        let maxStepPixels = desktop.isEmpty ? 260 : min(max(Double(max(desktop.width, desktop.height)) * 0.055, 180), 360)

        let next = calibratedCursorFilter.update(
            raw: rawCursor,
            now: now,
            freeze: freeze,
            settings: CalibratedCursorFilter.Settings(
                smoothing: min(effectiveSmoothing(settings), 0.82),
                sensitivity: max(settings.pointerSensitivity, 0.50),
                deadZonePixels: settings.deadZonePixels,
                stabilizationRadiusPixels: min(settings.stabilizationRadiusPixels, 32),
                noiseRadiusPixels: noiseRadiusPixels,
                maxStepPixels: maxStepPixels
            )
        )
        smoothedCursor = next
        return next
    }

    private func learnedCursor(
        from palm: CGPoint,
        profile: MotionTrainingProfile,
        now: TimeInterval,
        settings: AppSettings,
        freeze: Bool
    ) -> CGPoint {
        let desktop = displayMapper.desktopRect
        let desktopScale = max(desktop.width, desktop.height)
        let maxStepPixels = desktop.isEmpty ? 420 : min(max(Double(desktopScale) * 0.075, 260), 640)
        let next = learnedMotionController.update(
            hand: palm,
            profile: profile,
            previousCursor: smoothedCursor,
            desktop: desktop,
            now: now,
            freeze: freeze,
            settings: LearnedMotionController.Settings(
                smoothing: effectiveSmoothing(settings),
                sensitivity: settings.pointerSensitivity,
                deadZonePixels: settings.deadZonePixels,
                maxStepPixels: maxStepPixels
            )
        )
        smoothedCursor = next
        return next
    }

    private func controlMode(_ settings: AppSettings) -> ControlMode {
        ControlMode(rawValue: settings.controlModeID) ?? .smartWalk
    }

    private func smartWalkCursor(from palm: CGPoint, now: TimeInterval, settings: AppSettings, freeze: Bool) -> CGPoint {
        let desktop = displayMapper.desktopRect
        let fallbackCursor = displayMapper.mapPalmToDesktop(palm, settings: settings)
        let desktopScale = max(desktop.width, desktop.height)
        let maxStepPixels = desktop.isEmpty ? 280 : min(max(Double(desktopScale) * 0.052, 170), 360)
        let next = relativeMotionController.update(
            controlPoint: controlSpacePalm(palm, settings: settings),
            fallbackCursor: fallbackCursor,
            previousCursor: smoothedCursor,
            desktop: desktop,
            now: now,
            freeze: freeze,
            settings: RelativeCursorMotionController.Settings(
                smoothing: effectiveSmoothing(settings),
                sensitivity: settings.pointerSensitivity,
                deadZonePixels: settings.deadZonePixels,
                stabilizationRadiusPixels: settings.stabilizationRadiusPixels,
                maxStepPixels: maxStepPixels,
                speedScale: throttleEMA
            )
        )
        smoothedCursor = next
        return next
    }

    private func controlSpacePalm(_ palm: CGPoint, settings: AppSettings) -> CGPoint {
        displayMapper.relativeControlPoint(palm, settings: settings)
    }

    private func scrollVelocity(from palm: CGPoint, settings: AppSettings) -> Int32 {
        guard let anchorY = scrollAnchorY else {
            scrollAnchorY = palm.y
            smoothedScroll = 0
            return 0
        }

        let displacement = Double(palm.y - anchorY)
        let deadzone = 0.02
        let magnitude = abs(displacement) - deadzone
        guard magnitude > 0 else {
            smoothedScroll = 0
            return 0
        }

        let direction: Double = displacement > 0 ? 1 : -1
        lastScrollOffset = CGFloat(min(max(displacement / 0.12, -1), 1))
        let speed = magnitude * magnitude * direction * settings.scrollSensitivity * 4.0
        let signed = settings.invertScroll ? speed : -speed
        smoothedScroll = smoothedScroll * 0.6 + signed * 0.4
        guard abs(smoothedScroll) >= 1 else { return 0 }
        return Int32(max(min(smoothedScroll, 80), -80))
    }

    private func spaceSwipeAction(palmX: CGFloat, now: TimeInterval, settings: AppSettings) -> PointerAction {
        guard let anchorX = spaceSwipeAnchorX else {
            spaceSwipeAnchorX = palmX
            return .none
        }

        let threshold: CGFloat = 0.12
        let cooldown: TimeInterval = 0.5
        let displacement = palmX - anchorX
        guard abs(displacement) >= threshold, now - lastSpaceSwipeTime >= cooldown else {
            return .none
        }

        lastSpaceSwipeTime = now
        spaceSwipeAnchorX = palmX

        let cursorGoesRight = settings.mirrorX ? (displacement < 0) : (displacement > 0)
        lastActionText = cursorGoesRight ? "Space →" : "Space ←"
        return cursorGoesRight ? .switchSpaceRight : .switchSpaceLeft
    }

    private func palmCenter(_ frame: HandPoseFrame) -> CGPoint? {
        let joints: [HandJoint] = [.wrist, .indexMCP, .middleMCP, .ringMCP, .littleMCP]
        let locations = joints.compactMap { frame.location($0) }
        guard locations.count >= 4 else { return nil }
        return locations.reduce(.zero, +) / CGFloat(locations.count)
    }

    private func stabilizedControlPoint(_ measurement: HandPoseMeasurement, now: TimeInterval, freeze: Bool, settings: AppSettings) -> CGPoint {
        let palm = handPoseStabilizer.update(measurement: measurement, now: now, freeze: freeze)
        updateJitterEstimate(palm)
        guard settings.autoGaitCompensation, !freeze else { return palm }
        return autoGait.process(point: palm, now: now)
    }

    var detectedWalkingHz: Double? {
        autoGait.detectedHz
    }

    private func updateJitterEstimate(_ palm: CGPoint) {
        if let p1 = prevPalm, let p2 = prevPalm2 {
            let accel = ((palm - p1 * 2) + p2).length
            jitterEMA += (accel - jitterEMA) * 0.04
            smoothingBoost = Double(min(max((jitterEMA - 0.005) * 20, 0), 0.12))
        }
        prevPalm2 = prevPalm
        prevPalm = palm
    }

    private func effectiveSmoothing(_ settings: AppSettings) -> Double {
        min(settings.smoothing + smoothingBoost, 0.94)
    }

    private func handPoseMeasurement(_ frame: HandPoseFrame) -> HandPoseMeasurement? {
        guard let wrist = frame.location(.wrist),
              let indexMCP = frame.location(.indexMCP),
              let middleMCP = frame.location(.middleMCP),
              let ringMCP = frame.location(.ringMCP),
              let littleMCP = frame.location(.littleMCP)
        else {
            return nil
        }

        let knuckleCenter = (indexMCP + middleMCP + ringMCP + littleMCP) / 4
        let axisPoint = wrist + (middleMCP - wrist) * 0.62
        let palmControlPoint = axisPoint * 0.72 + knuckleCenter * 0.28
        let controlPoint = indexMCP * 0.45 + palmControlPoint * 0.55
        let palmAxis = middleMCP - wrist

        return HandPoseMeasurement(
            controlPoint: controlPoint,
            palmWidth: max(indexMCP.distance(to: littleMCP), 0.01),
            palmLength: max(palmAxis.length, 0.01),
            palmAngle: atan2(palmAxis.y, palmAxis.x)
        )
    }

    private func classify(_ frame: HandPoseFrame, settings: AppSettings) -> (gesture: GestureName, curls: FingerCurls)? {
        guard let wrist = frame.location(.wrist),
              let indexMCP = frame.location(.indexMCP),
              let middleMCP = frame.location(.middleMCP),
              let littleMCP = frame.location(.littleMCP),
              let palmCenter = palmCenter(frame)
        else {
            return nil
        }

        let palmAxis = (middleMCP - wrist).normalized()
        let palmWidth = max(indexMCP.distance(to: littleMCP), 0.04)
        let thumbCurl = thumbClosedScore(frame, palmCenter: palmCenter, palmWidth: palmWidth)
        let indexCurl = curlScore(frame, mcp: .indexMCP, pip: .indexPIP, tip: .indexTip, palmAxis: palmAxis)
        let middleCurl = curlScore(frame, mcp: .middleMCP, pip: .middlePIP, tip: .middleTip, palmAxis: palmAxis)
        let ringCurl = curlScore(frame, mcp: .ringMCP, pip: .ringPIP, tip: .ringTip, palmAxis: palmAxis)
        let littleCurl = curlScore(frame, mcp: .littleMCP, pip: .littlePIP, tip: .littleTip, palmAxis: palmAxis)

        let curls = FingerCurls(
            thumb: thumbCurl,
            index: indexCurl,
            middle: middleCurl,
            ring: ringCurl,
            little: littleCurl
        )

        let gesture = GestureIntentClassifier.classify(
            curls: curls,
            indexPinchScore: pinchScore(frame, fingerTip: .indexTip, palmWidth: palmWidth),
            middlePinchScore: pinchScore(frame, fingerTip: .middleTip, palmWidth: palmWidth),
            previous: candidateGesture
        )
        return (gesture, curls)
    }

    private func curlScore(
        _ frame: HandPoseFrame,
        mcp: HandJoint,
        pip: HandJoint,
        tip: HandJoint,
        palmAxis: CGPoint
    ) -> Double {
        guard let mcpPoint = frame.location(mcp),
              let pipPoint = frame.location(pip),
              let tipPoint = frame.location(tip)
        else {
            return 0
        }

        let angle = angleDegrees(a: mcpPoint - pipPoint, b: tipPoint - pipPoint)
        let angleCurl = 1 - min(max((angle - 78) / 92, 0), 1)
        let projection = (tipPoint - mcpPoint).dot(palmAxis)
        let projectionCurl = 1 - min(max((projection - 0.05) / 0.22, 0), 1)
        return min(max(Double(angleCurl * 0.62 + projectionCurl * 0.38), 0), 1)
    }

    private func thumbClosedScore(_ frame: HandPoseFrame, palmCenter: CGPoint, palmWidth: CGFloat) -> Double {
        guard let tip = frame.location(.thumbTip),
              let mp = frame.location(.thumbMP),
              let ip = frame.location(.thumbIP),
              let indexMCP = frame.location(.indexMCP)
        else {
            return 0
        }

        let closeToPalm = 1 - min(max((tip.distance(to: palmCenter) / palmWidth - 0.55) / 0.8, 0), 1)
        let closeToIndex = 1 - min(max((tip.distance(to: indexMCP) / palmWidth - 0.45) / 0.9, 0), 1)
        let angle = angleDegrees(a: mp - ip, b: tip - ip)
        let angleCurl = 1 - min(max((angle - 76) / 94, 0), 1)
        return min(max(Double(closeToPalm * 0.45 + closeToIndex * 0.30 + angleCurl * 0.25), 0), 1)
    }

    private func pinchScore(_ frame: HandPoseFrame, fingerTip: HandJoint, palmWidth: CGFloat) -> Double {
        guard let thumbTip = frame.location(.thumbTip),
              let targetTip = frame.location(fingerTip)
        else {
            return 0
        }

        let normalizedDistance = thumbTip.distance(to: targetTip) / max(palmWidth, 0.01)
        return Double(1 - min(max((normalizedDistance - 0.18) / 0.42, 0), 1))
    }

    private func angleDegrees(a: CGPoint, b: CGPoint) -> CGFloat {
        let denominator = max(a.length * b.length, 0.000_001)
        let cosine = min(max(a.dot(b) / denominator, -1), 1)
        return acos(cosine) * 180 / .pi
    }
}
