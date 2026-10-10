import CoreGraphics
import Foundation

@main
struct CalibrationChecks {
    static func main() throws {
        try testProfileLearnsNoisyDiagonalHorizontalAndVerticalAxes()
        try testSurfaceProfileMapsCurvedRailsLocally()
        testProfileRejectsTinyMotionsInsteadOfPretendingCalibrationWorked()
        try testProfileRoundTripsThroughSettingsJSON()
        try testProfileKeepsUnclampedControlDeltasOutsideTrainedRange()
        try testDisplayMapperUsesSurfaceCalibration()
        try testTrainingProfileLearnsRecordedCursorPath()
        try testTrainingProfileLearnsVelocityTransform()
        try testLearnedMotionControllerPreservesRecordedMotionSpeed()
        try testLearnedMotionControllerRejectsTinyJitter()
        try testLearnedMotionControllerStartsCoherentMicroAim()
        try testTrainingProfileRoundTripsThroughJSON()
        try testSettingsMigratesCalibratedDirectModeToSmartWalk()
        testTrainingStopPoseAcceptsClosedPalmEvenWhenGestureClassifierCallsItLeftClick()
        testCalibratedCursorFilterDoesNotDriftPastStationaryTarget()
        testCalibratedCursorFilterSoftensSmallAimingCorrections()
        testCalibratedCursorFilterRejectsPalmRotationJitter()
        testCalibratedCursorFilterCapsSingleFrameHalfScreenJump()
        testCalibratedCursorFilterRejectsZeroMeanStationaryNoise()
        testCalibratedCursorFilterRespondsQuicklyToRealMotion()
        testCalibratedCursorFilterPreservesContinuousMotionVelocity()
        testCalibratedCursorFilterDoesNotSwallowSlowAbsoluteMotion()
        testCalibratedCursorFilterMovesContinuouslyDuringSlowAbsoluteMotion()
        testCalibratedCursorFilterUsesAdaptiveSmoothingForFastMotion()
        testCalibratedCursorFilterStartsCoherentMicroAimWithoutUnlockDelay()
        testHandPoseStabilizerIgnoresPoseChangeWithoutTranslation()
        testHandPoseStabilizerAllowsRealMotionAfterPoseChange()
        testHandPoseStabilizerRejectsSingleFrameLandmarkOutlier()
        testHandPoseStabilizerAcceptsSustainedLargeMotion()
        testHandPoseStabilizerRespondsQuicklyToContinuousMotion()
        testRelativeCursorMotionDoesNotDriftWhenPalmIsStill()
        testRelativeCursorMotionAccumulatesSlowIntentionalMovement()
        testRelativeCursorMotionContinuesAfterSlowIntentLocks()
        testRelativeCursorMotionPreservesHandVelocity()
        testRelativeCursorMotionDoesNotJumpAfterFreeze()
        testRelativeCursorMotionDampsTremorWithoutSwallowingMicroAim()
        testGestureClassifierMapsThreeClosedFingersToAim()
        testGestureClassifierMapsIndexCloseToLeftClick()
        testGestureClassifierMapsOpenPalmToPause()
        testGestureClassifierDoesNotMapThumbFoldToAim()
        testGestureClassifierDoesNotMapMiddlePinchToRightClick()
        testGestureClassifierDoesNotMapFourFingerFistToRightClick()
        testGestureClassifierMapsTwoFingersToMissionControl()
        testGestureClassifierMapsTuckedFistToScroll()
        testGestureClassifierKeepsThumbOutFoldAsLeftClick()
        testGestureClassifierKeepsAimThroughMarginalCurls()
        testFingerThrottleMapsExtensionToSpeed()
        testClickIntentRejectsPostureDrift()
        testClickIntentFiresOnDeliberateTap()
        testGaitCompensationCancelsWalkingBobButKeepsAim()
        testAutoGaitEngagesOnWalkingAndReleasesWhenStill()
        reportAndExit()
    }

    private static func testProfileLearnsNoisyDiagonalHorizontalAndVerticalAxes() throws {
        let horizontal = (0..<90).map { index in
            let t = CGFloat(index) / 89
            return CGPoint(
                x: 0.18 + 0.58 * t + sin(t * 31) * 0.004,
                y: 0.72 - 0.16 * t + cos(t * 27) * 0.005
            )
        }

        let vertical = (0..<90).map { index in
            let t = CGFloat(index) / 89
            return CGPoint(
                x: 0.45 + 0.10 * t + sin(t * 29) * 0.004,
                y: 0.78 - 0.56 * t + cos(t * 23) * 0.005
            )
        }

        let profile = try MotionCalibrationProfileBuilder().build(
            horizontalSamples: horizontal,
            verticalSamples: vertical,
            createdAt: 42
        )

        require(profile.normalizedPoint(for: horizontal[3]).x.isApproximately(0, accuracy: 0.06), "horizontal start maps left")
        require(profile.normalizedPoint(for: horizontal[86]).x.isApproximately(1, accuracy: 0.06), "horizontal end maps right")
        require(profile.normalizedPoint(for: vertical[3]).y.isApproximately(0, accuracy: 0.06), "vertical start maps top")
        require(profile.normalizedPoint(for: vertical[86]).y.isApproximately(1, accuracy: 0.06), "vertical end maps bottom")
        require(profile.horizontal.quality > 0.75, "horizontal quality")
        require(profile.vertical.quality > 0.75, "vertical quality")
    }

    private static func testSurfaceProfileMapsCurvedRailsLocally() throws {
        let top = curvedRail(yBase: 0.24)
        let middle = curvedRail(yBase: 0.50)
        let bottom = curvedRail(yBase: 0.76)

        let profile = try MotionCalibrationProfileBuilder().buildSurface(
            topSamples: top,
            middleSamples: middle,
            bottomSamples: bottom,
            createdAt: 42
        )

        let leftMiddle = profile.normalizedPoint(for: middle[0])
        let rightMiddle = profile.normalizedPoint(for: middle[middle.count - 1])
        require(leftMiddle.x.isApproximately(0, accuracy: 0.06), "surface left middle maps to left edge")
        require(rightMiddle.x.isApproximately(1, accuracy: 0.06), "surface right middle maps to right edge")
        require(leftMiddle.y.isApproximately(0.5, accuracy: 0.06), "surface middle rail maps to local center")

        let sampleIndex = 50
        let u = CGFloat(sampleIndex) / CGFloat(middle.count - 1)
        let localPoint = top[sampleIndex] * 0.25 + bottom[sampleIndex] * 0.75
        let mapped = profile.normalizedPoint(for: localPoint)
        require(mapped.x.isApproximately(u, accuracy: 0.07), "surface local x changed while moving vertically")
        require(mapped.y.isApproximately(0.75, accuracy: 0.08), "surface local vertical interpolation failed")
    }

    private static func testProfileRejectsTinyMotionsInsteadOfPretendingCalibrationWorked() {
        let tiny = (0..<40).map { index in
            CGPoint(x: 0.4 + CGFloat(index) * 0.0002, y: 0.5)
        }

        do {
            _ = try MotionCalibrationProfileBuilder().build(
                horizontalSamples: tiny,
                verticalSamples: tiny,
                createdAt: 42
            )
            fatalError("Expected tiny calibration motion to fail")
        } catch is MotionCalibrationError {
            return
        } catch {
            fatalError("Unexpected tiny calibration error: \(error)")
        }
    }

    private static func testProfileRoundTripsThroughSettingsJSON() throws {
        let profile = try MotionCalibrationProfileBuilder().buildSurface(
            topSamples: straightLine(from: CGPoint(x: 0.15, y: 0.25), to: CGPoint(x: 0.85, y: 0.25)),
            middleSamples: straightLine(from: CGPoint(x: 0.15, y: 0.5), to: CGPoint(x: 0.85, y: 0.5)),
            bottomSamples: straightLine(from: CGPoint(x: 0.15, y: 0.75), to: CGPoint(x: 0.85, y: 0.75)),
            createdAt: 42
        )

        let encoded = try profile.encodedString()
        guard let decoded = MotionCalibrationProfile(encodedString: encoded) else {
            fatalError("Could not decode profile JSON")
        }

        require(decoded.normalizedPoint(for: CGPoint(x: 0.15, y: 0.5)).x.isApproximately(0, accuracy: 0.04), "decoded horizontal start")
        require(decoded.normalizedPoint(for: CGPoint(x: 0.85, y: 0.5)).x.isApproximately(1, accuracy: 0.04), "decoded horizontal end")
        require(decoded.normalizedPoint(for: CGPoint(x: 0.5, y: 0.25)).y.isApproximately(0, accuracy: 0.04), "decoded top rail")
        require(decoded.normalizedPoint(for: CGPoint(x: 0.5, y: 0.75)).y.isApproximately(1, accuracy: 0.04), "decoded bottom rail")
    }

    private static func testProfileKeepsUnclampedControlDeltasOutsideTrainedRange() throws {
        let profile = try MotionCalibrationProfileBuilder().build(
            horizontalSamples: straightLine(from: CGPoint(x: 0.20, y: 0.5), to: CGPoint(x: 0.80, y: 0.5)),
            verticalSamples: straightLine(from: CGPoint(x: 0.5, y: 0.80), to: CGPoint(x: 0.5, y: 0.20)),
            createdAt: 42
        )

        let clampedA = profile.normalizedPoint(for: CGPoint(x: 0.92, y: 0.50))
        let clampedB = profile.normalizedPoint(for: CGPoint(x: 0.96, y: 0.50))
        require(clampedA.x.isApproximately(1, accuracy: 0.001), "absolute profile should clamp first outside X")
        require(clampedB.x.isApproximately(1, accuracy: 0.001), "absolute profile should clamp second outside X")

        let relativeA = profile.relativeControlPoint(for: CGPoint(x: 0.92, y: 0.50))
        let relativeB = profile.relativeControlPoint(for: CGPoint(x: 0.96, y: 0.50))
        require(relativeB.x - relativeA.x > 0.04, "relative profile lost horizontal delta outside trained range")
    }

    private static func testDisplayMapperUsesSurfaceCalibration() throws {
        let suiteName = "AirHand.CalibrationChecks.\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suiteName) else {
            fatalError("Could not create isolated defaults")
        }
        defer {
            defaults.removePersistentDomain(forName: suiteName)
        }

        let profile = try MotionCalibrationProfileBuilder().buildSurface(
            topSamples: straightLine(from: CGPoint(x: 0.20, y: 0.25), to: CGPoint(x: 0.80, y: 0.25)),
            middleSamples: straightLine(from: CGPoint(x: 0.20, y: 0.50), to: CGPoint(x: 0.80, y: 0.50)),
            bottomSamples: straightLine(from: CGPoint(x: 0.20, y: 0.75), to: CGPoint(x: 0.80, y: 0.75)),
            createdAt: 42
        )
        defaults.set(try profile.encodedString(), forKey: "AirHand.motionCalibrationJSON")
        defaults.set(true, forKey: "AirHand.useCalibration")
        defaults.set(true, forKey: "AirHand.calibrationRecorded")
        defaults.set(ControlMode.smartWalk.rawValue, forKey: "AirHand.controlModeID")
        defaults.set(6, forKey: "AirHand.tuningVersion")
        defaults.set(1, forKey: "AirHand.controlStrategyVersion")

        let settings = AppSettings(defaults: defaults)
        let mapper = DisplayMapper()
        let left = mapper.normalizedControlPoint(CGPoint(x: 0.20, y: 0.50), settings: settings)
        let middleTop = mapper.normalizedControlPoint(CGPoint(x: 0.50, y: 0.25), settings: settings)
        let rightBottom = mapper.normalizedControlPoint(CGPoint(x: 0.80, y: 0.75), settings: settings)

        require(left.x.isApproximately(0, accuracy: 0.04), "surface mapper left x")
        require(left.y.isApproximately(0.5, accuracy: 0.04), "surface mapper middle y")
        require(middleTop.x.isApproximately(0.5, accuracy: 0.04), "surface mapper top x")
        require(middleTop.y.isApproximately(0, accuracy: 0.04), "surface mapper top y")
        require(rightBottom.x.isApproximately(1, accuracy: 0.04), "surface mapper right x")
        require(rightBottom.y.isApproximately(1, accuracy: 0.04), "surface mapper bottom y")
    }

    private static func testTrainingProfileLearnsRecordedCursorPath() throws {
        let target = (0..<90).map { index in
            let t = CGFloat(index) / 89
            return CGPoint(x: 0.08 + 0.84 * t, y: 0.46 + sin(t * .pi * 2) * 0.08)
        }
        let examples = (0..<8).map { take in
            MotionTrainingExample(
                targetPath: target,
                handPath: (0..<90).map { index in
                    let t = CGFloat(index) / 89
                    let takeOffset = CGFloat(take - 4) * 0.002
                    return CGPoint(
                        x: 0.22 + 0.52 * t + sin(t * .pi) * 0.045 + takeOffset,
                        y: 0.62 - 0.18 * t + cos(t * .pi * 2) * 0.035 - takeOffset
                    )
                }
            )
        }

        let profile = try MotionTrainingProfileBuilder().build(examples: examples, createdAt: 42)
        let quarterHand = CGPoint(
            x: 0.22 + 0.52 * 0.25 + sin(0.25 * .pi) * 0.045,
            y: 0.62 - 0.18 * 0.25 + cos(0.25 * .pi * 2) * 0.035
        )
        let threeQuarterHand = CGPoint(
            x: 0.22 + 0.52 * 0.75 + sin(0.75 * .pi) * 0.045,
            y: 0.62 - 0.18 * 0.75 + cos(0.75 * .pi * 2) * 0.035
        )

        let quarter = profile.normalizedPoint(for: quarterHand)
        let threeQuarter = profile.normalizedPoint(for: threeQuarterHand)
        require(quarter.x.isApproximately(0.29, accuracy: 0.07), "trained profile did not learn quarter x")
        require(threeQuarter.x.isApproximately(0.71, accuracy: 0.07), "trained profile did not learn three-quarter x")
        require(abs(quarter.y - threeQuarter.y) > 0.06, "trained profile lost target path curvature")
    }

    private static func testTrainingProfileLearnsVelocityTransform() throws {
        let examples = (0..<5).map { take in
            let offset = CGFloat(take - 2) * 0.0015
            return MotionTrainingExample(
                targetPath: straightLine(
                    from: CGPoint(x: 0.10, y: 0.44),
                    to: CGPoint(x: 0.85, y: 0.64)
                ),
                handPath: straightLine(
                    from: CGPoint(x: 0.24 + offset, y: 0.66 - offset),
                    to: CGPoint(x: 0.49 + offset, y: 0.56 - offset)
                )
            )
        }

        let profile = try MotionTrainingProfileBuilder().build(examples: examples, createdAt: 42)
        let predicted = profile.predictedTargetDelta(
            forHandDelta: CGPoint(x: 0.010, y: -0.004),
            fromHand: CGPoint(x: 0.36, y: 0.61)
        )

        require(predicted.x.isApproximately(0.030, accuracy: 0.006), "trained profile did not learn horizontal speed gain")
        require(predicted.y.isApproximately(0.008, accuracy: 0.004), "trained profile did not learn vertical speed gain")
        require(profile.motionQuality > 0.82, "trained profile velocity quality too low")
    }

    private static func testLearnedMotionControllerPreservesRecordedMotionSpeed() throws {
        let examples = (0..<5).map { take in
            let offset = CGFloat(take - 2) * 0.001
            return MotionTrainingExample(
                targetPath: straightLine(
                    from: CGPoint(x: 0.10, y: 0.50),
                    to: CGPoint(x: 0.90, y: 0.50)
                ),
                handPath: straightLine(
                    from: CGPoint(x: 0.30 + offset, y: 0.55),
                    to: CGPoint(x: 0.50 + offset, y: 0.55)
                )
            )
        }

        let profile = try MotionTrainingProfileBuilder().build(examples: examples, createdAt: 42)
        var controller = LearnedMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = LearnedMotionController.Settings(
            smoothing: 0.70,
            sensitivity: 0.50,
            deadZonePixels: 8,
            maxStepPixels: 420
        )

        var cursor = controller.update(
            hand: CGPoint(x: 0.30, y: 0.55),
            profile: profile,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )
        let startX = cursor.x

        for frame in 1...8 {
            let previous = cursor
            cursor = controller.update(
                hand: CGPoint(x: 0.30 + CGFloat(frame) * 0.005, y: 0.55),
                profile: profile,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
            let step = cursor.x - previous.x
            require(step >= 82 && step <= 145, "learned controller did not preserve recorded speed")
        }

        require(cursor.x - startX > 720, "learned controller lagged behind trained movement")
    }

    private static func testLearnedMotionControllerRejectsTinyJitter() throws {
        let examples = (0..<4).map { take in
            let offset = CGFloat(take) * 0.001
            return MotionTrainingExample(
                targetPath: straightLine(
                    from: CGPoint(x: 0.12, y: 0.48),
                    to: CGPoint(x: 0.88, y: 0.52)
                ),
                handPath: straightLine(
                    from: CGPoint(x: 0.28 + offset, y: 0.58),
                    to: CGPoint(x: 0.52 + offset, y: 0.56)
                )
            )
        }

        let profile = try MotionTrainingProfileBuilder().build(examples: examples, createdAt: 42)
        var controller = LearnedMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = LearnedMotionController.Settings(
            smoothing: 0.78,
            sensitivity: 0.45,
            deadZonePixels: 10,
            maxStepPixels: 420
        )
        let centerHand = CGPoint(x: 0.40, y: 0.57)
        var cursor = controller.update(
            hand: centerHand,
            profile: profile,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )
        let start = cursor

        for frame in 1...180 {
            let jitter = CGPoint(
                x: centerHand.x + sin(CGFloat(frame) * 0.71) * 0.00018,
                y: centerHand.y + cos(CGFloat(frame) * 0.83) * 0.00016
            )
            cursor = controller.update(
                hand: jitter,
                profile: profile,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.distance(to: start) < 2.0, "learned controller drifted while hand only jittered")
    }

    private static func testLearnedMotionControllerStartsCoherentMicroAim() throws {
        let examples = (0..<4).map { take in
            let offset = CGFloat(take) * 0.001
            return MotionTrainingExample(
                targetPath: straightLine(
                    from: CGPoint(x: 0.10, y: 0.50),
                    to: CGPoint(x: 0.90, y: 0.50)
                ),
                handPath: straightLine(
                    from: CGPoint(x: 0.30 + offset, y: 0.55),
                    to: CGPoint(x: 0.50 + offset, y: 0.55)
                )
            )
        }

        let profile = try MotionTrainingProfileBuilder().build(examples: examples, createdAt: 42)
        var controller = LearnedMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = LearnedMotionController.Settings(
            smoothing: 0.78,
            sensitivity: 0.45,
            deadZonePixels: 10,
            maxStepPixels: 420
        )

        let startHand = CGPoint(x: 0.34, y: 0.55)
        var cursor = controller.update(
            hand: startHand,
            profile: profile,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )
        let start = cursor

        for frame in 1...42 {
            cursor = controller.update(
                hand: CGPoint(x: startHand.x + CGFloat(frame) * 0.00009, y: startHand.y),
                profile: profile,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.x - start.x > 34, "learned micro aim was swallowed by stationary gate")
        require(abs(cursor.y - start.y) < 2.0, "learned micro aim drifted vertically")
    }

    private static func testTrainingProfileRoundTripsThroughJSON() throws {
        let target = straightLine(from: CGPoint(x: 0.10, y: 0.40), to: CGPoint(x: 0.90, y: 0.60))
        let examples = (0..<4).map { take in
            MotionTrainingExample(
                targetPath: target,
                handPath: straightLine(
                    from: CGPoint(x: 0.20 + CGFloat(take) * 0.002, y: 0.70),
                    to: CGPoint(x: 0.78 + CGFloat(take) * 0.002, y: 0.30)
                )
            )
        }

        let profile = try MotionTrainingProfileBuilder().build(examples: examples, createdAt: 42)
        let encoded = try profile.encodedString()
        guard let decoded = MotionTrainingProfile(encodedString: encoded) else {
            fatalError("Could not decode training profile")
        }

        let mapped = decoded.normalizedPoint(for: CGPoint(x: 0.49, y: 0.50))
        require(mapped.x.isApproximately(0.50, accuracy: 0.08), "decoded training profile x")
        require(mapped.y.isApproximately(0.50, accuracy: 0.08), "decoded training profile y")
    }

    private static func testSettingsMigratesCalibratedDirectModeToSmartWalk() throws {
        let suiteName = "AirHand.CalibrationChecks.\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suiteName) else {
            fatalError("Could not create isolated defaults")
        }
        defer {
            defaults.removePersistentDomain(forName: suiteName)
        }

        let profile = try MotionCalibrationProfileBuilder().buildSurface(
            topSamples: straightLine(from: CGPoint(x: 0.15, y: 0.25), to: CGPoint(x: 0.85, y: 0.25)),
            middleSamples: straightLine(from: CGPoint(x: 0.15, y: 0.5), to: CGPoint(x: 0.85, y: 0.5)),
            bottomSamples: straightLine(from: CGPoint(x: 0.15, y: 0.75), to: CGPoint(x: 0.85, y: 0.75)),
            createdAt: 42
        )
        defaults.set(try profile.encodedString(), forKey: "AirHand.motionCalibrationJSON")
        defaults.set(true, forKey: "AirHand.useCalibration")
        defaults.set(true, forKey: "AirHand.calibrationRecorded")
        defaults.set(ControlMode.direct.rawValue, forKey: "AirHand.controlModeID")
        defaults.set(6, forKey: "AirHand.tuningVersion")

        let settings = AppSettings(defaults: defaults)

        require(settings.shouldUseCalibration, "saved motion calibration was not loaded")
        require(settings.controlModeID == ControlMode.smartWalk.rawValue, "calibrated direct mode was not migrated to Smart Walk")
    }

    private static func testTrainingStopPoseAcceptsClosedPalmEvenWhenGestureClassifierCallsItLeftClick() {
        let curls = FingerCurls(thumb: 0.35, index: 0.72, middle: 0.70, ring: 0.68, little: 0.66)
        let pose = MotionCalibrationPoseClassifier.pose(gesture: .leftClick, curls: curls)
        require(pose == .closedPalm, "closed palm ends training even when classified as left click")
    }

    private static func testCalibratedCursorFilterDoesNotDriftPastStationaryTarget() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.76,
            sensitivity: 0.55,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 28,
            noiseRadiusPixels: 0,
            maxStepPixels: 170
        )
        let start = CGPoint(x: 200, y: 500)
        let target = CGPoint(x: 1_400, y: 500)
        var cursor = filter.update(raw: start, now: 0, freeze: false, settings: settings)

        for frame in 1...180 {
            cursor = filter.update(raw: target, now: Double(frame) / 60.0, freeze: false, settings: settings)
            require(cursor.x <= target.x + 0.001, "calibrated cursor overshot stationary target")
            require(cursor.y.isApproximately(target.y, accuracy: 0.001), "calibrated cursor drifted vertically")
        }

        require(cursor.x > 1_240, "calibrated cursor did not approach target")

        let frozen = filter.update(raw: CGPoint(x: 1_800, y: 900), now: 4, freeze: true, settings: settings)
        require(frozen.distance(to: cursor) < 0.001, "calibrated cursor moved while frozen")
    }

    private static func testCalibratedCursorFilterSoftensSmallAimingCorrections() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.76,
            sensitivity: 0.55,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 28,
            noiseRadiusPixels: 0,
            maxStepPixels: 170
        )

        let start = CGPoint(x: 700, y: 600)
        let smallTarget = CGPoint(x: 790, y: 600)
        var cursor = filter.update(raw: start, now: 0, freeze: false, settings: settings)
        cursor = filter.update(raw: smallTarget, now: 1.0 / 60.0, freeze: false, settings: settings)
        require(cursor.x - start.x >= 25, "small correction did not start moving")
        require(cursor.x - start.x <= 75, "small correction snapped too sharply in one frame")

        for frame in 2...30 {
            cursor = filter.update(raw: smallTarget, now: Double(frame) / 60.0, freeze: false, settings: settings)
        }
        require(cursor.x > smallTarget.x - 18, "small correction never settled near target")
        require(cursor.x <= smallTarget.x + 0.001, "small correction overshot target")
    }

    private static func testCalibratedCursorFilterRejectsPalmRotationJitter() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.76,
            sensitivity: 0.55,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 28,
            noiseRadiusPixels: 0,
            maxStepPixels: 170
        )

        let center = CGPoint(x: 700, y: 600)
        var cursor = filter.update(raw: center, now: 0, freeze: false, settings: settings)
        for frame in 1...90 {
            let offset: CGFloat = frame.isMultiple(of: 2) ? 12 : -12
            let raw = CGPoint(x: center.x + offset, y: center.y + sin(CGFloat(frame)) * 10)
            cursor = filter.update(raw: raw, now: Double(frame) / 60.0, freeze: false, settings: settings)
            require(abs(cursor.x - center.x) <= 22, "palm rotation jitter moved cursor too far horizontally")
            require(abs(cursor.y - center.y) <= 8, "palm rotation jitter moved cursor too far vertically")
        }
    }

    private static func testCalibratedCursorFilterCapsSingleFrameHalfScreenJump() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.76,
            sensitivity: 0.55,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 28,
            noiseRadiusPixels: 0,
            maxStepPixels: 170
        )

        let start = CGPoint(x: 700, y: 600)
        _ = filter.update(raw: start, now: 0, freeze: false, settings: settings)
        let cursor = filter.update(raw: CGPoint(x: 3_200, y: 1_100), now: 1.0 / 60.0, freeze: false, settings: settings)

        require(cursor.distance(to: start) <= 175, "single-frame screen outlier moved too far")
    }

    private static func testCalibratedCursorFilterRejectsZeroMeanStationaryNoise() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.76,
            sensitivity: 0.55,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 28,
            noiseRadiusPixels: 0,
            maxStepPixels: 170
        )

        let center = CGPoint(x: 1_000, y: 700)
        var cursor = filter.update(raw: center, now: 0, freeze: false, settings: settings)
        var maxExcursion: CGFloat = 0
        for frame in 1...240 {
            let raw = CGPoint(
                x: center.x + sin(CGFloat(frame) * 0.9) * 6 + sin(CGFloat(frame) * 0.31) * 3,
                y: center.y + cos(CGFloat(frame) * 0.7) * 4
            )
            cursor = filter.update(raw: raw, now: Double(frame) / 60.0, freeze: false, settings: settings)
            maxExcursion = max(maxExcursion, cursor.distance(to: center))
        }

        require(maxExcursion < 4.0, "zero-mean stationary noise leaked into the cursor")
        require(cursor.distance(to: center) < 2.0, "zero-mean stationary noise drifted cursor")
    }

    private static func testCalibratedCursorFilterRespondsQuicklyToRealMotion() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.76,
            sensitivity: 0.55,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 28,
            noiseRadiusPixels: 0,
            maxStepPixels: 260
        )

        let start = CGPoint(x: 700, y: 600)
        let target = CGPoint(x: 1_900, y: 650)
        var cursor = filter.update(raw: start, now: 0, freeze: false, settings: settings)
        for frame in 1...10 {
            cursor = filter.update(raw: target, now: Double(frame) / 60.0, freeze: false, settings: settings)
        }

        require(cursor.x > 1_650, "real screen motion lagged too much")
    }

    private static func testCalibratedCursorFilterPreservesContinuousMotionVelocity() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.76,
            sensitivity: 0.55,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 28,
            noiseRadiusPixels: 0,
            maxStepPixels: 260
        )

        var previous = filter.update(raw: CGPoint(x: 500, y: 500), now: 0, freeze: false, settings: settings)
        for frame in 1...12 {
            let raw = CGPoint(x: 500 + frame * 100, y: 500)
            let cursor = filter.update(raw: raw, now: Double(frame) / 60.0, freeze: false, settings: settings)
            let cursorDelta = cursor.x - previous.x
            if frame >= 4 {
                require(cursorDelta >= 78 && cursorDelta <= 122, "cursor velocity did not match hand velocity")
            }
            previous = cursor
        }
    }

    private static func testCalibratedCursorFilterDoesNotSwallowSlowAbsoluteMotion() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            noiseRadiusPixels: 0,
            maxStepPixels: 280
        )

        let start = CGPoint(x: 1_000, y: 700)
        var cursor = filter.update(raw: start, now: 0, freeze: false, settings: settings)
        for frame in 1...90 {
            cursor = filter.update(
                raw: CGPoint(x: start.x + CGFloat(frame) * 2, y: start.y),
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.x - start.x > 130, "slow absolute movement was swallowed")
        require(cursor.x - start.x < 190, "slow absolute movement overshot target")
    }

    private static func testCalibratedCursorFilterMovesContinuouslyDuringSlowAbsoluteMotion() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            noiseRadiusPixels: 0,
            maxStepPixels: 280
        )

        let start = CGPoint(x: 1_000, y: 700)
        var cursor = filter.update(raw: start, now: 0, freeze: false, settings: settings)
        var movingFrames = 0
        for frame in 1...120 {
            let previous = cursor
            cursor = filter.update(
                raw: CGPoint(x: start.x + CGFloat(frame) * 1.6, y: start.y),
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
            if cursor.distance(to: previous) > 0.1 {
                movingFrames += 1
            }
        }

        require(movingFrames > 80, "slow absolute movement advanced in chunks instead of continuously")
    }

    private static func testCalibratedCursorFilterUsesAdaptiveSmoothingForFastMotion() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            noiseRadiusPixels: 0,
            maxStepPixels: 360
        )

        var cursor = filter.update(raw: CGPoint(x: 800, y: 700), now: 0, freeze: false, settings: settings)
        for frame in 1...8 {
            cursor = filter.update(
                raw: CGPoint(x: 800 + CGFloat(frame) * 180, y: 700),
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.x > 1_900, "fast absolute movement lagged too much")
        require(cursor.x < 2_260, "fast absolute movement overshot")
    }

    private static func testCalibratedCursorFilterStartsCoherentMicroAimWithoutUnlockDelay() {
        var filter = CalibratedCursorFilter()
        let settings = CalibratedCursorFilter.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            noiseRadiusPixels: 0,
            maxStepPixels: 280
        )

        let start = CGPoint(x: 1_000, y: 700)
        var cursor = filter.update(raw: start, now: 0, freeze: false, settings: settings)
        for frame in 1...40 {
            let offset: CGFloat = frame.isMultiple(of: 2) ? 10 : -10
            cursor = filter.update(
                raw: CGPoint(x: start.x + offset, y: start.y + sin(CGFloat(frame)) * 5),
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }
        require(cursor.distance(to: start) < 3.0, "alternating tremor leaked into calibrated cursor")

        for frame in 41...58 {
            cursor = filter.update(
                raw: CGPoint(x: start.x + CGFloat(frame - 40) * 1.25, y: start.y),
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.x - start.x > 9, "coherent micro aim waited for a fixed unlock radius")
        require(abs(cursor.y - start.y) < 2, "coherent micro aim drifted vertically")
    }

    private static func testHandPoseStabilizerIgnoresPoseChangeWithoutTranslation() {
        var stabilizer = HandPoseStabilizer()
        let first = HandPoseMeasurement(controlPoint: CGPoint(x: 0.50, y: 0.50), palmWidth: 0.26, palmLength: 0.24, palmAngle: 0)
        var point = stabilizer.update(measurement: first, now: 0, freeze: false)

        for frame in 1...60 {
            let width = CGFloat(0.26 - 0.10 * min(Double(frame) / 12.0, 1))
            let angle = CGFloat(0.22 * min(Double(frame) / 12.0, 1))
            let measurement = HandPoseMeasurement(
                controlPoint: CGPoint(x: 0.50, y: 0.50),
                palmWidth: width,
                palmLength: 0.21,
                palmAngle: angle
            )
            point = stabilizer.update(measurement: measurement, now: Double(frame) / 60.0, freeze: false)
        }

        require(abs(point.x - 0.50) < 0.002, "pose change injected horizontal motion")
        require(abs(point.y - 0.50) < 0.002, "pose change injected vertical motion")
    }

    private static func testHandPoseStabilizerAllowsRealMotionAfterPoseChange() {
        var stabilizer = HandPoseStabilizer()
        _ = stabilizer.update(
            measurement: HandPoseMeasurement(controlPoint: CGPoint(x: 0.50, y: 0.50), palmWidth: 0.26, palmLength: 0.24, palmAngle: 0),
            now: 0,
            freeze: false
        )
        _ = stabilizer.update(
            measurement: HandPoseMeasurement(controlPoint: CGPoint(x: 0.56, y: 0.50), palmWidth: 0.16, palmLength: 0.21, palmAngle: 0.22),
            now: 1.0 / 60.0,
            freeze: false
        )

        var point = CGPoint(x: 0.50, y: 0.50)
        for frame in 2...90 {
            let t = CGFloat(frame - 2) / 88
            let measurement = HandPoseMeasurement(
                controlPoint: CGPoint(x: 0.56 + 0.18 * t, y: 0.50),
                palmWidth: 0.16,
                palmLength: 0.21,
                palmAngle: 0.22
            )
            point = stabilizer.update(measurement: measurement, now: Double(frame) / 60.0, freeze: false)
        }

        require(point.x > 0.65, "real motion after pose change was blocked")
    }

    private static func testHandPoseStabilizerRejectsSingleFrameLandmarkOutlier() {
        var stabilizer = HandPoseStabilizer()
        let stable = HandPoseMeasurement(controlPoint: CGPoint(x: 0.50, y: 0.50), palmWidth: 0.24, palmLength: 0.22, palmAngle: 0)
        _ = stabilizer.update(measurement: stable, now: 0, freeze: false)
        for frame in 1...8 {
            _ = stabilizer.update(measurement: stable, now: Double(frame) / 60.0, freeze: false)
        }

        let outlier = HandPoseMeasurement(controlPoint: CGPoint(x: 0.94, y: 0.18), palmWidth: 0.24, palmLength: 0.22, palmAngle: 0)
        let point = stabilizer.update(measurement: outlier, now: 9.0 / 60.0, freeze: false)

        require(point.distance(to: CGPoint(x: 0.50, y: 0.50)) < 0.035, "single-frame landmark outlier moved hand tracker")
    }

    private static func testHandPoseStabilizerAcceptsSustainedLargeMotion() {
        var stabilizer = HandPoseStabilizer()
        let start = HandPoseMeasurement(controlPoint: CGPoint(x: 0.30, y: 0.50), palmWidth: 0.24, palmLength: 0.22, palmAngle: 0)
        _ = stabilizer.update(measurement: start, now: 0, freeze: false)

        var point = CGPoint(x: 0.30, y: 0.50)
        for frame in 1...90 {
            let t = CGFloat(frame) / 90
            let measurement = HandPoseMeasurement(
                controlPoint: CGPoint(x: 0.30 + 0.46 * t, y: 0.50),
                palmWidth: 0.24,
                palmLength: 0.22,
                palmAngle: 0
            )
            point = stabilizer.update(measurement: measurement, now: Double(frame) / 60.0, freeze: false)
        }

        require(point.x > 0.66, "sustained large motion was blocked by jump guard")
    }

    private static func testHandPoseStabilizerRespondsQuicklyToContinuousMotion() {
        var stabilizer = HandPoseStabilizer()
        let start = HandPoseMeasurement(controlPoint: CGPoint(x: 0.30, y: 0.50), palmWidth: 0.24, palmLength: 0.22, palmAngle: 0)
        _ = stabilizer.update(measurement: start, now: 0, freeze: false)

        var point = CGPoint(x: 0.30, y: 0.50)
        for frame in 1...10 {
            let t = CGFloat(frame) / 10
            let measurement = HandPoseMeasurement(
                controlPoint: CGPoint(x: 0.30 + 0.22 * t, y: 0.50),
                palmWidth: 0.24,
                palmLength: 0.22,
                palmAngle: 0
            )
            point = stabilizer.update(measurement: measurement, now: Double(frame) / 60.0, freeze: false)
        }

        require(point.x > 0.47, "continuous hand motion lagged too much")
    }

    private static func testRelativeCursorMotionDoesNotDriftWhenPalmIsStill() {
        var controller = RelativeCursorMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = RelativeCursorMotionController.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            maxStepPixels: 280
        )
        let fallback = CGPoint(x: 3_000, y: 700)
        var cursor = controller.update(
            controlPoint: CGPoint(x: 0.50, y: 0.50),
            fallbackCursor: fallback,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )

        for frame in 1...180 {
            let jitter = CGPoint(
                x: 0.50 + sin(CGFloat(frame) * 0.91) * 0.00055,
                y: 0.50 + cos(CGFloat(frame) * 0.73) * 0.00045
            )
            cursor = controller.update(
                controlPoint: jitter,
                fallbackCursor: fallback,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.distance(to: fallback) < 1.0, "relative cursor drifted while palm only jittered")
    }

    private static func testRelativeCursorMotionAccumulatesSlowIntentionalMovement() {
        var controller = RelativeCursorMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = RelativeCursorMotionController.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            maxStepPixels: 280
        )
        let fallback = CGPoint(x: 3_000, y: 700)
        var cursor = controller.update(
            controlPoint: CGPoint(x: 0.50, y: 0.50),
            fallbackCursor: fallback,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )

        for frame in 1...90 {
            cursor = controller.update(
                controlPoint: CGPoint(x: 0.50 + CGFloat(frame) * (2.0 / 6_000.0), y: 0.50),
                fallbackCursor: fallback,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.x - fallback.x > 145, "slow intentional horizontal movement was swallowed")
        require(cursor.x - fallback.x < 205, "slow intentional horizontal movement was over-amplified")
    }

    private static func testRelativeCursorMotionContinuesAfterSlowIntentLocks() {
        var controller = RelativeCursorMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = RelativeCursorMotionController.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            maxStepPixels: 280
        )
        let fallback = CGPoint(x: 3_000, y: 700)
        var cursor = controller.update(
            controlPoint: CGPoint(x: 0.50, y: 0.50),
            fallbackCursor: fallback,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )

        var movingFrames = 0
        for frame in 1...90 {
            let previous = cursor
            cursor = controller.update(
                controlPoint: CGPoint(x: 0.50 + CGFloat(frame) * (2.0 / 6_000.0), y: 0.50),
                fallbackCursor: fallback,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
            if cursor.distance(to: previous) > 0.1 {
                movingFrames += 1
            }
        }

        require(movingFrames > 68, "slow intentional movement advanced in visible chunks instead of continuously")
    }

    private static func testRelativeCursorMotionPreservesHandVelocity() {
        var controller = RelativeCursorMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = RelativeCursorMotionController.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            maxStepPixels: 280
        )
        var cursor = controller.update(
            controlPoint: CGPoint(x: 0.30, y: 0.50),
            fallbackCursor: CGPoint(x: 1_800, y: 700),
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )

        var previous = cursor
        for frame in 1...12 {
            let controlPoint = CGPoint(x: 0.30 + CGFloat(frame) * (100.0 / 6_000.0), y: 0.50)
            cursor = controller.update(
                controlPoint: controlPoint,
                fallbackCursor: CGPoint(x: 1_800, y: 700),
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
            let step = cursor.x - previous.x
            if frame >= 4 {
                require(step >= 82 && step <= 112, "relative cursor speed did not match hand speed")
            }
            previous = cursor
        }
    }

    private static func testRelativeCursorMotionDoesNotJumpAfterFreeze() {
        var controller = RelativeCursorMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = RelativeCursorMotionController.Settings(
            smoothing: 0.72,
            sensitivity: 0.52,
            deadZonePixels: 8,
            stabilizationRadiusPixels: 30,
            maxStepPixels: 280
        )
        let fallback = CGPoint(x: 2_000, y: 700)
        var cursor = controller.update(
            controlPoint: CGPoint(x: 0.40, y: 0.50),
            fallbackCursor: fallback,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )
        cursor = controller.update(
            controlPoint: CGPoint(x: 0.56, y: 0.50),
            fallbackCursor: fallback,
            previousCursor: cursor,
            desktop: desktop,
            now: 1.0 / 60.0,
            freeze: true,
            settings: settings
        )
        let afterRelease = controller.update(
            controlPoint: CGPoint(x: 0.56, y: 0.50),
            fallbackCursor: fallback,
            previousCursor: cursor,
            desktop: desktop,
            now: 2.0 / 60.0,
            freeze: false,
            settings: settings
        )

        require(afterRelease.distance(to: cursor) < 0.001, "relative cursor jumped after freeze release")
    }

    private static func testRelativeCursorMotionDampsTremorWithoutSwallowingMicroAim() {
        var controller = RelativeCursorMotionController()
        let desktop = CGRect(x: 0, y: 0, width: 6_000, height: 1_400)
        let settings = RelativeCursorMotionController.Settings(
            smoothing: 0.84,
            sensitivity: 0.40,
            deadZonePixels: 11,
            stabilizationRadiusPixels: 42,
            maxStepPixels: 280
        )
        let fallback = CGPoint(x: 3_000, y: 700)
        var cursor = controller.update(
            controlPoint: CGPoint(x: 0.50, y: 0.50),
            fallbackCursor: fallback,
            previousCursor: nil,
            desktop: desktop,
            now: 0,
            freeze: false,
            settings: settings
        )

        for frame in 1...60 {
            let offset = CGFloat(frame.isMultiple(of: 2) ? 8 : -8) / desktop.width
            cursor = controller.update(
                controlPoint: CGPoint(x: 0.50 + offset, y: 0.50),
                fallbackCursor: fallback,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }
        require(cursor.distance(to: fallback) < 2.5, "relative tremor leaked into cursor")

        let afterTremor = cursor
        for frame in 61...78 {
            let offset = CGFloat(frame - 60) * 2.0 / desktop.width
            cursor = controller.update(
                controlPoint: CGPoint(x: 0.50 + offset, y: 0.50),
                fallbackCursor: fallback,
                previousCursor: cursor,
                desktop: desktop,
                now: Double(frame) / 60.0,
                freeze: false,
                settings: settings
            )
        }

        require(cursor.x - afterTremor.x > 18, "relative micro aim was swallowed by fixed intent gate")
        require(abs(cursor.y - afterTremor.y) < 1.5, "relative micro aim drifted vertically")
    }

    private static func testGestureClassifierMapsThreeClosedFingersToAim() {
        let curls = FingerCurls(thumb: 0.18, index: 0.24, middle: 0.72, ring: 0.74, little: 0.76)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture == .aim, "three closed support fingers did not become aim mode")
    }

    private static func testGestureClassifierMapsIndexCloseToLeftClick() {
        let curls = FingerCurls(thumb: 0.18, index: 0.70, middle: 0.74, ring: 0.76, little: 0.78)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture == .leftClick, "closing index from aim pose did not become left click")
    }

    private static func testGestureClassifierMapsOpenPalmToPause() {
        let curls = FingerCurls(thumb: 0.18, index: 0.22, middle: 0.20, ring: 0.22, little: 0.24)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture == .openPalm, "open palm did not become pause pose")
    }

    private static func testGestureClassifierDoesNotMapThumbFoldToAim() {
        let curls = FingerCurls(thumb: 0.78, index: 0.24, middle: 0.24, ring: 0.30, little: 0.32)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture != .aim && gesture != .leftClick, "thumb fold became active pointer gesture")
    }

    private static func testGestureClassifierDoesNotMapMiddlePinchToRightClick() {
        let curls = FingerCurls(thumb: 0.18, index: 0.22, middle: 0.20, ring: 0.22, little: 0.24)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0.16, middlePinchScore: 0.86)
        require(gesture != .rightClick, "middle pinch still became right click")
    }

    private static func testGestureClassifierDoesNotMapFourFingerFistToRightClick() {
        let curls = FingerCurls(thumb: 0.45, index: 0.86, middle: 0.85, ring: 0.84, little: 0.83)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture != .rightClick, "four-finger fist became right click")
    }

    private static func testGestureClassifierMapsTwoFingersToMissionControl() {
        let curls = FingerCurls(thumb: 0.30, index: 0.20, middle: 0.22, ring: 0.70, little: 0.72)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture == .missionControl, "index + middle up did not become Mission Control")
    }

    private static func testGestureClassifierMapsTuckedFistToScroll() {
        let curls = FingerCurls(thumb: 0.80, index: 0.82, middle: 0.84, ring: 0.83, little: 0.85)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture == .fist, "tucked-thumb closed hand did not become scroll fist")
    }

    private static func testGestureClassifierKeepsThumbOutFoldAsLeftClick() {
        let curls = FingerCurls(thumb: 0.30, index: 0.82, middle: 0.84, ring: 0.83, little: 0.85)
        let gesture = GestureIntentClassifier.classify(curls: curls, indexPinchScore: 0, middlePinchScore: 0)
        require(gesture == .leftClick, "thumb-out index fold should stay a left click, not a scroll fist")
    }

    private static func testGestureClassifierKeepsAimThroughMarginalCurls() {
        let marginal = FingerCurls(thumb: 0.68, index: 0.24, middle: 0.48, ring: 0.50, little: 0.47)
        let stayed = GestureIntentClassifier.classify(curls: marginal, indexPinchScore: 0, middlePinchScore: 0, previous: .aim)
        require(stayed == .aim, "marginal curls kicked an active aim pose to none")
        let entered = GestureIntentClassifier.classify(curls: marginal, indexPinchScore: 0, middlePinchScore: 0, previous: .none)
        require(entered == .none, "marginal curls should not be enough to ENTER aim")
    }

    private static func testFingerThrottleMapsExtensionToSpeed() {
        require(FingerThrottle.speedFactor(indexCurl: 0.05) > 0.97, "extended finger should give full speed")
        require(abs(FingerThrottle.speedFactor(indexCurl: 0.50) - FingerThrottle.minFactor) < 0.02, "curled finger should give the precision crawl")
        var previous = 1.0
        for step in 0...20 {
            let factor = FingerThrottle.speedFactor(indexCurl: Double(step) * 0.03)
            require(factor <= previous + 0.000_1, "finger throttle must decrease monotonically with curl")
            previous = factor
        }
    }

    private static func testClickIntentRejectsPostureDrift() {
        var detector = ClickIntentDetector()
        var clicks = 0
        var wasClicking = false
        for frame in 0..<22 {
            let curl = 0.20 + 0.50 * Double(frame) / 21
            let clicking = detector.update(indexCurl: curl)
            if clicking, !wasClicking { clicks += 1 }
            wasClicking = clicking
        }
        require(clicks == 0, "gradual posture drift was misread as a click")
    }

    private static func testGaitCompensationCancelsWalkingBobButKeepsAim() {
        let fps = 30.0
        let dt = 1.0 / fps
        func bob(_ t: Double) -> Double { 0.05 * sin(2 * .pi * 1.9 * t) + 0.02 * sin(2 * .pi * 0.95 * t) }
        func sway(_ t: Double) -> Double { 0.035 * sin(2 * .pi * 0.95 * t + 0.5) }

        var samples: [CGPoint] = []
        for index in 0..<Int(fps * 15) {
            let t = Double(index) * dt
            samples.append(CGPoint(x: 0.5 + sway(t), y: 0.5 + bob(t)))
        }
        guard let profile = GaitAnalyzer.analyze(samples: samples, sampleRate: fps) else {
            require(false, "gait analyzer found no pattern in clearly periodic walking data")
            return
        }
        require(abs(profile.fundamentalHz - 0.95) < 0.2, "gait fundamental frequency estimate is off")

        var stabilizer = GaitStabilizer()
        stabilizer.configure(profile: profile)
        func intent(_ t: Double) -> Double { t < 10 ? 0 : min((t - 10) / 1.0, 1) * 0.15 }

        var inputEnergy = 0.0, outputEnergy = 0.0, holdCount = 0
        var startX = 0.0, startCount = 0, endX = 0.0, endCount = 0
        for index in 0..<Int(fps * 20) {
            let t = Double(index) * dt
            let raw = CGPoint(x: 0.5 + sway(t) + intent(t), y: 0.5 + bob(t))
            let out = stabilizer.process(point: raw, now: t)
            if t >= 5, t < 9 {
                inputEnergy += pow(Double(raw.y) - 0.5, 2)
                outputEnergy += pow(Double(out.y) - 0.5, 2)
                holdCount += 1
            }
            if t >= 5, t < 7 { startX += Double(out.x); startCount += 1 }
            if t >= 13, t < 15 { endX += Double(out.x); endCount += 1 }
        }
        let reduction = 1 - (outputEnergy / Double(holdCount)).squareRoot() / (inputEnergy / Double(holdCount)).squareRoot()
        require(reduction > 0.5, "gait compensation did not remove at least half the walking bob")

        let preserved = endX / Double(endCount) - startX / Double(startCount)
        require(abs(preserved - 0.15) < 0.04, "intentional motion was not preserved through gait compensation")
    }

    private static func testAutoGaitEngagesOnWalkingAndReleasesWhenStill() {
        let fps = 30.0
        let dt = 1.0 / fps
        var seed: UInt64 = 0x1234_5678_ABCD_EF01
        func rnd() -> Double {
            seed = seed &* 6364136223846793005 &+ 1442695040888963407
            return Double(seed >> 11) / Double(1 << 53)
        }
        func gauss() -> Double { (rnd() + rnd() + rnd() + rnd() - 2) * 0.7 }
        func bob(_ t: Double) -> Double { 0.04 * sin(2 * .pi * 1.9 * t) + 0.018 * sin(2 * .pi * 0.95 * t) }
        func sway(_ t: Double) -> Double { 0.028 * sin(2 * .pi * 0.95 * t + 0.5) }

        var auto = AutoGaitCanceller()
        var t = 0.0

        while t < 8 {
            _ = auto.process(point: CGPoint(x: 0.5 + gauss() * 0.008, y: 0.5 + gauss() * 0.008), now: t)
            t += dt
        }
        require(auto.detectedHz == nil, "auto gait engaged on a still hand")

        var inputEnergy = 0.0, outputEnergy = 0.0, count = 0
        let walkStart = t
        while t < walkStart + 18 {
            let raw = CGPoint(x: 0.5 + sway(t - walkStart) + gauss() * 0.01, y: 0.5 + bob(t - walkStart) + gauss() * 0.01)
            let out = auto.process(point: raw, now: t)
            if t > walkStart + 10 {
                inputEnergy += pow(Double(raw.y) - 0.5, 2)
                outputEnergy += pow(Double(out.y) - 0.5, 2)
                count += 1
            }
            t += dt
        }
        guard let hz = auto.detectedHz else {
            require(false, "auto gait did not lock onto an obvious walking rhythm")
            return
        }
        require(hz > 0.6 && hz < 2.4, "auto gait locked an implausible walking frequency")
        let reduction = 1 - (outputEnergy / Double(count)).squareRoot() / (inputEnergy / Double(count)).squareRoot()
        require(reduction > 0.5, "auto gait removed less than half the walking bob")

        let stopStart = t
        while t < stopStart + 8 {
            _ = auto.process(point: CGPoint(x: 0.5 + gauss() * 0.008, y: 0.5 + gauss() * 0.008), now: t)
            t += dt
        }
        require(auto.detectedHz == nil, "auto gait kept 'walking' status after the user stopped")
    }

    private static func testClickIntentFiresOnDeliberateTap() {
        var detector = ClickIntentDetector()
        var clicks = 0
        var wasClicking = false
        let sequence = Array(repeating: 0.20, count: 8)
            + [0.5, 0.78, 0.9, 0.9, 0.5, 0.22]
            + Array(repeating: 0.20, count: 5)
        for curl in sequence {
            let clicking = detector.update(indexCurl: curl)
            if clicking, !wasClicking { clicks += 1 }
            wasClicking = clicking
        }
        require(clicks == 1, "a deliberate index tap did not register exactly one click")
    }

    private static func straightLine(from start: CGPoint, to end: CGPoint) -> [CGPoint] {
        (0..<70).map { index in
            let t = CGFloat(index) / 69
            return start + (end - start) * t
        }
    }

    private static func curvedRail(yBase: CGFloat) -> [CGPoint] {
        (0..<80).map { index in
            let t = CGFloat(index) / 79
            return CGPoint(
                x: 0.14 + 0.72 * t + sin(t * .pi * 2.0) * 0.035,
                y: yBase + sin(t * .pi) * 0.08 + cos(t * .pi * 2.0) * 0.018
            )
        }
    }

    nonisolated(unsafe) private static var failures: [String] = []

    private static func require(_ condition: Bool, _ label: String) {
        if !condition { failures.append(label) }
    }

    private static func reportAndExit() {
        if failures.isEmpty {
            print("Calibration checks passed")
            return
        }
        print("Calibration checks FAILED (\(failures.count)):")
        for failure in failures {
            print("  - \(failure)")
        }
        exit(1)
    }
}

private extension CGFloat {
    func isApproximately(_ expected: CGFloat, accuracy: CGFloat) -> Bool {
        abs(self - expected) <= accuracy
    }
}
