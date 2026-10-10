import Foundation

final class DesktopParentChannel: @unchecked Sendable {
    private let lock = NSLock()
    private var open = true
    private var pending = Data()
    private let handle: FileHandle

    init(_ handle: FileHandle) { self.handle = handle }

    var isOpen: Bool { lock.lock(); defer { lock.unlock() }; return open }

    func packets() -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream(bufferingPolicy: .bufferingOldest(64)) { continuation in
            handle.readabilityHandler = { [self] input in
                let bytes = input.availableData
                lock.lock(); defer { lock.unlock() }
                if bytes.isEmpty {
                    open = false; input.readabilityHandler = nil
                    if pending.isEmpty { continuation.finish() }
                    else { continuation.finish(throwing: DesktopFailure(code:"truncated_request",message:"Parent input ended inside a packet")) }
                    return
                }
                pending.append(bytes)
                while let end = pending.firstIndex(of: 10) {
                    guard end < 1_048_576 else { stop(); continuation.finish(throwing: DesktopFailure(code:"request_too_large",message:"Parent packet exceeded its bound")); return }
                    let packet = pending.prefix(upTo: end); pending.removeSubrange(...end)
                    if case .dropped = continuation.yield(Data(packet)) { stop(); continuation.finish(throwing: DesktopFailure(code:"request_queue_full",message:"Parent input exceeded its bounded queue")); return }
                }
                if pending.count > 1_048_576 { stop(); continuation.finish(throwing: DesktopFailure(code:"request_too_large",message:"Parent packet exceeded its bound")) }
            }
        }
    }

    private func stop() { open = false; handle.readabilityHandler = nil; pending.removeAll() }
}
