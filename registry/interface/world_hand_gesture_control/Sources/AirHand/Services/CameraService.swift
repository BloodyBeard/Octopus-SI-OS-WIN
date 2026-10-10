import AVFoundation
import CoreGraphics
import Foundation
import os

struct CameraDevice: Identifiable, Equatable {
    var id: String { uniqueID }
    var uniqueID: String
    var name: String
    var isExternal: Bool
    var isContinuityCamera: Bool
    var isDeskView: Bool
    var kindLabel: String
}

final class CameraService: NSObject, ObservableObject {
    @Published private(set) var devices: [CameraDevice] = []
    @Published private(set) var authorizationStatus: AVAuthorizationStatus = AVCaptureDevice.authorizationStatus(for: .video)
    @Published private(set) var isRunning = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var diagnostics: [String] = []
    @Published private(set) var statusMessage = "Camera idle"
    @Published private(set) var activeDeviceName = "None"
    @Published private(set) var frameCount = 0
    @Published private(set) var videoAspectRatio: Double = 16.0 / 9.0
    @Published private(set) var lastFrame: CGImage?

    let session = AVCaptureSession()
    var frameHandler: ((CMSampleBuffer) -> Void)?

    private let logger = Logger(subsystem: "local.airhand.app", category: "camera")
    private let sessionQueue = DispatchQueue(label: "airhand.camera.session")
    private let videoQueue = DispatchQueue(label: "airhand.camera.video", qos: .userInteractive)
    private var currentInput: AVCaptureDeviceInput?
    private var deliveredFrameCount = 0
    private var deliveredVideoAspectRatio = 16.0 / 9.0
    private let targetFrameRate: Double = 60
    private let previewOnlyDiagnostic = false

    override init() {
        super.init()
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(sessionRuntimeError(_:)),
            name: .AVCaptureSessionRuntimeError,
            object: session
        )
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(sessionWasInterrupted(_:)),
            name: .AVCaptureSessionWasInterrupted,
            object: session
        )
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(sessionInterruptionEnded(_:)),
            name: .AVCaptureSessionInterruptionEnded,
            object: session
        )
        refreshDevices()
    }

    deinit {
        NotificationCenter.default.removeObserver(self)
    }

    func requestAccess(completion: @escaping (Bool) -> Void) {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            authorizationStatus = .authorized
            completion(true)
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { [weak self] granted in
                DispatchQueue.main.async {
                    self?.authorizationStatus = AVCaptureDevice.authorizationStatus(for: .video)
                    self?.refreshDevices()
                    completion(granted)
                }
            }
        case .denied, .restricted:
            authorizationStatus = AVCaptureDevice.authorizationStatus(for: .video)
            completion(false)
        @unknown default:
            authorizationStatus = AVCaptureDevice.authorizationStatus(for: .video)
            completion(false)
        }
    }

    func refreshDevices() {
        let rawDevices = Self.discoverVideoDevices()
        let selectableDevices = rawDevices.filter { $0.deviceType != .deskViewCamera }
        let nextDevices = selectableDevices.map {
            CameraDevice(
                uniqueID: $0.uniqueID,
                name: $0.localizedName,
                isExternal: Self.isExternal($0),
                isContinuityCamera: $0.isContinuityCamera,
                isDeskView: $0.deviceType == .deskViewCamera,
                kindLabel: Self.kindLabel(for: $0)
            )
        }

        DispatchQueue.main.async {
            self.devices = nextDevices
            self.authorizationStatus = AVCaptureDevice.authorizationStatus(for: .video)
            self.diagnostics = Self.makeDiagnostics(rawDevices: rawDevices)
        }
    }

    func defaultDeviceID(preferredCurrent: String) -> String {
        let discoveredDevices = Self.discoverVideoDevices().filter { $0.deviceType != .deskViewCamera }

        if let preferred = discoveredDevices.first(where: {
            $0.uniqueID == preferredCurrent
                && ($0.isContinuityCamera || $0.localizedName.localizedCaseInsensitiveContains("iPhone"))
        }) {
            return preferred.uniqueID
        }

        if let continuity = discoveredDevices.first(where: \.isContinuityCamera) {
            return continuity.uniqueID
        }

        if let iPhone = discoveredDevices.first(where: { $0.localizedName.localizedCaseInsensitiveContains("iPhone") }) {
            return iPhone.uniqueID
        }

        if let external = discoveredDevices.first(where: Self.isExternal) {
            return external.uniqueID
        }

        return discoveredDevices.first?.uniqueID ?? ""
    }

    func start(deviceID: String) {
        requestAccess { [weak self] granted in
            guard let self else { return }
            guard granted else {
                self.errorMessage = "Camera permission is not granted"
                self.statusMessage = "Camera permission is not granted"
                return
            }
            self.configureAndStart(deviceID: deviceID)
        }
    }

    func stop() {
        sessionQueue.async {
            if self.session.isRunning {
                self.session.stopRunning()
            }
            DispatchQueue.main.async {
                self.isRunning = false
                self.statusMessage = "Camera stopped"
                self.activeDeviceName = "None"
            }
        }
    }

    private func configureAndStart(deviceID: String) {
        sessionQueue.async {
            self.session.beginConfiguration()

            if let currentInput = self.currentInput {
                self.session.removeInput(currentInput)
                self.currentInput = nil
            }

            for output in self.session.outputs {
                self.session.removeOutput(output)
            }

            guard let device = self.captureDevice(id: deviceID) else {
                self.finishConfigurationWithError("No camera device found")
                return
            }

            self.deliveredFrameCount = 0
            self.deliveredVideoAspectRatio = 16.0 / 9.0
            DispatchQueue.main.async {
                self.activeDeviceName = device.localizedName
                self.frameCount = 0
                self.videoAspectRatio = 16.0 / 9.0
                self.lastFrame = nil
                self.errorMessage = nil
                self.statusMessage = "Opening \(device.localizedName)"
            }

            do {
                let systemEffectsStatus = self.disableSystemAutoFramingIfSupported()
                let manualFramingStatus = ManualFramingController.shared.forceUltraWideMinimumZoom(for: device)
                let configuredFormat = self.configureSystemFormatFrameRate(for: device)
                let input = try AVCaptureDeviceInput(device: device)
                guard self.session.canAddInput(input) else {
                    self.finishConfigurationWithError("Cannot attach camera input")
                    return
                }
                self.session.addInput(input)
                self.currentInput = input

                if !self.previewOnlyDiagnostic {
                    let output = AVCaptureVideoDataOutput()
                    output.alwaysDiscardsLateVideoFrames = true
                    output.setSampleBufferDelegate(self, queue: self.videoQueue)

                    guard self.session.canAddOutput(output) else {
                        self.finishConfigurationWithError("Cannot attach video output")
                        return
                    }
                    self.session.addOutput(output)

                    if let connection = output.connection(with: .video) {
                        connection.isEnabled = true
                        if connection.isVideoMirroringSupported {
                            connection.isVideoMirrored = false
                        }
                    }
                }

                self.session.commitConfiguration()
                self.session.startRunning()
                self.logger.info("Camera started name=\(device.localizedName, privacy: .public) type=\(device.deviceType.rawValue, privacy: .public) continuity=\(device.isContinuityCamera) displayZoom=\(Self.displayZoomMultiplier(for: device), privacy: .public) format=\(configuredFormat, privacy: .public) effects=\(systemEffectsStatus, privacy: .public) framing=\(manualFramingStatus, privacy: .public)")
                DispatchQueue.main.async {
                    self.errorMessage = nil
                    self.isRunning = self.session.isRunning
                    if self.session.isRunning, self.previewOnlyDiagnostic {
                        self.statusMessage = "Preview-only diagnostic: \(device.localizedName) \(configuredFormat)"
                    } else {
                        self.statusMessage = self.session.isRunning
                            ? "Camera running: \(device.localizedName) \(configuredFormat)"
                            : "Camera did not start: \(device.localizedName)"
                    }
                }
            } catch {
                self.finishConfigurationWithError(error.localizedDescription)
            }
        }
    }

    private func disableSystemAutoFramingIfSupported() -> String {
        guard #available(macOS 12.3, *) else {
            return "Center Stage unavailable"
        }

        let oldMode = AVCaptureDevice.centerStageControlMode
        let oldEnabled = AVCaptureDevice.isCenterStageEnabled
        AVCaptureDevice.centerStageControlMode = .app
        AVCaptureDevice.isCenterStageEnabled = false

        return "Center Stage mode \(Self.centerStageModeLabel(oldMode))->\(Self.centerStageModeLabel(AVCaptureDevice.centerStageControlMode)), enabled \(oldEnabled)->\(AVCaptureDevice.isCenterStageEnabled)"
    }

    private func finishConfigurationWithError(_ message: String) {
        session.commitConfiguration()
        DispatchQueue.main.async {
            self.errorMessage = message
            self.isRunning = false
            self.statusMessage = "Camera failed: \(message)"
        }
    }

    private func configureSystemFormatFrameRate(for device: AVCaptureDevice) -> String {
        let dimensions = CMVideoFormatDescriptionGetDimensions(device.activeFormat.formatDescription)

        do {
            try device.lockForConfiguration()
            defer { device.unlockForConfiguration() }

            let supportedRate = device.activeFormat.videoSupportedFrameRateRanges
                .filter { $0.maxFrameRate >= 30 }
                .map { min(targetFrameRate, $0.maxFrameRate) }
                .max()

            guard let supportedRate else {
                return "\(dimensions.width)x\(dimensions.height) system fps"
            }

            let supportsRate = device.activeFormat.videoSupportedFrameRateRanges.contains { range in
                range.minFrameRate <= supportedRate && supportedRate <= range.maxFrameRate
            }

            guard supportsRate else {
                return "\(dimensions.width)x\(dimensions.height) system fps"
            }

            let frameDuration = CMTime(value: 1, timescale: CMTimeScale(supportedRate.rounded()))
            device.activeVideoMinFrameDuration = frameDuration
            device.activeVideoMaxFrameDuration = frameDuration
            return "\(dimensions.width)x\(dimensions.height) @ \(Int(supportedRate.rounded()))fps"
        } catch {
            DispatchQueue.main.async {
                self.statusMessage = "Camera frame-rate lock failed: \(error.localizedDescription)"
            }
            return "\(dimensions.width)x\(dimensions.height) system fps"
        }
    }

    @objc private func sessionRuntimeError(_ notification: Notification) {
        let error = notification.userInfo?[AVCaptureSessionErrorKey] as? NSError
        let message = error?.localizedDescription ?? "Unknown camera runtime error"
        DispatchQueue.main.async {
            self.errorMessage = "Camera runtime error: \(message)"
            self.statusMessage = "Camera runtime error: \(message)"
            self.isRunning = self.session.isRunning
        }
    }

    @objc private func sessionWasInterrupted(_ notification: Notification) {
        DispatchQueue.main.async {
            self.errorMessage = "Camera interrupted"
            self.statusMessage = "Camera interrupted"
            self.isRunning = self.session.isRunning
        }
    }

    @objc private func sessionInterruptionEnded(_ notification: Notification) {
        DispatchQueue.main.async {
            self.errorMessage = nil
            self.statusMessage = self.session.isRunning ? "Camera running: \(self.activeDeviceName)" : "Camera interruption ended"
            self.isRunning = self.session.isRunning
        }
    }

    private func captureDevice(id: String) -> AVCaptureDevice? {
        let devices = Self.discoverVideoDevices()
        if !id.isEmpty,
           let selected = devices.first(where: { $0.uniqueID == id && $0.deviceType != .deskViewCamera }) {
            return selected
        }

        return devices.first(where: { $0.isContinuityCamera })
            ?? devices.first(where: { $0.localizedName.localizedCaseInsensitiveContains("iPhone") })
            ?? devices.first(where: { Self.isExternal($0) && $0.deviceType != .deskViewCamera })
            ?? devices.first(where: { $0.deviceType != .deskViewCamera })
    }

    private static var discoveryDeviceTypes: [AVCaptureDevice.DeviceType] {
        var types: [AVCaptureDevice.DeviceType] = [
            .builtInWideAngleCamera,
            .externalUnknown,
            .deskViewCamera
        ]

        if #available(macOS 14.0, *) {
            types.append(.continuityCamera)
            types.append(.external)
        }

        return types
    }

    private static func discoverVideoDevices() -> [AVCaptureDevice] {
        var devices: [AVCaptureDevice] = []

        if #available(macOS 14.0, *) {
            let continuityDiscovery = AVCaptureDevice.DiscoverySession(
                deviceTypes: [.continuityCamera],
                mediaType: .video,
                position: .unspecified
            )
            devices.append(contentsOf: continuityDiscovery.devices)

            if let defaultContinuity = AVCaptureDevice.default(.continuityCamera, for: .video, position: .unspecified) {
                devices.append(defaultContinuity)
            }
        }

        let discovery = AVCaptureDevice.DiscoverySession(
            deviceTypes: discoveryDeviceTypes,
            mediaType: .video,
            position: .unspecified
        )
        devices.append(contentsOf: discovery.devices)

        return uniqueDevices(from: devices)
    }

    private static func uniqueDevices(from devices: [AVCaptureDevice]) -> [AVCaptureDevice] {
        var seen = Set<String>()
        return devices.filter { device in
            guard !seen.contains(device.uniqueID) else { return false }
            seen.insert(device.uniqueID)
            return true
        }
    }

    private static func isExternal(_ device: AVCaptureDevice) -> Bool {
        if #available(macOS 14.0, *) {
            return device.deviceType == .external || device.deviceType == .externalUnknown
        }
        return device.deviceType == .externalUnknown
    }

    private static func kindLabel(for device: AVCaptureDevice) -> String {
        if device.isContinuityCamera {
            return "Continuity"
        }

        if #available(macOS 14.0, *), device.deviceType == .external {
            return "External"
        }

        if device.deviceType == .externalUnknown {
            return "External"
        }

        if device.deviceType == .deskViewCamera {
            return "Desk View"
        }

        return "Built-in"
    }

    static func diagnosticsReport() -> [String] {
        makeDiagnostics(rawDevices: discoverVideoDevices())
    }

    private static func makeDiagnostics(rawDevices: [AVCaptureDevice]) -> [String] {
        let continuityKey = Bundle.main.object(forInfoDictionaryKey: "NSCameraUseContinuityCameraDeviceType") as? Bool
        var lines = [
            "Auth: \(authorizationStatusLabel(AVCaptureDevice.authorizationStatus(for: .video)))",
            "Bundle: \(Bundle.main.bundleIdentifier ?? "nil")",
            "Continuity key: \(continuityKey == true ? "true" : "false/nil")",
            "Center Stage: \(centerStageStatusLabel())",
            "Video devices: \(rawDevices.count)"
        ]

        for device in rawDevices {
            var details = [
                "\(device.localizedName)",
                kindLabel(for: device),
                "type=\(device.deviceType.rawValue)",
                "continuity=\(device.isContinuityCamera)"
            ]

            if #available(macOS 14.0, *) {
                details.append(String(format: "displayZoom=%.2f", displayZoomMultiplier(for: device)))
            }

            if #available(macOS 12.3, *) {
                details.append("centerStageActive=\(device.isCenterStageActive)")
                details.append("formatCenterStage=\(device.activeFormat.isCenterStageSupported)")
            }

            if let manualFraming = ManualFramingController.shared.diagnostics(for: device) {
                details.append(manualFraming)
            }

            if let companion = device.companionDeskViewCamera {
                details.append("deskCompanion=\(companion.localizedName)")
            }

            lines.append(details.joined(separator: " | "))
        }

        if rawDevices.isEmpty {
            lines.append("No video devices returned by AVFoundation")
        }

        return lines
    }

    private static func displayZoomMultiplier(for device: AVCaptureDevice) -> Double {
        if #available(macOS 14.0, *) {
            return device.displayVideoZoomFactorMultiplier
        }
        return 1
    }

    private static func centerStageStatusLabel() -> String {
        guard #available(macOS 12.3, *) else {
            return "unavailable"
        }

        return "mode=\(centerStageModeLabel(AVCaptureDevice.centerStageControlMode)), enabled=\(AVCaptureDevice.isCenterStageEnabled)"
    }

    @available(macOS 12.3, *)
    private static func centerStageModeLabel(_ mode: AVCaptureDevice.CenterStageControlMode) -> String {
        switch mode {
        case .user:
            return "user"
        case .app:
            return "app"
        case .cooperative:
            return "cooperative"
        @unknown default:
            return "unknown"
        }
    }

    private static func authorizationStatusLabel(_ status: AVAuthorizationStatus) -> String {
        switch status {
        case .authorized: return "authorized"
        case .notDetermined: return "notDetermined"
        case .denied: return "denied"
        case .restricted: return "restricted"
        @unknown default: return "unknown"
        }
    }
}

extension CameraService: AVCaptureVideoDataOutputSampleBufferDelegate {
    func captureOutput(
        _ output: AVCaptureOutput,
        didOutput sampleBuffer: CMSampleBuffer,
        from connection: AVCaptureConnection
    ) {
        deliveredFrameCount += 1
        let nextFrameCount = deliveredFrameCount
        if let imageBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) {
            let width = CVPixelBufferGetWidth(imageBuffer)
            let height = CVPixelBufferGetHeight(imageBuffer)
            if width > 0, height > 0 {
                let nextAspectRatio = Double(width) / Double(height)
                if abs(deliveredVideoAspectRatio - nextAspectRatio) > 0.001 {
                    deliveredVideoAspectRatio = nextAspectRatio
                    DispatchQueue.main.async {
                        self.videoAspectRatio = nextAspectRatio
                    }
                }
            }
        }
        if nextFrameCount == 1 || nextFrameCount % 30 == 0 {
            DispatchQueue.main.async {
                self.frameCount = nextFrameCount
                if self.errorMessage == nil {
                    self.statusMessage = "Camera running: \(self.activeDeviceName), frames \(nextFrameCount)"
                }
            }
        }
        frameHandler?(sampleBuffer)
    }
}
