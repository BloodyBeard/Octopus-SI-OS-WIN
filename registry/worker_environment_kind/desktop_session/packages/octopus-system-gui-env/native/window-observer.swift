import AppKit
import ApplicationServices
import CoreGraphics
import Darwin
import Foundation

struct DesktopWindowStamp: Equatable {
    let incarnation: UInt64
    let revision: UInt64
    let terminated: Bool
}

private final class DesktopWindowCallbackState: @unchecked Sendable {
    private let lock = NSLock()
    private var windows: [UInt32: DesktopWindowStamp] = [:]
    private var sequence: UInt64 = 0
    private var failure: String?
    private var closed = false
    private var listeners: [UUID: (Set<UInt32>, @MainActor () -> Void)] = [:]
    private var changed: Set<UInt32> = []
    private var notificationPending = false

    func subscribe(_ ids: Set<UInt32>, changed: @escaping @MainActor () -> Void) throws -> UUID {
        lock.lock(); defer { lock.unlock() }
        try requireOpen()
        guard !ids.isEmpty, ids.isSubset(of: Set(windows.keys)), listeners.count < 256 else {
            throw DesktopFailure(code: "window_observer_limit", message: "Live capture must observe a bounded retained native window set")
        }
        let id = UUID(); listeners[id] = (ids, changed); return id
    }

    func unsubscribe(_ id: UUID) { lock.lock(); listeners.removeValue(forKey: id); lock.unlock() }

    private func notify(_ ids: Set<UInt32>) {
        changed.formUnion(ids)
        guard !notificationPending else { return }
        notificationPending = true
        DispatchQueue.main.async { [weak self] in self?.deliverChanges() }
    }

    @MainActor private func deliverChanges() {
        lock.lock()
        let ids = changed; changed.removeAll(); notificationPending = false
        let callbacks = listeners.values.filter { closed || failure != nil || !$0.0.isDisjoint(with: ids) }.map { $0.1 }
        if closed { listeners.removeAll() }
        lock.unlock()
        for callback in callbacks { callback() }
    }

    func watch(_ ids: Set<UInt32>) throws {
        lock.lock(); defer { lock.unlock() }
        try requireOpen()
        guard ids.count <= 4096, !ids.contains(0) else {
            throw DesktopFailure(code: "window_observer_limit", message: "Native window observation exceeds its exact identity bound")
        }
        let removed = Set(windows.keys).subtracting(ids)
        windows = windows.filter { ids.contains($0.key) }
        if !removed.isEmpty { notify(removed) }
        for id in ids where windows[id] == nil || windows[id]?.terminated == true {
            let next = try advance()
            windows[id] = DesktopWindowStamp(incarnation: next, revision: next, terminated: false)
        }
    }

    func snapshot() throws -> [UInt32: DesktopWindowStamp] {
        lock.lock(); defer { lock.unlock() }
        try requireOpen()
        return windows
    }

    func receive(_ event: UInt32, payload: UnsafeMutableRawPointer?, length: UInt32) {
        lock.lock(); defer { lock.unlock() }
        guard !closed, failure == nil else { return }
        guard let payload, length >= UInt32(MemoryLayout<UInt32>.size) else {
            failure = "Native window notification omitted its window identity"
            notify(Set(windows.keys))
            return
        }
        let id = payload.loadUnaligned(as: UInt32.self)
        guard let previous = windows[id] else { return }
        do {
            let next = try advance()
            windows[id] = DesktopWindowStamp(incarnation: event == 811 ? next : previous.incarnation,
                                             revision: next,
                                             terminated: event == 804 || (previous.terminated && event != 811))
        } catch { failure = "Native window notification sequence overflowed" }
        notify([id])
    }

    func close() { lock.lock(); closed = true; notify(Set(windows.keys)); windows.removeAll(); lock.unlock() }

    private func advance() throws -> UInt64 {
        guard sequence < 9_007_199_254_740_991 else {
            failure = "Native window notification sequence overflowed"
            throw DesktopFailure(code: "window_observer_unavailable", message: failure!)
        }
        sequence += 1
        return sequence
    }

    private func requireOpen() throws {
        if let failure { throw DesktopFailure(code: "window_observer_unavailable", message: failure) }
        guard !closed else { throw DesktopFailure(code: "window_observer_closed", message: "Native window observation was released") }
    }
}

private final class DesktopWindowCallbackOwners: @unchecked Sendable {
    static let shared = DesktopWindowCallbackOwners()
    private let lock = NSLock()
    private var next: UInt = 1
    private var owners: [UInt: DesktopWindowCallbackState] = [:]

    func insert(_ state: DesktopWindowCallbackState) throws -> UnsafeMutableRawPointer {
        lock.lock(); defer { lock.unlock() }
        guard next < UInt.max, owners.count < 256, let token = UnsafeMutableRawPointer(bitPattern: next) else {
            throw DesktopFailure(code: "window_observer_limit", message: "Native window callback ownership exceeds its bound")
        }
        owners[next] = state
        next += 1
        return token
    }

    func owner(_ token: UnsafeMutableRawPointer?) -> DesktopWindowCallbackState? {
        guard let token else { return nil }
        lock.lock(); defer { lock.unlock() }
        return owners[UInt(bitPattern: token)]
    }

    func remove(_ token: UnsafeMutableRawPointer) {
        lock.lock(); owners.removeValue(forKey: UInt(bitPattern: token)); lock.unlock()
    }
}

final class DesktopWindowObserver {
    private typealias Callback = @convention(c) (UInt32, UnsafeMutableRawPointer?, UInt32, UnsafeMutableRawPointer?) -> Void
    private typealias MainConnection = @convention(c) () -> OpaquePointer?
    private typealias Notify = @convention(c) (OpaquePointer?, Callback, UInt32, UnsafeMutableRawPointer?) -> CGError
    private typealias Request = @convention(c) (OpaquePointer?, UnsafePointer<UInt32>, UInt32) -> Void
    private static let callback: Callback = { event, payload, length, context in
        DesktopWindowCallbackOwners.shared.owner(context)?.receive(event, payload: payload, length: length)
    }
    private let library: UnsafeMutableRawPointer
    private let connection: OpaquePointer
    private let remove: Notify
    private let request: Request
    private let state: DesktopWindowCallbackState
    private let token: UnsafeMutableRawPointer
    private let operations = NSLock()
    private var registered: [UInt32] = []
    private var associationLibrary: UnsafeMutableRawPointer?
    private var copyAssociated: (@convention(c) (OpaquePointer?, UInt32) -> Unmanaged<CFArray>?)?
    private var closed = false

    init() throws {
        guard let library = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_NOW | RTLD_LOCAL) else {
            throw DesktopFailure(code: "window_observer_unavailable", message: "The native window server library is unavailable")
        }
        guard let main = dlsym(library, "CGSMainConnectionID"),
              let register = dlsym(library, "CGSRegisterConnectionNotifyProc"),
              let remove = dlsym(library, "CGSRemoveConnectionNotifyProc"),
              let request = dlsym(library, "CGSRequestNotificationsForWindows") else {
            dlclose(library)
            throw DesktopFailure(code: "window_observer_unavailable", message: "The exact native window notification functions are unavailable")
        }
        guard let connection = unsafeBitCast(main, to: MainConnection.self)() else {
            dlclose(library)
            throw DesktopFailure(code: "window_observer_unavailable", message: "The native window server connection is unavailable")
        }
        let state = DesktopWindowCallbackState()
        let token: UnsafeMutableRawPointer
        do { token = try DesktopWindowCallbackOwners.shared.insert(state) }
        catch { dlclose(library); throw error }
        self.library = library
        self.connection = connection
        self.remove = unsafeBitCast(remove, to: Notify.self)
        self.request = unsafeBitCast(request, to: Request.self)
        self.state = state
        self.token = token
        let add = unsafeBitCast(register, to: Notify.self)
        for event in [UInt32(804), 806, 807, 809, 811] {
            let result = add(connection, Self.callback, event, token)
            guard result == .success else {
                let cleanup = Result { try close() }
                throw DesktopFailure(code: "window_observer_unavailable", message: "Native window notification registration failed (\(result.rawValue)); cleanup=\(cleanup)")
            }
            registered.append(event)
        }
    }

    deinit { try? close(); if let associationLibrary { dlclose(associationLibrary) }; dlclose(library) }

    func watch(_ ids: Set<UInt32>) throws {
        operations.lock(); defer { operations.unlock() }
        try state.watch(ids)
        let ordered = ids.sorted()
        if !ordered.isEmpty {
            ordered.withUnsafeBufferPointer { buffer in
                request(connection, buffer.baseAddress!, UInt32(buffer.count))
            }
        }
    }

    func associatedWindowIDs(_ id: UInt32) throws -> [UInt32] {
        operations.lock(); defer { operations.unlock() }
        guard !closed, id != 0 else {
            throw DesktopFailure(code: "window_observer_closed", message: "Native window associations require a live exact window")
        }
        if copyAssociated == nil {
            guard let loaded = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW | RTLD_LOCAL) else {
                throw DesktopFailure(code: "window_association_unavailable", message: "The native window association library is unavailable")
            }
            guard let function = dlsym(loaded, "SLSCopyAssociatedWindows") else {
                dlclose(loaded)
                throw DesktopFailure(code: "window_association_unavailable", message: "The exact native window association function is unavailable")
            }
            associationLibrary = loaded
            copyAssociated = unsafeBitCast(function, to: (@convention(c) (OpaquePointer?, UInt32) -> Unmanaged<CFArray>?).self)
        }
        guard let copyAssociated else {
            throw DesktopFailure(code: "window_association_unavailable", message: "Native window association function was released")
        }
        guard let result = copyAssociated(connection, id)?.takeRetainedValue() else { return [] }
        guard CFArrayGetCount(result) <= 4096 else {
            throw DesktopFailure(code: "window_association_limit", message: "Native window association list exceeds its bound")
        }
        var ids: [UInt32] = []
        for index in 0..<CFArrayGetCount(result) {
            guard let pointer = CFArrayGetValueAtIndex(result, index) else {
                throw DesktopFailure(code: "window_association_invalid", message: "Native window association contains an empty identity")
            }
            let value = unsafeBitCast(pointer, to: CFTypeRef.self)
            guard CFGetTypeID(value) == CFNumberGetTypeID() else {
                throw DesktopFailure(code: "window_association_invalid", message: "Native window association is not a numeric identity")
            }
            let number = value as! NSNumber
            guard number.doubleValue.isFinite, number.doubleValue > 0,
                  number.doubleValue <= Double(UInt32.max), number.doubleValue.rounded() == number.doubleValue else {
                throw DesktopFailure(code: "window_association_invalid", message: "Native window association contains an invalid window identity")
            }
            let candidate = number.uint32Value
            if !ids.contains(candidate) { ids.append(candidate) }
        }
        return ids
    }

    func subscribe(_ ids: Set<UInt32>, changed: @escaping @MainActor () -> Void) throws -> UUID {
        try state.subscribe(ids, changed: changed)
    }

    func unsubscribe(_ id: UUID) { state.unsubscribe(id) }

    func validateIncarnation(_ id: UInt32, incarnation: UInt64) throws -> DesktopWindowStamp {
        guard let current = try snapshot()[id], !current.terminated, current.incarnation == incarnation else {
            throw DesktopFailure(code: "stale_window", message: "The retained native window incarnation ended")
        }
        return current
    }

    func snapshot() throws -> [UInt32: DesktopWindowStamp] { try state.snapshot() }

    func validate(_ id: UInt32, stamp: DesktopWindowStamp) throws {
        guard !stamp.terminated, try snapshot()[id] == stamp else {
            throw DesktopFailure(code: "stale_window", message: "Native window lifecycle changed after the observation")
        }
    }

    func close() throws {
        operations.lock(); defer { operations.unlock() }
        guard !closed else { return }
        closed = true
        state.close()
        DesktopWindowCallbackOwners.shared.remove(token)
        var errors: [Int32] = []
        for event in registered {
            let result = remove(connection, Self.callback, event, token)
            if result != .success { errors.append(result.rawValue) }
        }
        registered.removeAll()
        guard errors.isEmpty else {
            throw DesktopFailure(code: "window_observer_release_failed", message: "Native window callbacks were invalidated but unregister failed (\(errors))")
        }
    }
}

@MainActor
final class DesktopAXApplicationWindowObserver {
    private let application: NSRunningApplication
    private let root: AXUIElement
    private let check: () throws -> Void
    private let changed: (Result<Void, Error>) -> Void
    private var observer: AXObserver?
    private var registered: [String] = []
    private var destroyed: [AXUIElement] = []
    private var additional: [AXUIElement] = []
    private var closed = false
    private(set) var revision: UInt64 = 0

    init(application: NSRunningApplication, check: @escaping () throws -> Void,
         changed: @escaping (Result<Void, Error>) -> Void) throws {
        self.application = application; self.check = check; self.changed = changed
        root = AXUIElementCreateApplication(application.processIdentifier)
        try check()
        var created: AXObserver?
        let status = AXObserverCreate(application.processIdentifier, { _, element, notification, context in
            guard let context else { return }
            MainActor.assumeIsolated {
                Unmanaged<DesktopAXApplicationWindowObserver>.fromOpaque(context).takeUnretainedValue().receive(element, notification: notification as String)
            }
        }, &created)
        guard status == .success, let created else {
            throw DesktopFailure(code: "app_window_observer_unavailable", message: "Native application window observation could not be created (\(status.rawValue))")
        }
        observer = created
        for name in [kAXWindowCreatedNotification, kAXMenuOpenedNotification, kAXMenuClosedNotification,
                     kAXSheetCreatedNotification, kAXMovedNotification, kAXResizedNotification] {
            let status = AXObserverAddNotification(created, root, name as CFString, Unmanaged.passUnretained(self).toOpaque())
            guard status == .success else {
                close()
                throw DesktopFailure(code: "app_window_observer_unavailable", message: "Native application window notification was refused (\(status.rawValue))")
            }
            registered.append(name)
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(created), .commonModes)
    }

    func elements() throws -> [AXUIElement] { try requireCurrent(); return additional }

    func track(_ elements: [AXUIElement], destroyedElements: [AXUIElement]) throws {
        try requireCurrent()
        additional.removeAll { value in destroyedElements.contains { CFEqual($0, value) } }
        guard elements.count <= 256, let observer else {
            throw DesktopFailure(code: "app_window_observer_limit", message: "Native window destruction observation exceeds its bound")
        }
        for old in destroyed where !elements.contains(where: { CFEqual($0, old) }) {
            let status = AXObserverRemoveNotification(observer, old, kAXUIElementDestroyedNotification as CFString)
            guard status == .success || status == .notificationNotRegistered || status == .invalidUIElement else {
                throw DesktopFailure(code: "app_window_observer_failed", message: "Native window destruction observation could not be removed (\(status.rawValue))")
            }
        }
        let old = destroyed
        destroyed = old.filter { candidate in elements.contains { CFEqual($0, candidate) } }
        for element in elements where !old.contains(where: { CFEqual($0, element) }) {
            try requireCurrent()
            let status = AXObserverAddNotification(observer, element, kAXUIElementDestroyedNotification as CFString,
                Unmanaged.passUnretained(self).toOpaque())
            guard status == .success || status == .notificationAlreadyRegistered else {
                throw DesktopFailure(code: "app_window_observer_failed", message: "Native window destruction observation was refused (\(status.rawValue))")
            }
            destroyed.append(element)
        }
    }

    private func requireCurrent() throws {
        guard !closed, !application.isTerminated,
              NSRunningApplication(processIdentifier: application.processIdentifier)?.isEqual(application) == true else {
            throw DesktopFailure(code: "app_window_observer_closed", message: "Native application window observation was released")
        }
        try check()
    }

    private func receive(_ element: AXUIElement, notification: String) {
        do {
            try requireCurrent()
            if notification == kAXUIElementDestroyedNotification {
                guard destroyed.contains(where: { CFEqual($0, element) }) else { return }
            } else {
                var pid: pid_t = 0
                guard AXUIElementGetPid(element, &pid) == .success, pid == application.processIdentifier else {
                    throw DesktopFailure(code: "app_window_owner_changed", message: "Native window notification belongs to another application")
                }
            }
            if notification == kAXMenuClosedNotification || notification == kAXUIElementDestroyedNotification {
                additional.removeAll { CFEqual($0, element) }
                if notification == kAXUIElementDestroyedNotification { destroyed.removeAll { CFEqual($0, element) } }
            } else if [kAXWindowCreatedNotification, kAXMenuOpenedNotification, kAXSheetCreatedNotification].contains(notification),
                      !additional.contains(where: { CFEqual($0, element) }) {
                guard additional.count < 256 else {
                    throw DesktopFailure(code: "app_window_observer_limit", message: "Native application window notifications exceed their bound")
                }
                additional.append(element)
            }
            guard revision < 9_007_199_254_740_991 else {
                throw DesktopFailure(code: "app_window_observer_limit", message: "Native application window notification revision overflowed")
            }
            revision += 1
            changed(.success(()))
        } catch { close(); changed(.failure(error)) }
    }

    func close() {
        guard !closed else { return }; closed = true
        if let observer {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .commonModes)
            for name in registered { AXObserverRemoveNotification(observer, root, name as CFString) }
            for element in destroyed { AXObserverRemoveNotification(observer, element, kAXUIElementDestroyedNotification as CFString) }
        }
        observer = nil; registered.removeAll(); destroyed.removeAll(); additional.removeAll()
    }
}
