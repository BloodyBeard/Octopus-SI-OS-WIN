import AppKit
import AVFoundation
import Combine
import CoreFoundation
import Foundation
import QuartzCore

private enum MotionCalibrationPhase {
    case idle
    case waitingMiddleOpen
    case recordingMiddle
    case waitingTopOpen
    case recordingTop
    case waitingBottomOpen
    case recordingBottom
}

private enum CalibrationCursorTarget {
    case middleStart
    case middleEnd
    case topStart
    case topEnd
    case bottomStart
    case bottomEnd
}

final class AppModel: ObservableObject {
    @Published var settings = AppSettings()
    @Published var snapshot = EngineSnapshot.stopped
    @Published var permissionMessage = ""
    @Published var isCalibrating = false
    @Published var calibrationProgress: Double = 0
    @Published var calibrationMessage = "Not calibrated"
    @Published var trainingMessage = "No training takes"
    @Published var trainingTakeCount = 0
    @Published var isRecordingTargetPath = false
    @Published var isRecordingHandTake = false

    @Published private(set) var droppedFrames: Int = 0

    let camera = CameraService()
    let windowCapture = WindowCaptureService()
    let phoneStream = PhoneStreamService()

    var isRunning: Bool {
        camera.isRunning || windowCapture.isRunning || phoneStream.isRunning
    }

    var activeVideoAspectRatio: Double {
        switch InputSource(rawValue: settings.inputSource) {
        case .window:
            return windowCapture.videoAspectRatio
        case .phone:
            return phoneStream.videoAspectRatio
        default:
            return camera.videoAspectRatio
        }
    }

    private let detector = HandPoseDetector()
    private let engine = GestureEngine()
    private let mouse = MouseDriver()
    private let interfaceBridge = OctopusInterfaceBridge()
    private let magneticTargeting = MagneticTargeting()
    private let hud = HudOverlayController()
    private let processingQueue = DispatchQueue(label: "airhand.vision.processing", qos: .userInteractive)
    private let frameLock = NSLock()
    private var processingFrame = false
    private var nextVisionFrameTime: TimeInterval = 0
    private var lastSnapshotPublishTime: TimeInterval = 0
    private var lastPermissionRefreshTime: TimeInterval = 0
    private var droppedFrameTotal = 0
    private var cancellables: Set<AnyCancellable> = []
    private var calibrationPhase: MotionCalibrationPhase = .idle
    private var topCalibrationSamples: [CGPoint] = []
    private var middleCalibrationSamples: [CGPoint] = []
    private var bottomCalibrationSamples: [CGPoint] = []
    private var targetRecordingTimer: Timer?
    private var trainingTargetPath: [CGPoint] = []
    private var activeHandTrainingPath: [CGPoint] = []
    private var motionTrainingExamples: [MotionTrainingExample] = []
    private var calibrationPose: MotionCalibrationPose = .other
    private var calibrationPoseChangedAt: TimeInterval = 0
    private let calibrationGestureHold: TimeInterval = 0.14
    private let minimumCalibrationSamples = 24
    private let visionFrameInterval: TimeInterval = 1.0 / 30.0
    private let snapshotPublishInterval: TimeInterval = 1.0 / 30.0
    private let permissionRefreshInterval: TimeInterval = 1.0
    private var reportedGesture: GestureName = .none

    init() {
        engine.targetLocator = magneticTargeting

        settings.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)

        camera.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)

        windowCapture.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)

        phoneStream.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &cancellables)

        camera.frameHandler = { [weak self] sampleBuffer in
            self?.handle(sampleBuffer: sampleBuffer)
        }
        windowCapture.frameHandler = { [weak self] sampleBuffer in
            self?.handle(sampleBuffer: sampleBuffer)
        }
        phoneStream.frameHandler = { [weak self] image in
            self?.handle(cgImage: image)
        }
        switch InputSource(rawValue: settings.inputSource) ?? .camera {
        case .camera:
            camera.refreshDevices()
        case .window:
            windowCapture.refreshWindows()
        case .phone:
            break
        }
        refreshPermissionMessage()
        interfaceBridge.commandHandler = { [weak self] command, arguments in
            guard let self else {
                return .failure(InterfaceCommandError("AirHand is unavailable"))
            }
            return self.handleInterfaceCommand(command, arguments: arguments)
        }
        interfaceBridge.start()
    }

    deinit {
        interfaceBridge.stop()
    }

    func appDidAppear() {
        refreshCurrentSource()
        refreshPermissionMessage()
        if settings.startTrackingOnLaunch, !isRunning {
            start()
        }
    }

    func refreshCurrentSource() {
        switch InputSource(rawValue: settings.inputSource) ?? .camera {
        case .camera:
            refreshCameras()
        case .window:
            refreshWindows()
        case .phone:
            break
        }
    }

    func inputSourceChanged() {
        if isRunning {
            stop()
        }
        refreshCurrentSource()
    }

    func refreshCameras() {
        refreshCameraListAndSelection()
    }

    func requestCameraPermission() {
        camera.requestAccess { [weak self] _ in
            guard let self else { return }
            self.refreshCameraListAndSelection()
            self.refreshPermissionMessage()
        }
    }

    func refreshWindows() {
        windowCapture.refreshWindows()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35) {
            let next = self.windowCapture.defaultWindowID(preferredCurrent: self.settings.selectedWindowID)
            if self.settings.selectedWindowID != next {
                self.settings.selectedWindowID = next
            }
        }
    }

    func openScreenRecordingPrivacySettings() {
        windowCapture.openScreenRecordingPrivacy()
    }

    func openCameraPrivacySettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera") else {
            return
        }
        NSWorkspace.shared.open(url)
    }

    func openVideoEffectsSettings() {
        AVCaptureDevice.showSystemUserInterface(.videoEffects)
    }

    func openAccessibilityPrivacySettings() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility") else {
            return
        }
        NSWorkspace.shared.open(url)
    }

    func toggleRunning() {
        isRunning ? stop() : start()
    }

    func start() {
        if InputSource(rawValue: settings.inputSource) == .window {
            startWindowCapture()
            return
        }

        if InputSource(rawValue: settings.inputSource) == .phone {
            startPhoneStream()
            return
        }

        windowCapture.stop()
        phoneStream.stop()
        refreshPermissionMessage()
        let accessibilityTrusted = mouse.isAccessibilityTrusted

        engine.reset()
        resetPerformanceCounters()
        snapshot = EngineSnapshot(
            mode: .searching,
            gesture: .none,
            confidence: 0,
            palmCenter: nil,
            cursorPoint: nil,
            curls: FingerCurls(),
            landmarks: [:],
            message: accessibilityTrusted ? "Starting camera" : "Starting camera preview; grant Accessibility for cursor control",
            desktopDescription: DisplayMapper().desktopDescription,
            lastAction: "None"
        )
        camera.start(deviceID: settings.selectedDeviceID)
        emitTracking(state: "start_requested", reason: "camera_start")
    }

    func stop() {
        camera.stop()
        windowCapture.stop()
        phoneStream.stop()
        apply(action: engine.forceReleasePointer())
        engine.reset()
        resetPerformanceCounters()
        snapshot = .stopped
        hud.update(snapshot: nil, enabled: false)
        reportedGesture = .none
        emitTracking(state: "stopped", reason: "operator_stop")
    }

    func requestAccessibilityPermission() {
        mouse.requestAccessibilityPermission()
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
            self.refreshPermissionMessage()
        }
    }

    func resetAccessibilityPermission() {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/tccutil")
        task.arguments = ["reset", "Accessibility", "local.airhand.app"]

        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            permissionMessage = "\(permissionMessage) / Accessibility reset failed"
            return
        }

        refreshPermissionMessage()
        mouse.requestAccessibilityPermission()
        openAccessibilityPrivacySettings()
    }

    func startCalibration() {
        if !isRunning {
            start()
        }

        isCalibrating = true
        calibrationProgress = 0
        calibrationPhase = .waitingMiddleOpen
        clearSurfaceCalibrationSamples()
        calibrationPose = .other
        calibrationPoseChangedAt = CACurrentMediaTime()
        calibrationMessage = "Open palm to start middle rail"
        emitCalibration(state: "started")
    }

    func cancelCalibration() {
        isCalibrating = false
        calibrationProgress = 0
        calibrationPhase = .idle
        clearSurfaceCalibrationSamples()
        calibrationMessage = settings.hasCalibrationBounds ? "Using saved calibration" : "Not calibrated"
        emitCalibration(state: "cancelled")
    }

    func clearCalibration() {
        cancelCalibration()
        settings.clearCalibration()
        calibrationMessage = "Calibration cleared"
        emitCalibration(state: "cleared")
    }

    func startTargetPathRecording() {
        stopTargetPathRecording(save: false)
        stopHandTakeRecording(save: false)
        trainingTargetPath.removeAll(keepingCapacity: true)
        isRecordingTargetPath = true
        trainingMessage = "Recording cursor path"
        emitTraining(state: "target_started")

        let timer = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            self?.recordCurrentCursorPoint()
        }
        targetRecordingTimer = timer
        RunLoop.main.add(timer, forMode: .common)
    }

    func stopTargetPathRecording(save: Bool = true) {
        targetRecordingTimer?.invalidate()
        targetRecordingTimer = nil
        guard isRecordingTargetPath else { return }
        isRecordingTargetPath = false

        if save, trainingTargetPath.count >= 18 {
            trainingMessage = "Target path saved · \(trainingTargetPath.count) points"
        } else if save {
            trainingTargetPath.removeAll(keepingCapacity: true)
            trainingMessage = "Target path too short"
        }
        emitTraining(state: "target_stopped")
    }

    func startHandTakeRecording() {
        stopTargetPathRecording()
        guard trainingTargetPath.count >= 18 else {
            trainingMessage = "Record cursor path first"
            return
        }
        if !isRunning {
            start()
        }

        activeHandTrainingPath.removeAll(keepingCapacity: true)
        isRecordingHandTake = true
        trainingMessage = "Recording hand take"
        emitTraining(state: "hand_started")
    }

    func stopHandTakeRecording(save: Bool = true) {
        guard isRecordingHandTake else { return }
        isRecordingHandTake = false

        if save, activeHandTrainingPath.count >= 18 {
            motionTrainingExamples.append(
                MotionTrainingExample(
                    targetPath: trainingTargetPath,
                    handPath: activeHandTrainingPath
                )
            )
            trainingTakeCount = motionTrainingExamples.count
            trainingMessage = "Take saved · \(trainingTakeCount)"
        } else if save {
            trainingMessage = "Hand take too short"
        }
        activeHandTrainingPath.removeAll(keepingCapacity: true)
        emitTraining(state: "hand_stopped")
    }

    func trainMotionFromTakes() {
        stopTargetPathRecording()
        stopHandTakeRecording()

        do {
            let profile = try MotionTrainingProfileBuilder().build(examples: motionTrainingExamples)
            try settings.setMotionTraining(profile)
            engine.reset()
            trainingMessage = "Training saved · \(profile.exampleCount) takes"
            emitTraining(state: "saved")
        } catch {
            trainingMessage = "Training failed: \(error)"
            emitTraining(state: "failed")
        }
    }

    func clearMotionTraining() {
        stopTargetPathRecording(save: false)
        stopHandTakeRecording(save: false)
        trainingTargetPath.removeAll(keepingCapacity: true)
        activeHandTrainingPath.removeAll(keepingCapacity: true)
        motionTrainingExamples.removeAll(keepingCapacity: true)
        trainingTakeCount = 0
        settings.clearMotionTraining()
        trainingMessage = "Training cleared"
        emitTraining(state: "cleared")
    }

    func refreshPermissionMessage() {
        let cameraStatus = AVCaptureDevice.authorizationStatus(for: .video)
        let cameraText: String
        switch cameraStatus {
        case .authorized:
            cameraText = "Camera OK"
        case .notDetermined:
            cameraText = "Camera not requested"
        case .denied:
            cameraText = "Camera denied"
        case .restricted:
            cameraText = "Camera restricted"
        @unknown default:
            cameraText = "Camera unknown"
        }

        let accessibilityText = mouse.isAccessibilityTrusted ? "Accessibility OK" : "Accessibility missing"
        let nextMessage = "\(cameraText) / \(accessibilityText)"
        if permissionMessage != nextMessage {
            permissionMessage = nextMessage
            interfaceBridge.emit(
                label: "hand_gesture_control.permissions",
                payload: [
                    "camera": cameraPermissionID(cameraStatus),
                    "accessibility": mouse.isAccessibilityTrusted,
                    "summary": nextMessage
                ]
            )
        }
    }

    private func refreshCameraListAndSelection() {
        camera.refreshDevices()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
            let next = self.camera.defaultDeviceID(preferredCurrent: self.settings.selectedDeviceID)
            if self.settings.selectedDeviceID != next {
                self.settings.selectedDeviceID = next
            }
            self.refreshPermissionMessage()
        }
    }

    private func startWindowCapture() {
        camera.stop()
        phoneStream.stop()

        let accessibilityTrusted = mouse.isAccessibilityTrusted

        engine.reset()
        resetPerformanceCounters()
        snapshot = EngineSnapshot(
            mode: .searching,
            gesture: .none,
            confidence: 0,
            palmCenter: nil,
            cursorPoint: nil,
            curls: FingerCurls(),
            landmarks: [:],
            message: accessibilityTrusted ? "Starting window capture" : "Starting window preview; grant Accessibility for cursor control",
            desktopDescription: DisplayMapper().desktopDescription,
            lastAction: "None"
        )

        windowCapture.start(windowID: settings.selectedWindowID)
        emitTracking(state: "start_requested", reason: "window_start")
    }

    private func startPhoneStream() {
        camera.stop()
        windowCapture.stop()

        let accessibilityTrusted = mouse.isAccessibilityTrusted

        engine.reset()
        resetPerformanceCounters()
        snapshot = EngineSnapshot(
            mode: .searching,
            gesture: .none,
            confidence: 0,
            palmCenter: nil,
            cursorPoint: nil,
            curls: FingerCurls(),
            landmarks: [:],
            message: accessibilityTrusted ? "Waiting for iPhone ultra-wide stream" : "Phone preview; grant Accessibility for cursor control",
            desktopDescription: DisplayMapper().desktopDescription,
            lastAction: "None"
        )

        phoneStream.start()
        emitTracking(state: "start_requested", reason: "phone_start")
    }

    private func handle(sampleBuffer: CMSampleBuffer) {
        let now = CACurrentMediaTime()
        var droppedFramesToPublish: Int?

        frameLock.lock()
        if processingFrame || now < nextVisionFrameTime {
            droppedFrameTotal += 1
            if droppedFrameTotal == 1 || droppedFrameTotal % 30 == 0 {
                droppedFramesToPublish = droppedFrameTotal
            }
            frameLock.unlock()
            if let droppedFramesToPublish {
                DispatchQueue.main.async {
                    self.droppedFrames = droppedFramesToPublish
                }
            }
            return
        }

        processingFrame = true
        nextVisionFrameTime = now + visionFrameInterval
        frameLock.unlock()
        let minimumConfidence = Float(settings.confidenceThreshold)

        processingQueue.async {
            defer {
                self.frameLock.lock()
                self.processingFrame = false
                self.frameLock.unlock()
            }

            do {
                guard let frame = try self.detector.detect(in: sampleBuffer, minimumConfidence: minimumConfidence) else {
                    let output = self.engine.lostHand(now: CACurrentMediaTime())
                    if !self.isCalibrating && !self.isRecordingTargetPath && !self.isRecordingHandTake {
                        self.apply(action: output.action)
                    }
                    self.publish(snapshot: output.snapshot, now: now)
                    return
                }

                let output = self.engine.process(frame: frame, settings: self.settings)
                if !self.isCalibrating && !self.isRecordingTargetPath && !self.isRecordingHandTake {
                    self.apply(action: output.action)
                }
                self.publish(snapshot: output.snapshot, now: frame.timestamp)
            } catch {
                DispatchQueue.main.async {
                    self.snapshot = EngineSnapshot(
                        mode: .unavailable,
                        gesture: .none,
                        confidence: 0,
                        palmCenter: nil,
                        cursorPoint: nil,
                        curls: FingerCurls(),
                        landmarks: [:],
                        message: "Vision failed: \(error.localizedDescription)",
                        desktopDescription: DisplayMapper().desktopDescription,
                        lastAction: self.snapshot.lastAction
                    )
                }
            }
        }
    }

    private func handle(cgImage: CGImage) {
        let now = CACurrentMediaTime()
        var droppedFramesToPublish: Int?

        frameLock.lock()
        if processingFrame || now < nextVisionFrameTime {
            droppedFrameTotal += 1
            if droppedFrameTotal == 1 || droppedFrameTotal % 30 == 0 {
                droppedFramesToPublish = droppedFrameTotal
            }
            frameLock.unlock()
            if let droppedFramesToPublish {
                DispatchQueue.main.async {
                    self.droppedFrames = droppedFramesToPublish
                }
            }
            return
        }

        processingFrame = true
        nextVisionFrameTime = now + visionFrameInterval
        frameLock.unlock()
        let minimumConfidence = Float(settings.confidenceThreshold)

        processingQueue.async {
            defer {
                self.frameLock.lock()
                self.processingFrame = false
                self.frameLock.unlock()
            }

            do {
                guard let frame = try self.detector.detect(in: cgImage, minimumConfidence: minimumConfidence) else {
                    let output = self.engine.lostHand(now: CACurrentMediaTime())
                    if !self.isCalibrating {
                        self.apply(action: output.action)
                    }
                    self.publish(snapshot: output.snapshot, now: now)
                    return
                }

                let output = self.engine.process(frame: frame, settings: self.settings)
                if !self.isCalibrating {
                    self.apply(action: output.action)
                }
                self.publish(snapshot: output.snapshot, now: frame.timestamp)
            } catch {
                DispatchQueue.main.async {
                    self.snapshot = EngineSnapshot(
                        mode: .unavailable,
                        gesture: .none,
                        confidence: 0,
                        palmCenter: nil,
                        cursorPoint: nil,
                        curls: FingerCurls(),
                        landmarks: [:],
                        message: "Vision failed: \(error.localizedDescription)",
                        desktopDescription: DisplayMapper().desktopDescription,
                        lastAction: self.snapshot.lastAction
                    )
                }
            }
        }
    }

    private func resetPerformanceCounters() {
        frameLock.lock()
        processingFrame = false
        nextVisionFrameTime = 0
        droppedFrameTotal = 0
        frameLock.unlock()

        lastSnapshotPublishTime = 0
        lastPermissionRefreshTime = 0
        droppedFrames = 0
    }

    private func publish(snapshot next: EngineSnapshot, now: TimeInterval, force: Bool = false) {
        guard force || now - lastSnapshotPublishTime >= snapshotPublishInterval else { return }
        lastSnapshotPublishTime = now

        DispatchQueue.main.async {
            self.snapshot = next
            self.hud.update(snapshot: next, enabled: self.settings.showOverlay)
            if self.reportedGesture != next.gesture {
                self.reportedGesture = next.gesture
                self.interfaceBridge.emit(
                    label: "hand_gesture_control.gesture",
                    payload: [
                        "gesture": self.gestureID(next.gesture),
                        "mode": self.trackingModeID(next.mode),
                        "confidence": Double(next.confidence)
                    ]
                )
            }
            if self.isRecordingHandTake, let palm = next.palmCenter {
                self.activeHandTrainingPath.append(palm)
                self.trainingMessage = "Recording hand take · \(self.activeHandTrainingPath.count)"
            }
            self.advanceMotionCalibration(with: next, now: now)
            if now - self.lastPermissionRefreshTime >= self.permissionRefreshInterval {
                self.lastPermissionRefreshTime = now
                self.refreshPermissionMessage()
            }
        }
    }

    private func apply(action: PointerAction) {
        guard mouse.isAccessibilityTrusted else { return }

        switch action {
        case .none:
            return
        case .move(let point):
            mouse.moveCursor(to: point)
        case .leftDown(let point):
            mouse.pressLeft(at: point)
            emitPointerEffect("left_down", point: point)
            DispatchQueue.main.async {
                self.hud.flash(at: point)
            }
        case .drag(let point):
            mouse.dragLeft(to: point)
        case .leftUp(let point):
            mouse.releaseLeft(at: point)
            emitPointerEffect("left_up", point: point)
        case .rightClick(let point):
            mouse.rightClick(at: point)
            emitPointerEffect("right_click", point: point)
        case .scroll(let point, let deltaY):
            mouse.scroll(at: point, deltaY: deltaY)
        case .switchSpaceLeft:
            mouse.switchSpace(toRight: false)
            emitPointerEffect("space_left", point: nil)
        case .switchSpaceRight:
            mouse.switchSpace(toRight: true)
            emitPointerEffect("space_right", point: nil)
        }
    }

    private func recordCurrentCursorPoint() {
        let rect = DisplayMapper().desktopRect
        guard !rect.isEmpty else { return }
        let point = mouse.currentLocation
        let normalized = CGPoint(
            x: min(max((point.x - rect.minX) / rect.width, 0), 1),
            y: min(max((point.y - rect.minY) / rect.height, 0), 1)
        )
        trainingTargetPath.append(normalized)
        trainingMessage = "Recording cursor path · \(trainingTargetPath.count)"
    }

    private func advanceMotionCalibration(with next: EngineSnapshot, now: TimeInterval) {
        guard isCalibrating else { return }

        updateCalibrationPose(with: next, now: now)

        switch calibrationPhase {
        case .idle:
            calibrationPhase = .waitingMiddleOpen
            calibrationMessage = "Open palm to start middle rail"
            calibrationProgress = 0
        case .waitingMiddleOpen:
            calibrationProgress = 0.03
            if next.palmCenter == nil {
                calibrationMessage = "Show hand, then open palm for middle rail"
            } else {
                calibrationMessage = "Open palm: middle rail starts at left center"
            }

            if next.palmCenter != nil, calibrationPoseHeld(.openPalm, now: now) {
                middleCalibrationSamples.removeAll(keepingCapacity: true)
                if let palm = next.palmCenter {
                    middleCalibrationSamples.append(palm)
                }
                calibrationPhase = .recordingMiddle
                moveCalibrationCursor(to: .middleStart)
                calibrationMessage = "Move middle rail to the right, then close fist"
            }
        case .recordingMiddle:
            if let palm = next.palmCenter, calibrationPose != .closedPalm {
                middleCalibrationSamples.append(palm)
            }
            calibrationProgress = min(0.30, 0.06 + Double(middleCalibrationSamples.count) / 110 * 0.24)
            calibrationMessage = "Recording middle rail: close fist at the right edge"

            if calibrationPoseHeld(.closedPalm, now: now) {
                guard middleCalibrationSamples.count >= minimumCalibrationSamples else {
                    middleCalibrationSamples.removeAll(keepingCapacity: true)
                    calibrationPhase = .waitingMiddleOpen
                    calibrationProgress = 0
                    calibrationMessage = "Middle rail failed: move wider, open palm again"
                    return
                }

                calibrationPhase = .waitingTopOpen
                calibrationPose = .other
                calibrationPoseChangedAt = now
                moveCalibrationCursor(to: .middleEnd)
                calibrationProgress = 0.34
                calibrationMessage = "Middle saved. Open palm for top rail"
            }
        case .waitingTopOpen:
            calibrationProgress = 0.36
            if next.palmCenter == nil {
                calibrationMessage = "Show hand, then open palm for top rail"
            } else {
                calibrationMessage = "Open palm: top rail starts at left top"
            }

            if next.palmCenter != nil, calibrationPoseHeld(.openPalm, now: now) {
                topCalibrationSamples.removeAll(keepingCapacity: true)
                if let palm = next.palmCenter {
                    topCalibrationSamples.append(palm)
                }
                calibrationPhase = .recordingTop
                moveCalibrationCursor(to: .topStart)
                calibrationMessage = "Move top rail to the right, then close fist"
            }
        case .recordingTop:
            if let palm = next.palmCenter, calibrationPose != .closedPalm {
                topCalibrationSamples.append(palm)
            }
            calibrationProgress = min(0.64, 0.40 + Double(topCalibrationSamples.count) / 110 * 0.24)
            calibrationMessage = "Recording top rail: close fist at the right edge"

            if calibrationPoseHeld(.closedPalm, now: now) {
                guard topCalibrationSamples.count >= minimumCalibrationSamples else {
                    topCalibrationSamples.removeAll(keepingCapacity: true)
                    calibrationPhase = .waitingTopOpen
                    calibrationProgress = 0.34
                    calibrationMessage = "Top rail failed: move wider, open palm again"
                    return
                }

                calibrationPhase = .waitingBottomOpen
                calibrationPose = .other
                calibrationPoseChangedAt = now
                moveCalibrationCursor(to: .topEnd)
                calibrationProgress = 0.68
                calibrationMessage = "Top saved. Open palm for bottom rail"
            }
        case .waitingBottomOpen:
            calibrationProgress = 0.70
            if next.palmCenter == nil {
                calibrationMessage = "Show hand, then open palm for bottom rail"
            } else {
                calibrationMessage = "Open palm: bottom rail starts at left bottom"
            }

            if next.palmCenter != nil, calibrationPoseHeld(.openPalm, now: now) {
                bottomCalibrationSamples.removeAll(keepingCapacity: true)
                if let palm = next.palmCenter {
                    bottomCalibrationSamples.append(palm)
                }
                calibrationPhase = .recordingBottom
                moveCalibrationCursor(to: .bottomStart)
                calibrationMessage = "Move bottom rail to the right, then close fist"
            }
        case .recordingBottom:
            if let palm = next.palmCenter, calibrationPose != .closedPalm {
                bottomCalibrationSamples.append(palm)
            }
            calibrationProgress = min(0.98, 0.74 + Double(bottomCalibrationSamples.count) / 110 * 0.24)
            calibrationMessage = "Recording bottom rail: close fist at the right edge"

            if calibrationPoseHeld(.closedPalm, now: now) {
                finishMotionCalibration()
            }
        }
    }

    private func updateCalibrationPose(with snapshot: EngineSnapshot, now: TimeInterval) {
        let nextPose = MotionCalibrationPoseClassifier.pose(gesture: snapshot.gesture, curls: snapshot.curls)
        if nextPose != calibrationPose {
            calibrationPose = nextPose
            calibrationPoseChangedAt = now
        }
    }

    private func calibrationPoseHeld(_ pose: MotionCalibrationPose, now: TimeInterval) -> Bool {
        calibrationPose == pose && now - calibrationPoseChangedAt >= calibrationGestureHold
    }

    private func finishMotionCalibration() {
        guard bottomCalibrationSamples.count >= minimumCalibrationSamples else {
            bottomCalibrationSamples.removeAll(keepingCapacity: true)
            calibrationPhase = .waitingBottomOpen
            calibrationProgress = 0.70
            calibrationMessage = "Bottom rail failed: move wider, open palm again"
            return
        }

        do {
            let profile = try MotionCalibrationProfileBuilder().buildSurface(
                topSamples: topCalibrationSamples,
                middleSamples: middleCalibrationSamples,
                bottomSamples: bottomCalibrationSamples,
                createdAt: Date().timeIntervalSince1970
            )
            try settings.setMotionCalibration(profile)
            settings.controlModeID = ControlMode.smartWalk.rawValue
            engine.reset()
            isCalibrating = false
            calibrationPhase = .idle
            calibrationProgress = 1
            clearSurfaceCalibrationSamples()
            moveCalibrationCursor(to: .bottomEnd)
            calibrationMessage = String(
                format: "Surface saved · X %.0f%% / Y %.0f%%",
                profile.horizontal.quality * 100,
                profile.vertical.quality * 100
            )
            interfaceBridge.emit(
                label: "hand_gesture_control.calibration",
                payload: [
                    "state": "saved",
                    "message": calibrationMessage,
                    "progress": calibrationProgress,
                    "horizontal_quality": profile.horizontal.quality,
                    "vertical_quality": profile.vertical.quality
                ]
            )
        } catch {
            clearSurfaceCalibrationSamples()
            calibrationPhase = .waitingMiddleOpen
            calibrationProgress = 0
            calibrationMessage = "Calibration failed: \(error)"
            emitCalibration(state: "failed")
        }
    }

    private func clearSurfaceCalibrationSamples() {
        topCalibrationSamples.removeAll(keepingCapacity: true)
        middleCalibrationSamples.removeAll(keepingCapacity: true)
        bottomCalibrationSamples.removeAll(keepingCapacity: true)
    }

    private func moveCalibrationCursor(to target: CalibrationCursorTarget) {
        let rect = DisplayMapper().desktopRect
        guard !rect.isEmpty else { return }

        let inset: CGFloat = 8
        let point: CGPoint
        switch target {
        case .middleStart:
            point = CGPoint(x: rect.minX + inset, y: rect.midY)
        case .middleEnd:
            point = CGPoint(x: rect.maxX - inset, y: rect.midY)
        case .topStart:
            point = CGPoint(x: rect.minX + inset, y: rect.minY + inset)
        case .topEnd:
            point = CGPoint(x: rect.maxX - inset, y: rect.minY + inset)
        case .bottomStart:
            point = CGPoint(x: rect.minX + inset, y: rect.maxY - inset)
        case .bottomEnd:
            point = CGPoint(x: rect.maxX - inset, y: rect.maxY - inset)
        }

        apply(action: .move(point))
    }

    private func handleInterfaceCommand(
        _ command: String,
        arguments: [String: Any]
    ) -> Result<[String: Any], Error> {
        do {
            switch command {
            case "tracking.set":
                try requireCommandKeys(arguments, ["enabled"])
                guard let enabled = commandBool(arguments["enabled"]) else {
                    throw InterfaceCommandError("tracking.set enabled must be a boolean")
                }
                if enabled, !isRunning {
                    start()
                } else if !enabled, isRunning {
                    stop()
                }
            case "source.set":
                try requireCommandKeys(arguments, ["source"])
                guard let source = arguments["source"] as? String,
                      InputSource(rawValue: source) != nil
                else {
                    throw InterfaceCommandError("source.set source is invalid")
                }
                let resume = isRunning
                if resume {
                    stop()
                }
                settings.inputSource = source
                refreshCurrentSource()
                if resume {
                    start()
                }
            case "source.refresh":
                try requireCommandKeys(arguments, [])
                refreshCurrentSource()
            case "status.read":
                try requireCommandKeys(arguments, [])
            case "calibration.start":
                try requireCommandKeys(arguments, [])
                startCalibration()
            case "calibration.cancel":
                try requireCommandKeys(arguments, [])
                cancelCalibration()
            case "calibration.clear":
                try requireCommandKeys(arguments, [])
                clearCalibration()
            case "training.clear":
                try requireCommandKeys(arguments, [])
                clearMotionTraining()
            case "settings.update":
                try requireCommandKeys(arguments, ["values"])
                guard let values = arguments["values"] as? [String: Any], !values.isEmpty else {
                    throw InterfaceCommandError("settings.update values must be a non-empty object")
                }
                try applyInterfaceSettings(values)
            default:
                throw InterfaceCommandError("Unsupported AirHand command: \(command)")
            }
            return .success(interfaceStatus())
        } catch {
            return .failure(error)
        }
    }

    private func applyInterfaceSettings(_ values: [String: Any]) throws {
        let booleanKeys: Set<String> = [
            "mirror_x",
            "preview_mirror_x",
            "invert_y",
            "invert_scroll",
            "start_tracking_on_launch",
            "magnetic_targeting",
            "auto_gait_compensation",
            "show_overlay",
            "finger_throttle"
        ]
        let numberRanges: [String: ClosedRange<Double>] = [
            "magnetic_strength": 0...1,
            "smoothing": 0...0.99,
            "dead_zone_pixels": 0...200,
            "pointer_sensitivity": 0.05...4,
            "stabilization_radius_pixels": 0...400,
            "confidence_threshold": 0...1,
            "click_hold_seconds": 0.01...2,
            "click_cooldown_seconds": 0...5,
            "scroll_sensitivity": 1...10_000,
            "camera_margin_x": 0...0.45,
            "camera_margin_y": 0...0.45
        ]
        let enumKeys: Set<String> = ["tuning_preset", "control_mode"]
        let allowed = booleanKeys.union(numberRanges.keys).union(enumKeys)
        let unknown = Set(values.keys).subtracting(allowed)
        guard unknown.isEmpty else {
            throw InterfaceCommandError("Unknown settings: \(unknown.sorted())")
        }

        var booleans: [String: Bool] = [:]
        var numbers: [String: Double] = [:]
        for key in booleanKeys where values[key] != nil {
            guard let value = commandBool(values[key]) else {
                throw InterfaceCommandError("\(key) must be a boolean")
            }
            booleans[key] = value
        }
        for (key, range) in numberRanges where values[key] != nil {
            guard let value = commandNumber(values[key]), range.contains(value) else {
                throw InterfaceCommandError("\(key) is outside its allowed range")
            }
            numbers[key] = value
        }
        if let value = values["tuning_preset"] {
            guard let raw = value as? String, let preset = TuningPreset(rawValue: raw) else {
                throw InterfaceCommandError("tuning_preset is invalid")
            }
            settings.applyPreset(preset)
        }
        if let value = values["control_mode"] {
            guard let raw = value as? String, ControlMode(rawValue: raw) != nil else {
                throw InterfaceCommandError("control_mode is invalid")
            }
            settings.controlModeID = raw
        }

        if let value = booleans["mirror_x"] { settings.mirrorX = value }
        if let value = booleans["preview_mirror_x"] { settings.previewMirrorX = value }
        if let value = booleans["invert_y"] { settings.invertY = value }
        if let value = booleans["invert_scroll"] { settings.invertScroll = value }
        if let value = booleans["start_tracking_on_launch"] { settings.startTrackingOnLaunch = value }
        if let value = booleans["magnetic_targeting"] { settings.magneticTargeting = value }
        if let value = booleans["auto_gait_compensation"] { settings.autoGaitCompensation = value }
        if let value = booleans["show_overlay"] { settings.showOverlay = value }
        if let value = booleans["finger_throttle"] { settings.fingerThrottle = value }
        if let value = numbers["magnetic_strength"] { settings.magneticStrength = value }
        if let value = numbers["smoothing"] { settings.smoothing = value }
        if let value = numbers["dead_zone_pixels"] { settings.deadZonePixels = value }
        if let value = numbers["pointer_sensitivity"] { settings.pointerSensitivity = value }
        if let value = numbers["stabilization_radius_pixels"] { settings.stabilizationRadiusPixels = value }
        if let value = numbers["confidence_threshold"] { settings.confidenceThreshold = value }
        if let value = numbers["click_hold_seconds"] { settings.clickHoldSeconds = value }
        if let value = numbers["click_cooldown_seconds"] { settings.clickCooldownSeconds = value }
        if let value = numbers["scroll_sensitivity"] { settings.scrollSensitivity = value }
        if let value = numbers["camera_margin_x"] { settings.cameraMarginX = value }
        if let value = numbers["camera_margin_y"] { settings.cameraMarginY = value }
    }

    private func interfaceStatus() -> [String: Any] {
        [
            "running": isRunning,
            "source": settings.inputSource,
            "tracking_mode": trackingModeID(snapshot.mode),
            "gesture": gestureID(snapshot.gesture),
            "confidence": Double(snapshot.confidence),
            "permission_summary": permissionMessage,
            "calibrating": isCalibrating,
            "calibration_progress": calibrationProgress,
            "calibration_message": calibrationMessage,
            "training_message": trainingMessage,
            "training_take_count": trainingTakeCount,
            "settings": [
                "tuning_preset": settings.tuningPresetID,
                "control_mode": settings.controlModeID,
                "mirror_x": settings.mirrorX,
                "invert_y": settings.invertY,
                "invert_scroll": settings.invertScroll,
                "magnetic_targeting": settings.magneticTargeting,
                "auto_gait_compensation": settings.autoGaitCompensation,
                "show_overlay": settings.showOverlay,
                "finger_throttle": settings.fingerThrottle
            ]
        ]
    }

    private func emitTracking(state: String, reason: String) {
        interfaceBridge.emit(
            label: "hand_gesture_control.tracking",
            payload: ["state": state, "source": settings.inputSource, "reason": reason]
        )
    }

    private func emitPointerEffect(_ action: String, point: CGPoint?) {
        var payload: [String: Any] = ["action": action]
        if let point {
            payload["x"] = Double(point.x)
            payload["y"] = Double(point.y)
        }
        interfaceBridge.emit(label: "hand_gesture_control.pointer_effect", payload: payload)
    }

    private func emitCalibration(state: String) {
        interfaceBridge.emit(
            label: "hand_gesture_control.calibration",
            payload: [
                "state": state,
                "message": calibrationMessage,
                "progress": calibrationProgress
            ]
        )
    }

    private func emitTraining(state: String) {
        interfaceBridge.emit(
            label: "hand_gesture_control.training",
            payload: [
                "state": state,
                "message": trainingMessage,
                "take_count": trainingTakeCount
            ]
        )
    }

    private func gestureID(_ gesture: GestureName) -> String {
        switch gesture {
        case .none: return "none"
        case .aim: return "aim"
        case .openPalm: return "open_palm"
        case .leftClick: return "left_click"
        case .rightClick: return "right_click"
        case .fist: return "fist"
        case .missionControl: return "mission_control"
        }
    }

    private func trackingModeID(_ mode: TrackingMode) -> String {
        switch mode {
        case .stopped: return "stopped"
        case .searching: return "searching"
        case .pointing: return "pointing"
        case .scrolling: return "scrolling"
        case .unavailable: return "unavailable"
        }
    }

    private func cameraPermissionID(_ status: AVAuthorizationStatus) -> String {
        switch status {
        case .authorized: return "authorized"
        case .notDetermined: return "not_determined"
        case .denied: return "denied"
        case .restricted: return "restricted"
        @unknown default: return "unknown"
        }
    }

    private func requireCommandKeys(_ value: [String: Any], _ expected: Set<String>) throws {
        guard Set(value.keys) == expected else {
            throw InterfaceCommandError("Command arguments do not match the declared shape")
        }
    }

    private func commandBool(_ value: Any?) -> Bool? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) == CFBooleanGetTypeID()
        else {
            return nil
        }
        return number.boolValue
    }

    private func commandNumber(_ value: Any?) -> Double? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              number.doubleValue.isFinite
        else {
            return nil
        }
        return number.doubleValue
    }
}

private struct InterfaceCommandError: LocalizedError {
    let message: String

    init(_ message: String) {
        self.message = message
    }

    var errorDescription: String? {
        message
    }
}
