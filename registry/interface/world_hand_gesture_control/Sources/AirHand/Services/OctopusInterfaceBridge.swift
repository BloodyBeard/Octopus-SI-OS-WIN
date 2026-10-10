import CoreFoundation
import Foundation
import Network

final class OctopusInterfaceBridge {
    typealias CommandHandler = (String, [String: Any]) -> Result<[String: Any], Error>

    var commandHandler: CommandHandler?

    private let protocolVersion = 1
    private let maxWireBytes = 262_144
    private let maxPendingEvents = 128
    private let queue = DispatchQueue(label: "airhand.octopus.interface.bridge", qos: .userInitiated)
    private let socketPath: String
    private let pendingEventsPath: URL
    private var connection: NWConnection?
    private var connectionGeneration = 0
    private var receiveBuffer = Data()
    private var protocolReady = false
    private var stopped = false
    private var startupError: Error?
    private var pendingEvents: [String: [String: Any]] = [:]
    private var pendingEventOrder: [String] = []

    init(socketPath: String = OctopusInterfaceBridge.defaultSocketPath()) {
        self.socketPath = socketPath
        self.pendingEventsPath = Self.pendingEventsURL()
        do {
            try loadPendingEvents()
        } catch {
            startupError = error
        }
    }

    func start() {
        queue.async {
            guard !self.stopped, self.connection == nil else { return }
            if let error = self.startupError {
                self.writeError(error)
                return
            }
            self.connect()
        }
    }

    func stop() {
        queue.async {
            self.stopped = true
            self.connectionGeneration += 1
            self.protocolReady = false
            self.connection?.cancel()
            self.connection = nil
        }
    }

    func emit(label: String, payload: [String: Any]) {
        queue.async {
            let eventID = UUID().uuidString.lowercased()
            let message: [String: Any] = [
                "type": "event",
                "protocol": self.protocolVersion,
                "event_id": eventID,
                "label": label,
                "payload": payload
            ]
            guard JSONSerialization.isValidJSONObject(message) else { return }
            if self.pendingEventOrder.count >= self.maxPendingEvents,
               let removed = self.pendingEventOrder.first {
                self.pendingEventOrder.removeFirst()
                self.pendingEvents.removeValue(forKey: removed)
            }
            self.pendingEvents[eventID] = message
            self.pendingEventOrder.append(eventID)
            do {
                try self.persistPendingEvents()
            } catch {
                self.failClosed(error)
                return
            }
            self.send(message)
        }
    }

    private func connect() {
        connectionGeneration += 1
        let generation = connectionGeneration
        let next = NWConnection(to: .unix(path: socketPath), using: .tcp)
        connection = next
        protocolReady = false
        receiveBuffer.removeAll(keepingCapacity: true)
        next.stateUpdateHandler = { [weak self, weak next] state in
            guard let self, let next else { return }
            self.queue.async {
                guard self.connection === next, self.connectionGeneration == generation else { return }
                self.handleConnectionState(state, connection: next, generation: generation)
            }
        }
        next.start(queue: queue)
    }

    private func handleConnectionState(
        _ state: NWConnection.State,
        connection: NWConnection,
        generation: Int
    ) {
        switch state {
        case .ready:
            sendHello()
            receive(on: connection, generation: generation)
        case .waiting:
            scheduleReconnect(connection: connection, generation: generation, delay: 1)
        case .failed:
            disconnect(connection: connection, generation: generation, delay: 1)
        case .cancelled:
            if !stopped, self.connection === connection {
                self.connection = nil
                protocolReady = false
                scheduleConnect(generation: generation, delay: 1)
            }
        default:
            break
        }
    }

    private func scheduleReconnect(connection: NWConnection, generation: Int, delay: TimeInterval) {
        queue.asyncAfter(deadline: .now() + delay) {
            guard !self.stopped,
                  self.connection === connection,
                  self.connectionGeneration == generation,
                  !self.protocolReady
            else {
                return
            }
            self.connection = nil
            connection.cancel()
            self.connect()
        }
    }

    private func disconnect(connection: NWConnection, generation: Int, delay: TimeInterval) {
        guard self.connection === connection, self.connectionGeneration == generation else { return }
        self.connection = nil
        protocolReady = false
        receiveBuffer.removeAll(keepingCapacity: true)
        connection.cancel()
        scheduleConnect(generation: generation, delay: delay)
    }

    private func scheduleConnect(generation: Int, delay: TimeInterval) {
        queue.asyncAfter(deadline: .now() + delay) {
            guard !self.stopped,
                  self.connection == nil,
                  self.connectionGeneration == generation
            else {
                return
            }
            self.connect()
        }
    }

    private func sendHello() {
        sendRaw([
            "type": "hello",
            "protocol": protocolVersion,
            "product": "AirHand",
            "version": "0.1.0",
            "capabilities": ["commands", "events", "command_results"]
        ])
    }

    private func receive(on connection: NWConnection, generation: Int) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65_536) {
            [weak self, weak connection] data, _, complete, error in
            guard let self, let connection else { return }
            self.queue.async {
                guard self.connection === connection, self.connectionGeneration == generation else { return }
                if let data, !data.isEmpty {
                    self.receiveBuffer.append(data)
                    if self.receiveBuffer.count > self.maxWireBytes {
                        self.disconnect(connection: connection, generation: generation, delay: 1)
                        return
                    }
                    do {
                        try self.consumeLines()
                    } catch {
                        self.disconnect(connection: connection, generation: generation, delay: 1)
                        return
                    }
                }
                if complete || error != nil {
                    self.disconnect(connection: connection, generation: generation, delay: 1)
                    return
                }
                self.receive(on: connection, generation: generation)
            }
        }
    }

    private func consumeLines() throws {
        while let newline = receiveBuffer.firstIndex(of: 0x0A) {
            let line = receiveBuffer[..<newline]
            receiveBuffer.removeSubrange(...newline)
            guard !line.isEmpty else { continue }
            let value = try JSONSerialization.jsonObject(with: Data(line))
            guard let message = value as? [String: Any] else {
                throw BridgeError.invalidMessage("message must be an object")
            }
            try handle(message)
        }
    }

    private func handle(_ message: [String: Any]) throws {
        guard let type = message["type"] as? String else {
            throw BridgeError.invalidMessage("message type is missing")
        }
        switch type {
        case "hello_ack":
            try requireExactKeys(message, ["type", "protocol", "interface"])
            guard integer(message["protocol"]) == protocolVersion,
                  message["interface"] as? String == "world_hand_gesture_control"
            else {
                throw BridgeError.invalidMessage("hello acknowledgement does not match the interface")
            }
            protocolReady = true
            emit(
                label: "hand_gesture_control.lifecycle",
                payload: ["state": "connected", "product": "AirHand", "version": "0.1.0"]
            )
            flushPendingEvents()
        case "event_result":
            try requireExactKeys(message, ["type", "protocol", "event_id", "delivery"])
            guard integer(message["protocol"]) == protocolVersion,
                  let eventID = message["event_id"] as? String,
                  message["delivery"] is [String: Any]
            else {
                throw BridgeError.invalidMessage("event result is malformed")
            }
            pendingEvents.removeValue(forKey: eventID)
            pendingEventOrder.removeAll { $0 == eventID }
            do {
                try persistPendingEvents()
            } catch {
                failClosed(error)
            }
        case "command":
            try requireExactKeys(message, ["type", "protocol", "source_movement_seq", "command", "arguments"])
            guard integer(message["protocol"]) == protocolVersion,
                  let sourceSequence = integer(message["source_movement_seq"]),
                  sourceSequence >= 0,
                  let command = message["command"] as? String,
                  let arguments = message["arguments"] as? [String: Any]
            else {
                throw BridgeError.invalidMessage("command is malformed")
            }
            executeCommand(sourceSequence: sourceSequence, command: command, arguments: arguments)
        case "command_result_ack":
            try requireExactKeys(message, ["type", "protocol", "source_movement_seq", "delivery"])
            guard integer(message["protocol"]) == protocolVersion,
                  integer(message["source_movement_seq"]) != nil,
                  message["delivery"] is [String: Any]
            else {
                throw BridgeError.invalidMessage("command result acknowledgement is malformed")
            }
        default:
            throw BridgeError.invalidMessage("unknown message type")
        }
    }

    private func executeCommand(sourceSequence: Int, command: String, arguments: [String: Any]) {
        guard let handler = commandHandler else {
            sendCommandResult(
                sourceSequence: sourceSequence,
                status: "failed",
                result: [:],
                error: "AirHand command handler is unavailable"
            )
            return
        }
        DispatchQueue.main.async {
            let outcome = handler(command, arguments)
            self.queue.async {
                switch outcome {
                case .success(let result):
                    self.sendCommandResult(
                        sourceSequence: sourceSequence,
                        status: "accepted",
                        result: result,
                        error: nil
                    )
                case .failure(let error):
                    self.sendCommandResult(
                        sourceSequence: sourceSequence,
                        status: "failed",
                        result: [:],
                        error: error.localizedDescription
                    )
                }
            }
        }
    }

    private func sendCommandResult(
        sourceSequence: Int,
        status: String,
        result: [String: Any],
        error: String?
    ) {
        var message: [String: Any] = [
            "type": "command_result",
            "protocol": protocolVersion,
            "source_movement_seq": sourceSequence,
            "status": status,
            "result": result
        ]
        if let error {
            message["error"] = error
        }
        sendRaw(message)
    }

    private func flushPendingEvents() {
        guard protocolReady else { return }
        for eventID in pendingEventOrder {
            if let message = pendingEvents[eventID] {
                sendRaw(message)
            }
        }
    }

    private func send(_ value: [String: Any]) {
        guard protocolReady else { return }
        sendRaw(value)
    }

    private func sendRaw(_ value: [String: Any]) {
        guard let connection else { return }
        guard JSONSerialization.isValidJSONObject(value),
              var data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        else {
            return
        }
        data.append(0x0A)
        guard data.count <= maxWireBytes else { return }
        let generation = connectionGeneration
        connection.send(content: data, completion: .contentProcessed { [weak self, weak connection] error in
            guard let self, let connection, error != nil else { return }
            self.queue.async {
                self.disconnect(connection: connection, generation: generation, delay: 1)
            }
        })
    }

    private func loadPendingEvents() throws {
        guard FileManager.default.fileExists(atPath: pendingEventsPath.path) else {
            return
        }
        let data = try Data(contentsOf: pendingEventsPath)
        let value = try JSONSerialization.jsonObject(with: data)
        guard let root = value as? [String: Any],
              integer(root["version"]) == 1,
              Set(root.keys) == Set(["version", "events"]),
              let events = root["events"] as? [[String: Any]]
        else {
            throw BridgeError.invalidMessage("pending event store is malformed")
        }
        for event in events.suffix(maxPendingEvents) {
            guard Set(event.keys) == Set(["type", "protocol", "event_id", "label", "payload"]),
                  event["type"] as? String == "event",
                  integer(event["protocol"]) == 1,
                  let eventID = event["event_id"] as? String,
                  UUID(uuidString: eventID) != nil,
                  event["label"] is String,
                  event["payload"] is [String: Any]
            else {
                pendingEvents.removeAll()
                pendingEventOrder.removeAll()
                throw BridgeError.invalidMessage("pending event store contains a malformed event")
            }
            guard pendingEvents[eventID] == nil else {
                pendingEvents.removeAll()
                pendingEventOrder.removeAll()
                throw BridgeError.invalidMessage("pending event store contains duplicate event ids")
            }
            pendingEvents[eventID] = event
            pendingEventOrder.append(eventID)
        }
    }

    private func persistPendingEvents() throws {
        let events = pendingEventOrder.compactMap { pendingEvents[$0] }
        let root: [String: Any] = ["version": 1, "events": events]
        guard JSONSerialization.isValidJSONObject(root),
              let data = try? JSONSerialization.data(withJSONObject: root, options: [.sortedKeys])
        else {
            throw BridgeError.invalidMessage("pending event store cannot be encoded")
        }
        try FileManager.default.createDirectory(
            at: pendingEventsPath.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try data.write(to: pendingEventsPath, options: [.atomic])
    }

    private func failClosed(_ error: Error) {
        startupError = error
        stopped = true
        protocolReady = false
        connectionGeneration += 1
        connection?.cancel()
        connection = nil
        writeError(error)
    }

    private func writeError(_ error: Error) {
        let line = "AirHand Octopus interface bridge failed closed: \(error.localizedDescription)\n"
        if let data = line.data(using: .utf8) {
            try? FileHandle.standardError.write(contentsOf: data)
        }
    }

    private func requireExactKeys(_ value: [String: Any], _ keys: Set<String>) throws {
        guard Set(value.keys) == keys else {
            throw BridgeError.invalidMessage("message fields do not match the protocol")
        }
    }

    private func integer(_ value: Any?) -> Int? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID()
        else {
            return nil
        }
        let result = number.doubleValue
        guard result.isFinite, result.rounded() == result,
              result >= Double(Int.min), result <= Double(Int.max)
        else {
            return nil
        }
        return number.intValue
    }

    private static func defaultSocketPath() -> String {
        let arguments = CommandLine.arguments
        if let index = arguments.firstIndex(of: "--octopus-socket"),
           arguments.indices.contains(index + 1),
           !arguments[index + 1].isEmpty {
            return arguments[index + 1]
        }
        if let explicit = ProcessInfo.processInfo.environment["AIRHAND_OCTOPUS_SOCKET"],
           !explicit.isEmpty {
            return explicit
        }
        let home = ProcessInfo.processInfo.environment["OCTOPUS_HOME"]
            ?? (NSHomeDirectory() as NSString).appendingPathComponent(".octopus")
        return (home as NSString)
            .appendingPathComponent("interfaces/world_hand_gesture_control/airhand.sock")
    }

    private static func pendingEventsURL() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
        return base
            .appendingPathComponent("AirHand", isDirectory: true)
            .appendingPathComponent("octopus-pending-events.json", isDirectory: false)
    }
}

private enum BridgeError: LocalizedError {
    case invalidMessage(String)

    var errorDescription: String? {
        switch self {
        case .invalidMessage(let message):
            return message
        }
    }
}
