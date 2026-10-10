import AppKit
import CryptoKit
import ApplicationServices
import Darwin
import Foundation

struct DesktopAppFocusState: Equatable {
    private(set) var applicationBelievesItIsActive: Bool
    private(set) var applicationBelievesItHasFocus: Bool
    private(set) var applicationIsActive: Bool

    init(applicationIsActive: Bool, applicationHasKeyFocus: Bool) {
        self.applicationIsActive = applicationIsActive
        applicationBelievesItIsActive = applicationIsActive
        applicationBelievesItHasFocus = applicationHasKeyFocus
    }

    var hasSyntheticActivation: Bool {
        applicationBelievesItIsActive && !applicationIsActive
    }

    mutating func physicalActivationChanged(isActive: Bool) {
        let wasActive = applicationIsActive
        applicationIsActive = isActive
        if wasActive || isActive {
            applicationBelievesItIsActive = isActive
            applicationBelievesItHasFocus = isActive
        }
    }

    mutating func keyFocusChanged(hasFocus: Bool) {
        applicationBelievesItHasFocus = hasFocus
    }

    mutating func activationEventsDispatched() {
        applicationBelievesItIsActive = true
        applicationBelievesItHasFocus = true
    }

    mutating func deactivationEventsDispatched() {
        applicationBelievesItIsActive = false
        applicationBelievesItHasFocus = false
    }
}

@MainActor
enum DesktopAppFocusEvents {
    enum Change {
        case activateApplication
        case activateWindow(activationPoint: CGPoint?)
        case restoreKeyFocus
        case deactivateApplication
    }

    static func prepare(_ change: Change, app: DesktopAppRef, target: DesktopAppInputWindow?,
                        input: DesktopAppInput, check: () throws -> Void) throws -> [CGEvent] {
        try Task.checkCancellation()
        guard target == nil || target?.app == app else {
            throw DesktopFailure(code: "invalid_app_focus", message: "The native focus target belongs to a different application")
        }
        if let target { _ = try input.validate(target, check: check) }
        else { _ = try input.validate(app, check: check) }
        let events: [CGEvent]
        switch change {
        case .activateApplication:
            events = [try notification(type: 13, subtype: 1, window: 0, flags: [])]
        case .activateWindow(let activationPoint):
            guard let target else { throw DesktopFailure(code: "app_window_required", message: "Window activation requires the exact native window") }
            let activation = try notification(type: 13, subtype: 1, window: target.windowID,
                                              flags: NSEvent.ModifierFlags(rawValue: 0xc0000))
            if let point = activationPoint {
                guard point.x.isFinite, point.y.isFinite, target.bounds.contains(point) else {
                    throw DesktopFailure(code: "app_focus_geometry_invalid", message: "The accessibility activation point is outside the retained window")
                }
                let spi = try DesktopAppInputSPI()
                let down = try activationMouse(type: .leftMouseDown, number: 1, point: point,
                                               target: target, spi: spi)
                let up = try activationMouse(type: .leftMouseUp, number: 2, point: point,
                                             target: target, spi: spi)
                events = [activation, down, up]
            } else {
                events = [activation]
            }
        case .restoreKeyFocus:
            events = [try notification(type: 21, subtype: Int16(bitPattern: 0x8000), window: 0, flags: [])]
        case .deactivateApplication:
            events = [try notification(type: 13, subtype: 2, window: 0, flags: [])]
        }
        try Task.checkCancellation()
        if let target { _ = try input.validate(target, check: check) }
        else { _ = try input.validate(app, check: check) }
        return events
    }

    nonisolated static func notification(type: UInt, subtype: Int16, window: CGWindowID,
                                     flags: NSEvent.ModifierFlags) throws -> CGEvent {
        guard let eventType = NSEvent.EventType(rawValue: type),
              let event = NSEvent.otherEvent(with: eventType, location: .zero, modifierFlags: flags,
                                             timestamp: 0, windowNumber: Int(window), context: nil,
                                             subtype: subtype, data1: 0, data2: 0)?.cgEvent else {
            throw DesktopFailure(code: "app_focus_event_unavailable", message: "The native application focus event could not be constructed")
        }
        return event
    }

    private static func activationMouse(type: NSEvent.EventType, number: Int, point: CGPoint,
                                        target: DesktopAppInputWindow, spi: DesktopAppInputSPI) throws -> CGEvent {
        guard let event = NSEvent.mouseEvent(with: type, location: point, modifierFlags: [],
                                             timestamp: 0, windowNumber: Int(target.windowID), context: nil,
                                             eventNumber: number, clickCount: 1, pressure: 1)?.cgEvent else {
            throw DesktopFailure(code: "app_focus_event_unavailable", message: "The native window activation click could not be constructed")
        }
        event.flags = []
        event.location = point
        event.setIntegerValueField(.mouseEventButtonNumber, value: 0)
        event.setIntegerValueField(.mouseEventSubtype, value: 3)
        spi.locate(event, window: target.windowID, bounds: target.bounds, flipped: false)
        return event
    }
}

struct DesktopAppFocusEffect: Codable, Sendable {
    let mechanism: String
    let target_pid: Int32
    let event_type: UInt32?
    let subtype: UInt16?
    let dispatched_events: Int
    let native_status: Int32?
    let application_effect_verified: Bool
    var original_target_pid: Int32? = nil
    var focus_id: UInt32? = nil
}

private final class DesktopFocusBoundary: @unchecked Sendable {
    let application: NSRunningApplication
    let pid: pid_t
    private let bundleURL: URL?
    private let bundleID: String?
    private let checkCaller: @Sendable () throws -> Void
    private let interrupt: @Sendable (DesktopFailure) -> Void
    let record: @Sendable (DesktopAppFocusEffect) -> Void
    private let lock = NSLock()
    private var closed = false
    private var failure: DesktopFailure?
    private var interruptionRevision: UInt64 = 0

    init(application: NSRunningApplication,
         checkCaller: @escaping @Sendable () throws -> Void,
         interrupt: @escaping @Sendable (DesktopFailure) -> Void,
         record: @escaping @Sendable (DesktopAppFocusEffect) -> Void) {
        self.application = application
        pid = application.processIdentifier
        bundleURL = application.bundleURL?.resolvingSymlinksInPath().standardizedFileURL
        bundleID = application.bundleIdentifier
        self.checkCaller = checkCaller
        self.interrupt = interrupt
        self.record = record
    }

    func check() throws {
        lock.lock()
        let state = (closed, failure)
        lock.unlock()
        if let failure = state.1 { throw failure }
        guard !state.0 else {
            throw DesktopFailure(code: "app_focus_closed", message: "The application focus lease was released")
        }
        try checkCaller()
        try checkForCleanup()
    }

    func checkForCleanup() throws {
        lock.lock()
        let isClosed = closed
        lock.unlock()
        guard !isClosed else {
            throw DesktopFailure(code: "app_focus_closed", message: "The application focus lease was released before native assertion cleanup")
        }
        guard !application.isTerminated,
              let current = NSRunningApplication(processIdentifier: pid), current.isEqual(application),
              current.bundleIdentifier == bundleID,
              current.bundleURL?.resolvingSymlinksInPath().standardizedFileURL == bundleURL else {
            throw DesktopFailure(code: "app_instance_expired", message: "The retained application focus instance no longer exists")
        }
    }

    func revision() -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return interruptionRevision
    }

    func check(revision: UInt64) throws {
        try check()
        guard self.revision() == revision else {
            throw DesktopFailure(code: "application_interrupted", message: "Physical application input interrupted the owned operation")
        }
    }

    func userInterrupted(_ message: String) {
        lock.lock()
        guard !closed else { lock.unlock(); return }
        interruptionRevision &+= 1
        lock.unlock()
        interrupt(DesktopFailure(code: "application_interrupted", message: message))
    }

    func fail(_ error: Error) {
        let reason = (error as? DesktopFailure) ?? DesktopFailure(code: "app_focus_interrupted", message: String(describing: error))
        lock.lock()
        guard !closed, failure == nil else { lock.unlock(); return }
        failure = reason
        interruptionRevision &+= 1
        lock.unlock()
        interrupt(reason)
    }

    func close() {
        lock.lock()
        closed = true
        lock.unlock()
    }
}

private final class DesktopFocusSPI: @unchecked Sendable {
    typealias ProcessPID = @convention(c) (UnsafePointer<ProcessSerialNumber>, UnsafeMutablePointer<pid_t>) -> OSStatus
    typealias GetKeyFocus = @convention(c) (UnsafeMutablePointer<ProcessSerialNumber>, UnsafeMutablePointer<DarwinBoolean>) -> Int32
    typealias ReleaseKeyFocus = @convention(c) (UInt32) -> Int32
    typealias ActualPID = @convention(c) (AXUIElement, UnsafeMutablePointer<pid_t>) -> AXError
    typealias PIDTap = @convention(c) (pid_t, CGEventTapPlacement, CGEventTapOptions, CGEventMask, CGEventTapCallBack, UnsafeMutableRawPointer?) -> CFMachPort?
    private let graphics: UnsafeMutableRawPointer
    private let accessibility: UnsafeMutableRawPointer
    let processPID: ProcessPID
    let getKeyFocus: GetKeyFocus
    let releaseKeyFocus: ReleaseKeyFocus
    let actualPID: ActualPID
    let tapForPID: PIDTap

    init() throws {
        guard let graphics = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_NOW | RTLD_LOCAL) else {
            throw DesktopFailure(code: "app_focus_unavailable", message: "The native graphics focus library is unavailable")
        }
        guard let accessibility = dlopen("/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices", RTLD_NOW | RTLD_LOCAL) else {
            dlclose(graphics)
            throw DesktopFailure(code: "app_focus_unavailable", message: "The native accessibility focus library is unavailable")
        }
        guard let process = dlsym(accessibility, "GetProcessPID"),
              let get = dlsym(graphics, "CPSGetKeyFocusProcess"),
              let release = dlsym(graphics, "CPSReleaseKeyFocusWithID"),
              let actual = dlsym(accessibility, "_AXUIElementGetActualPid"),
              let tap = dlsym(graphics, "CGEventTapCreateForPid") else {
            dlclose(accessibility)
            dlclose(graphics)
            throw DesktopFailure(code: "app_focus_unavailable", message: "The exact native focus functions are unavailable")
        }
        self.graphics = graphics
        self.accessibility = accessibility
        processPID = unsafeBitCast(process, to: ProcessPID.self)
        getKeyFocus = unsafeBitCast(get, to: GetKeyFocus.self)
        releaseKeyFocus = unsafeBitCast(release, to: ReleaseKeyFocus.self)
        actualPID = unsafeBitCast(actual, to: ActualPID.self)
        tapForPID = unsafeBitCast(tap, to: PIDTap.self)
    }

    deinit {
        dlclose(accessibility)
        dlclose(graphics)
    }

    func keyFocusPID(auxiliaryPID: pid_t?) -> pid_t? {
        var serial = ProcessSerialNumber(highLongOfPSN: 0, lowLongOfPSN: 0)
        var flag = DarwinBoolean(false)
        guard getKeyFocus(&serial, &flag) == noErr else { return nil }
        var pid: pid_t = 0
        let status = processPID(&serial, &pid)
        if status == noErr { return pid }
        if status == -600, !flag.boolValue, let auxiliaryPID,
           NSRunningApplication(processIdentifier: auxiliaryPID) == nil { return auxiliaryPID }
        return nil
    }
}

private final class DesktopFocusTap: @unchecked Sendable {
    private final class Context: @unchecked Sendable {
        let body: (CGEvent) -> Bool
        let fault: (Error) -> Void
        var port: CFMachPort?
        init(body: @escaping (CGEvent) -> Bool, fault: @escaping (Error) -> Void) {
            self.body = body
            self.fault = fault
        }
    }

    private final class RunState: @unchecked Sendable {
        let condition = NSCondition()
        var loop: CFRunLoop?
        var stopping = false
        var finished = false
    }

    private let state = RunState()
    private let port: CFMachPort
    private let context: Context

    init(spi: DesktopFocusSPI, pid: pid_t?, place: CGEventTapPlacement,
         options: CGEventTapOptions, types: [UInt32],
         body: @escaping (CGEvent) -> Bool, fault: @escaping (Error) -> Void) throws {
        let context = Context(body: body, fault: fault)
        let opaque = Unmanaged.passUnretained(context).toOpaque()
        let mask = types.reduce(CGEventMask(0)) { $0 | (CGEventMask(1) << $1) }
        let callback: CGEventTapCallBack = { _, type, event, opaque in
            guard let opaque else { return Unmanaged.passUnretained(event) }
            let context = Unmanaged<Context>.fromOpaque(opaque).takeUnretainedValue()
            if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                context.fault(DesktopFailure(code: "app_focus_tap_disabled", message: "The native focus tap was disabled; the owned input operation is interrupted"))
                if type == .tapDisabledByTimeout, let port = context.port, CFMachPortIsValid(port) {
                    CGEvent.tapEnable(tap: port, enable: true)
                }
                return Unmanaged.passUnretained(event)
            }
            return context.body(event) ? Unmanaged.passUnretained(event) : nil
        }
        let created: CFMachPort?
        if let pid {
            created = spi.tapForPID(pid, place, options, mask, callback, opaque)
        } else {
            created = CGEvent.tapCreate(tap: .cgAnnotatedSessionEventTap, place: place,
                                       options: options, eventsOfInterest: mask, callback: callback, userInfo: opaque)
        }
        guard let port = created else {
            throw DesktopFailure(code: "app_focus_tap_unavailable", message: "The exact native application focus event tap could not be installed")
        }
        guard let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, port, 0) else {
            CFMachPortInvalidate(port)
            throw DesktopFailure(code: "app_focus_tap_unavailable", message: "The native focus event tap has no run loop source")
        }
        self.port = port
        self.context = context
        context.port = port
        let state = self.state
        let thread = Thread {
            withExtendedLifetime((context, spi)) {
                let loop = CFRunLoopGetCurrent()!
                CFRunLoopAddSource(loop, source, .commonModes)
                state.condition.lock()
                state.loop = loop
                let stopping = state.stopping
                if !stopping { CGEvent.tapEnable(tap: port, enable: true) }
                state.condition.broadcast()
                state.condition.unlock()
                if !stopping {
                    CFRunLoopRun()
                }
                CGEvent.tapEnable(tap: port, enable: false)
                CFMachPortInvalidate(port)
                CFRunLoopRemoveSource(loop, source, .commonModes)
                state.condition.lock()
                state.finished = true
                state.loop = nil
                state.condition.broadcast()
                state.condition.unlock()
            }
        }
        thread.name = "Octopus application focus event tap"
        thread.start()
        state.condition.lock()
        while state.loop == nil && !state.finished { state.condition.wait() }
        state.condition.unlock()
    }

    func stop() {
        state.condition.lock()
        state.stopping = true
        let loop = state.loop
        state.condition.unlock()
        CGEvent.tapEnable(tap: port, enable: false)
        CFMachPortInvalidate(port)
        if let loop {
            CFRunLoopPerformBlock(loop, CFRunLoopMode.commonModes.rawValue) { CFRunLoopStop(loop) }
            CFRunLoopWakeUp(loop)
        }
    }

    deinit { stop() }
}

private enum DesktopLogicalFocus: Equatable {
    case regularApp(pid_t)
    case viewService(host: pid_t, service: pid_t?, auxiliary: Bool)

    var host: pid_t {
        switch self {
        case .regularApp(let pid): return pid
        case .viewService(let host, _, _): return host
        }
    }

    var isAuxiliary: Bool {
        if case .viewService(_, _, let auxiliary) = self { return auxiliary }
        return false
    }
}

private enum DesktopFocusAX {
    static func element(_ element: AXUIElement, _ attribute: String) -> AXUIElement? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success,
              let value, CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return (value as! AXUIElement)
    }

    static func publicPID(_ element: AXUIElement) -> pid_t? {
        var pid: pid_t = 0
        return AXUIElementGetPid(element, &pid) == .success ? pid : nil
    }

    static func windowOwner(_ window: CGWindowID) -> pid_t? {
        guard window != 0 else { return nil }
        var identifier = UnsafeRawPointer(bitPattern: UInt(window))
        let array = withUnsafeMutablePointer(to: &identifier) { CFArrayCreate(kCFAllocatorDefault, $0, 1, nil) }
        guard let array,
              let descriptions = CGWindowListCreateDescriptionFromArray(array) as? [[String: Any]],
              let owner = descriptions.first?[kCGWindowOwnerPID as String] as? NSNumber else { return nil }
        return owner.int32Value
    }

    static func auxiliaryPID(check: () throws -> Void) throws -> pid_t? {
        try check()
        let size = proc_listpids(UInt32(PROC_UID_ONLY), getuid(), nil, 0)
        guard size > 0 else { return nil }
        var pids = [pid_t](repeating: 0, count: Int(size) / MemoryLayout<pid_t>.size)
        let copied = pids.withUnsafeMutableBytes { proc_listpids(UInt32(PROC_UID_ONLY), getuid(), $0.baseAddress, Int32($0.count)) }
        guard copied > 0 else { return nil }
        for pid in pids.prefix(min(pids.count, Int(copied) / MemoryLayout<pid_t>.size)) where pid > 0 {
            try check()
            var info = proc_bsdinfo()
            let infoSize = Int32(MemoryLayout<proc_bsdinfo>.size)
            guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, infoSize) == infoSize else { continue }
            let name = withUnsafeBytes(of: &info.pbi_name) { bytes in
                String(decoding: bytes.prefix(while: { $0 != 0 }), as: UTF8.self)
            }
            if name == "ViewBridgeAuxiliary" { return pid }
        }
        return nil
    }
}

private final class DesktopFocusResolver {
    let spi: DesktopFocusSPI
    let auxiliaryPID: pid_t?
    var lastSyntheticAction: (uptime: TimeInterval, pid: pid_t)?

    init(spi: DesktopFocusSPI, check: () throws -> Void) throws {
        self.spi = spi
        auxiliaryPID = try DesktopFocusAX.auxiliaryPID(check: check)
    }

    func initialFocus(check: () throws -> Void) throws -> DesktopLogicalFocus {
        try check()
        if let pid = spi.keyFocusPID(auxiliaryPID: auxiliaryPID) { return try classify(pid, check: check) }
        return .regularApp(NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0)
    }

    func classify(_ pid: pid_t, check: () throws -> Void) throws -> DesktopLogicalFocus {
        try check()
        if pid == auxiliaryPID {
            let app = AXUIElementCreateApplication(pid)
            let host = try auxiliaryHost(app, check: check)
            return .viewService(host: host, service: try actualService(app, check: check), auxiliary: true)
        }
        if NSRunningApplication(processIdentifier: pid)?.activationPolicy == .prohibited {
            return .viewService(host: try host(of: AXUIElementCreateApplication(pid), focused: nil,
                                               allowNameLookup: true, check: check), service: pid, auxiliary: false)
        }
        return .regularApp(pid)
    }

    private func auxiliaryHost(_ app: AXUIElement, check: () throws -> Void) throws -> pid_t {
        var focused = DesktopFocusAX.element(app, kAXFocusedUIElementAttribute)
        if focused == nil, let lastSyntheticAction,
           ProcessInfo.processInfo.systemUptime - lastSyntheticAction.uptime < 1 {
            for _ in 0..<12 {
                try check()
                usleep(15_000)
                try check()
                focused = DesktopFocusAX.element(app, kAXFocusedUIElementAttribute)
                if focused != nil { break }
            }
            try check()
            return lastSyntheticAction.pid
        }
        return try host(of: app, focused: focused, allowNameLookup: false, check: check)
    }

    private func actualService(_ app: AXUIElement, check: () throws -> Void) throws -> pid_t? {
        try check()
        guard let focused = DesktopFocusAX.element(app, kAXFocusedUIElementAttribute) else { return nil }
        var pid: pid_t = 0
        guard spi.actualPID(focused, &pid) == .success else { return nil }
        if let running = NSRunningApplication(processIdentifier: pid), running.activationPolicy != .prohibited { return nil }
        return pid
    }

    private func regularPID(_ element: AXUIElement) -> pid_t? {
        guard let pid = DesktopFocusAX.publicPID(element),
              let app = NSRunningApplication(processIdentifier: pid), app.activationPolicy != .prohibited else { return nil }
        return pid
    }

    private func host(of app: AXUIElement, focused: AXUIElement?, allowNameLookup: Bool,
                      check: () throws -> Void) throws -> pid_t {
        try check()
        if let focused = focused ?? DesktopFocusAX.element(app, kAXFocusedUIElementAttribute) {
            if let pid = regularPID(focused) { return pid }
            if let pid = DesktopFocusAX.publicPID(focused) {
                let owner = AXUIElementCreateApplication(pid)
                if let pid = regularPID(owner) { return pid }
                if let window = DesktopFocusAX.element(owner, kAXFocusedWindowAttribute) ?? DesktopFocusAX.element(owner, kAXMainWindowAttribute),
                   let pid = regularPID(window) { return pid }
            }
        }
        try check()
        if allowNameLookup, let pid = DesktopFocusAX.publicPID(app),
           let name = NSRunningApplication(processIdentifier: pid)?.localizedName,
           let hostName = parenthesizedHostName(name) {
            let matches = NSWorkspace.shared.runningApplications.filter {
                $0.activationPolicy != .prohibited && $0.localizedName == hostName
            }
            if matches.count == 1 { return matches[0].processIdentifier }
        }
        return NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0
    }

    private func parenthesizedHostName(_ name: String) -> String? {
        guard name.contains("("), name.contains(")") else { return nil }
        var end = name.endIndex
        while end > name.startIndex, name[name.index(before: end)] == ")" { end = name.index(before: end) }
        guard end > name.startIndex else { return nil }
        var position = name.index(before: end)
        while position > name.startIndex {
            position = name.index(before: position)
            if name[position] == "(" { return String(name[name.index(after: position)..<end]) }
        }
        return nil
    }

    func containingProcess(of element: AXUIElement, check: () throws -> Void) throws -> (pid_t, Bool)? {
        var current: AXUIElement? = element
        for _ in 0..<300 {
            try check()
            guard let element = current else { return nil }
            var pid: pid_t = 0
            if spi.actualPID(element, &pid) == .success,
               let app = NSRunningApplication(processIdentifier: pid), app.isFinishedLaunching {
                return (pid, app.activationPolicy == .prohibited)
            }
            current = DesktopFocusAX.element(element, kAXParentAttribute)
        }
        return nil
    }
}

private final class DesktopSystemFocus: @unchecked Sendable {
    private struct Suppression {
        let thiefPID: pid_t
        let thiefAppPID: pid_t?
        let victimPID: pid_t
        var releasedTheft = false
        var suppressedReturnEvent = false
    }

    private final class Entry {
        let id = UUID()
        let boundary: DesktopFocusBoundary
        let focusChanged: @Sendable (Bool) -> Void
        var keyboard: DesktopFocusTap?
        var mouse: [DesktopFocusTap] = []
        var menuPID: pid_t?
        var menuApplication: NSRunningApplication?
        var suppressMenuDismissal = false

        init(boundary: DesktopFocusBoundary, focusChanged: @escaping @Sendable (Bool) -> Void) {
            self.boundary = boundary
            self.focusChanged = focusChanged
        }

        func stop() {
            keyboard?.stop()
            keyboard = nil
            mouse.forEach { $0.stop() }
            mouse.removeAll()
            suppressMenuDismissal = false
        }
    }

    @MainActor private static weak var shared: DesktopSystemFocus?
    @MainActor static func acquire(boundary: DesktopFocusBoundary) throws -> DesktopSystemFocus {
        if let shared, shared.hasRegistrations { return shared }
        let created = try DesktopSystemFocus(boundary: boundary)
        shared = created
        return created
    }

    private let lock = NSRecursiveLock()
    let spi: DesktopFocusSPI
    private let resolver: DesktopFocusResolver
    private var currentFocus: DesktopLogicalFocus
    private var previousFocus: DesktopLogicalFocus?
    private var suppression: Suppression?
    private var lastViewBridgeFocusStealWasSuppressed = false
    private weak var lastViewBridgeSuppressionBoundary: DesktopFocusBoundary?
    private var entries: [Entry] = []
    private var systemTap: DesktopFocusTap?
    private var auxiliaryKeyboardTap: DesktopFocusTap?

    private var hasRegistrations: Bool {
        lock.lock()
        defer { lock.unlock() }
        return !entries.isEmpty
    }

    private init(boundary: DesktopFocusBoundary) throws {
        try boundary.check()
        spi = try DesktopFocusSPI()
        resolver = try DesktopFocusResolver(spi: spi, check: boundary.check)
        currentFocus = try resolver.initialFocus(check: boundary.check)
    }

    func register(boundary: DesktopFocusBoundary, focusChanged: @escaping @Sendable (Bool) -> Void) throws -> UUID {
        try boundary.check()
        let entry = Entry(boundary: boundary, focusChanged: focusChanged)
        lock.lock()
        entries.append(entry)
        lock.unlock()
        do {
            let keyboard = try keyboardTap(pid: boundary.pid)
            lock.lock()
            entry.keyboard = keyboard
            let needsSystemTap = systemTap == nil
            lock.unlock()
            if needsSystemTap {
                let tap = try DesktopFocusTap(spi: spi, pid: nil, place: .tailAppendEventTap,
                                             options: .defaultTap, types: [21], body: { [weak self] event in
                    self?.systemEvent(event) ?? true
                }, fault: { [weak self] error in self?.failAll(error) })
                lock.lock()
                systemTap = tap
                lock.unlock()
                if let pid = resolver.auxiliaryPID {
                    let keyboard = try keyboardTap(pid: pid)
                    lock.lock()
                    auxiliaryKeyboardTap = keyboard
                    lock.unlock()
                }
            }
            try boundary.check()
            return entry.id
        } catch {
            remove(entry.id)
            throw error
        }
    }

    private func keyboardTap(pid: pid_t) throws -> DesktopFocusTap {
        try DesktopFocusTap(spi: spi, pid: pid, place: .tailAppendEventTap,
                            options: .defaultTap, types: [10, 11, 12], body: { [weak self] event in
            self?.keyboardEvent(event) ?? true
        }, fault: { [weak self] error in self?.failAll(error) })
    }

    func remove(_ id: UUID) {
        lock.lock()
        let removed = entries.first { $0.id == id }
        entries.removeAll { $0.id == id }
        if let suppression, !entries.contains(where: { $0.boundary.pid == suppression.thiefAppPID }) {
            self.suppression = nil
        }
        let taps: [DesktopFocusTap]
        if entries.isEmpty {
            taps = [systemTap, auxiliaryKeyboardTap].compactMap { $0 }
            systemTap = nil
            auxiliaryKeyboardTap = nil
        } else { taps = [] }
        lock.unlock()
        removed?.stop()
        taps.forEach { $0.stop() }
    }

    func isFocused(_ pid: pid_t) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return currentFocus.host == pid
    }

    func synthesizedActionWasPerformed(_ boundary: DesktopFocusBoundary) throws {
        try boundary.check()
        lock.lock()
        resolver.lastSyntheticAction = (ProcessInfo.processInfo.systemUptime, boundary.pid)
        lock.unlock()
    }

    private func liveEntry(for pid: pid_t) -> Entry? {
        for entry in entries where entry.boundary.pid == pid {
            do { try entry.boundary.check(); return entry }
            catch { entry.boundary.fail(error) }
        }
        return nil
    }

    private func failAll(_ error: Error) {
        lock.lock()
        let boundaries = entries.map(\.boundary)
        lock.unlock()
        boundaries.forEach { $0.fail(error) }
    }

    private static func field(_ event: CGEvent, _ number: UInt32) -> Int64 {
        event.getIntegerValueField(CGEventField(rawValue: number)!)
    }

    private func keyboardEvent(_ event: CGEvent) -> Bool {
        if Self.field(event, 41) == getpid() { return true }
        lock.lock()
        defer { lock.unlock() }
        guard let suppression, Self.field(event, 40) == suppression.thiefPID,
              let thief = suppression.thiefAppPID, let entry = liveEntry(for: thief), suppression.victimPID > 0 else { return true }
        do {
            try entry.boundary.check()
            entry.boundary.userInterrupted("Physical keyboard input interrupted the owned application operation")
            event.postToPid(suppression.victimPID)
            entry.boundary.record(DesktopAppFocusEffect(mechanism: "physical_key_relay", target_pid: suppression.victimPID,
                                                       event_type: event.type.rawValue, subtype: nil, dispatched_events: 1,
                                                       native_status: nil, application_effect_verified: false,
                                                       original_target_pid: suppression.thiefPID))
            return false
        } catch {
            entry.boundary.fail(error)
            return true
        }
    }

    private func systemEvent(_ event: CGEvent) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let boundary = entries.lazy.compactMap({ entry -> DesktopFocusBoundary? in
            do { try entry.boundary.check(); return entry.boundary }
            catch { entry.boundary.fail(error); return nil }
        }).first else { return true }
        do {
            let target = pid_t(truncatingIfNeeded: Self.field(event, 40))
            let subtype = UInt16(truncatingIfNeeded: Self.field(event, 64))
            switch subtype {
            case 0x4000:
                let thief = pid_t(truncatingIfNeeded: Self.field(event, 73))
                let candidate = try resolver.classify(thief, check: boundary.check)
                if candidate.host != currentFocus.host, let entry = liveEntry(for: candidate.host) {
                    try entry.boundary.check()
                    suppression = Suppression(thiefPID: thief, thiefAppPID: candidate.host, victimPID: target)
                    entry.boundary.record(DesktopAppFocusEffect(mechanism: "key_focus_taken_suppressed", target_pid: target,
                                                               event_type: 21, subtype: subtype, dispatched_events: 0,
                                                               native_status: nil, application_effect_verified: false))
                    return false
                }
                suppression = nil
                return true
            case 0x8000:
                if var pending = suppression, !pending.suppressedReturnEvent, target == pending.victimPID,
                   let thief = pending.thiefAppPID, let entry = liveEntry(for: thief) {
                    pending.suppressedReturnEvent = true
                    suppression = pending
                    entry.boundary.record(DesktopAppFocusEffect(mechanism: "key_focus_return_suppressed", target_pid: target,
                                                               event_type: 21, subtype: subtype, dispatched_events: 0,
                                                               native_status: nil, application_effect_verified: false))
                    return false
                }
                return true
            default:
                guard target == resolver.auxiliaryPID else { return true }
                if subtype == 0xf102 {
                    let pid = pid_t(truncatingIfNeeded: Self.field(event, 73))
                    let focus = try resolver.classify(pid, check: boundary.check)
                    try focusChanged(to: focus, rawPID: pid, event: event, boundary: boundary)
                    return true
                }
                if subtype == 2 {
                    if pid_t(truncatingIfNeeded: Self.field(event, 73)) == suppression?.thiefPID { suppression = nil }
                    guard lastViewBridgeFocusStealWasSuppressed, let owner = lastViewBridgeSuppressionBoundary else { return true }
                    try owner.check()
                    owner.record(DesktopAppFocusEffect(mechanism: "viewbridge_new_front_suppressed", target_pid: target,
                                                       event_type: 21, subtype: 2, dispatched_events: 0,
                                                       native_status: nil, application_effect_verified: false))
                    return false
                }
                return true
            }
        } catch {
            boundary.fail(error)
            return true
        }
    }

    private func focusChanged(to focus: DesktopLogicalFocus, rawPID: pid_t, event: CGEvent,
                              boundary: DesktopFocusBoundary) throws {
        let old = currentFocus
        previousFocus = old
        currentFocus = focus
        for entry in entries where entry.boundary.pid != suppression?.thiefAppPID {
            if entry.boundary.pid == focus.host, entry.boundary.pid != old.host { entry.focusChanged(true) }
            if entry.boundary.pid == old.host, entry.boundary.pid != focus.host { entry.focusChanged(false) }
        }
        var firstRelease = false
        var inTheft = false
        if var pending = suppression {
            if rawPID == pending.thiefPID {
                firstRelease = !pending.releasedTheft
                pending.releasedTheft = true
                inTheft = true
                suppression = pending
            } else if rawPID != pending.victimPID {
                suppression = nil
            }
        }
        var releaseForAuxiliary = false
        if old.isAuxiliary, focus.isAuxiliary, !inTheft, old.host != focus.host,
           let entry = liveEntry(for: focus.host),
           let focused = DesktopFocusAX.element(AXUIElementCreateApplication(old.host), kAXFocusedUIElementAttribute),
           let containing = try resolver.containingProcess(of: focused, check: entry.boundary.check),
           liveEntry(for: old.host) == nil {
            if containing.1 {
                restoreAuxiliaryHost(old.host, boundary: entry.boundary)
            } else {
                releaseForAuxiliary = true
            }
        }
        var released = false
        var releaseBoundary: DesktopFocusBoundary?
        if firstRelease || releaseForAuxiliary {
            let thiefOwner = suppression.flatMap(\.thiefAppPID).flatMap { liveEntry(for: $0)?.boundary }
            let owner = thiefOwner ?? liveEntry(for: focus.host)?.boundary ?? boundary
            try owner.check()
            let focusID = UInt32(truncatingIfNeeded: Self.field(event, 71))
            let status = spi.releaseKeyFocus(focusID)
            owner.record(DesktopAppFocusEffect(mechanism: "CPSReleaseKeyFocusWithID", target_pid: rawPID,
                                              event_type: 21, subtype: 0xf102, dispatched_events: 0,
                                              native_status: status, application_effect_verified: false, focus_id: focusID))
            released = status == noErr
            releaseBoundary = owner
            if !released {
                throw DesktopFailure(code: "app_focus_release_failed", message: "The native stolen focus release failed (\(status))")
            }
        }
        if focus.isAuxiliary {
            lastViewBridgeFocusStealWasSuppressed = released
            lastViewBridgeSuppressionBoundary = released ? releaseBoundary : nil
        }
    }

    private func restoreAuxiliaryHost(_ pid: pid_t, boundary: DesktopFocusBoundary) {
        guard pid > 0, let host = NSRunningApplication(processIdentifier: pid), !host.isTerminated else {
            boundary.fail(DesktopFailure(code: "app_focus_restore_unavailable", message: "The observed prior focus host no longer exists"))
            return
        }
        let revision = boundary.revision()
        DispatchQueue.global(qos: .userInitiated).async {
            var dispatched = 0
            do {
                for subtype in [UInt16(0x4000), UInt16(0x8000)] {
                    let event = try DesktopAppFocusEvents.notification(type: 21, subtype: Int16(bitPattern: subtype), window: 0, flags: [])
                    try boundary.check(revision: revision)
                    guard !host.isTerminated, NSRunningApplication(processIdentifier: pid)?.isEqual(host) == true else {
                        throw DesktopFailure(code: "app_focus_restore_unavailable", message: "The observed prior focus host changed during native restoration")
                    }
                    event.timestamp = DispatchTime.now().uptimeNanoseconds
                    event.postToPid(pid)
                    dispatched += 1
                    boundary.record(DesktopAppFocusEffect(mechanism: "viewbridge_focus_restore", target_pid: pid,
                                                         event_type: 21, subtype: subtype, dispatched_events: 1,
                                                         native_status: nil, application_effect_verified: false))
                }
            } catch {
                boundary.fail(DesktopFailure(code: "app_focus_restore_interrupted", message: "Native ViewBridge restoration stopped after \(dispatched) events: \(error)"))
            }
        }
    }

    func startMenuDismissalSuppression(_ id: UUID, menuApplication: NSRunningApplication?) throws {
        lock.lock()
        guard let entry = entries.first(where: { $0.id == id }) else {
            lock.unlock()
            throw DesktopFailure(code: "app_focus_closed", message: "The application focus registration was released")
        }
        let alreadyStarted = entry.suppressMenuDismissal
        lock.unlock()
        guard !alreadyStarted else { return }
        try entry.boundary.check()
        let menuPID = menuApplication?.processIdentifier
        let boundary = entry.boundary
        var taps: [DesktopFocusTap] = []
        do {
            for pid in [entry.boundary.pid, menuPID].compactMap({ $0 }) {
                taps.append(try DesktopFocusTap(spi: spi, pid: pid, place: .headInsertEventTap,
                                               options: .defaultTap, types: [1, 2, 6, 3, 4, 7, 25, 26, 27], body: { [weak self] event in
                    self?.menuEvent(event, id: id) ?? true
                }, fault: { error in boundary.fail(error) }))
            }
            try entry.boundary.check()
            lock.lock()
            entry.mouse = taps
            entry.menuPID = menuPID
            entry.menuApplication = menuApplication
            entry.suppressMenuDismissal = true
            lock.unlock()
        } catch {
            taps.forEach { $0.stop() }
            throw error
        }
    }

    func stopMenuDismissalSuppression(_ id: UUID) {
        lock.lock()
        let entry = entries.first { $0.id == id }
        let taps = entry?.mouse ?? []
        entry?.mouse = []
        entry?.suppressMenuDismissal = false
        entry?.menuPID = nil
        entry?.menuApplication = nil
        lock.unlock()
        taps.forEach { $0.stop() }
    }

    private func menuEvent(_ event: CGEvent, id: UUID) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard let entry = entries.first(where: { $0.id == id }), entry.suppressMenuDismissal else { return true }
        do {
            try entry.boundary.check()
            if let menu = entry.menuApplication {
                guard !menu.isTerminated, NSRunningApplication(processIdentifier: menu.processIdentifier)?.isEqual(menu) == true else {
                    throw DesktopFailure(code: "app_instance_expired", message: "The retained native menu application changed")
                }
            }
            guard let owner = DesktopFocusAX.windowOwner(CGWindowID(truncatingIfNeeded: Self.field(event, 51))) else { return true }
            if Self.field(event, 41) != getpid() {
                entry.boundary.userInterrupted("Physical mouse input interrupted the owned application menu operation")
            }
            let pass = owner == entry.boundary.pid || owner == entry.menuPID
            if !pass {
                entry.boundary.record(DesktopAppFocusEffect(mechanism: "menu_dismissal_suppressed", target_pid: entry.boundary.pid,
                                                           event_type: event.type.rawValue, subtype: nil, dispatched_events: 0,
                                                           native_status: nil, application_effect_verified: false))
            }
            return pass
        } catch {
            entry.boundary.fail(error)
            return true
        }
    }

    deinit {
        entries.forEach { $0.stop() }
        systemTap?.stop()
        auxiliaryKeyboardTap?.stop()
    }
}

@MainActor
final class DesktopAppFocusEnforcer {
    enum Activation {
        case accessibility
        case application
        case window(activationPoint: CGPoint?)
    }

    let app: DesktopAppRef
    private let applications: DesktopApplications
    private let input: DesktopAppInput
    private let boundary: DesktopFocusBoundary
    private let system: DesktopSystemFocus
    private var registration: UUID?
    private var activationObserver: NSObjectProtocol?
    private var clickTap: DesktopFocusTap?
    private(set) var state: DesktopAppFocusState
    private var closed = false
    private var sending = false

    init(app: DesktopAppRef, applications: DesktopApplications, input: DesktopAppInput,
         checkCaller: @escaping @Sendable () throws -> Void,
         interrupt: @escaping @Sendable (DesktopFailure) -> Void,
         record: @escaping @Sendable (DesktopAppFocusEffect) -> Void) throws {
        try Task.checkCancellation()
        let running = try applications.application(app)
        guard AXIsProcessTrusted() else {
            throw DesktopFailure(code: "accessibility_permission_required", message: "Accessibility permission is required for native application focus isolation")
        }
        let boundary = DesktopFocusBoundary(application: running, checkCaller: checkCaller,
                                             interrupt: interrupt, record: record)
        try boundary.check()
        self.app = app
        self.applications = applications
        self.input = input
        self.boundary = boundary
        system = try DesktopSystemFocus.acquire(boundary: boundary)
        state = DesktopAppFocusState(applicationIsActive: running.isActive,
                                     applicationHasKeyFocus: system.isFocused(running.processIdentifier))
        do {
            registration = try system.register(boundary: boundary) { [weak self] focused in
                Task { @MainActor [weak self] in
                    guard let self, !self.closed else { return }
                    self.state.keyFocusChanged(hasFocus: focused)
                }
            }
            activationObserver = NSWorkspace.shared.notificationCenter.addObserver(
                forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: nil
            ) { [weak self, boundary] notification in
                guard let activated = notification.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication else { return }
                boundary.userInterrupted("Physical application activation interrupted the owned operation")
                let active = activated.isEqual(boundary.application)
                Task { @MainActor [weak self] in
                    guard let self, !self.closed else { return }
                    self.state.physicalActivationChanged(isActive: active)
                    do { try self.updateClickTap() }
                    catch { boundary.fail(error) }
                }
            }
            try boundary.check()
            try updateClickTap()
        } catch {
            if let activationObserver { NSWorkspace.shared.notificationCenter.removeObserver(activationObserver) }
            if let registration { system.remove(registration) }
            boundary.close()
            throw error
        }
    }

    func check() throws {
        guard !closed else {
            throw DesktopFailure(code: "app_focus_closed", message: "The application focus enforcer was released")
        }
        try Task.checkCancellation()
        try boundary.check()
        _ = try applications.application(app)
    }

    func enforce(target: DesktopAppInputWindow?, activation: Activation = .accessibility,
                 check: @escaping () throws -> Void) async throws -> DesktopAppInputDelivery {
        try self.check()
        guard (target == nil || target?.app == app), !sending else {
            throw DesktopFailure(code: "invalid_app_focus", message: "The application focus target differs from the retained instance or dispatch is busy")
        }
        let revision = boundary.revision()
        let checked = {
            try check()
            try self.check()
            try self.boundary.check(revision: revision)
        }
        try validateTarget(target, check: checked)
        if state.applicationBelievesItHasFocus {
            return DesktopAppInputDelivery(mechanism: "app_focus_unchanged", dispatched_events: 0, application_effect_verified: false)
        }
        let change: DesktopAppFocusEvents.Change
        if state.applicationBelievesItIsActive {
            change = .restoreKeyFocus
        } else {
            switch activation {
            case .application: change = .activateApplication
            case .window(let point): change = .activateWindow(activationPoint: point)
            case .accessibility: change = try target.map { try accessibilityActivation($0.element) } ?? .activateApplication
            }
        }
        let events = try DesktopAppFocusEvents.prepare(change, app: app, target: target, input: input, check: checked)
        sending = true
        defer { sending = false }
        let result = try await dispatch(events, target: target, check: checked)
        state.activationEventsDispatched()
        do {
            try updateClickTap()
            try checked()
        } catch {
            boundary.fail(error)
            throw DesktopAppInputInterrupted(cause: error, dispatched_events: result.dispatched_events)
        }
        return result
    }

    func prepareAccessibilityClick(target: DesktopAppInputWindow?, check: @escaping () throws -> Void) async throws {
        let revision = boundary.revision()
        let checked = {
            try check()
            try self.check()
            try self.boundary.check(revision: revision)
            try self.validateTarget(target, check: check)
            guard AXIsProcessTrusted() else {
                throw DesktopFailure(code: "accessibility_permission_required", message: "Accessibility permission was withdrawn during native click preparation")
            }
        }
        try checked()
        guard !state.applicationBelievesItHasFocus else { return }
        let root = AXUIElementCreateApplication(boundary.pid)
        func read(_ element: AXUIElement, _ attribute: CFString) throws -> (AXError, CFTypeRef?) {
            try checked()
            let timeout = AXUIElementSetMessagingTimeout(element, 1)
            guard timeout == .success else {
                throw DesktopFailure(code: "ax_timeout_unavailable", message: "Native click preparation could not bound accessibility messaging (\(timeout.rawValue))")
            }
            var value: CFTypeRef?
            let status = AXUIElementCopyAttributeValue(element, attribute, &value)
            try checked()
            return (status, status == .success ? value : nil)
        }
        let (_, catalystValue) = try read(root, "_AXExhibitsMacCatalystBehavior" as CFString)
        let catalyst = catalystValue.map { CFGetTypeID($0) == CFBooleanGetTypeID() && ($0 as? Bool) == true } == true
        guard catalyst else { return }
        _ = try await enforce(target: target, check: checked)
        let started = Date()
        var lastStatus = AXError.noValue
        var frontmostObserved = false
        while Date().timeIntervalSince(started) <= 2.0 {
            let (status, value) = try read(root, kAXFrontmostAttribute as CFString)
            lastStatus = status
            if let value, CFGetTypeID(value) == CFBooleanGetTypeID(), (value as? Bool) == true {
                frontmostObserved = true
                break
            }
            try await ContinuousClock().sleep(for: .milliseconds(100))
            try checked()
        }
        try checked()
        boundary.record(DesktopAppFocusEffect(mechanism: frontmostObserved ? "app_focus_AXFrontmost_observed" : "app_focus_AXFrontmost_timeout",
            target_pid: boundary.pid, event_type: nil, subtype: nil, dispatched_events: 0,
            native_status: lastStatus.rawValue, application_effect_verified: false))
    }

    func deactivate(target: DesktopAppInputWindow?, check: @escaping () throws -> Void) async throws -> DesktopAppInputDelivery {
        try self.check()
        guard (target == nil || target?.app == app), !sending else {
            throw DesktopFailure(code: "invalid_app_focus", message: "The application deactivation target differs from the retained instance or dispatch is busy")
        }
        let revision = boundary.revision()
        let checked = {
            try check()
            try self.check()
            try self.boundary.check(revision: revision)
        }
        try validateTarget(target, check: checked)
        guard state.hasSyntheticActivation else {
            return DesktopAppInputDelivery(mechanism: "app_focus_unchanged", dispatched_events: 0, application_effect_verified: false)
        }
        let events = try DesktopAppFocusEvents.prepare(.deactivateApplication, app: app, target: target, input: input, check: checked)
        sending = true
        defer { sending = false }
        let result = try await dispatch(events, target: target, check: checked)
        state.deactivationEventsDispatched()
        try updateClickTap()
        return result
    }

    func synthesizedActionWasPerformed() throws {
        try check()
        try system.synthesizedActionWasPerformed(boundary)
    }

    func enableAccessibility(target: DesktopAppInputWindow?, options: DesktopAXEnablementOptions,
                             check: @escaping () throws -> Void) async throws -> DesktopAXEnablementAssertion? {
        try self.check()
        guard target == nil || target?.app == app else {
            throw DesktopFailure(code: "invalid_app_focus", message: "The accessibility enablement target differs from the retained application")
        }
        let revision = boundary.revision()
        let checked = {
            try check()
            try self.check()
            try self.boundary.check(revision: revision)
            try self.validateTarget(target, check: check)
        }
        try checked()
        return try await DesktopAXEnablementAssertion.acquire(app: app, boundary: boundary, target: target,
                                                              options: options, makeFocus: { [self] in
            let outer = boundary
            return try DesktopAppFocusEnforcer(app: app, applications: applications, input: input,
                                               checkCaller: { try outer.check() }, interrupt: { reason in
                if reason.code == "application_interrupted" { outer.userInterrupted(reason.message) }
                else { outer.fail(reason) }
            }, record: outer.record)
        }, check: checked)
    }

    func startMenuDismissalSuppression(menuApplication: NSRunningApplication? = nil) throws {
        try check()
        guard let registration else {
            throw DesktopFailure(code: "app_focus_closed", message: "The application focus registration was released")
        }
        if let menuApplication {
            guard !menuApplication.isTerminated,
                  NSRunningApplication(processIdentifier: menuApplication.processIdentifier)?.isEqual(menuApplication) == true else {
                throw DesktopFailure(code: "app_instance_expired", message: "The observed native menu application changed")
            }
        }
        try system.startMenuDismissalSuppression(registration, menuApplication: menuApplication)
    }

    func stopMenuDismissalSuppression() {
        if let registration { system.stopMenuDismissalSuppression(registration) }
    }

    func close() {
        guard !closed else { return }
        closed = true
        boundary.close()
        clickTap?.stop()
        clickTap = nil
        if let activationObserver { NSWorkspace.shared.notificationCenter.removeObserver(activationObserver) }
        activationObserver = nil
        if let registration { system.remove(registration) }
        registration = nil
        DesktopAXEnablementAssertion.forgetApplication(app)
    }

    private func validateTarget(_ target: DesktopAppInputWindow?, check: () throws -> Void) throws {
        if let target { _ = try input.validate(target, check: check) }
        else { _ = try input.validate(app, check: check) }
    }

    private func dispatch(_ events: [CGEvent], target: DesktopAppInputWindow?,
                          check: () throws -> Void) async throws -> DesktopAppInputDelivery {
        do {
            let result: DesktopAppInputDelivery
            if let target { result = try await input.send(events, to: target, interval: .zero, check: check) }
            else { result = try await input.send(events, to: app, interval: .zero, check: check) }
            boundary.record(DesktopAppFocusEffect(mechanism: "app_focus_CGEventPostToPid", target_pid: boundary.pid,
                                                 event_type: nil, subtype: nil, dispatched_events: result.dispatched_events,
                                                 native_status: nil, application_effect_verified: false))
            return result
        } catch let error as DesktopAppInputInterrupted {
            boundary.record(DesktopAppFocusEffect(mechanism: "app_focus_CGEventPostToPid_interrupted", target_pid: boundary.pid,
                                                 event_type: nil, subtype: nil, dispatched_events: error.dispatched_events,
                                                 native_status: nil, application_effect_verified: false))
            boundary.fail(error.cause)
            throw error
        }
    }

    private func accessibilityActivation(_ window: AXUIElement) throws -> DesktopAppFocusEvents.Change {
        var value: CFTypeRef?
        let status = AXUIElementCopyAttributeValue(window, "AXActivationPoint" as CFString, &value)
        if status == .attributeUnsupported || status == .noValue { return .activateApplication }
        guard status == .success else {
            throw DesktopFailure(code: "app_focus_activation_point_unavailable", message: "The native activation point could not be read (\(status.rawValue))")
        }
        guard let value, !(value is NSNull) else { return .activateWindow(activationPoint: nil) }
        guard CFGetTypeID(value) == AXValueGetTypeID() else {
            throw DesktopFailure(code: "app_focus_activation_point_invalid", message: "The native activation point is not an accessibility point")
        }
        let encoded = value as! AXValue
        var point = CGPoint.zero
        guard AXValueGetType(encoded) == .cgPoint, AXValueGetValue(encoded, .cgPoint, &point) else {
            throw DesktopFailure(code: "app_focus_activation_point_invalid", message: "The native activation point could not be decoded")
        }
        return .activateWindow(activationPoint: point)
    }

    private func updateClickTap() throws {
        if !state.hasSyntheticActivation {
            clickTap?.stop()
            clickTap = nil
            return
        }
        guard clickTap == nil else { return }
        try boundary.check()
        let boundary = self.boundary
        clickTap = try DesktopFocusTap(spi: system.spi, pid: nil, place: .headInsertEventTap,
                                      options: .listenOnly, types: [1], body: { event in
            guard event.getIntegerValueField(.eventSourceUnixProcessID) != getpid(),
                  event.getIntegerValueField(.eventTargetUnixProcessID) != boundary.pid else { return true }
            do {
                try boundary.check()
                let window = CGWindowID(truncatingIfNeeded: event.getIntegerValueField(CGEventField(rawValue: 51)!))
                guard DesktopFocusAX.windowOwner(window) == boundary.pid else { return true }
                boundary.userInterrupted("A physical click selected the synthetically active application")
                try boundary.check()
                let accepted = boundary.application.activate(options: [])
                boundary.record(DesktopAppFocusEffect(mechanism: "physical_click_application_activation", target_pid: boundary.pid,
                                                     event_type: 1, subtype: nil, dispatched_events: 0,
                                                     native_status: accepted ? 0 : -1, application_effect_verified: false))
            } catch { boundary.fail(error) }
            return true
        }, fault: { error in boundary.fail(error) })
    }

    deinit {
        boundary.close()
        clickTap?.stop()
        if let activationObserver { NSWorkspace.shared.notificationCenter.removeObserver(activationObserver) }
        if let registration { system.remove(registration) }
    }
}

struct DesktopAXEnablementOptions: Codable, Sendable {
    enum AttributeMode: String, Codable, Sendable {
        case none
        case manualAccessibility
        case enhancedUserInterface
        case manualIfSupportedOtherwiseEnhanced
    }
    enum UniversalAccessMode: String, Codable, Sendable {
        case none
        case untilRelease
        case pulse100ms
    }
    enum Handshake: String, Codable, Sendable {
        case none
        case nativeWindow
    }
    let attribute: AttributeMode
    let enforceFocusBeforeEnhanced: Bool
    let universalAccess: UniversalAccessMode
    let handshake: Handshake
}

struct DesktopAXEnablementInterrupted: Error {
    let cause: Error
    let cleanupError: Error
    let cleanup: DesktopAXEnablementAssertion
}

@MainActor
final class DesktopAXEnablementAssertion {
    private enum Mode: Hashable {
        case manual, enhanced, universalAccess
        var attribute: String? {
            switch self {
            case .manual: return "AXManualAccessibility"
            case .enhanced: return "AXEnhancedUserInterface"
            case .universalAccess: return nil
            }
        }
    }
    private struct Key: Hashable {
        let app: DesktopAppRef
        let mode: Mode
    }
    private final class Ownership {
        let application: NSRunningApplication
        var leases: Set<UUID>
        var changing = true
        var restoreRequired = false
        var pulse: Task<Void, Never>?
        init(application: NSRunningApplication, token: UUID) {
            self.application = application
            leases = [token]
        }
    }
    private static var ownership: [Key: Ownership] = [:]
    private static var confirmedApplications: [DesktopAppRef: NSRunningApplication] = [:]
    private let token: UUID
    private let boundary: DesktopFocusBoundary
    private var keys: [Key]

    private init(token: UUID, boundary: DesktopFocusBoundary, keys: [Key]) {
        self.token = token
        self.boundary = boundary
        self.keys = keys
    }

    static func acquireRead(app: DesktopAppRef, application: NSRunningApplication, target: DesktopAppInputWindow?,
                            options: DesktopAXEnablementOptions, checkCaller: @escaping @Sendable () throws -> Void,
                            interrupt: @escaping @Sendable (DesktopFailure) -> Void,
                            record: @escaping @Sendable (DesktopAppFocusEffect) -> Void,
                            makeFocus: @MainActor () throws -> DesktopAppFocusEnforcer,
                            check: @escaping () throws -> Void) async throws -> DesktopAXEnablementAssertion? {
        let boundary = DesktopFocusBoundary(application: application, checkCaller: checkCaller, interrupt: interrupt, record: record)
        return try await acquire(app: app, boundary: boundary, target: target, options: options,
            makeFocus: makeFocus, check: { try check(); try boundary.check() })
    }

    fileprivate static func acquire(app: DesktopAppRef, boundary: DesktopFocusBoundary, target: DesktopAppInputWindow?,
                                    options: DesktopAXEnablementOptions,
                                    makeFocus: @MainActor () throws -> DesktopAppFocusEnforcer,
                                    check: @escaping () throws -> Void) async throws -> DesktopAXEnablementAssertion? {
        try check()
        confirmedApplications = confirmedApplications.filter { !$0.value.isTerminated }
        let element = AXUIElementCreateApplication(boundary.pid)
        try boundAX(element)
        let attribute: Mode?
        switch options.attribute {
        case .none: attribute = nil
        case .manualAccessibility: attribute = .manual
        case .enhancedUserInterface: attribute = .enhanced
        case .manualIfSupportedOtherwiseEnhanced:
            var names: CFArray?
            let status = AXUIElementCopyAttributeNames(element, &names)
            guard status == .success, let names = names as? [String] else {
                throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "The native accessibility attribute list is unavailable (\(status.rawValue))")
            }
            attribute = names.contains("AXManualAccessibility") ? .manual : .enhanced
        }
        let token = UUID()
        var acquired: [Key] = []
        var changedForHandshake = false
        do {
            let modes = [attribute, options.universalAccess == .none ? nil : Mode.universalAccess].compactMap { $0 }
            for mode in modes {
                try check()
                let key = Key(app: app, mode: mode)
                if let existing = ownership[key] {
                    guard !existing.changing, existing.application.isEqual(boundary.application) else {
                        throw DesktopFailure(code: "app_ax_enablement_busy", message: "The retained application's native accessibility assertion is changing")
                    }
                    existing.leases.insert(token)
                    acquired.append(key)
                    continue
                }
                if let attribute = mode.attribute {
                    guard try boolean(element, attribute: attribute) == false else { continue }
                } else {
                    guard let preferences = UserDefaults(suiteName: "com.apple.universalaccess") else {
                        throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "The native universal-access preference domain is unavailable")
                    }
                    if preferences.bool(forKey: "voiceOverOnOffKey") { continue }
                }
                let owned = Ownership(application: boundary.application, token: token)
                ownership[key] = owned
                acquired.append(key)
                if mode == .enhanced, options.enforceFocusBeforeEnhanced, !boundary.application.isActive {
                    let temporary = try makeFocus()
                    do {
                        _ = try await temporary.enforce(target: target, check: check)
                        let started = Date()
                        while Date().timeIntervalSince(started) <= 1, !boundary.application.isActive {
                            try check()
                            try await ContinuousClock().sleep(for: .milliseconds(50))
                        }
                        try check()
                        owned.restoreRequired = true
                        try setBoolean(element, attribute: "AXEnhancedUserInterface", value: true, boundary: boundary)
                        changedForHandshake = true
                        _ = try await temporary.deactivate(target: target, check: check)
                        temporary.close()
                    } catch {
                        temporary.close()
                        throw error
                    }
                } else if let attribute = mode.attribute {
                    try check()
                    owned.restoreRequired = true
                    try setBoolean(element, attribute: attribute, value: true, boundary: boundary)
                    changedForHandshake = mode == .manual
                } else {
                    try check()
                    owned.restoreRequired = true
                    try setUniversalAccess(true, notify: true, boundary: boundary)
                    if options.universalAccess == .pulse100ms {
                        owned.pulse = Task { @MainActor [weak owned] in
                            do {
                                try await ContinuousClock().sleep(for: .milliseconds(100))
                                guard let owned, !owned.leases.isEmpty else { return }
                                try boundary.checkForCleanup()
                                try setUniversalAccess(false, notify: false, boundary: boundary)
                                owned.restoreRequired = false
                            } catch is CancellationError {
                            } catch { boundary.fail(error) }
                        }
                    }
                }
                owned.changing = false
            }
            let nativeTree = try !changedForHandshake && options.handshake == .nativeWindow ? target.map { try hasNativeTreeAttribute($0.element) } ?? false : false
            let needsHandshake = options.handshake == .nativeWindow && (changedForHandshake || nativeTree)
            if let target, needsHandshake, attribute == .manual || confirmedApplications[app]?.isEqual(boundary.application) != true {
                try check()
                try await ContinuousClock().sleep(for: .milliseconds(100))
                let started = Date()
                var populated = false
                while Date().timeIntervalSince(started) <= 1 {
                    try check()
                    if try hasWindowDescendant(target.element, check: {
                        try check()
                        guard Date().timeIntervalSince(started) <= 1 else {
                            throw DesktopFailure(code: "app_ax_enablement_timeout", message: "Native accessibility preparation exceeded its one-second window bound")
                        }
                    }) { populated = true; break }
                    try await ContinuousClock().sleep(for: .milliseconds(50))
                }
                guard populated else {
                    throw DesktopFailure(code: "app_ax_enablement_timeout", message: "The native accessibility window descendant did not appear within the source timeout")
                }
                confirmedApplications[app] = boundary.application
            }
            try check()
            guard !acquired.isEmpty else { return nil }
            return DesktopAXEnablementAssertion(token: token, boundary: boundary, keys: acquired)
        } catch {
            let cause = error
            let cleanup = DesktopAXEnablementAssertion(token: token, boundary: boundary, keys: acquired)
            do { try cleanup.disable() }
            catch {
                boundary.fail(error)
                throw DesktopAXEnablementInterrupted(cause: cause, cleanupError: error, cleanup: cleanup)
            }
            throw cause
        }
    }

    func disable() throws {
        guard !keys.isEmpty else { return }
        try Self.release(keys, token: token, boundary: boundary)
        keys.removeAll()
    }

    private static func release(_ keys: [Key], token: UUID, boundary: DesktopFocusBoundary) throws {
        var firstFailure: Error?
        for key in keys {
            guard let owned = ownership[key] else { continue }
            owned.leases.remove(token)
            guard owned.leases.isEmpty else { continue }
            owned.changing = true
            owned.pulse?.cancel()
            owned.pulse = nil
            do {
                if owned.restoreRequired {
                    try boundary.checkForCleanup()
                    if let attribute = key.mode.attribute {
                        try setBoolean(AXUIElementCreateApplication(boundary.pid), attribute: attribute, value: false, boundary: boundary)
                    } else if NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.VoiceOver").isEmpty {
                        try setUniversalAccess(false, notify: true, boundary: boundary)
                    }
                }
                ownership.removeValue(forKey: key)
            } catch {
                if firstFailure == nil { firstFailure = error }
            }
        }
        if let firstFailure { throw firstFailure }
    }

    private static func boolean(_ element: AXUIElement, attribute: String) throws -> Bool? {
        try boundAX(element)
        var value: CFTypeRef?
        let status = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
        if status == .noValue || status == .attributeUnsupported { return nil }
        guard status == .success else {
            throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "The native accessibility assertion value could not be read (\(status.rawValue))")
        }
        guard let value else { return nil }
        guard let bool = value as? Bool else {
            throw DesktopFailure(code: "app_ax_enablement_invalid", message: "The native accessibility assertion value is not Boolean")
        }
        return bool
    }

    fileprivate static func forgetApplication(_ app: DesktopAppRef) {
        if !ownership.keys.contains(where: { $0.app == app }) { confirmedApplications.removeValue(forKey: app) }
    }

    private static func setBoolean(_ element: AXUIElement, attribute: String, value: Bool, boundary: DesktopFocusBoundary) throws {
        try boundAX(element)
        let encoded: CFBoolean = value ? kCFBooleanTrue : kCFBooleanFalse
        let status = AXUIElementSetAttributeValue(element, attribute as CFString, encoded)
        boundary.record(DesktopAppFocusEffect(mechanism: "\(attribute)_\(value ? "enable" : "disable")", target_pid: boundary.pid,
                                             event_type: nil, subtype: nil, dispatched_events: 0,
                                             native_status: status.rawValue, application_effect_verified: false))
        guard status == .success else {
            throw DesktopFailure(code: "app_ax_enablement_failed", message: "The native accessibility assertion write failed (\(status.rawValue))")
        }
    }

    private static func setUniversalAccess(_ value: Bool, notify: Bool, boundary: DesktopFocusBoundary) throws {
        guard let preferences = UserDefaults(suiteName: "com.apple.universalaccess") else {
            throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "The native universal-access preference domain is unavailable")
        }
        preferences.set(value, forKey: "voiceOverOnOffKey")
        if notify {
            DistributedNotificationCenter.default().postNotificationName(
                NSNotification.Name("com.apple.universalaccess.VoiceOverSettingsDidChange"), object: nil)
        }
        boundary.record(DesktopAppFocusEffect(mechanism: "universal_access_\(value ? "enable" : "disable")\(notify ? "_notify" : "")",
                                             target_pid: boundary.pid, event_type: nil, subtype: nil, dispatched_events: 0,
                                             native_status: nil, application_effect_verified: false))
    }

    private static func boundAX(_ element: AXUIElement) throws {
        let status = AXUIElementSetMessagingTimeout(element, 1)
        guard status == .success else { throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "Cannot bound native accessibility preparation (\(status.rawValue))") }
    }

    private static func children(_ element: AXUIElement, maximum: Int) throws -> [AXUIElement] {
        try boundAX(element)
        var count: CFIndex = 0
        let counted = AXUIElementGetAttributeValueCount(element, kAXChildrenAttribute as CFString, &count)
        if counted == .noValue || counted == .attributeUnsupported { return [] }
        guard counted == .success, count >= 0, count <= maximum else {
            throw DesktopFailure(code: "app_ax_enablement_limit", message: "Native accessibility preparation exceeded its bounded descendants")
        }
        if count == 0 { return [] }
        var value: CFArray?
        let status = AXUIElementCopyAttributeValues(element, kAXChildrenAttribute as CFString, 0, count, &value)
        guard status == .success, let value else {
            throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "The native accessibility descendants could not be read (\(status.rawValue))")
        }
        let values = value as [AnyObject]
        guard values.count <= maximum, values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else {
            throw DesktopFailure(code: "app_ax_enablement_invalid", message: "The native accessibility descendants are not a bounded element array")
        }
        return values.map { $0 as! AXUIElement }
    }

    private static func hasNativeTreeAttribute(_ root: AXUIElement) throws -> Bool {
        try boundAX(root)
        var values: CFArray?
        let status = AXUIElementCopyAttributeValues(root, kAXChildrenAttribute as CFString, 0, 1, &values)
        if status == .noValue || status == .attributeUnsupported { return false }
        guard status == .success, let values else {
            throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "The native accessibility window child could not be read (\(status.rawValue))")
        }
        if CFArrayGetCount(values) == 0 { return false }
        let first = unsafeBitCast(CFArrayGetValueAtIndex(values, 0), to: CFTypeRef.self)
        guard CFGetTypeID(first) == AXUIElementGetTypeID() else {
            throw DesktopFailure(code: "app_ax_enablement_invalid", message: "The native accessibility window child is invalid")
        }
        let element = first as! AXUIElement
        try boundAX(element)
        var names: CFArray?
        let listed = AXUIElementCopyAttributeNames(element, &names)
        guard listed == .success, let names = names as? [String] else {
            throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "The native accessibility child attribute list is unavailable")
        }
        return names.contains("ChromeAXNodeId")
    }

    private static func hasWindowDescendant(_ root: AXUIElement, check: () throws -> Void) throws -> Bool {
        var queue = try children(root, maximum: 4096)
        var index = 0
        while index < queue.count {
            try check()
            let element = queue[index]; index += 1
            try boundAX(element)
            var role: CFTypeRef?
            let status = AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &role)
            guard status == .success || status == .noValue || status == .attributeUnsupported else {
                throw DesktopFailure(code: "app_ax_enablement_unavailable", message: "Native accessibility preparation role lookup failed (\(status.rawValue))")
            }
            if status == .success, role as? String == kAXWindowRole { return true }
            queue.append(contentsOf: try children(element, maximum: 4096 - queue.count))
        }
        return false
    }

    deinit {
        let keys = keys
        let token = token
        let boundary = boundary
        if !keys.isEmpty {
            Task { @MainActor in
                do { try Self.release(keys, token: token, boundary: boundary) }
                catch { boundary.fail(error) }
            }
        }
    }
}


struct DesktopApplicationBehavior: Decodable, Sendable {
    enum MouseTargeting: String, Decodable, Sendable {
        case nativeHit = "native_hit"
        case suppliedWindow = "supplied_window"
    }
    struct MenuBarPresentation: Decodable, Sendable, Equatable {
        let omit_first_item: Bool
        let excluded_title_substrings: [String]
        let clear_immediate_actions: Bool
    }
    struct Profile: Decodable, Sendable {
        let full_ui: DesktopAXEnablementOptions
        let event_stream: DesktopAXEnablementOptions
        let informational: DesktopAXEnablementOptions
        let typing_interval_us: UInt32
        let focus_dom_field_by_click: Bool
        let mouse_targeting: MouseTargeting
        let suppress_menu_dismissal_on_window: Bool
        let menu_bar_presentation: MenuBarPresentation
        let accessibility_presentation: DesktopAXPresentationProfile
    }
    struct Selector: Decodable, Sendable {
        enum Match: String, Decodable, Sendable { case exact, prefix }
        let match: Match
        let bundle_id: String
    }
    struct Selected: Decodable, Sendable {
        let selector: Selector
        let behavior: Profile
    }
    let schema_version: UInt32
    let defaults: Profile
    let profiles: [Selected]

    static func read() throws -> Self {
        guard let value = ProcessInfo.processInfo.environment["OCTOPUS_DESKTOP_APPLICATION_BEHAVIOR"],
              !value.isEmpty, value.utf8.count <= 65536 else {
            throw DesktopFailure(code: "application_behavior_unavailable", message: "Native application behavior was not provided by the owning desktop host")
        }
        let config = try JSONDecoder().decode(Self.self, from: Data(value.utf8))
        guard config.schema_version == 1, config.profiles.count <= 64 else {
            throw DesktopFailure(code: "invalid_application_behavior", message: "Native application behavior has an unsupported schema or profile count")
        }
        var selectors = Set<String>()
        for selected in config.profiles {
            let selector = selected.selector
            guard !selector.bundle_id.isEmpty, selector.bundle_id.utf8.count <= 255,
                  selector.bundle_id.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45,46,95].contains($0) }),
                  selectors.insert(selector.match.rawValue + ":" + selector.bundle_id).inserted else {
                throw DesktopFailure(code: "invalid_application_behavior", message: "Native application behavior has an invalid or repeated selector")
            }
        }
        for profile in [config.defaults] + config.profiles.map(\.behavior) {
            let presentation = profile.accessibility_presentation
            guard presentation.context_type == "full_ui", presentation.excluded_action_descriptions.count <= 128,
                  presentation.excluded_action_descriptions.allSatisfy({ !$0.isEmpty && $0.utf8.count <= 2048 && !$0.contains("\0") }),
                  (presentation.capture_note?.utf8.count ?? 0) <= 32768, presentation.capture_note?.contains("\0") != true else {
                throw DesktopFailure(code: "invalid_application_behavior", message: "Native AX presentation has an unsupported context or unbounded profile data")
            }
            let menu = profile.menu_bar_presentation
            guard menu.excluded_title_substrings.count <= 16,
                  Set(menu.excluded_title_substrings).count == menu.excluded_title_substrings.count,
                  menu.excluded_title_substrings.allSatisfy({ !$0.isEmpty && !$0.contains("\0") && $0.utf8.count <= 2048 }) else {
                throw DesktopFailure(code: "invalid_application_behavior", message: "Native menu display filters are not bounded unique strings")
            }
            guard profile.typing_interval_us <= 1_000_000 else {
                throw DesktopFailure(code: "invalid_application_behavior", message: "Native typing interval is outside its finite event domain")
            }
            for options in [profile.full_ui, profile.event_stream, profile.informational] {
                guard !options.enforceFocusBeforeEnhanced || options.attribute == .enhancedUserInterface || options.attribute == .manualIfSupportedOtherwiseEnhanced else {
                    throw DesktopFailure(code: "invalid_application_behavior", message: "Native focus preparation requires enhanced accessibility")
                }
            }
        }
        return config
    }

    func resolve(_ bundleID: String) throws -> Profile {
        let matches = profiles.filter { selected in
            switch selected.selector.match {
            case .exact: return bundleID == selected.selector.bundle_id
            case .prefix: return bundleID.hasPrefix(selected.selector.bundle_id)
            }
        }
        guard matches.count <= 1 else {
            throw DesktopFailure(code: "ambiguous_application_behavior", message: "Native application behavior has overlapping selectors for the exact target")
        }
        return matches.first?.behavior ?? defaults
    }
}


struct DesktopAppInstructions: Encodable, Sendable {
    let bundle_id: String
    let bundle_path: String
    let content_sha256: String
    let text: String
}

struct DesktopApplicationInstructionCatalog: Decodable, Sendable {
    struct Text: Decodable, Sendable {
        let content_sha256: String
        let text: String

        func validate() throws {
            guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  text.utf8.count <= 32768, !text.contains("\0"),
                  Self.digest(text) == content_sha256 else {
                throw DesktopFailure(code: "invalid_application_instructions", message: "Native app guidance text or content digest is invalid")
            }
        }
        static func digest(_ text: String) -> String {
            SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
        }
    }
    let schema_version: UInt32
    let resources: [String: Text]
    let aliases: [String: [String]]
    let suppressed_bundle_ids: [String]
    let http_scheme: Text?

    static func read() throws -> Self {
        guard let raw = ProcessInfo.processInfo.environment["OCTOPUS_DESKTOP_APPLICATION_INSTRUCTIONS"],
              !raw.isEmpty, raw.utf8.count <= 65536 else {
            throw DesktopFailure(code: "application_instructions_unavailable", message: "Desktop host did not provide its declared app guidance resources")
        }
        let catalog = try JSONDecoder().decode(Self.self, from: Data(raw.utf8))
        guard catalog.schema_version == 1, catalog.resources.count <= 64, catalog.aliases.count <= 64,
              catalog.suppressed_bundle_ids.count <= 64 else {
            throw DesktopFailure(code: "invalid_application_instructions", message: "Native app guidance catalog exceeds its schema or bounds")
        }
        for (name, text) in catalog.resources {
            guard !name.isEmpty, name.utf8.count <= 255, name == name.trimmingCharacters(in: .whitespacesAndNewlines),
                  !name.contains("/"), !name.contains("\\"), !name.contains("\0"), name != ".", name != ".." else {
                throw DesktopFailure(code: "invalid_application_instructions", message: "Native app guidance has an invalid resource name")
            }
            try text.validate()
        }
        func bundle(_ value: String) -> Bool {
            !value.isEmpty && value.utf8.count <= 255 && value.utf8.allSatisfy { (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45,46,95].contains($0) }
        }
        for (id, names) in catalog.aliases {
            guard bundle(id), names.count <= 8, Set(names).count == names.count,
                  names.allSatisfy({ catalog.resources[$0] != nil }) else {
                throw DesktopFailure(code: "invalid_application_instructions", message: "Native app guidance alias does not reference its exact resources")
            }
        }
        guard catalog.suppressed_bundle_ids.allSatisfy(bundle), Set(catalog.suppressed_bundle_ids).count == catalog.suppressed_bundle_ids.count else {
            throw DesktopFailure(code: "invalid_application_instructions", message: "Native app guidance suppression has an invalid bundle identity")
        }
        try catalog.http_scheme?.validate()
        return catalog
    }

    @MainActor func resolve(_ info: DesktopAppInfo, application: NSRunningApplication, check: () throws -> Void) throws -> DesktopAppInstructions? {
        try check()
        guard application.processIdentifier == info.process_id, !application.isTerminated,
              application.bundleIdentifier == info.bundle_id,
              let url = application.bundleURL, url.resolvingSymlinksInPath().path == info.bundle_path,
              let bundle = Bundle(url: url), bundle.bundleIdentifier == info.bundle_id else {
            throw DesktopFailure(code: "stale_application", message: "Native app guidance no longer identifies the approved application")
        }
        if suppressed_bundle_ids.contains(info.bundle_id) { return nil }
        var candidates = aliases[info.bundle_id] ?? []
        if let name = bundle.object(forInfoDictionaryKey: "CFBundleName") as? String { candidates.append(name) }
        candidates.append(info.bundle_id)
        var parts: [String] = []
        if let http_scheme, let types = bundle.object(forInfoDictionaryKey: "CFBundleURLTypes") as? [[String: Any]],
           types.contains(where: { ($0["CFBundleURLSchemes"] as? [String])?.contains("http") == true }) {
            parts.append(http_scheme.text)
        }
        for candidate in candidates {
            if let resource = resources[candidate] { parts.append(resource.text); break }
        }
        try check()
        guard !parts.isEmpty else { return nil }
        let text = parts.joined(separator: "\n\n")
        let digest = Text.digest(text)
        try Text(content_sha256: digest, text: text).validate()
        return DesktopAppInstructions(bundle_id: info.bundle_id, bundle_path: info.bundle_path, content_sha256: digest, text: text)
    }
}
