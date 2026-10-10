import CoreGraphics
import Foundation
import ImageIO
import Network

final class PhoneStreamService: ObservableObject, @unchecked Sendable {
    @Published private(set) var isRunning = false
    @Published private(set) var statusMessage = "Phone stream idle"
    @Published private(set) var errorMessage: String?
    @Published private(set) var frameCount = 0
    @Published private(set) var videoAspectRatio: Double = 4.0 / 3.0
    @Published private(set) var lastFrame: CGImage?

    var frameHandler: ((CGImage) -> Void)?

    private let queue = DispatchQueue(label: "airhand.phone.stream", qos: .userInteractive)
    private let port: NWEndpoint.Port = 47831
    private var listener: NWListener?
    private var connection: NWConnection?
    private var deliveredFrameCount = 0

    var endpointDescription: String {
        "\(Host.current().localizedName ?? "Mac").local:\(port.rawValue)"
    }

    func start() {
        stop()

        do {
            let parameters = NWParameters.tcp
            parameters.allowLocalEndpointReuse = true
            let listener = try NWListener(using: parameters, on: port)
            listener.service = NWListener.Service(name: "AirHand", type: "_airhand._tcp")
            listener.newConnectionHandler = { [weak self] connection in
                self?.accept(connection)
            }
            listener.stateUpdateHandler = { [weak self] state in
                self?.handle(state: state)
            }
            self.listener = listener
            deliveredFrameCount = 0
            DispatchQueue.main.async {
                self.isRunning = true
                self.errorMessage = nil
                self.frameCount = 0
                self.lastFrame = nil
                self.statusMessage = "Waiting for AirHand Phone on \(self.endpointDescription)"
            }
            listener.start(queue: queue)
        } catch {
            DispatchQueue.main.async {
                self.isRunning = false
                self.errorMessage = error.localizedDescription
                self.statusMessage = "Phone stream failed: \(error.localizedDescription)"
            }
        }
    }

    func stop() {
        connection?.cancel()
        listener?.cancel()
        connection = nil
        listener = nil
        DispatchQueue.main.async {
            self.isRunning = false
            self.statusMessage = "Phone stream stopped"
        }
    }

    private func accept(_ nextConnection: NWConnection) {
        connection?.cancel()
        connection = nextConnection
        nextConnection.stateUpdateHandler = { [weak self] state in
            self?.handle(connectionState: state)
        }
        nextConnection.start(queue: queue)
        receiveLength(from: nextConnection)
    }

    private func receiveLength(from connection: NWConnection) {
        connection.receive(minimumIncompleteLength: 4, maximumLength: 4) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let error {
                self.failConnection("Phone stream read failed: \(error.localizedDescription)")
                return
            }
            guard !isComplete, let data, data.count == 4 else {
                self.failConnection("Phone disconnected")
                return
            }

            let length = data.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
            guard length > 0, length <= 8_000_000 else {
                self.failConnection("Invalid phone frame size: \(length)")
                return
            }
            self.receiveFrame(length: Int(length), from: connection)
        }
    }

    private func receiveFrame(length: Int, from connection: NWConnection) {
        connection.receive(minimumIncompleteLength: length, maximumLength: length) { [weak self] data, _, isComplete, error in
            guard let self else { return }
            if let error {
                self.failConnection("Phone frame failed: \(error.localizedDescription)")
                return
            }
            guard !isComplete, let data, data.count == length else {
                self.failConnection("Phone disconnected")
                return
            }

            if let image = Self.decodeImage(data) {
                self.publish(image)
            }
            self.receiveLength(from: connection)
        }
    }

    private func publish(_ image: CGImage) {
        deliveredFrameCount += 1
        let nextFrameCount = deliveredFrameCount
        let aspectRatio = Double(image.width) / Double(max(image.height, 1))
        DispatchQueue.main.async {
            self.frameCount = nextFrameCount
            self.videoAspectRatio = aspectRatio
            self.lastFrame = image
            self.statusMessage = "Phone stream running: \(image.width)x\(image.height), frames \(nextFrameCount)"
        }
        frameHandler?(image)
    }

    private func handle(state: NWListener.State) {
        switch state {
        case .ready:
            DispatchQueue.main.async {
                self.isRunning = true
                self.errorMessage = nil
                self.statusMessage = "Waiting for AirHand Phone on \(self.endpointDescription)"
            }
        case .failed(let error):
            DispatchQueue.main.async {
                self.isRunning = false
                self.errorMessage = error.localizedDescription
                self.statusMessage = "Phone listener failed: \(error.localizedDescription)"
            }
        case .cancelled:
            DispatchQueue.main.async {
                self.isRunning = false
                self.statusMessage = "Phone stream stopped"
            }
        default:
            break
        }
    }

    private func handle(connectionState state: NWConnection.State) {
        switch state {
        case .ready:
            DispatchQueue.main.async {
                self.errorMessage = nil
                self.statusMessage = "Phone connected"
            }
        case .failed(let error):
            failConnection("Phone connection failed: \(error.localizedDescription)")
        case .cancelled:
            failConnection("Phone disconnected")
        default:
            break
        }
    }

    private func failConnection(_ message: String) {
        connection?.cancel()
        connection = nil
        DispatchQueue.main.async {
            self.errorMessage = message
            self.statusMessage = message
        }
    }

    private static func decodeImage(_ data: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else {
            return nil
        }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }
}
