import AVFoundation
import CoreImage
import Network
import SwiftUI

final class AirHandPhoneStreamer: NSObject, ObservableObject {
    @Published private(set) var status = "Idle"
    @Published private(set) var isRunning = false
    @Published private(set) var sentFrames = 0

    let session = AVCaptureSession()

    private let captureQueue = DispatchQueue(label: "airhand.phone.capture", qos: .userInteractive)
    private let networkQueue = DispatchQueue(label: "airhand.phone.network", qos: .userInteractive)
    private let ciContext = CIContext()
    private var browser: NWBrowser?
    private var connection: NWConnection?
    private var lastFrameTime: CFTimeInterval = 0
    private var isSending = false
    private let targetFrameInterval: CFTimeInterval = 1.0 / 24.0

    func start() {
        guard !isRunning else { return }
        isRunning = true
        sentFrames = 0
        configureCamera()
        startBrowser()
        captureQueue.async {
            if !self.session.isRunning {
                self.session.startRunning()
            }
        }
    }

    func stop() {
        isRunning = false
        browser?.cancel()
        connection?.cancel()
        browser = nil
        connection = nil
        captureQueue.async {
            if self.session.isRunning {
                self.session.stopRunning()
            }
        }
        status = "Stopped"
    }

    private func configureCamera() {
        guard session.inputs.isEmpty else { return }

        session.beginConfiguration()
        session.sessionPreset = .hd1280x720

        guard let device = AVCaptureDevice.default(.builtInUltraWideCamera, for: .video, position: .back)
            ?? AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back)
        else {
            status = "No back camera"
            session.commitConfiguration()
            return
        }

        do {
            try configureFrameRate(device)
            let input = try AVCaptureDeviceInput(device: device)
            guard session.canAddInput(input) else {
                status = "Cannot add camera input"
                session.commitConfiguration()
                return
            }
            session.addInput(input)
        } catch {
            status = "Camera failed: \(error.localizedDescription)"
            session.commitConfiguration()
            return
        }

        let output = AVCaptureVideoDataOutput()
        output.alwaysDiscardsLateVideoFrames = true
        output.videoSettings = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
        output.setSampleBufferDelegate(self, queue: captureQueue)
        guard session.canAddOutput(output) else {
            status = "Cannot add camera output"
            session.commitConfiguration()
            return
        }
        session.addOutput(output)

        if let connection = output.connection(with: .video), connection.isVideoOrientationSupported {
            connection.videoOrientation = .landscapeRight
        }

        session.commitConfiguration()
        status = device.deviceType == .builtInUltraWideCamera ? "Ultra-wide camera ready" : "Wide camera fallback"
    }

    private func configureFrameRate(_ device: AVCaptureDevice) throws {
        try device.lockForConfiguration()
        defer { device.unlockForConfiguration() }

        let fps: Double = 30
        guard device.activeFormat.videoSupportedFrameRateRanges.contains(where: { $0.minFrameRate <= fps && fps <= $0.maxFrameRate }) else {
            return
        }
        let duration = CMTime(value: 1, timescale: CMTimeScale(fps))
        device.activeVideoMinFrameDuration = duration
        device.activeVideoMaxFrameDuration = duration
    }

    private func startBrowser() {
        let descriptor = NWBrowser.Descriptor.bonjour(type: "_airhand._tcp", domain: nil)
        let browser = NWBrowser(for: descriptor, using: .tcp)
        browser.stateUpdateHandler = { [weak self] state in
            DispatchQueue.main.async {
                switch state {
                case .ready:
                    self?.status = "Looking for AirHand Mac"
                case .failed(let error):
                    self?.status = "Browser failed: \(error.localizedDescription)"
                default:
                    break
                }
            }
        }
        browser.browseResultsChangedHandler = { [weak self] results, _ in
            guard let endpoint = results.first?.endpoint else { return }
            self?.connect(to: endpoint)
        }
        self.browser = browser
        browser.start(queue: networkQueue)
    }

    private func connect(to endpoint: NWEndpoint) {
        guard connection == nil else { return }
        let connection = NWConnection(to: endpoint, using: .tcp)
        self.connection = connection
        connection.stateUpdateHandler = { [weak self] state in
            DispatchQueue.main.async {
                switch state {
                case .ready:
                    self?.status = "Connected to AirHand"
                case .failed(let error):
                    self?.status = "Connection failed: \(error.localizedDescription)"
                    self?.connection = nil
                case .cancelled:
                    self?.connection = nil
                default:
                    break
                }
            }
        }
        connection.start(queue: networkQueue)
    }

    private func send(_ data: Data) {
        guard let connection, !isSending else { return }
        isSending = true
        var length = UInt32(data.count).bigEndian
        var packet = Data(bytes: &length, count: 4)
        packet.append(data)
        connection.send(content: packet, completion: .contentProcessed { [weak self] error in
            DispatchQueue.main.async {
                self?.isSending = false
                if let error {
                    self?.status = "Send failed: \(error.localizedDescription)"
                    return
                }
                self?.sentFrames += 1
            }
        })
    }
}

extension AirHandPhoneStreamer: AVCaptureVideoDataOutputSampleBufferDelegate {
    func captureOutput(_ output: AVCaptureOutput, didOutput sampleBuffer: CMSampleBuffer, from connection: AVCaptureConnection) {
        let now = CACurrentMediaTime()
        guard now - lastFrameTime >= targetFrameInterval else { return }
        lastFrameTime = now

        guard let imageBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let image = CIImage(cvPixelBuffer: imageBuffer)
        let colorSpace = CGColorSpaceCreateDeviceRGB()
        guard let jpeg = ciContext.jpegRepresentation(
            of: image,
            colorSpace: colorSpace,
            options: [kCGImageDestinationLossyCompressionQuality as CIImageRepresentationOption: 0.45]
        ) else {
            return
        }
        send(jpeg)
    }
}
