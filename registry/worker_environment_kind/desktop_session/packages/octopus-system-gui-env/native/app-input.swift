import AppKit
import ApplicationServices
import Carbon
import Darwin
import Foundation

final class DesktopAppInputSPI {
    private typealias WindowIDFunction = @convention(c) (AXUIElement, UnsafeMutablePointer<CGWindowID>) -> AXError
    private typealias ActualPIDFunction = @convention(c) (AXUIElement, UnsafeMutablePointer<pid_t>) -> AXError
    private typealias WindowLocationFunction = @convention(c) (CGEvent, CGPoint) -> Void
    private let accessibilityLibrary: UnsafeMutableRawPointer
    private let graphicsLibrary: UnsafeMutableRawPointer
    private let getWindowID: WindowIDFunction
    private let getActualPID: ActualPIDFunction
    private let setWindowLocation: WindowLocationFunction

    init() throws {
        guard let accessibility = dlopen("/System/Library/Frameworks/ApplicationServices.framework/Frameworks/HIServices.framework/HIServices", RTLD_NOW | RTLD_LOCAL) else {
            throw DesktopFailure(code: "app_input_unavailable", message: "The native accessibility library is unavailable")
        }
        guard let graphics = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_NOW | RTLD_LOCAL) else {
            dlclose(accessibility)
            throw DesktopFailure(code: "app_input_unavailable", message: "The native graphics library is unavailable")
        }
        guard let window = dlsym(accessibility, "_AXUIElementGetWindow"),
              let location = dlsym(graphics, "CGEventSetWindowLocation"),
              let actual = dlsym(accessibility, "_AXUIElementGetActualPid") else {
            dlclose(graphics)
            dlclose(accessibility)
            throw DesktopFailure(code: "app_input_unavailable", message: "The exact native window input functions are unavailable")
        }
        accessibilityLibrary = accessibility
        graphicsLibrary = graphics
        getWindowID = unsafeBitCast(window, to: WindowIDFunction.self)
        getActualPID = unsafeBitCast(actual, to: ActualPIDFunction.self)
        setWindowLocation = unsafeBitCast(location, to: WindowLocationFunction.self)
    }

    deinit {
        dlclose(graphicsLibrary)
        dlclose(accessibilityLibrary)
    }

    func actualPID(_ element: AXUIElement) throws -> pid_t? {
        var pid: pid_t = 0
        let result = getActualPID(element, &pid)
        if result == .attributeUnsupported || result == .noValue { return nil }
        guard result == .success, pid > 0 else {
            throw DesktopFailure(code: "app_process_unavailable", message: "Native accessibility process identity is unavailable (\(result.rawValue))")
        }
        return pid
    }

    func optionalWindowID(_ element: AXUIElement, allowNoValue: Bool) throws -> CGWindowID? {
        var window: CGWindowID = 0
        let result = getWindowID(element, &window)
        if result == .attributeUnsupported || (allowNoValue && (result == .noValue || (result == .success && window == 0))) { return nil }
        guard result == .success, window != 0 else {
            throw DesktopFailure(code: "app_window_unavailable", message: "Native accessibility window identity is unavailable (\(result.rawValue))")
        }
        return window
    }

    func windowID(_ element: AXUIElement) throws -> CGWindowID {
        var window: CGWindowID = 0
        let result = getWindowID(element, &window)
        guard result == .success, window != 0 else {
            throw DesktopFailure(code: "app_window_unavailable", message: "Native accessibility window identity is unavailable (\(result.rawValue))")
        }
        return window
    }

    func locate(_ event: CGEvent, window: CGWindowID, bounds: CGRect, flipped: Bool) {
        event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window))
        event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(window))
        let location = event.location
        let y = location.y - bounds.minY
        setWindowLocation(event, CGPoint(x: location.x - bounds.minX, y: flipped ? bounds.height - y : y))
    }
}

struct DesktopAppInputWindow {
    let app: DesktopAppRef
    let element: AXUIElement
    let windowID: CGWindowID
    let bounds: CGRect
    let flipped: Bool
}

struct DesktopAppMouseTarget {
    fileprivate let logicalWindow: DesktopAppInputWindow
    fileprivate let process: NSRunningApplication
    fileprivate let processAnchor: AXUIElement
    fileprivate let windowAnchor: AXUIElement
    fileprivate let missingElementWindowAnchor: AXUIElement?
    fileprivate let ancestry: [[AXUIElement]]
    fileprivate let visibleHitPoint: CGPoint?
    fileprivate let nativeBounds: CGRect
    fileprivate let stamp: DesktopWindowStamp
    let windowID: CGWindowID
    let bounds: CGRect
    let flipped: Bool
}

struct DesktopAppKeyboardTarget {
    fileprivate let app: DesktopAppRef
    fileprivate let logicalWindow: DesktopAppInputWindow?
    fileprivate let process: NSRunningApplication
    fileprivate let processAnchor: AXUIElement?
    fileprivate let ancestry: [AXUIElement]
    let focusedElement: AXUIElement?
}

struct DesktopAppInputDelivery: Codable {
    let mechanism: String
    let dispatched_events: Int
    let application_effect_verified: Bool
}

struct DesktopAppInputInterrupted: Error {
    let cause: Error
    let dispatched_events: Int
}

@MainActor
final class DesktopAppInput {
    enum MousePhase { case down, up, drag, move }
    struct KeyStroke {
        let keyCode: CGKeyCode
        let flags: CGEventFlags
        let text: String?
    }

    private let applications: DesktopApplications
    private var spi: DesktopAppInputSPI?
    private var targetObserver: DesktopWindowObserver?
    private var watchedTargets = Set<CGWindowID>()
    private var nextEventNumber = 1
    private var closed = false
    private var sending = false

    init(applications: DesktopApplications) { self.applications = applications }

    func validate(_ app: DesktopAppRef, check: () throws -> Void) throws -> pid_t {
        try check()
        guard !closed else { throw DesktopFailure(code: "app_input_closed", message: "Application input was released") }
        guard AXIsProcessTrusted() else { throw DesktopFailure(code: "accessibility_denied", message: "Application input requires Accessibility permission") }
        let application = try applications.application(app)
        try check()
        return application.processIdentifier
    }

    func validate(_ target: DesktopAppInputWindow, check: () throws -> Void) throws -> pid_t {
        let applicationPID = try validate(target.app, check: check)
        var pid: pid_t = 0
        guard AXUIElementGetPid(target.element, &pid) == .success, pid == applicationPID,
              target.windowID != 0, target.bounds.width > 0, target.bounds.height > 0,
              [target.bounds.minX, target.bounds.minY, target.bounds.width, target.bounds.height].allSatisfy({ $0.isFinite }) else {
            throw DesktopFailure(code: "app_window_changed", message: "The native input target no longer identifies the observed application window")
        }
        guard try nativeSPI().windowID(target.element) == target.windowID else {
            throw DesktopFailure(code: "app_window_changed", message: "The retained accessibility window no longer matches the native window")
        }
        try check()
        return pid
    }

    func preparationWindow(_ app: DesktopAppRef, windows: [DesktopAppCaptureWindow],
                           check: () throws -> Void) throws -> DesktopAppInputWindow? {
        let pid = try validate(app, check: check)
        let root = AXUIElementCreateApplication(pid)
        guard let value = try axAttribute(root, kAXFocusedWindowAttribute as CFString, check: check) else { return nil }
        guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
            throw DesktopFailure(code: "invalid_ax_window", message: "The native preparation window is not an accessibility element")
        }
        let element = value as! AXUIElement
        let id = try nativeSPI().windowID(element)
        guard let observed = windows.first(where: { $0.app == app && $0.cg_window_id == id }) else {
            throw DesktopFailure(code: "app_window_unavailable", message: "The native preparation window is absent from the exact application inventory")
        }
        let frame = observed.frame_screen_points
        let target = DesktopAppInputWindow(app: app, element: element, windowID: id,
            bounds: CGRect(x: frame.x, y: frame.y, width: frame.width, height: frame.height), flipped: false)
        _ = try validate(target, check: check)
        return target
    }

    func keyboardTarget(_ app: DesktopAppRef, window: DesktopAppInputWindow?,
                        check: () throws -> Void) throws -> DesktopAppKeyboardTarget {
        let appPID = try validate(app, check: check)
        guard window == nil || window?.app == app else {
            throw DesktopFailure(code: "app_event_target_changed", message: "The native keyboard window belongs to another application")
        }
        if let window { _ = try validate(window, check: check) }
        let application = try applications.application(app)
        let root = AXUIElementCreateApplication(appPID)
        let value = try axAttribute(root, kAXFocusedUIElementAttribute as CFString, check: check)
        let focused: AXUIElement?
        if let value {
            guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
                throw DesktopFailure(code: "invalid_ax_focus", message: "The native keyboard focus is not an accessibility element")
            }
            focused = (value as! AXUIElement)
        } else { focused = nil }
        var process = application
        var anchor: AXUIElement?
        var retainedPath: [AXUIElement] = []
        if let focused {
            let path = try ancestry(focused, check: check)
            for (index, element) in path.enumerated() {
                guard let pid = try nativeSPI().actualPID(element),
                      let native = NSRunningApplication(processIdentifier: pid),
                      native.isFinishedLaunching, !native.isTerminated else { continue }
                if native.activationPolicy == .prohibited, pid != appPID {
                    process = native; anchor = element; retainedPath = Array(path.prefix(index + 1))
                }
                break
            }
        }
        let target = DesktopAppKeyboardTarget(app: app, logicalWindow: window, process: process,
            processAnchor: anchor, ancestry: retainedPath, focusedElement: focused)
        _ = try validate(target, check: check)
        return target
    }

    func validate(_ target: DesktopAppKeyboardTarget, check: () throws -> Void) throws -> pid_t {
        _ = try validate(target.app, check: check)
        if let window = target.logicalWindow { _ = try validate(window, check: check) }
        let pid = target.process.processIdentifier
        guard !target.process.isTerminated, target.process.isFinishedLaunching,
              let current = NSRunningApplication(processIdentifier: pid), current.isEqual(target.process),
              current.launchDate == target.process.launchDate else {
            throw DesktopFailure(code: "app_event_target_changed", message: "The retained native keyboard process no longer exists")
        }
        if let anchor = target.processAnchor, try nativeSPI().actualPID(anchor) != pid {
            throw DesktopFailure(code: "app_event_target_changed", message: "The retained hosted keyboard control changed process")
        }
        for (child, parent) in zip(target.ancestry, target.ancestry.dropFirst()) {
            guard let now = try axAttribute(child, kAXParentAttribute as CFString, check: check),
                  CFGetTypeID(now) == AXUIElementGetTypeID(), CFEqual(now, parent) else {
                throw DesktopFailure(code: "app_event_target_changed", message: "The retained hosted keyboard control changed ancestry")
            }
        }
        try check()
        return pid
    }

    private struct MouseResolution {
        let processID: pid_t
        let processAnchor: AXUIElement
        let windowAnchor: AXUIElement
        let windowID: CGWindowID
        let ancestry: [[AXUIElement]]
        let flipped: Bool
        var missingElementWindowAnchor: AXUIElement? = nil
    }

    func mouseTarget(_ window: DesktopAppInputWindow, at point: CGPoint, element: AXUIElement? = nil,
                     mode: DesktopApplicationBehavior.MouseTargeting, check: () throws -> Void) throws -> DesktopAppMouseTarget {
        let appPID = try validate(window, check: check)
        guard point.x.isFinite, point.y.isFinite, window.bounds.contains(point) else {
            throw DesktopFailure(code: "invalid_app_input", message: "Native mouse targeting requires an exact observed application window point")
        }
        let native = try nativeSPI()
        var resolution: MouseResolution?
        var hitPoint: CGPoint?
        if let actual = try native.actualPID(window.element), actual != appPID {
            resolution = MouseResolution(processID: actual, processAnchor: window.element,
                windowAnchor: window.element, windowID: try native.windowID(window.element), ancestry: [], flipped: true)
        } else if let element {
            let children = try firstChild(element, check: check)
            resolution = try hostedTarget(children ?? element, appPID: appPID, check: check)
            if resolution == nil {
                resolution = try ordinaryTarget(window, element: element, appPID: appPID, check: check)
                return try retainTarget(resolution!, logicalWindow: window, hitPoint: nil, check: check)
            }
        } else if mode == .suppliedWindow {
            guard try native.actualPID(window.element) == appPID, try native.windowID(window.element) == window.windowID else {
                throw DesktopFailure(code: "app_event_target_changed", message: "The declared supplied-window target no longer identifies the admitted native application window")
            }
            let supplied = MouseResolution(processID: appPID, processAnchor: window.element,
                windowAnchor: window.element, windowID: window.windowID, ancestry: [], flipped: false)
            return try retainTarget(supplied, logicalWindow: window, hitPoint: nil, check: check)
        }
        if try windowAtPoint(point, check: check) == window.windowID {
            let system = AXUIElementCreateSystemWide()
            try prepareAX(system, check: check)
            var hit: AXUIElement?
            let status = AXUIElementCopyElementAtPosition(system, Float(point.x), Float(point.y), &hit)
            guard status == .success, let hit else {
                throw DesktopFailure(code: "ax_hit_test_failed", message: "The exact visible application window has no native hit target (\(status.rawValue))")
            }
            if let hosted = try hostedTarget(hit, appPID: appPID, check: check) {
                resolution = hosted; hitPoint = point
            } else {
                resolution = try ordinaryTarget(window, element: hit, appPID: appPID, check: check)
                hitPoint = point
            }
        }
        guard let resolution else {
            throw DesktopFailure(code: "app_mouse_target_unavailable", message: "The native mouse point does not identify the admitted window or a retained hosted control")
        }
        return try retainTarget(resolution, logicalWindow: window, hitPoint: hitPoint, check: check)
    }

    private func ordinaryTarget(_ window: DesktopAppInputWindow, element: AXUIElement,
                                appPID: pid_t, check: () throws -> Void) throws -> MouseResolution {
        let native = try nativeSPI()
        let path = try ancestry(element, check: check)
        guard let anchor = try path.first(where: { try native.actualPID($0) == appPID }) else {
            throw DesktopFailure(code: "app_process_changed", message: "The exact native target no longer belongs to the admitted application")
        }
        if let id = try native.optionalWindowID(anchor, allowNoValue: true) {
            return MouseResolution(processID: appPID, processAnchor: anchor, windowAnchor: anchor,
                windowID: id, ancestry: [path], flipped: false)
        }
        let associated = try axAttribute(anchor, kAXWindowAttribute as CFString, check: check)
        let sameWindow = path.contains { CFEqual($0, window.element) } || associated.map {
            CFGetTypeID($0) == AXUIElementGetTypeID() && CFEqual($0, window.element)
        } == true
        guard sameWindow, try validate(window, check: check) == appPID,
              try native.actualPID(window.element) == appPID, try native.windowID(window.element) == window.windowID else {
            throw DesktopFailure(code: "app_window_unavailable", message: "The element without a native window ID does not retain the exact admitted caller window")
        }
        return MouseResolution(processID: appPID, processAnchor: anchor, windowAnchor: window.element,
            windowID: window.windowID, ancestry: [path], flipped: false, missingElementWindowAnchor: anchor)
    }

    private func hostedTarget(_ element: AXUIElement, appPID: pid_t,
                              check: () throws -> Void) throws -> MouseResolution? {
        let native = try nativeSPI()
        let path = try ancestry(element, check: check)
        for current in path {
            guard let pid = try native.actualPID(current), let process = NSRunningApplication(processIdentifier: pid),
                  process.isFinishedLaunching, !process.isTerminated else { continue }
            guard process.activationPolicy == .prohibited, pid != appPID else { return nil }
            let windowPath = try ancestry(current, check: check)
            for candidate in windowPath {
                if let id = try native.optionalWindowID(candidate, allowNoValue: false) {
                    return MouseResolution(processID: pid, processAnchor: current, windowAnchor: candidate,
                        windowID: id, ancestry: [path, windowPath], flipped: true)
                }
                let role = try axAttribute(candidate, kAXRoleAttribute as CFString, check: check) as? String
                if role == kAXWindowRole as String || role == kAXSheetRole as String { break }
            }
            throw DesktopFailure(code: "app_window_unavailable", message: "The exact hosted native control has no native window identity")
        }
        return nil
    }

    private func retainTarget(_ source: MouseResolution, logicalWindow: DesktopAppInputWindow,
                              hitPoint: CGPoint?, check: () throws -> Void) throws -> DesktopAppMouseTarget {
        guard let process = NSRunningApplication(processIdentifier: source.processID), !process.isTerminated,
              process.isFinishedLaunching, watchedTargets.contains(source.windowID) || watchedTargets.count < 4096 else {
            throw DesktopFailure(code: "app_process_unavailable", message: "The retained native event process is unavailable or its target bound is exhausted")
        }
        if targetObserver == nil { targetObserver = try DesktopWindowObserver() }
        watchedTargets.insert(source.windowID)
        try targetObserver!.watch(watchedTargets)
        guard let stamp = try targetObserver!.snapshot()[source.windowID], !stamp.terminated else {
            throw DesktopFailure(code: "app_window_unavailable", message: "The exact native event window cannot be observed")
        }
        let bounds = try nativeWindowBounds(source.windowID, processID: source.processID, check: check)
        let target = DesktopAppMouseTarget(logicalWindow: logicalWindow, process: process,
            processAnchor: source.processAnchor, windowAnchor: source.windowAnchor,
            missingElementWindowAnchor: source.missingElementWindowAnchor, ancestry: source.ancestry,
            visibleHitPoint: hitPoint, nativeBounds: bounds, stamp: stamp, windowID: source.windowID,
            bounds: source.flipped ? bounds : logicalWindow.bounds, flipped: source.flipped)
        _ = try validate(target, check: check)
        return target
    }

    func validate(_ target: DesktopAppMouseTarget, check: () throws -> Void) throws -> pid_t {
        _ = try validate(target.logicalWindow, check: check)
        let pid = target.process.processIdentifier
        guard !target.process.isTerminated, target.process.isFinishedLaunching,
              let current = NSRunningApplication(processIdentifier: pid), current.isEqual(target.process),
              current.launchDate == target.process.launchDate,
              try nativeSPI().actualPID(target.processAnchor) == pid,
              try nativeSPI().windowID(target.windowAnchor) == target.windowID,
              try nativeWindowBounds(target.windowID, processID: pid, check: check) == target.nativeBounds else {
            throw DesktopFailure(code: "app_event_target_changed", message: "The retained native event process or window changed before delivery")
        }
        if let element = target.missingElementWindowAnchor {
            if let current = try nativeSPI().optionalWindowID(element, allowNoValue: true), current != target.windowID {
                throw DesktopFailure(code: "app_event_target_changed", message: "The retained element now identifies a different native window")
            }
            let associated = try axAttribute(element, kAXWindowAttribute as CFString, check: check)
            guard target.ancestry.contains(where: { $0.contains { CFEqual($0, target.windowAnchor) } }) || associated.map({
                CFGetTypeID($0) == AXUIElementGetTypeID() && CFEqual($0, target.windowAnchor)
            }) == true else {
                throw DesktopFailure(code: "app_event_target_changed", message: "The retained element no longer belongs to the exact supplied native window")
            }
        }
        for path in target.ancestry {
            for (child, parent) in zip(path, path.dropFirst()) {
                guard let now = try axAttribute(child, kAXParentAttribute as CFString, check: check),
                      CFGetTypeID(now) == AXUIElementGetTypeID(), CFEqual(now, parent) else {
                    throw DesktopFailure(code: "app_event_target_changed", message: "Native hosted control ancestry changed before delivery")
                }
            }
        }
        if let point = target.visibleHitPoint, try windowAtPoint(point, check: check) != target.logicalWindow.windowID {
            throw DesktopFailure(code: "app_event_target_changed", message: "The native visible hit no longer belongs to the admitted application window")
        }
        guard let observer = targetObserver else { throw DesktopFailure(code: "app_input_closed", message: "Native mouse target observation was released") }
        try observer.validate(target.windowID, stamp: target.stamp)
        try check()
        return pid
    }

    private func nativeWindowBounds(_ id: CGWindowID, processID: pid_t, check: () throws -> Void) throws -> CGRect {
        try check()
        guard let rows = CGWindowListCopyWindowInfo(.optionIncludingWindow, id) as? [[String: Any]], rows.count == 1,
              let row = rows.first, (row[kCGWindowNumber as String] as? NSNumber)?.uint32Value == id,
              (row[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == processID,
              let value = row[kCGWindowBounds as String] as? [String: Any],
              let bounds = CGRect(dictionaryRepresentation: value as CFDictionary),
              [bounds.minX, bounds.minY, bounds.width, bounds.height].allSatisfy({ $0.isFinite }),
              bounds.width > 0, bounds.height > 0 else {
            throw DesktopFailure(code: "app_window_changed", message: "The exact native event window and process have no current finite geometry")
        }
        try check()
        return bounds
    }

    private func windowAtPoint(_ point: CGPoint, check: () throws -> Void) throws -> CGWindowID? {
        try check()
        guard let screen = NSScreen.screens.first(where: { NSMouseInRect(point, $0.frame, false) }) else { return nil }
        let cocoa = CGPoint(x: point.x, y: screen.frame.height - point.y)
        let id = NSWindow.windowNumber(at: cocoa, belowWindowWithWindowNumber: 0)
        try check()
        return id > 0 && id <= Int(UInt32.max) ? CGWindowID(id) : nil
    }

    private func prepareAX(_ element: AXUIElement, check: () throws -> Void) throws {
        try check()
        let result = AXUIElementSetMessagingTimeout(element, 1)
        guard result == .success else { throw DesktopFailure(code: "ax_timeout_unavailable", message: "Cannot bound native target identity reads (\(result.rawValue))") }
    }

    private func axAttribute(_ element: AXUIElement, _ name: CFString, check: () throws -> Void) throws -> CFTypeRef? {
        try prepareAX(element, check: check)
        var value: CFTypeRef?
        let result = AXUIElementCopyAttributeValue(element, name, &value)
        if result == .attributeUnsupported || result == .noValue { return nil }
        guard result == .success else { throw DesktopFailure(code: "ax_read_failed", message: "Native target identity read failed (\(result.rawValue))") }
        return value
    }

    private func firstChild(_ element: AXUIElement, check: () throws -> Void) throws -> AXUIElement? {
        try prepareAX(element, check: check)
        var values: CFArray?
        let result = AXUIElementCopyAttributeValues(element, kAXChildrenAttribute as CFString, 0, 1, &values)
        if result == .attributeUnsupported || result == .noValue { return nil }
        guard result == .success, let values else { throw DesktopFailure(code: "ax_read_failed", message: "Native child target read failed (\(result.rawValue))") }
        guard CFArrayGetCount(values) != 0 else { return nil }
        let first = unsafeBitCast(CFArrayGetValueAtIndex(values, 0), to: CFTypeRef.self)
        guard CFGetTypeID(first) == AXUIElementGetTypeID() else { throw DesktopFailure(code: "invalid_ax_child", message: "Native child target is not an accessibility element") }
        return first as! AXUIElement
    }

    private func ancestry(_ element: AXUIElement, check: () throws -> Void) throws -> [AXUIElement] {
        var path: [AXUIElement] = []
        var current: AXUIElement? = element
        while let node = current {
            try check()
            guard path.count < 300, !path.contains(where: { CFEqual($0, node) }) else {
                throw DesktopFailure(code: "ax_ancestry_limit", message: "Native target ancestry is cyclic or exceeds its finite bound")
            }
            path.append(node)
            guard let parent = try axAttribute(node, kAXParentAttribute as CFString, check: check) else { break }
            guard CFGetTypeID(parent) == AXUIElementGetTypeID() else { throw DesktopFailure(code: "invalid_ax_parent", message: "Native target parent is not an accessibility element") }
            current = parent as! AXUIElement
        }
        return path
    }

    func mouse(_ phase: MousePhase, at point: CGPoint, button: CGMouseButton, count: Int,
               flags: CGEventFlags, target: DesktopAppMouseTarget, check: () throws -> Void) throws -> CGEvent {
        _ = try validate(target, check: check)
        let validCount: Bool
        switch phase {
        case .down, .up, .move: validCount = (1...3).contains(count)
        case .drag: validCount = count == 0
        }
        guard point.x.isFinite, point.y.isFinite, target.bounds.contains(point), validCount,
              button.rawValue <= 31, nextEventNumber < Int.max else {
            throw DesktopFailure(code: "invalid_app_input", message: "Mouse input requires an observed window point, supported button and bounded click count")
        }
        let type: NSEvent.EventType
        switch (phase, button) {
        case (.move, _): type = .mouseMoved
        case (.down, .left): type = .leftMouseDown
        case (.up, .left): type = .leftMouseUp
        case (.drag, .left): type = .leftMouseDragged
        case (.down, .right): type = .rightMouseDown
        case (.up, .right): type = .rightMouseUp
        case (.drag, .right): type = .rightMouseDragged
        case (.down, _): type = .otherMouseDown
        case (.up, _): type = .otherMouseUp
        case (.drag, _): type = .otherMouseDragged
        }
        let number = nextEventNumber
        nextEventNumber += 1
        guard let event = NSEvent.mouseEvent(with: type, location: point, modifierFlags: [], timestamp: 0,
                                            windowNumber: Int(target.windowID), context: nil, eventNumber: number,
                                            clickCount: count, pressure: 1)?.cgEvent else {
            throw DesktopFailure(code: "app_event_unavailable", message: "Native mouse event construction failed")
        }
        event.flags = flags
        event.location = point
        event.setIntegerValueField(.mouseEventButtonNumber, value: Int64(button.rawValue))
        event.setIntegerValueField(.mouseEventSubtype, value: 3)
        try nativeSPI().locate(event, window: target.windowID, bounds: target.bounds, flipped: target.flipped)
        return event
    }

    func click(at point: CGPoint, dragTo destination: CGPoint? = nil, button: CGMouseButton,
               count: Int, flags: CGEventFlags, target: DesktopAppMouseTarget,
               check: () throws -> Void) throws -> [CGEvent] {
        guard (1...3).contains(count) else {
            throw DesktopFailure(code: "invalid_app_input", message: "Click count is outside its finite bound")
        }
        var result: [CGEvent] = []
        for clickCount in 1...count {
            result.append(try mouse(.down, at: point, button: button, count: clickCount,
                                    flags: flags, target: target, check: check))
            if let destination {
                let midpoint = CGPoint(x: (point.x + destination.x) / 2, y: (point.y + destination.y) / 2)
                for position in [point, midpoint, destination] {
                    result.append(try mouse(.drag, at: position, button: button, count: 0,
                                            flags: flags, target: target, check: check))
                }
            }
            result.append(try mouse(.up, at: destination ?? point, button: button, count: clickCount,
                                    flags: flags, target: target, check: check))
        }
        return result
    }

    func scroll(at point: CGPoint, horizontal: Int32, vertical: Int32, target: DesktopAppMouseTarget,
                check: () throws -> Void) throws -> CGEvent {
        _ = try validate(target, check: check)
        guard point.x.isFinite, point.y.isFinite, target.bounds.contains(point),
              let source = CGEventSource(stateID: .hidSystemState),
              let event = CGEvent(scrollWheelEvent2Source: source, units: .pixel, wheelCount: 1,
                                  wheel1: vertical, wheel2: horizontal, wheel3: 0) else {
            throw DesktopFailure(code: "app_event_unavailable", message: "Native scroll event construction requires a current window point")
        }
        event.location = point
        try nativeSPI().locate(event, window: target.windowID, bounds: target.bounds, flipped: target.flipped)
        return event
    }

    static func pageScroll(direction: String, pages: Double, axisExtent: CGFloat) throws -> (horizontal: Int32, vertical: Int32) {
        guard ["up", "down", "left", "right"].contains(direction), pages.isFinite, pages > 0,
              axisExtent.isFinite, axisExtent >= 0 else {
            throw DesktopFailure(code: "invalid_app_scroll", message: "Page scroll requires a direction, positive finite pages and exact native geometry")
        }
        let pixels = (pages * max(Double(axisExtent), 100)).rounded(.toNearestOrAwayFromZero)
        guard pixels.isFinite, pixels >= 1, pixels <= 10000 else {
            throw DesktopFailure(code: "invalid_app_scroll", message: "Page distance exceeds the existing nonzero native pixel-scroll bound")
        }
        let amount = Int32(pixels)
        switch direction {
        case "up": return (0, amount)
        case "down": return (0, -amount)
        case "left": return (-amount, 0)
        default: return (amount, 0)
        }
    }

    func keystrokes(for text: String, check: () throws -> Void) throws -> [KeyStroke] {
        try check()
        let units = Array(text.utf16)
        guard !units.isEmpty, units.count <= 65536, !units.contains(0) else {
            throw DesktopFailure(code: "invalid_app_input", message: "Typed input must contain 1...65536 non-NUL UTF16 units")
        }
        guard let source = TISCopyCurrentKeyboardLayoutInputSource()?.takeRetainedValue(),
              let property = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData) else {
            throw DesktopFailure(code: "keyboard_layout_unavailable", message: "The active keyboard layout has no native Unicode mapping")
        }
        let value = Unmanaged<CFTypeRef>.fromOpaque(property).takeUnretainedValue()
        guard CFGetTypeID(value) == CFDataGetTypeID() else {
            throw DesktopFailure(code: "keyboard_layout_unavailable", message: "The active keyboard layout data has an invalid native type")
        }
        let data = unsafeBitCast(value, to: CFData.self)
        guard CFDataGetLength(data) >= MemoryLayout<UCKeyboardLayout>.size,
              let bytes = CFDataGetBytePtr(data) else {
            throw DesktopFailure(code: "keyboard_layout_unavailable", message: "The active keyboard layout data is incomplete")
        }
        let layout = UnsafeRawPointer(bytes).assumingMemoryBound(to: UCKeyboardLayout.self)
        let keyboardType = UInt32(LMGetKbdType())
        let modifiers: [UInt32] = [0, 0x200, 0x800, 0xa00, 0x1000, 0x1200, 0x1800, 0x1a00]
        var mapped: [UInt16: KeyStroke] = [:]
        for modifier in modifiers {
            var flags: CGEventFlags = []
            if modifier & 0x200 != 0 { flags.insert(.maskShift) }
            if modifier & 0x800 != 0 { flags.insert(.maskAlternate) }
            if modifier & 0x1000 != 0 { flags.insert(.maskControl) }
            for code in UInt16(0)...UInt16(127) {
                try check()
                var deadState: UInt32 = 0
                var length: Int = 0
                var translated = [UniChar](repeating: 0, count: 4)
                let status = UCKeyTranslate(layout, code, UInt16(kUCKeyActionDisplay), modifier >> 8,
                    keyboardType, 0, &deadState, translated.count, &length, &translated)
                if status == noErr, length > 0, length <= translated.count, mapped[translated[0]] == nil {
                    mapped[translated[0]] = KeyStroke(keyCode: code, flags: flags,
                        text: String(decoding: translated.prefix(length), as: UTF16.self))
                }
            }
        }
        var strokes: [KeyStroke] = []
        strokes.reserveCapacity(units.count)
        for (index, unit) in units.enumerated() {
            try check()
            guard let stroke = mapped[unit == 10 ? 13 : unit] else {
                throw DesktopFailure(code: "keyboard_character_unavailable",
                    message: "The active keyboard layout cannot represent UTF16 unit at offset \(index); no text events were dispatched")
            }
            strokes.append(stroke)
        }
        withExtendedLifetime(source) {}
        return strokes
    }

    func keyboard(_ strokes: [KeyStroke], check: () throws -> Void) throws -> [CGEvent] {
        guard !strokes.isEmpty, strokes.count <= 65536 else {
            throw DesktopFailure(code: "invalid_app_input", message: "Native keystroke count is outside the text bound")
        }
        var events: [CGEvent] = []
        events.reserveCapacity(strokes.count * 4)
        for stroke in strokes { try check(); events.append(contentsOf: try keyboard(stroke)) }
        return events
    }

    func keyboard(_ stroke: KeyStroke) throws -> [CGEvent] {
        guard stroke.text.map({ $0.utf16.count <= 65536 }) ?? true,
              let source = CGEventSource(stateID: .hidSystemState),
              let flags = CGEvent(source: source),
              let down = CGEvent(keyboardEventSource: source, virtualKey: stroke.keyCode, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: stroke.keyCode, keyDown: false),
              let restore = CGEvent(source: source) else {
            throw DesktopFailure(code: "app_event_unavailable", message: "Native keyboard event construction failed or Unicode input exceeds its bound")
        }
        flags.type = .flagsChanged
        flags.flags = stroke.flags
        down.flags = stroke.flags
        up.flags = stroke.flags
        if let text = stroke.text {
            let units = Array(text.utf16)
            units.withUnsafeBufferPointer { buffer in
                if let base = buffer.baseAddress {
                    down.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: base)
                    up.keyboardSetUnicodeString(stringLength: buffer.count, unicodeString: base)
                }
            }
        }
        restore.type = .flagsChanged
        restore.flags = CGEventSource.flagsState(.combinedSessionState)
        return [flags, down, restore, up]
    }

    func send(_ events: [CGEvent], to target: DesktopAppMouseTarget, interval: Duration,
              check: () throws -> Void) async throws -> DesktopAppInputDelivery {
        try await send(events, interval: interval) { try self.validate(target, check: check) }
    }

    func send(_ events: [CGEvent], to target: DesktopAppKeyboardTarget, interval: Duration,
              maximumEvents: Int = 4096, check: () throws -> Void) async throws -> DesktopAppInputDelivery {
        try await send(events, interval: interval, maximumEvents: maximumEvents) { try self.validate(target, check: check) }
    }

    func send(_ events: [CGEvent], to target: DesktopAppInputWindow, interval: Duration,
              maximumEvents: Int = 4096, check: () throws -> Void) async throws -> DesktopAppInputDelivery {
        try await send(events, interval: interval, maximumEvents: maximumEvents) { try self.validate(target, check: check) }
    }

    func send(_ events: [CGEvent], to app: DesktopAppRef, interval: Duration,
              check: () throws -> Void) async throws -> DesktopAppInputDelivery {
        try await send(events, interval: interval) { try self.validate(app, check: check) }
    }

    private func send(_ events: [CGEvent], interval: Duration, maximumEvents: Int = 4096,
                      validateTarget: () throws -> pid_t) async throws -> DesktopAppInputDelivery {
        guard !sending, !events.isEmpty, (1...(65536 * 4)).contains(maximumEvents),
              events.count <= maximumEvents, interval >= .zero, interval <= .seconds(1) else {
            throw DesktopFailure(code: "invalid_app_input", message: "Application event delivery is busy or outside its finite bounds")
        }
        _ = try validateTarget()
        sending = true
        defer { sending = false }
        var dispatched = 0
        do {
            for event in events {
                try Task.checkCancellation()
                let pid = try validateTarget()
                event.timestamp = DispatchTime.now().uptimeNanoseconds
                event.postToPid(pid)
                dispatched += 1
                if interval > .zero { try await ContinuousClock().sleep(for: interval) }
                _ = try validateTarget()
            }
        } catch {
            throw DesktopAppInputInterrupted(cause: error, dispatched_events: dispatched)
        }
        return DesktopAppInputDelivery(mechanism: "CGEventPostToPid", dispatched_events: dispatched, application_effect_verified: false)
    }

    func close() { closed = true; try? targetObserver?.close(); targetObserver = nil; watchedTargets.removeAll(); spi = nil }

    private func nativeSPI() throws -> DesktopAppInputSPI {
        if let spi { return spi }
        let loaded = try DesktopAppInputSPI()
        spi = loaded
        return loaded
    }
}
