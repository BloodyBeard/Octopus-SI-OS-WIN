import AppKit
import ApplicationServices
import Foundation

struct DesktopPasteRepresentations {
    let values: [NSPasteboard.PasteboardType: Data]

    static func text(_ value: String) throws -> Self {
        try Self(values: [.string: Data(value.utf8)]).validated()
    }

    static func formatted(html: Data, rtf: Data, plainText: String) throws -> Self {
        guard String(data: html, encoding: .utf8) != nil, rtf.starts(with: Data("{\\rtf".utf8)) else {
            throw DesktopFailure(code: "invalid_paste_representation", message: "Formatted paste requires UTF-8 HTML and an RTF representation")
        }
        return try Self(values: [.html: html, .rtf: rtf, .string: Data(plainText.utf8)]).validated()
    }

    func validated() throws -> Self {
        guard Set(values.keys) == [.string] || Set(values.keys) == [.string, .html, .rtf],
              values.values.reduce(0, { $0 + $1.count }) <= 1_048_576 else {
            throw DesktopFailure(code: "invalid_paste_representation", message: "Paste representations exceed the native request bound or omit a required format")
        }
        return self
    }
}

struct DesktopPasteEvidence: Codable {
    let delivery: DesktopAppInputDelivery
    let clipboard_read_acknowledged: Bool
    let clipboard_restored: Bool
    let application_effect_verified: Bool
}

struct DesktopPasteInterrupted: Error {
    let cause: Error
    let delivery: DesktopAppInputDelivery?
    let clipboard_read_acknowledged: Bool
    let clipboard_restored: Bool
    let restoration_error: String?
}

private final class DesktopPasteProvider: NSObject, NSPasteboardItemDataProvider, @unchecked Sendable {
    enum Notice: Sendable { case read, replaced, failed }
    private let values: [NSPasteboard.PasteboardType: Data]
    private let continuation: AsyncStream<Notice>.Continuation
    let notices: AsyncStream<Notice>
    private let lock = NSLock()
    private var armed = false
    private var acknowledged = false
    private var finished = false

    init(values: [NSPasteboard.PasteboardType: Data]) {
        self.values = values
        let pair = AsyncStream<Notice>.makeStream(bufferingPolicy: .bufferingNewest(1))
        notices = pair.stream
        continuation = pair.continuation
        super.init()
    }

    func arm() { lock.lock(); armed = true; lock.unlock() }
    func wasRead() -> Bool { lock.lock(); defer { lock.unlock() }; return acknowledged }
    func finish() {
        lock.lock()
        finished = true
        armed = false
        lock.unlock()
        continuation.finish()
    }

    func pasteboard(_ pasteboard: NSPasteboard?, item: NSPasteboardItem, provideDataForType type: NSPasteboard.PasteboardType) {
        lock.lock()
        let active = !finished
        lock.unlock()
        guard active, let data = values[type] else { return }
        guard item.setData(data, forType: type) else { continuation.yield(.failed); return }
        lock.lock()
        let acknowledge = armed && !finished
        if acknowledge { acknowledged = true }
        lock.unlock()
        if acknowledge { continuation.yield(.read) }
    }

    func pasteboardFinishedWithDataProvider(_ pasteboard: NSPasteboard) { continuation.yield(.replaced) }
}

@MainActor
private final class DesktopPasteSettleObservation {
    private let element: AXUIElement?
    private let wasRead: () -> Bool
    private var observer: AXObserver?
    private var notifications: [String] = []
    private var attributes: [String] = []
    private var baseline: [String: CFTypeRef?]?
    private var armed = false
    private var receivedChange = false
    private var closed = false

    init(application: NSRunningApplication, element: AXUIElement?, wasRead: @escaping () -> Bool,
         check: () throws -> Void) throws {
        self.element = element
        self.wasRead = wasRead
        try check()
        guard let element else { return }
        guard AXUIElementSetMessagingTimeout(element, 1) == .success else {
            throw DesktopFailure(code: "paste_observation_unavailable", message: "Cannot bound native paste observation")
        }
        var names: CFArray?
        if AXUIElementCopyAttributeNames(element, &names) == .success {
            guard let names = names as? [String], names.count <= 4096 else {
                throw DesktopFailure(code: "paste_observation_unavailable", message: "Native paste attribute inventory is invalid")
            }
            attributes = [kAXSelectedTextRangeAttribute, kAXNumberOfCharactersAttribute].filter { names.contains($0) }
        }
        baseline = try sample(check: check)
        try check()
        var created: AXObserver?
        let status = AXObserverCreate(application.processIdentifier, { _, _, _, context in
            guard let context else { return }
            MainActor.assumeIsolated {
                Unmanaged<DesktopPasteSettleObservation>.fromOpaque(context).takeUnretainedValue().receive()
            }
        }, &created)
        if status == .success, let created {
            let context = Unmanaged.passUnretained(self).toOpaque()
            for name in [kAXSelectedTextChangedNotification, kAXValueChangedNotification] {
                if AXObserverAddNotification(created, element, name as CFString, context) == .success {
                    notifications.append(name)
                }
            }
            if !notifications.isEmpty {
                observer = created
                CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(created), .commonModes)
            }
        }
        do { try check() } catch { close(); throw error }
    }

    func arm() { armed = true }

    private func receive() {
        guard !closed, armed, wasRead() else { return }
        receivedChange = true
    }

    func wait(check: () throws -> Void) async throws {
        try check()
        let clock = ContinuousClock()
        if observer == nil, baseline?.isEmpty != false {
            try await clock.sleep(for: .milliseconds(100))
            try check()
            return
        }
        let deadline = clock.now.advanced(by: .seconds(2))
        while clock.now < deadline {
            try check()
            guard !closed else { throw DesktopFailure(code: "paste_observation_closed", message: "Native paste observation was released") }
            if receivedChange { return }
            if let baseline, !baseline.isEmpty, let current = try sample(check: check), !Self.equal(baseline, current) {
                try check()
                return
            }
            try await clock.sleep(until: min(deadline, clock.now.advanced(by: .milliseconds(25))))
        }
        try check()
        throw DesktopFailure(code: "paste_settle_timed_out", message: "Native paste was dispatched and read, but no native selection/value change was observed before its deadline")
    }

    private func sample(check: () throws -> Void) throws -> [String: CFTypeRef?]? {
        try check()
        guard let element, !attributes.isEmpty else { return nil }
        var values: CFArray?
        let result = AXUIElementCopyMultipleAttributeValues(element, attributes as CFArray, [], &values)
        try check()
        guard result == .success else { return nil }
        guard let values, CFArrayGetCount(values) == attributes.count else {
            throw DesktopFailure(code: "paste_observation_unavailable", message: "Native paste attribute result is incomplete")
        }
        var resultValues: [String: CFTypeRef?] = [:]
        for (index, name) in attributes.enumerated() {
            let value = unsafeBitCast(CFArrayGetValueAtIndex(values, index), to: CFTypeRef.self)
            let nativeError = CFGetTypeID(value) == AXValueGetTypeID() && AXValueGetType(value as! AXValue) == .axError
            resultValues[name] = .some(nativeError ? nil : value)
        }
        return resultValues
    }

    private static func equal(_ lhs: [String: CFTypeRef?], _ rhs: [String: CFTypeRef?]) -> Bool {
        guard Set(lhs.keys) == Set(rhs.keys) else { return false }
        for key in lhs.keys {
            switch (lhs[key]!, rhs[key]!) {
            case (nil, nil): continue
            case (let a?, let b?): if !CFEqual(a, b) { return false }
            default: return false
            }
        }
        return true
    }

    func close() {
        guard !closed else { return }
        closed = true; armed = false
        if let observer, let element {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .commonModes)
            for name in notifications { AXObserverRemoveNotification(observer, element, name as CFString) }
        }
        observer = nil; notifications.removeAll()
    }
}

@MainActor
final class DesktopPasteboard {
    static let shared = DesktopPasteboard()
    private var busy = false

    func paste(_ representations: DesktopPasteRepresentations, application: NSRunningApplication,
               focusedElement: AXUIElement?, check: () throws -> Void,
               dispatch: () async throws -> DesktopAppInputDelivery) async throws -> DesktopPasteEvidence {
        try Task.checkCancellation()
        try check()
        guard !busy else { throw DesktopFailure(code: "pasteboard_busy", message: "Another native clipboard operation is still pending") }
        let content = try representations.validated()
        busy = true
        defer { busy = false }
        let provider = DesktopPasteProvider(values: content.values)
        defer { provider.finish() }
        let settling = try DesktopPasteSettleObservation(application: application, element: focusedElement,
            wasRead: { provider.wasRead() }, check: check)
        defer { settling.close() }
        let board = NSPasteboard.general
        let originalRevision = board.changeCount
        let saved = try snapshot(board)
        try Task.checkCancellation()
        try check()
        guard board.changeCount == originalRevision else {
            throw DesktopFailure(code: "pasteboard_changed", message: "Clipboard changed while preparing the native paste")
        }
        let item = NSPasteboardItem()
        guard item.setDataProvider(provider, forTypes: Array(content.values.keys)) else {
            throw DesktopFailure(code: "pasteboard_unavailable", message: "The native clipboard rejected the lazy data provider")
        }
        try check()
        guard board.changeCount == originalRevision else {
            throw DesktopFailure(code: "pasteboard_changed", message: "Clipboard changed before native paste publication")
        }
        let revision = board.prepareForNewContents(with: .currentHostOnly)
        var delivery: DesktopAppInputDelivery?
        do {
            guard board.writeObjects([item]) else {
                throw DesktopFailure(code: "pasteboard_unavailable", message: "The native clipboard rejected the prepared paste item")
            }
            try check()
            guard board.changeCount == revision else {
                throw DesktopFailure(code: "pasteboard_changed", message: "Clipboard changed before native paste dispatch")
            }
            provider.arm(); settling.arm()
            delivery = try await dispatch()
            try Task.checkCancellation()
            try check()
            let notice = try await withThrowingTaskGroup(of: DesktopPasteProvider.Notice.self) { group in
                group.addTask {
                    for await notice in provider.notices { return notice }
                    try Task.checkCancellation()
                    throw DesktopFailure(code: "pasteboard_unavailable", message: "Clipboard provider closed without a read acknowledgement")
                }
                group.addTask {
                    try await ContinuousClock().sleep(for: .seconds(2))
                    throw DesktopFailure(code: "pasteboard_read_timed_out", message: "Native paste was dispatched but no clipboard read was acknowledged")
                }
                defer { group.cancelAll() }
                guard let value = try await group.next() else {
                    throw DesktopFailure(code: "pasteboard_unavailable", message: "Clipboard acknowledgement produced no result")
                }
                return value
            }
            try Task.checkCancellation()
            try check()
            guard board.changeCount == revision, notice == .read, provider.wasRead() else {
                throw DesktopFailure(code: "pasteboard_changed", message: "Clipboard ownership changed or the lazy paste representation could not be read")
            }
            try await settling.wait(check: check)
            try check()
            guard board.changeCount == revision else {
                throw DesktopFailure(code: "pasteboard_changed", message: "Clipboard changed while waiting for native paste completion")
            }
            provider.finish()
            let restored = try restore(saved, to: board, revision: revision)
            guard let delivery else { throw DesktopFailure(code: "pasteboard_unavailable", message: "Native paste has no dispatch evidence") }
            return DesktopPasteEvidence(delivery: delivery, clipboard_read_acknowledged: true,
                                        clipboard_restored: restored, application_effect_verified: false)
        } catch {
            if delivery == nil, let interrupted = error as? DesktopAppInputInterrupted {
                delivery = DesktopAppInputDelivery(mechanism: "CGEventPostToPid",
                                                   dispatched_events: interrupted.dispatched_events,
                                                   application_effect_verified: false)
            }
            provider.finish()
            let restored: Bool
            do { restored = try restore(saved, to: board, revision: revision) }
            catch let restoreError {
                throw DesktopPasteInterrupted(cause: error, delivery: delivery,
                                              clipboard_read_acknowledged: provider.wasRead(), clipboard_restored: false,
                                              restoration_error: String(describing: restoreError))
            }
            throw DesktopPasteInterrupted(cause: error, delivery: delivery,
                                          clipboard_read_acknowledged: provider.wasRead(), clipboard_restored: restored,
                                          restoration_error: nil)
        }
    }

    private func snapshot(_ board: NSPasteboard) throws -> [[NSPasteboard.PasteboardType: Data]] {
        let items = board.pasteboardItems ?? []
        guard items.count <= 256 else { throw DesktopFailure(code: "clipboard_snapshot_limit", message: "Clipboard item count exceeds the preservation bound") }
        var bytes = 0
        return try items.map { item in
            guard item.types.count <= 256 else { throw DesktopFailure(code: "clipboard_snapshot_limit", message: "Clipboard type count exceeds the preservation bound") }
            var result: [NSPasteboard.PasteboardType: Data] = [:]
            for type in item.types {
                guard let data = item.data(forType: type) else {
                    throw DesktopFailure(code: "clipboard_snapshot_unavailable", message: "A current clipboard representation could not be preserved")
                }
                guard data.count <= 33_554_432 - bytes else {
                    throw DesktopFailure(code: "clipboard_snapshot_limit", message: "Clipboard contents exceed the 32 MiB preservation bound")
                }
                bytes += data.count
                result[type] = data
            }
            return result
        }
    }

    private func restore(_ saved: [[NSPasteboard.PasteboardType: Data]], to board: NSPasteboard, revision: Int) throws -> Bool {
        guard board.changeCount == revision else { return false }
        let items = try saved.map { values in
            let item = NSPasteboardItem()
            for (type, data) in values {
                guard item.setData(data, forType: type) else {
                    throw DesktopFailure(code: "clipboard_restore_failed", message: "A saved clipboard representation could not be restored")
                }
            }
            return item
        }
        guard board.changeCount == revision else { return false }
        board.clearContents()
        guard items.isEmpty || board.writeObjects(items) else {
            throw DesktopFailure(code: "clipboard_restore_failed", message: "The native clipboard rejected the preserved contents")
        }
        return true
    }
}
