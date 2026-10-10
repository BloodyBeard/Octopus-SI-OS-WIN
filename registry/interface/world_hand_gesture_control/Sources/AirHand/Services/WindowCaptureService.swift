import AppKit
import CoreMedia
import Foundation
@preconcurrency import ScreenCaptureKit

struct CapturableWindow: Identifiable, Equatable {
    var id: UInt32
    var title: String
    var appName: String
    var frame: CGRect
    var isOnScreen: Bool

    var label: String {
        let name = title.isEmpty ? appName : "\(appName) · \(title)"
        return "\(name) (\(Int(frame.width)) x \(Int(frame.height)))"
    }
}

final class WindowCaptureService: NSObject, ObservableObject, @unchecked Sendable {
    @Published private(set) var windows: [CapturableWindow] = []
    @Published private(set) var isRunning = false
    @Published private(set) var errorMessage: String?
    @Published private(set) var diagnostics: [String] = []
    @Published private(set) var videoAspectRatio: Double = 16.0 / 9.0

    var frameHandler: ((CMSampleBuffer) -> Void)?

    private let outputQueue = DispatchQueue(label: "airhand.window.capture.output", qos: .userInteractive)
    private var stream: SCStream?
    private var contentByID: [UInt32: SCWindow] = [:]
    private var deliveredVideoAspectRatio = 16.0 / 9.0

    func refreshWindows() {
        Task {
            do {
                let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
                let visibleWindows = content.windows
                    .filter(Self.isUsableWindow)
                    .sorted { lhs, rhs in
                        Self.score(lhs) > Self.score(rhs)
                    }

                let mapped = visibleWindows.map {
                    CapturableWindow(
                        id: $0.windowID,
                        title: $0.title ?? "",
                        appName: $0.owningApplication?.applicationName ?? "Unknown",
                        frame: $0.frame,
                        isOnScreen: $0.isOnScreen
                    )
                }

                let byID = Dictionary(uniqueKeysWithValues: visibleWindows.map { ($0.windowID, $0) })
                DispatchQueue.main.async {
                    self.contentByID = byID
                    self.windows = mapped
                    self.errorMessage = nil
                    self.diagnostics = Self.makeDiagnostics(windows: mapped)
                }
            } catch {
                DispatchQueue.main.async {
                    self.errorMessage = "Window list failed: \(error.localizedDescription)"
                    self.diagnostics = ["Window list failed: \(error.localizedDescription)"]
                }
            }
        }
    }

    func defaultWindowID(preferredCurrent: UInt32) -> UInt32 {
        if let deskView = windows.first(where: Self.isDeskViewWindow) {
            return deskView.id
        }

        if let current = windows.first(where: { $0.id == preferredCurrent }) {
            return current.id
        }

        if let iPhone = windows.first(where: { $0.appName.localizedCaseInsensitiveContains("iPhone Mirroring") }) {
            return iPhone.id
        }

        if let continuity = windows.first(where: { $0.appName.localizedCaseInsensitiveContains("ScreenContinuity") }) {
            return continuity.id
        }

        return windows.first?.id ?? 0
    }

    func start(windowID: UInt32) {
        stop()
        refreshWindows()

        Task {
            do {
                let window = try await findWindow(id: windowID)
                let configuration = SCStreamConfiguration()
                let width = max(Int(window.frame.width), 640)
                let height = max(Int(window.frame.height), 480)
                configuration.width = min(width * 2, 1920)
                configuration.height = min(height * 2, 1440)
                configuration.minimumFrameInterval = CMTime(value: 1, timescale: 30)
                configuration.pixelFormat = kCVPixelFormatType_32BGRA
                configuration.queueDepth = 3
                configuration.showsCursor = false
                configuration.scalesToFit = true
                configuration.capturesAudio = false
                if #available(macOS 14.0, *) {
                    configuration.preservesAspectRatio = true
                    configuration.ignoreShadowsSingleWindow = true
                }

                let filter = SCContentFilter(desktopIndependentWindow: window)
                let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
                try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: outputQueue)

                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                    stream.startCapture { error in
                        if let error {
                            continuation.resume(throwing: error)
                        } else {
                            continuation.resume()
                        }
                    }
                }

                let initialAspectRatio = Double(width) / Double(height)
                self.deliveredVideoAspectRatio = initialAspectRatio
                DispatchQueue.main.async {
                    self.stream = stream
                    self.isRunning = true
                    self.errorMessage = nil
                    self.videoAspectRatio = initialAspectRatio
                }
            } catch {
                DispatchQueue.main.async {
                    self.isRunning = false
                    self.errorMessage = "Window capture failed: \(error.localizedDescription)"
                    self.diagnostics.insert("Start failed: \(error.localizedDescription)", at: 0)
                }
            }
        }
    }

    func stop() {
        guard let stream else {
            isRunning = false
            return
        }

        self.stream = nil
        stream.stopCapture { _ in }
        isRunning = false
    }

    func openScreenRecordingPrivacy() {
        guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") else {
            return
        }
        NSWorkspace.shared.open(url)
    }

    private func findWindow(id: UInt32) async throws -> SCWindow {
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        if let window = content.windows.first(where: { $0.windowID == id }) {
            return window
        }

        if let deskView = content.windows
            .filter(Self.isUsableWindow)
            .sorted(by: { Self.score($0) > Self.score($1) })
            .first(where: Self.isDeskViewWindow) {
            return deskView
        }

        if let iPhone = content.windows.first(where: { $0.owningApplication?.applicationName.localizedCaseInsensitiveContains("iPhone Mirroring") == true }) {
            return iPhone
        }

        throw NSError(
            domain: "AirHand.WindowCapture",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "Selected window is not available"]
        )
    }

    private static func score(_ window: SCWindow) -> Int {
        let app = window.owningApplication?.applicationName ?? ""
        let title = window.title ?? ""
        var value = 0
        if isDeskViewWindow(window) { value += 20_000 }
        if containsSetupHint(title) { value += 5_000 }
        if app.localizedCaseInsensitiveContains("iPhone Mirroring") { value += 7_000 }
        if app.localizedCaseInsensitiveContains("ScreenContinuity") { value += 6_000 }
        if window.isOnScreen { value += 1_000 }
        if #available(macOS 13.1, *), window.isActive { value += 500 }
        value += min(Int(window.frame.width * window.frame.height / 10_000), 400)
        return value
    }

    private static func makeDiagnostics(windows: [CapturableWindow]) -> [String] {
        var lines = ["Windows: \(windows.count)"]
        if windows.contains(where: isDeskViewWindow) {
            lines.append("Desk View window: found")
        } else {
            lines.append("Desk View window: missing")
        }

        if windows.contains(where: { $0.appName.localizedCaseInsensitiveContains("iPhone Mirroring") }) {
            lines.append("iPhone Mirroring window: found")
        } else {
            lines.append("iPhone Mirroring window: missing")
        }

        for window in windows.prefix(12) {
            lines.append("\(window.appName) | \(window.title.isEmpty ? "Untitled" : window.title) | id=\(window.id) | onScreen=\(window.isOnScreen)")
        }

        return lines
    }

    private static func isUsableWindow(_ window: SCWindow) -> Bool {
        guard let app = window.owningApplication else { return false }
        let appName = app.applicationName
        return !appName.isEmpty
            && appName != "AirHand"
            && window.frame.width >= 80
            && window.frame.height >= 80
    }

    private static func isDeskViewWindow(_ window: CapturableWindow) -> Bool {
        containsDeskViewHint(window.appName) || containsDeskViewHint(window.title)
    }

    private static func isDeskViewWindow(_ window: SCWindow) -> Bool {
        let appName = window.owningApplication?.applicationName ?? ""
        let title = window.title ?? ""
        return containsDeskViewHint(appName) || containsDeskViewHint(title)
    }

    private static func containsDeskViewHint(_ text: String) -> Bool {
        text.localizedCaseInsensitiveContains("Desk View")
            || text.localizedCaseInsensitiveContains("DeskView")
            || text.localizedCaseInsensitiveContains("Desk-View")
    }

    private static func containsSetupHint(_ text: String) -> Bool {
        text.localizedCaseInsensitiveContains("Setup")
            || text.localizedCaseInsensitiveContains("Set Up")
            || text.localizedCaseInsensitiveContains("Настрой")
    }
}

extension WindowCaptureService: SCStreamOutput {
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }
        guard sampleBuffer.isValid else { return }

        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
           let statusNumber = attachments.first?[.status] as? NSNumber,
           statusNumber.intValue != SCFrameStatus.complete.rawValue,
           statusNumber.intValue != SCFrameStatus.started.rawValue {
            return
        }

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

        frameHandler?(sampleBuffer)
    }
}

extension WindowCaptureService: SCStreamDelegate {
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        DispatchQueue.main.async {
            self.isRunning = false
            self.errorMessage = "Window capture stopped: \(error.localizedDescription)"
        }
    }
}
