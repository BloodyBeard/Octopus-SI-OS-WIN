import AppKit
import ApplicationServices
import CoreGraphics
import CoreImage
import CoreMedia
import CoreVideo
import CryptoKit
import Darwin
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

@main
struct OctopusDesktopNative {
    static let maxMediaBytes = 64 * 1024 * 1024
    static let maxMediaItems = 16
    static let maxResultBytes = ((maxMediaBytes + 2) / 3) * 4 + maxMediaItems * 256 + 8 * 1024 * 1024
    static let outputLock = NSLock()
    static let parentChannel = DesktopParentChannel(.standardInput)
    static let inputLeases = DesktopNativeInputLeases()

    static func main() async {
        let device: DesktopInheritedDeviceReference
        do { device = try DesktopInheritedDeviceReference() }
        catch {
            FileHandle.standardError.write(Data("native inherited device reference is invalid\n".utf8))
            exit(EXIT_FAILURE)
        }
        let streams = await MainActor.run { DesktopStreams() }
        await withTaskGroup(of: Void.self) { pending in
            var pendingCount = 0
            var previousOperation: Task<Void, Never>?
            do {
                for try await bytes in parentChannel.packets() {
                    guard let packet = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
                          let id = packet["id"] as? UInt64,
                          let action = packet["action"] as? [String: Any] else {
                        throw DesktopFailure(code: "invalid_request", message: "Expected a bounded identified JSON line")
                    }
                    if action["type"] as? String == "input_lease_revoke" {
                        let identity = try DesktopInputLeaseIdentity.decode(action["lease"])
                        inputLeases.invalidate(identity)
                    }
                    let permissionRequest = ["permissions", "permission_settings_source"].contains(action["type"] as? String ?? "")
                    let previous = permissionRequest ? nil : previousOperation
                    let task = Task {
                        await previous?.value
                        do {
                            let result = try await streams.execute(action)
                            try output(["id": id, "ok": true, "result": result])
                        } catch let error as DesktopAppReadFailure {
                            try? output(["id":id,"ok":false,"code":"application_observation_failed","message":error.message,
                                "details":try DesktopApplicationRoom.json(error)])
                        } catch let error as DesktopFailure {
                            try? output(["id": id, "ok": false, "code": error.code, "message": error.message])
                        } catch {
                            try? output(["id": id, "ok": false, "code": "native_error", "message": String(describing: error)])
                        }
                    }
                    pending.addTask { await task.value }
                    pendingCount += 1
                    if !permissionRequest { previousOperation = task }
                    if pendingCount == 64 { _ = await pending.next(); pendingCount -= 1 }
                }
            } catch { }
            inputLeases.close()
            await pending.waitForAll()
        }
        await streams.close()
        device.close()
    }

    static func output(_ value: [String: Any]) throws {
        var bytes = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
        bytes.append(10)
        guard bytes.count <= maxResultBytes else {
            throw DesktopFailure(code: "native_result_too_large", message: "Native evidence exceeds the bounded image and text result contract")
        }
        outputLock.lock()
        defer { outputLock.unlock() }
        FileHandle.standardOutput.write(bytes)
    }

    static func text(_ value: [String: Any], _ key: String) throws -> String {
        guard let text = value[key] as? String, !text.isEmpty, !text.contains("\0") else {
            throw DesktopFailure(code: "invalid_request", message: "Missing or invalid \(key)")
        }
        return text
    }

    static func number(_ value: [String: Any], _ key: String) throws -> Double {
        guard let number = value[key] as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID(), number.doubleValue.isFinite else {
            throw DesktopFailure(code: "invalid_request", message: "Expected a finite number: \(key)")
        }
        return number.doubleValue
    }

    static func screens() throws -> [[String: Any]] {
        var count: UInt32 = 0
        guard CGGetActiveDisplayList(0, nil, &count) == .success else {
            throw DesktopFailure(code: "display_unavailable", message: "Cannot enumerate active displays")
        }
        var ids = [CGDirectDisplayID](repeating: 0, count: Int(count))
        guard CGGetActiveDisplayList(count, &ids, &count) == .success else {
            throw DesktopFailure(code: "display_unavailable", message: "Active display list changed")
        }
        return ids.prefix(Int(count)).map { id in
            let rect = CGDisplayBounds(id)
            return ["id": String(id), "x": rect.origin.x, "y": rect.origin.y,
                    "width": rect.width, "height": rect.height,
                    "pixel_width": CGDisplayPixelsWide(id), "pixel_height": CGDisplayPixelsHigh(id)]
        }
    }

    @MainActor
    static func focus() -> Any {
        guard let app = NSWorkspace.shared.frontmostApplication,
              let appId = app.bundleIdentifier else { return NSNull() }
        let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
        guard let window = windows.first(where: {
            ($0[kCGWindowOwnerPID as String] as? Int32) == app.processIdentifier
                && ($0[kCGWindowLayer as String] as? Int) == 0
        }), let windowId = window[kCGWindowNumber as String] as? UInt32 else { return NSNull() }
        return ["app_id": appId, "process_id": app.processIdentifier, "window_id": windowId]
    }

    static func revision(_ state: CGEventSourceStateID) -> String {
        let types: [CGEventType] = [.mouseMoved, .leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp, .keyDown, .keyUp, .flagsChanged, .scrollWheel, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged]
        return types.map { String(CGEventSource.counterForEventType(state, eventType: $0)) }.joined(separator: ":")
    }

    @MainActor
    static func status() throws -> [String: Any] {
        return ["permissions": ["screen_capture": CGPreflightScreenCaptureAccess(), "accessibility": AXIsProcessTrusted()], "screens": try screens()]
    }

    @MainActor
    static func execute(_ action: [String: Any]) async throws -> [String: Any] {
        let operation = try text(action, "type")
        var result = try status()
        if operation == "status" { return result }
        guard parentChannel.isOpen else { throw DesktopFailure(code: "parent_disconnected", message: "Parent channel closed before native execution") }
        guard CGPreflightScreenCaptureAccess() else {
            throw DesktopFailure(code: "screen_capture_permission_required", message: "Enable Screen Recording for Octopus Desktop Native in macOS Privacy & Security, then retry explicitly")
        }
        let screenId = try text(action, "screen_id")
        guard let screen = (result["screens"] as? [[String: Any]])?.first(where: { $0["id"] as? String == screenId }) else {
            throw DesktopFailure(code: "display_changed", message: "The observed display is no longer active")
        }
        if operation == "observe" {
            let beforeRevision = revision(.combinedSessionState)
            let beforeFocus = focus()
            var image: Any = NSNull()
            if action["include_screenshot"] as? Bool == true {
                image = try await capture(screen, action)
            }
            let afterScreens = try screens()
            guard NSDictionary(dictionary: screen).isEqual(to: afterScreens.first(where: { $0["id"] as? String == screenId }) ?? [:]),
                  same(beforeFocus, focus()), beforeRevision == revision(.combinedSessionState) else {
                throw DesktopFailure(code: "observation_changed", message: "Display, focus or input changed during capture; observe again")
            }
            result["observation"] = ["id": "native", "screen": screen, "focus": beforeFocus,
                "input_revision": beforeRevision, "captured_at_unix_ms": UInt64(Date().timeIntervalSince1970 * 1000), "image": image]
            return result
        }
        guard AXIsProcessTrusted() else {
            throw DesktopFailure(code: "accessibility_permission_required", message: "Enable Accessibility for Octopus Desktop Native in macOS Privacy & Security, then retry explicitly")
        }
        guard let expected = action["expected"] as? [String: Any],
              let expectedScreen = expected["screen"] as? [String: Any],
              let expectedRevision = expected["input_revision"] as? String,
              let expectedFocus = expected["focus"] as? [String: Any],
              NSDictionary(dictionary: expectedScreen).isEqual(to: screen),
              same(expectedFocus, focus()), expectedRevision == revision(.combinedSessionState) else {
            throw DesktopFailure(code: "stale_observation", message: "Display, foreground window or input changed since observation; no input was posted")
        }
        let cancelPath = try text(action, "cancel_file")
        let hardwareRevision = revision(.hidSystemState)
        var posted = 0
        var interrupted = false
        guard let source = CGEventSource(stateID: .privateState) else {
            throw DesktopFailure(code: "event_source_unavailable", message: "Cannot create a private native input source")
        }
        func check() -> Bool {
            let cancelled = !parentChannel.isOpen || FileManager.default.fileExists(atPath: cancelPath) || hardwareRevision != revision(.hidSystemState)
            if cancelled { interrupted = true }
            return !cancelled
        }
        func post(_ event: CGEvent) { event.post(tap: .cghidEventTap); posted += 1 }
        func point(_ key: String) throws -> CGPoint {
            guard let data = action[key] as? [String: Any] else { throw DesktopFailure(code: "invalid_point", message: "Missing \(key)") }
            let value = CGPoint(x: try number(data, "x"), y: try number(data, "y"))
            let rect = CGRect(x: try number(screen, "x"), y: try number(screen, "y"), width: try number(screen, "width"), height: try number(screen, "height"))
            guard rect.contains(value) else { throw DesktopFailure(code: "invalid_point", message: "Point falls outside observed display") }
            return value
        }
        func mouse(_ type: CGEventType, _ point: CGPoint, _ button: CGMouseButton = .left, _ count: Int = 1) throws -> CGEvent {
            guard let event = CGEvent(mouseEventSource: source, mouseType: type, mouseCursorPosition: point, mouseButton: button) else {
                throw DesktopFailure(code: "event_unavailable", message: "Cannot construct mouse event")
            }
            event.setIntegerValueField(.mouseEventClickState, value: Int64(count))
            return event
        }
        do { if check() {
            switch operation {
            case "click":
                let target = try point("point")
                let (button, downType, upType, count) = try DesktopInput.click(action)
                let events = try (1...count).map { index in (try mouse(downType, target, button, index), try mouse(upType, target, button, index)) }
                for (down, up) in events {
                    if !check() { break }
                    post(down); post(up)
                }
            case "move":
                post(try mouse(.mouseMoved, point("point")))
            case "drag":
                let from = try point("from"), to = try point("to")
                let down = try mouse(.leftMouseDown, from), up = try mouse(.leftMouseUp, to)
                var releasedAt = from
                post(down)
                defer { up.location = interrupted ? (CGEvent(source: nil)?.location ?? releasedAt) : releasedAt; post(up) }
                for step in 1...20 {
                    if !check() { break }
                    let fraction = Double(step) / 20
                    releasedAt = CGPoint(x: from.x + (to.x-from.x)*fraction, y: from.y + (to.y-from.y)*fraction)
                    post(try mouse(.leftMouseDragged, releasedAt))
                }
            case "type":
                let value = try text(action, "text")
                guard value.utf16.count <= 16_384 else { throw DesktopFailure(code: "input_too_large", message: "Type at most 16384 UTF-16 units per observed action") }
                for character in value {
                    if !check() { break }
                    let units = Array(String(character).utf16)
                    guard let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true), let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else {
                        throw DesktopFailure(code: "event_unavailable", message: "Cannot construct Unicode input event")
                    }
                    down.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
                    up.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
                    post(down); post(up)
                }
            case "key":
                let (code, flags) = try DesktopInput.key(text(action, "key"))
                guard let down = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: true), let up = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: false) else { throw DesktopFailure(code: "event_unavailable", message: "Cannot construct key event") }
                down.flags = flags; up.flags = flags; post(down); post(up)
            case "scroll":
                let x = try number(action, "delta_x"), y = try number(action, "delta_y")
                guard abs(x) <= 10000, abs(y) <= 10000, x != 0 || y != 0 else { throw DesktopFailure(code: "invalid_scroll", message: "Scroll must be nonzero and at most 10000 pixels per axis") }
                guard let event = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 2, wheel1: Int32(y), wheel2: Int32(x), wheel3: 0) else { throw DesktopFailure(code: "event_unavailable", message: "Cannot construct scroll event") }
                post(event)
            default: throw DesktopFailure(code: "unsupported_action", message: "Unsupported native operation")
            }
        } } catch {
            interrupted = true
            result["execution_error"] = String(describing: error)
        }
        result["completed"] = !interrupted
        result["events_posted"] = posted
        return result
    }

    static func same(_ a: Any, _ b: Any) -> Bool {
        guard let a = a as? [String: Any], let b = b as? [String: Any] else { return a is NSNull && b is NSNull }
        return NSDictionary(dictionary: a).isEqual(to: b)
    }

    @MainActor
    static func capture(_ screen: [String: Any], _ action: [String: Any]) async throws -> [String: Any] {
        guard #available(macOS 14.0, *) else { throw DesktopFailure(code: "unsupported_platform", message: "ScreenCaptureKit screenshots require macOS 14 or newer") }
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        let id = try text(screen, "id")
        guard let display = content.displays.first(where: { String($0.displayID) == id }) else { throw DesktopFailure(code: "display_changed", message: "Display is no longer capturable") }
        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = Int(try number(screen, "pixel_width")); config.height = Int(try number(screen, "pixel_height"))
        config.showsCursor = false
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        let format = action["format"] as? String ?? "png"
        guard format == "png" || format == "jpeg" else { throw DesktopFailure(code: "unsupported_image_format", message: "Use png or jpeg") }
        let data = NSMutableData()
        let type = format == "png" ? UTType.png.identifier : UTType.jpeg.identifier
        guard let destination = CGImageDestinationCreateWithData(data, type as CFString, 1, nil) else { throw DesktopFailure(code: "image_encoding_failed", message: "Cannot create image encoder") }
        var properties: [CFString: Any] = [:]
        if let quality = action["quality"] as? NSNumber { properties[kCGImageDestinationLossyCompressionQuality] = quality.doubleValue / 100 }
        CGImageDestinationAddImage(destination, image, properties as CFDictionary)
        guard CGImageDestinationFinalize(destination) else { throw DesktopFailure(code: "image_encoding_failed", message: "Image encoder failed") }
        let bytes = data as Data
        return ["media_type": "image/\(format)", "data_base64": bytes.base64EncodedString(), "sha256": SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined(), "byte_length": bytes.count, "width": image.width, "height": image.height]
    }
}

final class DesktopInheritedDeviceReference {
    private var descriptor: Int32?

    init() throws {
        struct Reference: Decodable {
            let fd: Int32
            let device: UInt64
            let inode: UInt64
            let owner: UInt32
        }
        guard let value = ProcessInfo.processInfo.environment["OCTOPUS_DESKTOP_DEVICE_REFERENCE"],
              let bytes = value.data(using: .utf8), bytes.count <= 1024,
              let object = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
              Set(object.keys) == ["fd", "device", "inode", "owner"] else {
            throw DesktopFailure(code: "invalid_device_reference", message: "A private inherited device descriptor is required")
        }
        let reference = try JSONDecoder().decode(Reference.self, from: bytes)
        var state = stat()
        guard reference.fd >= 3, reference.owner == geteuid(),
              fstat(reference.fd, &state) == 0,
              state.st_mode & S_IFMT == S_IFREG, state.st_mode & 0o777 == 0o600,
              state.st_uid == reference.owner, state.st_nlink == 1,
              UInt64(UInt32(bitPattern: state.st_dev)) == reference.device,
              UInt64(state.st_ino) == reference.inode else {
            throw DesktopFailure(code: "invalid_device_reference", message: "Inherited device descriptor identity or ownership changed")
        }
        let flags = fcntl(reference.fd, F_GETFD)
        guard flags >= 0, fcntl(reference.fd, F_SETFD, flags | FD_CLOEXEC) == 0 else {
            throw DesktopFailure(code: "invalid_device_reference", message: "Cannot contain inherited device descriptor lifetime")
        }
        descriptor = reference.fd
        unsetenv("OCTOPUS_DESKTOP_DEVICE_REFERENCE")
    }

    func close() {
        if let descriptor = descriptor { self.descriptor = nil; _ = Darwin.close(descriptor) }
    }

    deinit { close() }
}

struct DesktopInputLeaseIdentity: Codable, Equatable {
    let scope: DesktopAppScope
    let control_generation: UInt64

    static func decode(_ value: Any?) throws -> Self {
        let identity = try JSONDecoder().decode(Self.self, from: JSONSerialization.data(withJSONObject: value ?? NSNull()))
        try identity.scope.validate()
        guard identity.control_generation > 0, identity.control_generation <= 9_007_199_254_740_991 else {
            throw DesktopFailure(code: "invalid_input_lease", message: "Native input requires an exact positive device control generation")
        }
        return identity
    }
}

final class DesktopNativeInputLease: @unchecked Sendable {
    let identity: DesktopInputLeaseIdentity
    private let lock = NSLock()
    private var active = true
    private var operation: UUID?
    private var failure: DesktopFailure?
    private var effects: [DesktopAppFocusEffect] = []

    init(_ identity: DesktopInputLeaseIdentity) { self.identity = identity }

    func checkCaller() throws {
        lock.lock(); let allowed = active; lock.unlock()
        guard allowed, OctopusDesktopNative.parentChannel.isOpen else {
            throw DesktopFailure(code: "control_changed", message: "The native input device lease or its owning channel was withdrawn")
        }
    }

    func begin(_ id: UUID) throws {
        try checkCaller()
        lock.lock(); defer { lock.unlock() }
        guard active, operation == nil else { throw DesktopFailure(code: "input_busy", message: "Native lease already has an active operation") }
        operation = id; failure = nil; effects.removeAll()
    }

    func checkOperation(_ id: UUID) throws {
        try checkCaller()
        lock.lock(); let current = operation; let error = failure; lock.unlock()
        guard current == id else { throw DesktopFailure(code: "input_changed", message: "Native operation identity changed") }
        if let error { throw error }
    }

    func interrupt(_ error: DesktopFailure) {
        lock.lock(); defer { lock.unlock() }
        if operation != nil, failure == nil { failure = error }
    }

    func record(_ effect: DesktopAppFocusEffect) {
        lock.lock(); defer { lock.unlock() }
        if effects.count < 4096 { effects.append(effect) }
        else if failure == nil { failure = DesktopFailure(code: "input_evidence_limit", message: "Native focus evidence exceeded its bound") }
    }

    func finish(_ id: UUID) -> [DesktopAppFocusEffect] {
        lock.lock(); defer { lock.unlock() }
        guard operation == id else { return [] }
        operation = nil
        return effects
    }

    func invalidate() {
        lock.lock(); defer { lock.unlock() }
        active = false
        if failure == nil { failure = DesktopFailure(code: "control_changed", message: "The native input device lease was withdrawn") }
    }
}

final class DesktopNativeInputLeases: @unchecked Sendable {
    private let lock = NSLock()
    private var current: DesktopNativeInputLease?
    private var revokedThrough: UInt64 = 0
    private var closed = false

    func begin(_ identity: DesktopInputLeaseIdentity) throws {
        lock.lock(); defer { lock.unlock() }
        guard !closed, identity.control_generation > revokedThrough else {
            throw DesktopFailure(code: "control_changed", message: "Native input cannot revive a withdrawn device generation")
        }
        if let current {
            guard current.identity == identity else { throw DesktopFailure(code: "control_changed", message: "A different native input lease is still retained") }
            try current.checkCaller()
        } else { current = DesktopNativeInputLease(identity) }
    }

    func require(_ identity: DesktopInputLeaseIdentity) throws -> DesktopNativeInputLease {
        lock.lock(); defer { lock.unlock() }
        guard !closed, let current, current.identity == identity else {
            throw DesktopFailure(code: "control_changed", message: "Application input does not identify the current native device lease")
        }
        try current.checkCaller()
        return current
    }

    func invalidate(_ identity: DesktopInputLeaseIdentity) {
        lock.lock(); defer { lock.unlock() }
        revokedThrough = max(revokedThrough, identity.control_generation)
        if current?.identity == identity { current?.invalidate() }
    }

    func release(_ identity: DesktopInputLeaseIdentity) {
        lock.lock(); defer { lock.unlock() }
        if current?.identity == identity { current?.invalidate(); current = nil }
    }

    func close() {
        lock.lock(); defer { lock.unlock() }
        closed = true; current?.invalidate()
    }
}


final class DesktopNativeAppGrant: @unchecked Sendable {
    let app: DesktopAppRef
    let requestID: String
    private let lock = NSLock()
    private var active = true
    private var failure: DesktopFailure?
    private var effects: [DesktopAppFocusEffect] = []

    init(app: DesktopAppRef, requestID: String) { self.app = app; self.requestID = requestID }

    func check() throws {
        lock.lock(); let allowed = active; let error = failure; lock.unlock()
        if let error { throw error }
        guard allowed, OctopusDesktopNative.parentChannel.isOpen else {
            throw DesktopFailure(code: "app_scope_closed", message: "The retained native application grant or its owning channel was withdrawn")
        }
    }

    func record(_ effect: DesktopAppFocusEffect) {
        lock.lock(); defer { lock.unlock() }
        if effects.count < 4096 { effects.append(effect) }
        else { failure = DesktopFailure(code: "app_read_evidence_limit", message: "Native read preparation exceeded its evidence bound") }
    }

    func fail(_ error: DesktopFailure) { lock.lock(); if failure == nil { failure = error }; lock.unlock() }
    func takeEffects() -> [DesktopAppFocusEffect] { lock.lock(); defer { lock.unlock() }; let value = effects; effects.removeAll(); return value }
    func invalidate() { lock.lock(); active = false; lock.unlock() }
}

struct DesktopAppReadFailure: Error, Encodable {
    let scope: DesktopAppScope
    let app: DesktopAppRef
    let message: String
    let preparation_effects: [DesktopAppFocusEffect]
    let effect_uncertain: Bool
}

struct DesktopAppActionTarget: Codable {
    let grant_request_id: String
    let app: DesktopAppRef
    let observation_id: String
    let revision: UInt64
    let control_generation: UInt64
    let window_id: String?

    func requiredWindowID() throws -> String {
        guard let window_id, !window_id.isEmpty else {
            throw DesktopFailure(code: "app_window_required", message: "This operation requires its exact captured application window")
        }
        return window_id
    }
}

enum DesktopAppCaptureSelection: String {
    case primary
    case allWindows = "all_windows"
}

@MainActor
final class DesktopApplicationRoom {
    let applications: DesktopApplications
    let accessibility: DesktopAccessibility
    let capture: DesktopApplicationCapture
    let input: DesktopAppInput
    private let behavior: DesktopApplicationBehavior
    private let instructionCatalog: DesktopApplicationInstructionCatalog
    private var readAssertions: [DesktopAppRef: DesktopAXEnablementAssertion] = [:]
    private struct ObservedImage {
        let imageID: String
        let window: DesktopAppCaptureWindow
        let geometry: DesktopAppCaptureGeometry
    }
    private struct ObservedApp {
        let observation: DesktopAXObservation
        let controlGeneration: UInt64
        let windows: [DesktopAppCaptureWindow]
        let images: [ObservedImage]
    }
    private var observed: [DesktopAppRef: ObservedApp] = [:]
    private struct ReadSource {
        let targetHandle: String
        let grant: String
        let lifetime: DesktopNativeAppGrant
        let source: [String: Any]
        let windows: [String]
    }
    private var readSources: [DesktopAppRef: ReadSource] = [:]
    private var inputFocus: [DesktopAppRef: DesktopAppFocusEnforcer] = [:]
    @MainActor private final class CursorBinding {
        let app: DesktopAppRef
        let generation: String
        let window: DesktopAppInputWindow
        let lease: DesktopNativeInputLease
        let grant: DesktopNativeAppGrant
        var action: UUID
        var cursor: DesktopAppCursor?
        var observation: DesktopAppPIPObservation?
        var receipts: [[String: Any]] = []
        var logicalPoint: CGPoint?

        init(app: DesktopAppRef, generation: String, window: DesktopAppInputWindow,
             lease: DesktopNativeInputLease, grant: DesktopNativeAppGrant, action: UUID) {
            self.app = app; self.generation = generation; self.window = window
            self.lease = lease; self.grant = grant; self.action = action
        }

        func check(_ action: UUID) throws {
            guard action == self.action else { throw DesktopFailure(code: "cursor_action_changed", message: "Cursor callback belongs to a replaced native operation") }
            try lease.checkCaller(); try grant.check()
        }

        func close() { observation?.invalidate(); observation = nil; cursor?.release(); cursor = nil }
    }
    private var inputCursors: [String: CursorBinding] = [:]
    private var inputLease: DesktopNativeInputLease?
    private var released = false
    let scope: DesktopAppScope

    init(scope: DesktopAppScope) throws {
        self.scope = scope
        behavior = try DesktopApplicationBehavior.read()
        instructionCatalog = try DesktopApplicationInstructionCatalog.read()
        applications = try DesktopApplications(scope: scope)
        accessibility = DesktopAccessibility(applications: applications)
        capture = DesktopApplicationCapture(applications: applications)
        input = DesktopAppInput(applications: applications)
    }

    func check() throws {
        guard !released, OctopusDesktopNative.parentChannel.isOpen else {
            throw DesktopFailure(code: "app_scope_closed", message: "Application room or its owning parent channel was released")
        }
    }

    func close() async throws {
        var firstFailure: Error?
        for assertion in readAssertions.values {
            do { try assertion.disable() } catch { if firstFailure == nil { firstFailure = error } }
        }
        readAssertions.removeAll()
        for source in readSources.values { source.lifetime.invalidate() }
        closeInput()
        released = true
        observed.removeAll(); readSources.removeAll()
        input.close()
        accessibility.close()
        applications.close()
        await capture.close()
        if let firstFailure { throw firstFailure }
    }

    func closeInput() {
        for focus in inputFocus.values { focus.close() }
        inputFocus.removeAll()
        for cursor in inputCursors.values { cursor.close() }
        inputCursors.removeAll()
        inputLease = nil
    }

    private func closeInput(_ app: DesktopAppRef) {
        inputFocus.removeValue(forKey: app)?.close()
        for id in inputCursors.keys.filter({ inputCursors[$0]?.app == app }) {
            inputCursors.removeValue(forKey: id)?.close()
        }
    }

    func pipSource(_ requested: DesktopPIPScope, windowID: String) throws -> DesktopAppPIPSource {
        try check()
        guard requested.room == scope, let retained = readSources[requested.app], retained.grant == requested.grant_request_id,
              retained.windows.contains(windowID) else {
            throw DesktopFailure(code: "pip_source_unavailable", message: "Native capture has no retained approved application-window source")
        }
        let readCheck = { [weak self] in
            guard let self else { throw DesktopFailure(code: "pip_source_unavailable", message: "Native source owner was released") }
            try self.check()
            guard let current = self.readSources[requested.app], current.grant == requested.grant_request_id,
                  current.lifetime === retained.lifetime else {
                throw DesktopFailure(code: "app_scope_closed", message: "Native presentation read grant was withdrawn")
            }
            try current.lifetime.check()
        }
        try capture.preparePresentation(requested.app, grant: requested.grant_request_id, sourceWindowID: windowID,
            check: readCheck, resolve: { [weak self] windows, additional in
                guard let self else { throw DesktopFailure(code: "pip_source_unavailable", message: "Native source owner was released") }
                try readCheck()
                return try self.accessibility.presentationInventory(requested.app, windowIDs: Set(windows.map(\.cg_window_id)),
                    additional: additional, check: readCheck)
            })
        let primary = try capture.liveWindow(requested.app, windowID: windowID)
        let snapshot = { [weak self] () throws -> DesktopAppPIPRenderer.Snapshot in
            guard let self else { throw DesktopFailure(code: "pip_source_unavailable", message: "Native source owner was released") }
            try self.check()
            guard let current = self.readSources[requested.app], current.grant == requested.grant_request_id,
                  current.windows.contains(windowID) else {
                throw DesktopFailure(code: "pip_source_unavailable", message: "Native application read scope was withdrawn")
            }
            try current.lifetime.check()
            return DesktopAppPIPRenderer.Snapshot(application: try self.applications.application(requested.app),
                primary: try self.capture.liveWindow(requested.app, windowID: windowID, incarnation: primary.incarnation),
                associated: try self.capture.presentationWindows(requested.app, primary: windowID, incarnation: primary.incarnation))
        }
        return DesktopAppPIPSource(scope: requested, windowID: windowID, source: retained.source, snapshot: snapshot,
            cursor: { [weak self] in
                _ = try snapshot()
                return try self?.inputCursors[windowID]?.cursor?.pipSnapshot()
            }, observe: { [weak self] callback in
                guard let self else { throw DesktopFailure(code: "pip_source_unavailable", message: "Native source owner was released") }
                _ = try snapshot()
                return try self.capture.observePresentation(requested.app, windowID: windowID, incarnation: primary.incarnation, changed: callback)
            }, humanFocus: { [weak self] in
                guard let self else { throw DesktopFailure(code: "pip_source_unavailable", message: "Native source owner was released") }
                _ = try snapshot()
                try self.accessibility.raiseWindow(requested.app, windowID: primary.descriptor.cg_window_id, check: { _ = try snapshot() })
            })
    }

    func state(target: String, includeScreenshots: Bool, captureSelection: DesktopAppCaptureSelection, cancelFile: String, controlGeneration: UInt64,
               grantRequestID: String, source: [String: Any], presentationBase: DesktopAXPresentationBase?, didObserve: (DesktopAppRef) -> Void) async throws -> [String: Any] {
        var retainedGrant: DesktopNativeAppGrant?
        let check = { [self] in
            try self.check(); try Task.checkCancellation()
            try retainedGrant?.check()
            try self.applications.validatePolicy(target)
            guard !FileManager.default.fileExists(atPath: cancelFile) else {
                throw DesktopFailure(code: "application_interrupted", message: "Application operation was cancelled by its owning control boundary")
            }
        }
        try check()
        if let presentationBase { try accessibility.validatePresentationBase(presentationBase) }
        let app = try await applications.openPreparedTarget(target, check: check)
        if let presentationBase, presentationBase.app != app.app { throw DesktopFailure(code: "diff_base_foreign", message: "Display compression base belongs to another native application") }
        try check()
        let lifetime: DesktopNativeAppGrant
        if let previous = readSources[app.app], previous.grant == grantRequestID {
            try previous.lifetime.check(); lifetime = previous.lifetime
        } else {
            if let previous = readSources[app.app] {
                try readAssertions.removeValue(forKey: app.app)?.disable()
                previous.lifetime.invalidate(); closeInput(app.app)
            }
            lifetime = DesktopNativeAppGrant(app: app.app, requestID: grantRequestID)
        }
        retainedGrant = lifetime
        var publishingEffects: [DesktopAppFocusEffect] = []
        var unpublishedObservation: DesktopAXObservation?
        readSources[app.app] = ReadSource(targetHandle: target, grant: grantRequestID, lifetime: lifetime, source: source,
            windows: readSources[app.app]?.windows ?? [])
        do {
            try accessibility.configureMenus(app.app, profile: behavior.resolve(app.bundle_id), checkRead: { [weak self] in
                guard let self else { throw DesktopFailure(code: "app_scope_closed", message: "Native menu read owner was released") }
                try self.check()
                guard let current = self.readSources[app.app], current.grant == grantRequestID, current.lifetime === lifetime else {
                    throw DesktopFailure(code: "app_scope_closed", message: "Native menu read grant was withdrawn")
                }
                try current.lifetime.check()
            }, onSignal: { [weak self] signal in
                guard let self, let enforcer = self.inputFocus[app.app] else { return }
                switch signal {
                case .closed: enforcer.stopMenuDismissalSuppression()
                case .opened(let menuApplication):
                    guard let lease = self.inputLease else { self.closeInput(app.app); return }
                    do {
                        try lease.checkCaller()
                        try enforcer.startMenuDismissalSuppression(menuApplication: menuApplication)
                    } catch let error as DesktopFailure {
                        lease.interrupt(error); self.closeInput(app.app)
                    } catch {
                        lease.interrupt(DesktopFailure(code: "menu_input_interrupted", message: String(describing: error)))
                        self.closeInput(app.app)
                    }
                }
            })
            let initial = try await capture.inventory(app.app, check: check)
            try check()
            let nativeWindow = try input.preparationWindow(app.app, windows: initial, check: check)
            let prepareCheck = { [self] in
                try check()
                if let nativeWindow, let window = initial.first(where: { $0.cg_window_id == nativeWindow.windowID }) {
                    try self.capture.validateRetainedWindow(app.app, windowID: window.window_id)
                    _ = try self.input.validate(nativeWindow, check: check)
                }
            }
            let options = try behavior.resolve(app.bundle_id).full_ui
            let operation = UUID()
            let lease: DesktopNativeInputLease?
            if options.enforceFocusBeforeEnhanced {
                let current = try OctopusDesktopNative.inputLeases.require(DesktopInputLeaseIdentity(scope: scope, control_generation: controlGeneration))
                try current.begin(operation); lease = current
            } else { lease = nil }
            do {
                let assertion = try await DesktopAXEnablementAssertion.acquireRead(app: app.app,
                    application: applications.application(app.app), target: nativeWindow, options: options,
                    checkCaller: { try lifetime.check() }, interrupt: { lifetime.fail($0) }, record: { lifetime.record($0) },
                    makeFocus: { [self] in
                        guard let lease else { throw DesktopFailure(code: "control_changed", message: "Native read focus preparation has no retained device lease") }
                        try lease.checkOperation(operation)
                        return try DesktopAppFocusEnforcer(app: app.app, applications: self.applications, input: self.input,
                            checkCaller: { try lifetime.check(); try lease.checkCaller() }, interrupt: { lease.interrupt($0) },
                            record: { lease.record($0); lifetime.record($0) })
                    }, check: { try prepareCheck(); try lease?.checkOperation(operation) })
                let previous = readAssertions.removeValue(forKey: app.app)
                readAssertions[app.app] = assertion
                try previous?.disable()
                _ = lease?.finish(operation)
            } catch {
                _ = lease?.finish(operation)
                if let interrupted = error as? DesktopAXEnablementInterrupted { readAssertions[app.app] = interrupted.cleanup }
                throw error
            }
            try check()
            let windows = try await capture.inventory(app.app, check: check)
            try check()
            var incarnations: [String: String] = [:]
            for window in windows { incarnations[window.window_id] = try capture.retainedWindowGeneration(app.app, windowID: window.window_id) }
            let primary = try accessibility.primaryWindow(app.app, windows: windows, incarnations: incarnations, check: check)
            let observation = try accessibility.observe(app.app, windows: windows, presentationBase: presentationBase, primary: primary,
                profile: behavior.resolve(app.bundle_id).accessibility_presentation, check: check)
            unpublishedObservation = observation
            let captureCheck = { [self] in
                try check()
                try self.accessibility.validateObservationContext(observation, primary: primary, check: check)
                if let primary {
                    guard try self.capture.retainedWindowGeneration(app.app, windowID: primary.window.window_id) == primary.incarnation else {
                        throw DesktopFailure(code: "primary_window_changed", message: "The primary native window incarnation changed during this operation")
                    }
                }
            }
            var captures: [DesktopAppCapture] = []
            var omissions: [[String: String]] = []
            var totalBytes = 0
            let orderedWindows: [DesktopAppCaptureWindow]
            switch captureSelection {
            case .primary:
                orderedWindows = primary.map { [$0.window] } ?? []
                omissions = windows.filter { $0.window_id != primary?.window.window_id }.map { ["window_id":$0.window_id,"reason":"not_selected"] }
            case .allWindows:
                if let primary { orderedWindows = [primary.window] + windows.filter { $0.window_id != primary.window.window_id } }
                else { orderedWindows = windows }
            }
            for window in orderedWindows {
                try check()
                if !includeScreenshots || captures.count >= OctopusDesktopNative.maxMediaItems || totalBytes == OctopusDesktopNative.maxMediaBytes {
                    omissions.append(["window_id":window.window_id,"reason":includeScreenshots ? "media_budget" : "screenshots_not_requested"])
                    continue
                }
                let cursor: DesktopAppCursor?
                if let primary, window.window_id == primary.window.window_id,
                   let binding = inputCursors[window.window_id], binding.app == app.app, binding.generation == primary.incarnation {
                    cursor = binding.cursor
                } else { cursor = nil }
                let frame = try await capture.capture(app.app, windowID: window.window_id, format: "png", cursor: cursor, check: captureCheck)
                try captureCheck()
                if frame.image.byte_length > OctopusDesktopNative.maxMediaBytes - totalBytes {
                    omissions.append(["window_id":window.window_id,"reason":"media_byte_budget"])
                } else { captures.append(frame); totalBytes += frame.image.byte_length }
            }
            try check()
            for window in windows { try capture.validateRetainedWindow(app.app, windowID: window.window_id) }
            try captureCheck()
            let primaryCapture = primary.map { primary in
                DesktopAppPrimaryCapture(observation_id: observation.observation_id, revision: observation.revision,
                    source_window_id: primary.window.window_id, source_window_incarnation: primary.incarnation,
                    selection: primary.selection, image_id: captures.first(where: { $0.window.window_id == primary.window.window_id })?.image.image_id)
            }
            let current = try applications.info(app.app)
            let instructions = try instructionCatalog.resolve(current, application: applications.application(app.app), check: check)
            let instructionsJSON: Any = try instructions.map { try Self.json($0) } ?? NSNull()
            publishingEffects = lifetime.takeEffects()
            let result: [String: Any] = ["scope":try Self.json(scope),"control_generation":controlGeneration,"application":try Self.json(current),
                "accessibility":try Self.json(observation),"windows":try Self.json(windows),"captures":try Self.json(captures),
                "capture_omissions":omissions,"preparation_effects":try Self.json(publishingEffects),"instructions":instructionsJSON,
                "primary_capture":try primaryCapture.map { try Self.json($0) } ?? NSNull()]
            try check()
            readSources[app.app] = ReadSource(targetHandle: target, grant: grantRequestID, lifetime: lifetime, source: source, windows: windows.map(\.window_id))
            try captureCheck()
            try accessibility.commitObservation(observation)
            unpublishedObservation = nil
            observed[app.app] = ObservedApp(observation: observation, controlGeneration: controlGeneration, windows: windows,
                images: captures.map { ObservedImage(imageID: $0.image.image_id, window: $0.window, geometry: $0.geometry) })
            didObserve(app.app)
            return result
        } catch {
            if let unpublishedObservation { accessibility.discardObservation(unpublishedObservation) }
            let effects = publishingEffects + lifetime.takeEffects()
            throw DesktopAppReadFailure(scope: scope, app: app.app, message: String(describing: error),
                preparation_effects: effects, effect_uncertain: !effects.isEmpty)
        }
    }

    private func originalImagePoint(_ target: DesktopAppActionTarget, imageID: String, x: Double, y: Double) throws -> CGPoint {
        try check()
        let windowID = try target.requiredWindowID()
        guard let previous = observed[target.app], previous.observation.observation_id == target.observation_id,
              previous.observation.revision == target.revision, previous.controlGeneration == target.control_generation,
              let image = previous.images.first(where: { $0.imageID == imageID && $0.window.window_id == windowID && $0.window.app == target.app }),
              x.isFinite, y.isFinite, x >= 0, y >= 0,
              x < Double(image.geometry.pixel_width), y < Double(image.geometry.pixel_height),
              image.geometry.pixel_to_screen.count == 6 else {
            throw DesktopFailure(code: "stale_image", message: "Input point must identify original pixels of the exact retained application-window image")
        }
        let matrix = image.geometry.pixel_to_screen
        let point = CGPoint(x: matrix[0] * x + matrix[2] * y + matrix[4],
                            y: matrix[1] * x + matrix[3] * y + matrix[5])
        let frame = image.window.frame_screen_points
        guard matrix.allSatisfy({ $0.isFinite }), point.x.isFinite, point.y.isFinite,
              CGRect(x: frame.x, y: frame.y, width: frame.width, height: frame.height).contains(point) else {
            throw DesktopFailure(code: "invalid_app_geometry", message: "Original image coordinates do not map into their exact captured native window")
        }
        try capture.validateRetainedWindow(target.app, windowID: windowID)
        return point
    }

    private func inputWindow(_ target: DesktopAppActionTarget, check: @escaping () throws -> Void) throws -> DesktopAppInputWindow {
        try check()
        let windowID = try target.requiredWindowID()
        guard let previous = observed[target.app], previous.observation.observation_id == target.observation_id,
              previous.observation.revision == target.revision, previous.controlGeneration == target.control_generation,
              let window = previous.windows.first(where: { $0.window_id == windowID && $0.app == target.app }) else {
            throw DesktopFailure(code: "stale_observation", message: "Native input requires its exact current app-window observation")
        }
        let frame = window.frame_screen_points
        return try accessibility.inputWindow(target.app, observationID: target.observation_id, revision: target.revision,
            windowID: window.cg_window_id, bounds: CGRect(x: frame.x, y: frame.y, width: frame.width, height: frame.height), check: check)
    }

    private func focus(_ app: DesktopAppRef, lease: DesktopNativeInputLease,
                       grant: DesktopNativeAppGrant) throws -> DesktopAppFocusEnforcer {
        try check(); try lease.checkCaller(); try grant.check()
        if inputLease !== lease { closeInput(); inputLease = lease }
        if let existing = inputFocus[app] { try existing.check(); return existing }
        let created = try DesktopAppFocusEnforcer(app: app, applications: applications, input: input,
            checkCaller: { try lease.checkCaller(); try grant.check() },
            interrupt: { lease.interrupt($0) }, record: { lease.record($0) })
        inputFocus[app] = created
        return created
    }

    private func cursor(_ target: DesktopAppActionTarget, window: DesktopAppInputWindow,
                        lease: DesktopNativeInputLease, grant: DesktopNativeAppGrant, action: UUID) throws -> CursorBinding {
        try check(); try lease.checkCaller(); try grant.check()
        let windowID = try target.requiredWindowID()
        let generation = try capture.retainedWindowGeneration(target.app, windowID: windowID)
        if let existing = inputCursors[windowID], existing.generation == generation,
           existing.lease === lease, existing.grant === grant, existing.cursor != nil {
            existing.action = action; existing.receipts.removeAll()
            return existing
        }
        inputCursors.removeValue(forKey: windowID)?.close()
        let binding = CursorBinding(app: target.app, generation: generation, window: window,
            lease: lease, grant: grant, action: action)
        let native = try DesktopAppCursor(application: applications.application(target.app), window: window,
            windowGeneration: generation, check: { [weak self, weak binding] id in
                guard let self, let binding else { throw DesktopFailure(code: "cursor_source_closed", message: "The native cursor owner was released") }
                try self.check(); try binding.check(id)
                let live = try self.capture.liveWindow(binding.app, windowID: windowID, incarnation: generation)
                let rect = live.descriptor.frame_screen_points
                let current = DesktopAppInputWindow(app: binding.app, element: binding.window.element,
                    windowID: binding.window.windowID, bounds: CGRect(x: rect.x, y: rect.y, width: rect.width, height: rect.height),
                    flipped: binding.window.flipped)
                let pid = try self.input.validate(current, check: { try binding.check(id) })
                return DesktopAppCursor.Snapshot(window: current, windowGeneration: generation, processID: pid,
                    isOnScreen: live.descriptor.on_screen, applicationIsActive: try self.applications.application(binding.app).isActive,
                    menusOpen: try self.accessibility.hasOpenMenu(binding.app, check: { try binding.check(id) }),
                    windowLevel: live.descriptor.window_layer)
            }, checkWallpaper: { [weak self, weak binding] id, display, generation in
                guard let self, let binding else { throw DesktopFailure(code: "cursor_source_closed", message: "The native cursor owner was released") }
                try binding.check(id)
                try self.capture.validateCursorWallpaper(binding.app, windowID: windowID,
                    actionIdentity: id, displayID: display, captureGeneration: generation, check: { try binding.check(id) })
            }, onEvent: { [weak binding] event in
                guard let binding else { return }
                switch event {
                case .presented(let receipt):
                    guard receipt.actionIdentity == binding.action, binding.receipts.count < 4096 else { return }
                    binding.receipts.append(["app":["host_generation":receipt.app.host_generation,"app_instance_id":receipt.app.app_instance_id], "window_id":windowID,
                        "window_generation":receipt.windowGeneration, "action_id":receipt.actionIdentity.uuidString.lowercased(),
                        "movement_id":receipt.movementIdentity.uuidString.lowercased(),
                        "milestone":receipt.milestone == .closeEnough ? "close_enough" : "finished",
                        "callback_provenance":receipt.callbackProvenance.rawValue, "callback_timestamp":receipt.callbackTimestamp,
                        "x":receipt.cgScreenPoint.x, "y":receipt.cgScreenPoint.y])
                case .interrupted(let id, _, let error):
                    if id == binding.action {
                        binding.lease.interrupt((error as? DesktopFailure) ?? DesktopFailure(code: "cursor_interrupted", message: String(describing: error)))
                    }
                }
            })
        binding.cursor = native
        binding.observation = try capture.observeLiveWindows(target.app, windows: [windowID]) { [weak binding] event in
            guard let binding, let native = binding.cursor else { return }
            switch event {
            case .geometryChanged:
                do { try native.refresh(actionIdentity: binding.action) }
                catch { native.invalidate(error) }
            case .sourceUnavailable(let error): native.invalidate(error)
            }
        }
        inputCursors[windowID] = binding
        return binding
    }

    private func positionCursor(_ target: DesktopAppActionTarget, window: DesktopAppInputWindow, point: CGPoint,
                                lease: DesktopNativeInputLease, grant: DesktopNativeAppGrant, action: UUID,
                                check: @escaping () throws -> Void) async throws -> CursorBinding {
        let windowID = try target.requiredWindowID()
        let binding = try cursor(target, window: window, lease: lease, grant: grant, action: action)
        let wallpaper = try await capture.cursorWallpaper(target.app, windowID: windowID,
            atCGScreenPoint: point, actionIdentity: action, check: check)
        try check()
        guard let cursor = binding.cursor else { throw DesktopFailure(code: "cursor_source_closed", message: "Native cursor was released") }
        _ = try await cursor.move(toCGScreenPoint: point, actionIdentity: action,
            wallpapers: [wallpaper], nextInteractionTiming: .closeEnough)
        try check()
        binding.logicalPoint = point
        return binding
    }

    func action(_ call: [String: Any], cancelFile: String) async throws -> [String: Any] {
        let target = try JSONDecoder().decode(DesktopAppActionTarget.self, from: JSONSerialization.data(withJSONObject: call["target"] ?? NSNull()))
        guard let operation = call["operation"] as? [String: Any], let name = operation["type"] as? String,
              let params = operation["params"] as? [String: Any] else {
            throw DesktopFailure(code: "invalid_app_action", message: "Application action requires a typed operation and exact observed target")
        }
        let operationID = UUID()
        var lease: DesktopNativeInputLease?
        var effect: DesktopAXEffect?
        var menuEffects: [DesktopAXMenuEffect] = []
        let preparation = DesktopAXPreparationEvidence()
        var delivery: DesktopAppInputDelivery?
        var paste: DesktopPasteEvidence?
        var clipboardFailure: [String: Any]?
        var cursorBinding: CursorBinding?
        var dispatchAttempted = false
        var dispatchedEvents = 0
        var focusPending = false
        var errorValue: [String: Any]?
        let check = { [self] in
            try self.check(); try Task.checkCancellation()
            guard !FileManager.default.fileExists(atPath: cancelFile) else {
                throw DesktopFailure(code: "application_interrupted", message: "Application action was cancelled by its owning control boundary")
            }
            guard let grant = self.readSources[target.app], grant.grant == target.grant_request_id else {
                throw DesktopFailure(code: "app_scope_closed", message: "Input does not identify the exact retained native app grant")
            }
            try grant.lifetime.check()
            try self.applications.validatePolicy(grant.targetHandle)
            try lease?.checkOperation(operationID)
            if let windowID = target.window_id { try self.capture.validateRetainedWindow(target.app, windowID: windowID) }
        }
        do {
            try check()
            let activeLease = try OctopusDesktopNative.inputLeases.require(DesktopInputLeaseIdentity(scope: scope, control_generation: target.control_generation))
            try activeLease.begin(operationID); lease = activeLease
            guard let previous = observed[target.app], previous.observation.observation_id == target.observation_id,
                  previous.observation.revision == target.revision, previous.controlGeneration == target.control_generation,
                  let grant = readSources[target.app] else {
                throw DesktopFailure(code: "stale_observation", message: "Application action requires its exact retained app, window, observation and control generation")
            }
            let mouseMode = try behavior.resolve(applications.info(target.app).bundle_id).mouse_targeting
            let isAX = ["app_set_value", "app_select_text", "app_secondary_action"].contains(name)
            let isElementClick = name == "app_click" && params["element_id"] is String
            let isElementScroll = name == "app_scroll" && params["element_id"] is String
            let isKeyboard = ["app_key", "app_type", "app_paste"].contains(name)
            let window: DesktopAppCaptureWindow?
            if let windowID = target.window_id {
                guard let observedWindow = previous.windows.first(where: { $0.window_id == windowID && $0.app == target.app }) else {
                    throw DesktopFailure(code: "stale_window", message: "The action window is absent from its exact retained observation")
                }
                let current = try await capture.validateWindow(target.app, windowID: windowID, check: check)
                try check()
                guard current == observedWindow else { throw DesktopFailure(code: "stale_window", message: "Native window changed after application observation") }
                window = observedWindow
                let normalWindow = previous.observation.nodes.contains { node in
                    node.window_id == windowID && node.role == kAXWindowRole &&
                    node.subrole != kAXFloatingWindowSubrole && node.subrole != kAXDialogSubrole
                }
                try capture.notePresentationInteraction(target.app, windowID: windowID, isNormalWindow: normalWindow)
            } else {
                guard isAX || isElementClick || isKeyboard else { throw DesktopFailure(code: "app_window_required", message: "Coordinate input requires its exact observed application window") }
                window = nil
            }
            if isElementClick {
                guard let count = params["count"] as? Int, (1...3).contains(count),
                      let buttonName = params["button"] as? String, ["left", "right", "middle"].contains(buttonName),
                      params["image_id"] == nil || params["image_id"] is NSNull,
                      params["x"] == nil || params["x"] is NSNull,
                      params["y"] == nil || params["y"] is NSNull else {
                    throw DesktopFailure(code: "invalid_app_action", message: "An element click cannot also supply an image point")
                }
                let guardValue = DesktopAXGuard(app: target.app, observation_id: target.observation_id,
                    revision: target.revision, element_id: try OctopusDesktopNative.text(params, "element_id"))
                let nativeWindow = try target.window_id.map { _ in try inputWindow(target, check: check) }
                let button: CGMouseButton = buttonName == "left" ? .left : buttonName == "right" ? .right : .center
                let prepared = try await accessibility.prepareClick(guardValue, windowID: window?.cg_window_id,
                    button: button, count: count, evidence: preparation, check: check)
                observed.removeValue(forKey: target.app)
                let enforcer = try focus(target.app, lease: activeLease, grant: grant.lifetime)
                if let nativeWindow, let point = prepared.point, nativeWindow.bounds.contains(point) {
                    cursorBinding = try await positionCursor(target, window: nativeWindow, point: point,
                        lease: activeLease, grant: grant.lifetime, action: operationID, check: check)
                }
                try check()
                if prepared.usesMenu {
                    focusPending = true
                    _ = try await enforcer.enforce(target: nativeWindow, check: check)
                    focusPending = false
                    menuEffects.append(try accessibility.performMenuClick(prepared, check: check))
                    dispatchAttempted = true
                } else if prepared.usesAccessibility {
                    focusPending = true
                    try await enforcer.prepareAccessibilityClick(target: nativeWindow, check: { [self] in
                        try check()
                        try self.accessibility.validateClick(prepared, check: check)
                    })
                    focusPending = false
                    try check()
                    dispatchAttempted = true
                    effect = try accessibility.performClick(prepared, check: check)
                } else {
                    guard let nativeWindow, let point = prepared.point else {
                        throw DesktopFailure(code: "element_not_clickable", message: "The prepared native mouse click has no exact window point")
                    }
                    focusPending = true
                    _ = try await enforcer.enforce(target: nativeWindow, check: check)
                    focusPending = false
                    try accessibility.validateClick(prepared, check: check)
                    let mouseTarget = try input.mouseTarget(nativeWindow, at: point,
                        element: accessibility.clickElement(prepared, check: check), mode: mouseMode, check: check)
                    let events = try input.click(at: point, button: button, count: count,
                        flags: [], target: mouseTarget, check: check)
                    try accessibility.consumeClick(prepared, check: check)
                    try check(); dispatchAttempted = true
                    delivery = try await input.send(events, to: mouseTarget, interval: .milliseconds(100), check: check)
                    dispatchedEvents = delivery?.dispatched_events ?? 0
                }
                if !prepared.usesMenu { try enforcer.synthesizedActionWasPerformed() }
            } else if isElementScroll {
                guard let window, let windowID = target.window_id,
                      params["image_id"] == nil || params["image_id"] is NSNull,
                      params["x"] == nil || params["x"] is NSNull,
                      params["y"] == nil || params["y"] is NSNull,
                      params["delta_x"] == nil || params["delta_x"] is NSNull,
                      params["delta_y"] == nil || params["delta_y"] is NSNull else {
                    throw DesktopFailure(code: "invalid_app_scroll", message: "Element scrolling requires its exact window and page-distance variant")
                }
                let direction = try OctopusDesktopNative.text(params, "direction")
                let pages = try OctopusDesktopNative.number(params, "pages")
                let guardValue = DesktopAXGuard(app: target.app, observation_id: target.observation_id,
                    revision: target.revision, element_id: try OctopusDesktopNative.text(params, "element_id"))
                let nativeWindow = try inputWindow(target, check: check)
                let prepared = try await accessibility.prepareScroll(guardValue, windowID: window.cg_window_id,
                    windowBounds: nativeWindow.bounds, direction: direction, evidence: preparation, check: check)
                let delta = try DesktopAppInput.pageScroll(direction: direction, pages: pages, axisExtent: prepared.axisExtent)
                observed.removeValue(forKey: target.app)
                let enforcer = try focus(target.app, lease: activeLease, grant: grant.lifetime)
                let point: CGPoint
                if let nativePoint = prepared.point {
                    point = nativePoint
                    cursorBinding = try await positionCursor(target, window: nativeWindow, point: point,
                        lease: activeLease, grant: grant.lifetime, action: operationID, check: check)
                } else {
                    let generation = try capture.retainedWindowGeneration(target.app, windowID: windowID)
                    guard let existing = inputCursors[windowID], existing.app == target.app,
                          existing.generation == generation,
                          existing.lease === activeLease, existing.grant === grant.lifetime,
                          existing.cursor != nil, let retainedPoint = existing.logicalPoint, nativeWindow.bounds.contains(retainedPoint) else {
                        throw DesktopFailure(code: "scroll_point_unavailable", message: "The native element has no clickable point or retained cursor in this exact window and control lease")
                    }
                    cursorBinding = try cursor(target, window: nativeWindow, lease: activeLease, grant: grant.lifetime, action: operationID)
                    point = retainedPoint
                }
                focusPending = true
                _ = try await enforcer.enforce(target: nativeWindow, check: check)
                focusPending = false
                let mouseTarget = try input.mouseTarget(nativeWindow, at: point, mode: mouseMode, check: check)
                let event = try input.scroll(at: point, horizontal: delta.horizontal, vertical: delta.vertical, target: mouseTarget, check: check)
                try accessibility.consumeScroll(prepared, check: check)
                try check(); dispatchAttempted = true
                delivery = try await input.send([event], to: mouseTarget, interval: .milliseconds(10), check: check)
                dispatchedEvents = delivery?.dispatched_events ?? 0
                try enforcer.synthesizedActionWasPerformed()
            } else if isAX {
                let guardValue = DesktopAXGuard(app: target.app, observation_id: target.observation_id,
                    revision: target.revision, element_id: try OctopusDesktopNative.text(params, "element_id"))
                let action: DesktopAccessibility.Action
                switch name {
                case "app_set_value":
                    guard let value = params["value"] as? String else { throw DesktopFailure(code: "invalid_app_action", message: "Set value requires an exact string") }
                    action = .setValue(value)
                case "app_select_text":
                    action = .selectText(text: try OctopusDesktopNative.text(params, "text"), prefix: params["prefix"] as? String,
                        suffix: params["suffix"] as? String, selectionType: try OctopusDesktopNative.text(params, "selection_type"))
                default:
                    action = .secondaryAction(try OctopusDesktopNative.text(params, "action_id"))
                }
                let nativeWindow = try target.window_id.map { _ in try inputWindow(target, check: check) }
                let prepared = try await accessibility.prepare(guardValue, windowID: window?.cg_window_id, action: action, evidence: preparation, check: check)
                observed.removeValue(forKey: target.app)
                let enforcer = try focus(target.app, lease: activeLease, grant: grant.lifetime)
                if let nativeWindow, let point = prepared.point,
                   point.x >= nativeWindow.bounds.minX, point.x <= nativeWindow.bounds.maxX,
                   point.y >= nativeWindow.bounds.minY, point.y <= nativeWindow.bounds.maxY {
                    cursorBinding = try await positionCursor(target, window: nativeWindow, point: point,
                        lease: activeLease, grant: grant.lifetime, action: operationID, check: check)
                }
                focusPending = true
                _ = try await enforcer.enforce(target: nativeWindow, check: check)
                focusPending = false
                if name == "app_set_value" || name == "app_select_text" {
                    let profile = try behavior.resolve(applications.info(target.app).bundle_id)
                    if let point = try accessibility.focusField(prepared, clickDOMField: profile.focus_dom_field_by_click,
                        evidence: preparation, check: check) {
                            guard let nativeWindow else { throw DesktopFailure(code: "field_not_focusable", message: "Native field focus has no exact window") }
                            cursorBinding = try await positionCursor(target, window: nativeWindow, point: point,
                                lease: activeLease, grant: grant.lifetime, action: operationID, check: check)
                            let element = try accessibility.fieldElement(prepared, at: point, check: check)
                            let mouseTarget = try input.mouseTarget(nativeWindow, at: point, element: element, mode: mouseMode, check: check)
                            let events = try input.click(at: point, button: .left, count: 1, flags: [], target: mouseTarget, check: check)
                            try check()
                            guard preparation.effects.count < 600 else { throw DesktopFailure(code: "ax_effect_limit", message: "Native field preparation exceeds its effect bound") }
                            let index = preparation.effects.count
                            preparation.effects.append(.init(source_element_id: guardValue.element_id, affected_element_id: guardValue.element_id,
                                mechanism: "CGEventPostToPid", name: "field_focus", value: nil, dispatch_state: "attempted", native_error_code: nil, dispatched_events: 0))
                            do {
                                let result = try await input.send(events, to: mouseTarget, interval: .milliseconds(100), check: check)
                                preparation.effects[index].dispatch_state = "returned"
                                preparation.effects[index].dispatched_events = UInt32(result.dispatched_events)
                                dispatchedEvents += result.dispatched_events
                            } catch let error as DesktopAppInputInterrupted {
                                preparation.effects[index].dispatched_events = UInt32(error.dispatched_events)
                                throw error
                            }
                    }
                }
                try check()
                if prepared.usesMenu {
                    menuEffects.append(try accessibility.performMenu(prepared, check: check))
                    dispatchAttempted = true
                } else {
                    effect = try accessibility.perform(prepared, check: check)
                    dispatchAttempted = true
                    try enforcer.synthesizedActionWasPerformed()
                }
            } else {
                let nativeWindow = try target.window_id.map { _ in try inputWindow(target, check: check) }
                let point: CGPoint?
                let destination: CGPoint?
                var typingPlan: [DesktopAppInput.KeyStroke]?
                var keyStroke: DesktopAppInput.KeyStroke?
                var scrollDelta: (horizontal: Int32, vertical: Int32)?
                switch name {
                case "app_click", "app_move", "app_scroll":
                    point = try originalImagePoint(target, imageID: OctopusDesktopNative.text(params, "image_id"),
                        x: OctopusDesktopNative.number(params, "x"), y: OctopusDesktopNative.number(params, "y"))
                    destination = nil
                case "app_drag":
                    let image = try OctopusDesktopNative.text(params, "image_id")
                    point = try originalImagePoint(target, imageID: image, x: OctopusDesktopNative.number(params, "from_x"), y: OctopusDesktopNative.number(params, "from_y"))
                    destination = try originalImagePoint(target, imageID: image, x: OctopusDesktopNative.number(params, "to_x"), y: OctopusDesktopNative.number(params, "to_y"))
                case "app_type":
                    guard let text = params["text"] as? String else { throw DesktopFailure(code: "invalid_app_action", message: "Type requires exact Unicode text") }
                    typingPlan = try input.keystrokes(for: text, check: check)
                    point = nil; destination = nil
                case "app_key":
                    let code: CGKeyCode
                    var flags: CGEventFlags = []
                    if params.keys.contains("key") {
                        guard let key = params["key"] as? String, !key.isEmpty, key.utf8.count <= 256,
                              params["key_code"] == nil, params["modifiers"] == nil else {
                            throw DesktopFailure(code: "invalid_app_action", message: "Named key input cannot also supply a virtual code or modifier array")
                        }
                        (code, flags) = try DesktopInput.key(key)
                    } else {
                        guard let native = params["key_code"] as? UInt16, let modifiers = params["modifiers"] as? [String],
                              Set(modifiers).count == modifiers.count else {
                            throw DesktopFailure(code: "invalid_app_action", message: "Key input requires a native code and unique declared modifiers")
                        }
                        code = native
                        for modifier in modifiers {
                            switch modifier {
                            case "command": flags.insert(.maskCommand)
                            case "control": flags.insert(.maskControl)
                            case "option": flags.insert(.maskAlternate)
                            case "shift": flags.insert(.maskShift)
                            default: throw DesktopFailure(code: "invalid_app_action", message: "Key modifier is not supported")
                            }
                        }
                    }
                    keyStroke = .init(keyCode: code, flags: flags, text: nil)
                    point = nil; destination = nil
                case "app_paste": point = nil; destination = nil
                default: throw DesktopFailure(code: "unsupported_app_action", message: "Application operation is not supported")
                }
                if name == "app_scroll" {
                    guard let nativeWindow else { throw DesktopFailure(code: "app_window_required", message: "Scroll has no exact native window") }
                    if let direction = params["direction"] as? String, let pages = params["pages"] as? Double {
                        guard params["delta_x"] == nil || params["delta_x"] is NSNull,
                              params["delta_y"] == nil || params["delta_y"] is NSNull else {
                            throw DesktopFailure(code: "invalid_app_scroll", message: "Page scroll cannot also provide pixel deltas")
                        }
                        scrollDelta = try DesktopAppInput.pageScroll(direction: direction, pages: pages,
                            axisExtent: ["up", "down"].contains(direction) ? nativeWindow.bounds.height : nativeWindow.bounds.width)
                    } else {
                        guard params["direction"] == nil || params["direction"] is NSNull,
                              params["pages"] == nil || params["pages"] is NSNull,
                              let x = params["delta_x"] as? Int32, let y = params["delta_y"] as? Int32,
                              abs(Int64(x)) <= 10000, abs(Int64(y)) <= 10000, x != 0 || y != 0 else {
                            throw DesktopFailure(code: "invalid_app_scroll", message: "Pixel scroll requires bounded nonzero integer deltas")
                        }
                        scrollDelta = (x,y)
                    }
                }
                try accessibility.consumeObservation(target.app, observationID: target.observation_id, revision: target.revision)
                observed.removeValue(forKey: target.app)
                let enforcer = try focus(target.app, lease: activeLease, grant: grant.lifetime)
                if let point {
                    guard let nativeWindow else { throw DesktopFailure(code: "app_window_required", message: "Native pointer input has no observed window") }
                    cursorBinding = try await positionCursor(target, window: nativeWindow, point: point,
                        lease: activeLease, grant: grant.lifetime, action: operationID, check: check)
                }
                focusPending = true
                _ = try await enforcer.enforce(target: nativeWindow, check: check)
                focusPending = false
                try check()
                let mouseTarget = try point.map { point in
                    guard let nativeWindow else { throw DesktopFailure(code: "app_window_required", message: "Native pointer input has no observed window") }
                    return try input.mouseTarget(nativeWindow, at: point, mode: mouseMode, check: check)
                }
                let keyboardTarget = try isKeyboard ? input.keyboardTarget(target.app, window: nativeWindow, check: check) : nil
                var events: [CGEvent] = []
                var interval: Duration = .zero
                switch name {
                case "app_click":
                    guard let point, let mouseTarget, let count = params["count"] as? Int, (1...3).contains(count),
                          let name = params["button"] as? String, ["left", "right", "middle"].contains(name) else {
                        throw DesktopFailure(code: "invalid_app_action", message: "Click requires an exact native point, button and count")
                    }
                    let button: CGMouseButton = name == "left" ? .left : name == "right" ? .right : .center
                    events = try input.click(at: point, button: button, count: count, flags: [], target: mouseTarget, check: check)
                    interval = .zero
                case "app_move":
                    guard let point, let mouseTarget else { throw DesktopFailure(code: "invalid_app_action", message: "Move requires a native point") }
                    events = [try input.mouse(.move, at: point, button: .left, count: 1, flags: [], target: mouseTarget, check: check)]
                    interval = .milliseconds(10)
                case "app_drag":
                    guard let point, let destination, let mouseTarget else { throw DesktopFailure(code: "invalid_app_action", message: "Drag requires two exact native points") }
                    events = try input.click(at: point, dragTo: destination, button: .left, count: 1,
                                             flags: [], target: mouseTarget, check: check)
                    interval = .zero
                case "app_scroll":
                    guard let point, let mouseTarget else { throw DesktopFailure(code: "invalid_app_action", message: "Scroll requires an exact native point and window") }
                    guard let delta = scrollDelta else { throw DesktopFailure(code: "invalid_app_scroll", message: "Scroll has no validated native distance") }
                    events = [try input.scroll(at: point, horizontal: delta.horizontal, vertical: delta.vertical, target: mouseTarget, check: check)]
                    interval = .milliseconds(10)
                case "app_key":
                    guard let keyStroke else { throw DesktopFailure(code: "invalid_app_action", message: "Key input has no validated native physical key") }
                    events = try input.keyboard(keyStroke)
                case "app_type":
                    guard let typingPlan else { throw DesktopFailure(code: "invalid_app_action", message: "Type has no retained native keyboard plan") }
                    events = try input.keyboard(typingPlan, check: check)
                    interval = .microseconds(Int64(try behavior.resolve(applications.info(target.app).bundle_id).typing_interval_us))
                case "app_paste":
                    guard let text = params["text"] as? String, let format = params["format"] as? String,
                          let kind = DesktopPasteFormat.Kind(rawValue: format) else {
                        throw DesktopFailure(code: "invalid_app_action", message: "Paste requires an exact supported representation")
                    }
                    guard let keyboardTarget else { throw DesktopFailure(code: "app_keyboard_target_unavailable", message: "Paste has no retained native keyboard target") }
                    let representations = try DesktopPasteFormat.convert(text, format: kind, check: check)
                    let key = try input.keyboard(.init(keyCode: 9, flags: .maskCommand, text: nil))
                    paste = try await DesktopPasteboard.shared.paste(representations,
                        application: applications.application(target.app), focusedElement: keyboardTarget.focusedElement,
                        check: check) { [self] in
                        try check(); dispatchAttempted = true
                        return try await self.input.send(key, to: keyboardTarget, interval: .zero, check: check)
                    }
                    delivery = paste?.delivery
                default: throw DesktopFailure(code: "unsupported_app_action", message: "Application operation is not supported")
                }
                if !events.isEmpty {
                    try check(); dispatchAttempted = true
                    if let mouseTarget {
                        delivery = try await input.send(events, to: mouseTarget, interval: interval, check: check)
                    } else if let keyboardTarget {
                        delivery = try await input.send(events, to: keyboardTarget, interval: interval,
                            maximumEvents: typingPlan.map { $0.count * 4 } ?? 4096, check: check)
                    } else {
                        throw DesktopFailure(code: "app_event_target_unavailable", message: "Native event delivery has no retained target")
                    }
                }
                dispatchedEvents = delivery?.dispatched_events ?? 0
                try enforcer.synthesizedActionWasPerformed()
            }
            try check()
            if menuEffects.isEmpty, effect != nil || delivery != nil,
               name == "app_click" || name == "app_secondary_action", let elementID = params["element_id"] as? String,
               let cleared = try accessibility.clearMenuAfterAction(target.app, sourceElementID: elementID,
                   revision: target.revision, check: check) {
                menuEffects.append(cleared)
            }
            try check()
        } catch let error as DesktopAXDispatchFailure {
            dispatchAttempted = true
            errorValue = ["code":"ax_action_failed", "message":"Native AX dispatch returned \(error.native_error_code); no action was retried", "native_error_code":error.native_error_code]
        } catch let error as DesktopAppInputInterrupted {
            if !focusPending { dispatchedEvents += error.dispatched_events }
            dispatchAttempted = dispatchAttempted || error.dispatched_events > 0
            errorValue = ["code":"application_input_interrupted", "message":String(describing: error.cause)]
        } catch let error as DesktopPasteInterrupted {
            dispatchedEvents = error.delivery?.dispatched_events ?? 0
            dispatchAttempted = true
            clipboardFailure = ["clipboard_read_acknowledged":error.clipboard_read_acknowledged,
                "clipboard_restored":error.clipboard_restored, "restoration_error":error.restoration_error as Any? ?? NSNull()]
            errorValue = ["code":"application_paste_interrupted", "message":String(describing: error.cause)]
        } catch let error as DesktopFailure {
            errorValue = ["code":error.code,"message":error.message]
        } catch {
            errorValue = ["code":"application_interrupted","message":String(describing: error)]
        }
        let focusEffects = lease?.finish(operationID) ?? []
        let focusCount = focusEffects.reduce(0) { $0 + $1.dispatched_events }
        dispatchAttempted = dispatchAttempted || focusCount > 0 || !preparation.effects.isEmpty
        if errorValue != nil {
            errorValue?["dispatch_attempted"] = dispatchAttempted
            errorValue?["dispatched_events"] = dispatchedEvents + focusCount
            errorValue?["effect_uncertain"] = dispatchAttempted
            if errorValue?["native_error_code"] == nil { errorValue?["native_error_code"] = NSNull() }
        }
        return ["scope":try Self.json(scope), "call":call, "completed":errorValue == nil,
            "ax_effect":try effect.map(Self.json) ?? NSNull(), "input_delivery":try delivery.map(Self.json) ?? NSNull(),
            "focus_effects":try Self.json(focusEffects), "cursor_milestones":cursorBinding?.receipts ?? [],
            "preparation_effects":try Self.json(preparation.effects), "menu_effects":try Self.json(menuEffects),
            "paste_evidence":try paste.map(Self.json) ?? NSNull(), "clipboard_failure":clipboardFailure as Any? ?? NSNull(),
            "error":errorValue as Any? ?? NSNull()]
    }

    static func json<T: Encodable>(_ value: T) throws -> Any {
        try JSONSerialization.jsonObject(with: JSONEncoder().encode(value))
    }
}

@MainActor
final class DesktopNativePermissions {
    private struct Requirement {
        let id = UUID().uuidString.lowercased()
        let kind: String
        let title: String
        let anchor: String
        let api: String
    }
    private var activeRequest: String?
    private var settingsSource: SettingsSource?

    private final class SettingsSource {
        let context: Continuation
        let permission: String
        var application: NSRunningApplication?
        var birth: ProcessBirth?
        var observer: DesktopWindowObserver?
        var selected: (UInt32, DesktopWindowStamp)?
        var ended = false
        var sequence: UInt64 = 0
        var lastPublication: Data?

        init(context: Continuation, permission: String) { self.context = context; self.permission = permission }
        func close() { ended = true; try? observer?.close(); observer = nil }
    }

    private struct ProcessBirth: Equatable {
        let pid: pid_t
        let seconds: UInt64
        let microseconds: UInt64
        let path: String

        init?(_ application: NSRunningApplication) {
            let pid = application.processIdentifier
            var info = proc_bsdinfo()
            let size = Int32(MemoryLayout<proc_bsdinfo>.size)
            var bytes = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
            guard pid > 0, proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size,
                  info.pbi_start_tvsec > 0, info.pbi_start_tvusec < 1_000_000,
                  bytes.withUnsafeMutableBytes({ proc_pidpath(pid, $0.baseAddress, UInt32($0.count)) }) > 0,
                  let path = bytes.withUnsafeBufferPointer({ String(validatingCString: $0.baseAddress!) }),
                  path.hasPrefix("/"), path.utf8.count <= 4096 else { return nil }
            self.pid = pid
            seconds = info.pbi_start_tvsec; microseconds = info.pbi_start_tvusec
            self.path = URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
        }
    }

    private let requirements = [
        Requirement(kind: "screen_capture", title: "Screen Recording", anchor: "Privacy_ScreenCapture", api: "CGPreflightScreenCaptureAccess"),
        Requirement(kind: "accessibility", title: "Accessibility", anchor: "Privacy_Accessibility", api: "AXIsProcessTrusted")
    ]

    private func snapshot() throws -> [String: Any] {
        guard OctopusDesktopNative.parentChannel.isOpen else {
            throw DesktopFailure(code: "parent_disconnected", message: "Native permission owner channel is closed")
        }
        let pid = getpid()
        var bytes = [CChar](repeating: 0, count: 4 * Int(MAXPATHLEN))
        let count = bytes.withUnsafeMutableBytes { proc_pidpath(pid, $0.baseAddress, UInt32($0.count)) }
        guard count > 0, let path = bytes.withUnsafeBufferPointer({ String(validatingCString: $0.baseAddress!) }),
              path.hasPrefix("/"), path.utf8.count <= 4096 else {
            throw DesktopFailure(code: "permission_identity_unavailable", message: "Native helper executable identity is unavailable")
        }
        let displayName = ProcessInfo.processInfo.processName
        guard !displayName.isEmpty, displayName.utf8.count <= 4096, !displayName.contains("\0") else {
            throw DesktopFailure(code: "permission_identity_unavailable", message: "Native helper process name is unavailable")
        }
        let current = NSRunningApplication.current
        var bundle: Any = NSNull()
        if let identifier = current.bundleIdentifier, !identifier.isEmpty,
           let url = current.bundleURL, url.isFileURL {
            bundle = ["identifier":identifier, "path":url.path]
        }
        let screen = CGPreflightScreenCaptureAccess()
        let accessibility = AXIsProcessTrusted()
        let rows: [[String: Any]] = requirements.map { requirement in
            let granted = requirement.kind == "screen_capture" ? screen : accessibility
            return ["id":requirement.id, "kind":requirement.kind, "title":requirement.title,
                "status":granted ? "granted" : "not_granted", "can_request":false, "can_open_settings":true,
                "detail":"Change this permission in System Settings. Native status does not distinguish a prior refusal from permission not yet granted.",
                "provenance":["process_id":pid, "api":requirement.api]]
        }
        return ["helper":["process_id":pid, "executable_path":path, "display_name":displayName,
            "bundle":bundle, "responsible_app":NSNull()], "permissions":rows]
    }

    private struct Continuation {
        let requestID: String
        let cancelPath: String
        let deadline: UInt64
        let room: String
        let generation: String

        init(_ action: [String: Any]) throws {
            room = try OctopusDesktopNative.text(action, "worker_environment_id")
            generation = try OctopusDesktopNative.text(action, "host_generation")
            guard room.utf8.count <= 512, generation.utf8.count == 32,
                  generation.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }) else {
                throw DesktopFailure(code: "invalid_permission_action", message: "Native permission room or host generation is invalid")
            }
            requestID = try OctopusDesktopNative.text(action, "request_id")
            cancelPath = try OctopusDesktopNative.text(action, "cancel_file")
            let expiry = try OctopusDesktopNative.text(action, "expires_at_mach_continuous_ticks")
            var timebase = mach_timebase_info_data_t()
            guard UUID(uuidString: requestID)?.uuidString.lowercased() == requestID,
                  cancelPath.hasPrefix("/"), cancelPath.utf8.count <= 4096,
                  let deadline = UInt64(expiry), String(deadline) == expiry,
                  mach_timebase_info(&timebase) == KERN_SUCCESS, timebase.numer > 0, timebase.denom > 0 else {
                throw DesktopFailure(code: "invalid_permission_action", message: "Native permission continuation identity or deadline is invalid")
            }
            let now = mach_continuous_time()
            guard deadline > now,
                  Double(deadline - now) * Double(timebase.numer) / Double(timebase.denom) <= 120_000_000_000 else {
                throw DesktopFailure(code: "permission_request_expired", message: "Native permission continuation is outside its admitted deadline")
            }
            self.deadline = deadline
        }

        func check() throws {
            try Task.checkCancellation()
            guard OctopusDesktopNative.parentChannel.isOpen else {
                throw DesktopFailure(code: "parent_disconnected", message: "Native permission owner channel is closed")
            }
            if access(cancelPath, F_OK) == 0 {
                throw DesktopFailure(code: "permission_request_cancelled", message: "Native permission caller cancelled this request")
            }
            guard errno == ENOENT else {
                throw DesktopFailure(code: "permission_cancellation_unavailable", message: "Native permission cancellation cannot be observed")
            }
            guard mach_continuous_time() < deadline else {
                throw DesktopFailure(code: "permission_request_expired", message: "Native permission continuation reached its declared deadline")
            }
        }
    }

    func readSettingsSource(_ action: [String: Any]) throws -> [String: Any] {
        guard Set(action.keys) == ["type", "permission_request_id", "worker_environment_id", "host_generation"],
              action["type"] as? String == "permission_settings_source" else {
            throw DesktopFailure(code: "invalid_permission_action", message: "Native Settings reader has invalid fields")
        }
        let id = try OctopusDesktopNative.text(action, "permission_request_id")
        let room = try OctopusDesktopNative.text(action, "worker_environment_id")
        let generation = try OctopusDesktopNative.text(action, "host_generation")
        guard UUID(uuidString: id)?.uuidString.lowercased() == id, room.utf8.count <= 512,
              generation.utf8.count == 32, generation.utf8.allSatisfy({ (48...57).contains($0) || (97...102).contains($0) }),
              OctopusDesktopNative.parentChannel.isOpen else {
            throw DesktopFailure(code: "invalid_permission_action", message: "Native Settings reader identity or owner channel is invalid")
        }
        var result: [String: Any] = ["schema_version":1, "host_generation":generation,
            "permission_request_id":id, "permission":NSNull(), "state":"ended", "settings":NSNull(), "window":NSNull()]
        guard let source = settingsSource else { return result }
        guard source.context.requestID == id, source.context.room == room, source.context.generation == generation,
              activeRequest == id else {
            throw DesktopFailure(code: "permission_source_mismatch", message: "Settings belongs to another exact pending operation")
        }
        func finish(_ result: [String: Any]) throws -> [String: Any] {
            try source.context.check()
            guard settingsSource === source, activeRequest == id else {
                throw DesktopFailure(code: "permission_source_mismatch", message: "Settings continuation changed during its read")
            }
            if result["state"] as? String != "observed" { source.lastPublication = nil }
            return result
        }
        try source.context.check()
        result["permission"] = source.permission
        guard !source.ended else { return try finish(result) }
        result["state"] = "waiting"
        guard let application = source.application else { return try finish(result) }
        guard !application.isTerminated,
              NSRunningApplication(processIdentifier: application.processIdentifier)?.isEqual(application) == true else {
            source.close(); result["state"] = "ended"; return try finish(result)
        }
        guard let currentBirth = ProcessBirth(application) else { return try finish(result) }
        if let birth = source.birth, birth != currentBirth {
            source.close(); result["state"] = "ended"; return try finish(result)
        }
        source.birth = currentBirth
        if let (id, stamp) = source.selected {
            guard let observed = try source.observer?.snapshot()[id], !observed.terminated,
                  observed.incarnation == stamp.incarnation else {
                source.close(); result["state"] = "ended"; return try finish(result)
            }
        }
        guard let numbers = NSWindow.windowNumbers(options: .allApplications),
              let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
            return try finish(result)
        }
        if let (id, _) = source.selected,
           let existing = windows.first(where: { ($0[kCGWindowNumber as String] as? UInt32) == id }),
           let owner = existing[kCGWindowOwnerPID as String] as? Int32, owner != currentBirth.pid {
            source.close(); result["state"] = "ended"; return try finish(result)
        }
        let visible = Set(numbers.map { $0.uint32Value })
        let candidates = windows.compactMap { window -> (UInt32, CGRect)? in
            guard (window[kCGWindowOwnerPID as String] as? Int32) == currentBirth.pid,
                  let id = window[kCGWindowNumber as String] as? UInt32, id > 0, visible.contains(id),
                  let bounds = window[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary),
                  [rect.origin.x, rect.origin.y, rect.width, rect.height].allSatisfy({ $0.isFinite }),
                  rect.width > 0, rect.height > 0 else { return nil }
            return (id, rect)
        }
        let candidate: (UInt32, CGRect)?
        if let (id, _) = source.selected { candidate = candidates.first { $0.0 == id } }
        else { candidate = candidates.first { $0.1.width > 600 && $0.1.height >= 470 } }
        guard let (windowID, bounds) = candidate else {
            source.lastPublication = nil
            return try finish(result)
        }
        if source.selected == nil {
            let observer = try DesktopWindowObserver()
            do {
                try observer.watch([windowID])
                guard let stamp = try observer.snapshot()[windowID], !stamp.terminated else {
                    throw DesktopFailure(code: "permission_source_unavailable", message: "Selected Settings window has no native lifetime")
                }
                source.observer = observer; source.selected = (windowID, stamp)
            } catch { try? observer.close(); throw error }
        }
        guard let (_, stamp) = source.selected, let observer = source.observer else {
            throw DesktopFailure(code: "permission_source_unavailable", message: "Selected Settings window was released")
        }
        let currentStamp = try observer.validateIncarnation(windowID, incarnation: stamp.incarnation)
        guard let displayName = application.localizedName, !displayName.isEmpty, displayName.utf8.count <= 4096,
              !displayName.contains("\0"), ProcessBirth(application) == currentBirth, !application.isTerminated else { return try finish(result) }
        var bundle: Any = NSNull()
        if let identifier = application.bundleIdentifier, !identifier.isEmpty,
           let url = application.bundleURL, url.isFileURL {
            bundle = ["identifier":identifier, "path":url.resolvingSymlinksInPath().standardizedFileURL.path]
        }
        let identity: [String: Any] = ["process_id":currentBirth.pid, "executable_path":currentBirth.path,
            "start_seconds":String(currentBirth.seconds), "start_microseconds":String(currentBirth.microseconds),
            "display_name":displayName, "bundle":bundle, "responsible_app":NSNull()]
        var window: [String: Any] = ["id":windowID,
            "bounds":["x":bounds.origin.x, "y":bounds.origin.y, "width":bounds.width, "height":bounds.height]]
        let publication = try JSONSerialization.data(withJSONObject: ["settings":identity, "window":window,
            "revision":currentStamp.revision], options: [.sortedKeys])
        if source.lastPublication != publication {
            guard source.sequence < 9_007_199_254_740_991 else {
                throw DesktopFailure(code: "permission_source_overflow", message: "Settings observation sequence reached its bound")
            }
            source.sequence += 1; source.lastPublication = publication
        }
        try source.context.check()
        window["observation_sequence"] = source.sequence
        result["state"] = "observed"; result["settings"] = identity; result["window"] = window
        return try finish(result)
    }

    private func validateScreenSettings(_ application: NSRunningApplication) throws -> Bool {
        guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
            throw DesktopFailure(code: "permission_settings_unavailable", message: "System Settings window metadata is unavailable")
        }
        return windows.contains { window in
            guard (window[kCGWindowOwnerPID as String] as? Int32) == application.processIdentifier,
                  window[kCGWindowName as String] is String,
                  let bounds = window[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return false }
            return rect.width > 600 && rect.height >= 470
        }
    }

    func execute(_ action: [String: Any]) async throws -> [String: Any] {
        let operation = try OctopusDesktopNative.text(action, "operation")
        let basic: Set<String> = ["type", "operation", "permission"]
        let expected = operation == "open_settings"
            ? basic.union(["request_id", "cancel_file", "expires_at_mach_continuous_ticks", "worker_environment_id", "host_generation"]) : basic
        guard Set(action.keys) == expected, action["type"] as? String == "permissions" else {
            throw DesktopFailure(code: "invalid_permission_action", message: "Native permission action has invalid fields")
        }
        if operation == "status" {
            guard action["permission"] is NSNull else {
                throw DesktopFailure(code: "invalid_permission_action", message: "Permission status does not accept a target")
            }
            return try snapshot()
        }
        guard operation == "request" || operation == "open_settings",
              let id = action["permission"] as? String, let requirement = requirements.first(where: { $0.id == id }) else {
            throw DesktopFailure(code: "invalid_permission_action", message: "Native permission target was not observed for this helper")
        }
        guard operation == "open_settings" else {
            throw DesktopFailure(code: "permission_settings_required", message: "This native permission is changed in System Settings; no direct request is supported")
        }
        let context = try Continuation(action)
        guard activeRequest == nil else {
            throw DesktopFailure(code: "permission_request_busy", message: "A native Settings continuation is already pending")
        }
        activeRequest = context.requestID
        let source = SettingsSource(context: context, permission: id)
        settingsSource = source
        defer { source.close(); settingsSource = nil; activeRequest = nil }
        try context.check()
        guard let url = URL(string: "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?" + requirement.anchor) else {
            throw DesktopFailure(code: "permission_settings_unavailable", message: "Native permission Settings destination is invalid")
        }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = true
        do {
            try context.check()
            let application: NSRunningApplication = try await withCheckedThrowingContinuation { continuation in
                NSWorkspace.shared.open(url, configuration: configuration) { application, error in
                    if let error { continuation.resume(throwing: error) }
                    else if let application { continuation.resume(returning: application) }
                    else { continuation.resume(throwing: DesktopFailure(code: "permission_settings_unavailable", message: "System Settings did not report an opened application")) }
                }
            }
            try context.check()
            source.application = application
            source.birth = ProcessBirth(application)
            while true {
                try context.check()
                guard activeRequest == context.requestID, !application.isTerminated else {
                    throw DesktopFailure(code: "permission_settings_closed", message: "The exact opened Settings application is no longer available")
                }
                let validated: Bool
                if requirement.kind == "accessibility" { validated = AXIsProcessTrusted() }
                else { validated = try validateScreenSettings(application) }
                if validated {
                    try context.check()
                    if requirement.kind == "screen_capture" { _ = CGRequestScreenCaptureAccess() }
                    try context.check()
                    let result = try snapshot()
                    try context.check()
                    return result
                }
                try await Task.sleep(for: .milliseconds(100))
            }
        } catch {
            throw DesktopFailure(code: "permission_settings_incomplete", message: "Explicit Settings operation did not finish: \(error). Settings may have opened; an already dispatched OS request cannot be recalled. Read current status before any new request.")
        }
    }
}

@MainActor
final class DesktopStreams {
    private let permissions = DesktopNativePermissions()
    private var streams: [String: (SCStream, DesktopFrameSink)] = [:]
    private var views: [String: String] = [:]
    private var applicationRooms: [String: DesktopApplicationRoom] = [:]
    private var presentationDiagnostics: [String: [String: String]] = [:]
    private lazy var presentations = DesktopAppPIPPublisher(resolve: { [weak self] scope, window in
        guard let room = self?.applicationRooms[scope.worker_environment_id], room.scope == scope.room else {
            throw DesktopFailure(code: "pip_source_unavailable", message: "Native presentation cannot construct or replace an application room")
        }
        return try room.pipSource(scope, windowID: window)
    }, diagnostic: { [weak self] id, status, code in
        guard let self, self.presentationDiagnostics[id] != nil || self.presentationDiagnostics.count < 256 else { return }
        self.presentationDiagnostics[id] = ["status":status,"code":code]
    })

    func execute(_ action: [String: Any]) async throws -> [String: Any] {
        let type = try OctopusDesktopNative.text(action, "type")
        if type == "permissions" { return try await permissions.execute(action) }
        if type == "permission_settings_source" { return try permissions.readSettingsSource(action) }
        if type == "pip_prepare" { return try presentations.prepare(action) }
        if type == "pip_install" { return try presentations.install(action) }
        if type == "pip_complete" { return try await presentations.complete(action) }
        if type == "pip_cancel" {
            presentations.cancel(try OctopusDesktopNative.text(action, "bootstrap_id"))
            return ["cancelled":true]
        }
        if type == "pip_status" { return ["presentations":presentationDiagnostics] }
        if type == "input_lease_begin" || type == "input_lease_revoke" {
            let identity = try DesktopInputLeaseIdentity.decode(action["lease"])
            if type == "input_lease_begin" { try OctopusDesktopNative.inputLeases.begin(identity) }
            else {
                OctopusDesktopNative.inputLeases.invalidate(identity)
                if let room = applicationRooms[identity.scope.worker_environment_id] {
                    guard room.scope == identity.scope else { throw DesktopFailure(code: "application_formation_changed", message: "Input teardown belongs to a different native room") }
                    room.closeInput()
                }
                OctopusDesktopNative.inputLeases.release(identity)
            }
            return ["input_lease":try DesktopApplicationRoom.json(identity), "input_quiesced":type == "input_lease_revoke"]
        }
        if type == "app_release" {
            let id = try OctopusDesktopNative.text(action, "worker_environment_id")
            let pin = try OctopusDesktopNative.text(action, "formation_pin")
            if let room = applicationRooms[id], room.scope.formation_pin != pin {
                throw DesktopFailure(code: "application_formation_changed", message: "Application release belongs to a different formation")
            }
            if let room = applicationRooms.removeValue(forKey: id) { await presentations.revoke(scope: room.scope); try await room.close() }
            return try OctopusDesktopNative.status()
        }
        if type == "apps" || type == "app_resolve" || type == "app_state" || type == "app_action" {
            guard OctopusDesktopNative.parentChannel.isOpen else { throw DesktopFailure(code: "parent_disconnected", message: "Application owner channel is closed") }
            let scope = try JSONDecoder().decode(DesktopAppScope.self, from: JSONSerialization.data(withJSONObject: action["scope"] ?? NSNull()))
            try scope.validate()
            let room: DesktopApplicationRoom
            if let existing = applicationRooms[scope.worker_environment_id] {
                guard existing.scope == scope else { throw DesktopFailure(code: "application_formation_changed", message: "Existing room has a different immutable formation") }
                room = existing
            } else {
                guard type == "app_resolve" || type == "apps", applicationRooms.count < 256 else {
                    throw DesktopFailure(code: "application_room_unavailable", message: "Application room has not resolved its target or exceeds the host bound")
                }
                room = try DesktopApplicationRoom(scope: scope)
                applicationRooms[scope.worker_environment_id] = room
            }
            try room.check()
            var result = try OctopusDesktopNative.status()
            if type == "apps" {
                let entries = try await room.applications.catalogEntries(check: { try room.check() })
                try room.check()
                result["catalog"] = ["scope":try DesktopApplicationRoom.json(scope), "applications":try DesktopApplicationRoom.json(entries)]
            } else if type == "app_resolve" {
                let selector = try JSONDecoder().decode(DesktopAppSelector.self, from: JSONSerialization.data(withJSONObject: action["selector"] ?? NSNull()))
                result["resolved"] = try DesktopApplicationRoom.json(room.applications.prepareTarget(selector))
            } else if type == "app_action" {
                let cancel = try OctopusDesktopNative.text(action, "cancel_file")
                guard cancel.hasPrefix("/"), let call = action["call"] as? [String: Any] else {
                    throw DesktopFailure(code: "invalid_app_action", message: "Application action requires an exact typed call and cancellation path")
                }
                result["application_action"] = try await room.action(call, cancelFile: cancel)
            } else {
                let target = try OctopusDesktopNative.text(action, "target_handle")
                let cancel = try OctopusDesktopNative.text(action, "cancel_file")
                guard cancel.hasPrefix("/"), let include = action["include_screenshots"] as? Bool,
                      let selectionName = action["capture_selection"] as? String,
                      let selection = DesktopAppCaptureSelection(rawValue: selectionName),
                      let generation = action["control_generation"] as? UInt64, generation <= 9_007_199_254_740_991 else {
                    throw DesktopFailure(code: "invalid_app_request", message: "Application capture requires exact cancellation and image mode")
                }
                let grant = try OctopusDesktopNative.text(action, "grant_request_id")
                guard let source = action["source"] as? [String: Any], Set(source.keys) == ["tool","correlation_id","source_movement_seq","source_artifact"] else {
                    throw DesktopFailure(code: "invalid_app_request", message: "Native app capture requires its exact admitted source evidence")
                }
                let presentationBase: DesktopAXPresentationBase?
                if let value = action["presentation_base"], !(value is NSNull) {
                    presentationBase = try JSONDecoder().decode(DesktopAXPresentationBase.self, from: JSONSerialization.data(withJSONObject: value))
                } else { presentationBase = nil }
                result["application"] = try await room.state(target: target, includeScreenshots: include, captureSelection: selection, cancelFile: cancel,
                    controlGeneration: generation, grantRequestID: grant, source: source, presentationBase: presentationBase, didObserve: { [self] app in
                        presentations.changed(scope: scope, app: app)
                        presentations.noteInteraction(scope: scope, app: app)
                    })
                try room.check()
            }
            return result
        }
        if type == "view_stop" {
            let view = try OctopusDesktopNative.text(action, "view_id")
            if let screen = views.removeValue(forKey: view), !views.values.contains(screen), let (stream, _) = streams.removeValue(forKey: screen) {
                try await stream.stopCapture()
            }
            return try OctopusDesktopNative.status()
        }
        if type != "view_start" { return try await OctopusDesktopNative.execute(action) }
        guard OctopusDesktopNative.parentChannel.isOpen else { throw DesktopFailure(code: "parent_disconnected", message: "Parent channel closed before capture attachment") }
        guard CGPreflightScreenCaptureAccess() else { throw DesktopFailure(code: "screen_capture_permission_required", message: "Screen Recording is not permitted") }
        let view = try OctopusDesktopNative.text(action, "view_id")
        let screenId = try OctopusDesktopNative.text(action, "screen_id")
        if let existing = views[view], existing != screenId { throw DesktopFailure(code: "view_identity_changed", message: "A view must keep its exact display identity") }
        if streams[screenId] == nil {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            guard let display = content.displays.first(where: { String($0.displayID) == screenId }),
                  let screen = try OctopusDesktopNative.screens().first(where: { $0["id"] as? String == screenId }) else { throw DesktopFailure(code: "display_changed", message: "Display is unavailable") }
            let config = SCStreamConfiguration()
            config.width = Int(try OctopusDesktopNative.number(screen, "pixel_width"))
            config.height = Int(try OctopusDesktopNative.number(screen, "pixel_height"))
            config.minimumFrameInterval = CMTime(value: 1, timescale: 15)
            config.queueDepth = 2
            config.showsCursor = true
            let sink = DesktopFrameSink(screen: screen)
            let stream = SCStream(filter: SCContentFilter(display: display, excludingWindows: []), configuration: config, delegate: sink)
            try stream.addStreamOutput(sink, type: .screen, sampleHandlerQueue: sink.queue)
            try await stream.startCapture()
            streams[screenId] = (stream, sink)
        }
        views[view] = screenId
        return try OctopusDesktopNative.status()
    }

    func close() async {
        await presentations.close()
        for room in applicationRooms.values {
            do { try await room.close() }
            catch { FileHandle.standardError.write(Data("Native application assertion cleanup failed: \(error)\n".utf8)) }
        }
        applicationRooms.removeAll()
        for (_, (stream, _)) in streams { try? await stream.stopCapture() }
        streams.removeAll(); views.removeAll()
    }
}

final class DesktopFrameSink: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    let queue = DispatchQueue(label: "com.octopus.desktop.frames")
    private let context = CIContext(options: [.cacheIntermediates: false])
    private let screen: [String: Any]
    private var frame: UInt64 = 0
    private var failed = false

    init(screen: [String: Any]) { self.screen = screen }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        try? OctopusDesktopNative.output(["event": "stream_error", "screen_id": screen["id"] ?? "", "message": String(describing: error)])
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard !failed, type == .screen, buffer.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let rawStatus = attachments.first?[.status] as? Int,
              SCFrameStatus(rawValue: rawStatus) == .complete,
              let pixels = CMSampleBufferGetImageBuffer(buffer) else { return }
        do {
            let current = try OctopusDesktopNative.screens().first(where: { $0["id"] as? String == screen["id"] as? String })
            guard let current, NSDictionary(dictionary: screen).isEqual(to: current) else { throw DesktopFailure(code: "display_changed", message: "The live display coordinate domain changed; reattach the viewer") }
            let source = CIImage(cvPixelBuffer: pixels)
            guard let image = context.createCGImage(source, from: source.extent) else { throw DesktopFailure(code: "frame_unavailable", message: "Cannot materialize captured frame") }
            let output = NSMutableData()
            guard let destination = CGImageDestinationCreateWithData(output, UTType.jpeg.identifier as CFString, 1, nil) else { throw DesktopFailure(code: "frame_unavailable", message: "Cannot encode captured frame") }
            CGImageDestinationAddImage(destination, image, [kCGImageDestinationLossyCompressionQuality: 0.8] as CFDictionary)
            guard CGImageDestinationFinalize(destination) else { throw DesktopFailure(code: "frame_unavailable", message: "Frame encoder failed") }
            let bytes = output as Data
            frame += 1
            try OctopusDesktopNative.output(["event": "frame", "screen_id": screen["id"] ?? "", "observation": [
                "id": "live-\(screen["id"] ?? "")-\(frame)", "screen": screen, "focus": NSNull(),
                "input_revision": OctopusDesktopNative.revision(.combinedSessionState),
                "captured_at_unix_ms": UInt64(Date().timeIntervalSince1970 * 1000),
                "image": ["media_type": "image/jpeg", "data_base64": bytes.base64EncodedString(), "byte_length": bytes.count,
                    "width": image.width, "height": image.height, "sha256": SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()]
            ]])
        } catch {
            failed = true
            try? OctopusDesktopNative.output(["event": "stream_error", "screen_id": screen["id"] ?? "", "message": String(describing: error)])
        }
    }
}
