import AppKit
import ApplicationServices
import Foundation

struct DesktopAppRect: Codable, Equatable {
    let x: Double
    let y: Double
    let width: Double
    let height: Double

    init(_ rect: CGRect) throws {
        guard [rect.minX, rect.minY, rect.width, rect.height].allSatisfy({ $0.isFinite }), rect.width >= 0, rect.height >= 0 else {
            throw DesktopFailure(code: "invalid_app_geometry", message: "Application geometry is not finite")
        }
        x = rect.minX; y = rect.minY; width = rect.width; height = rect.height
    }
}

struct DesktopAXValueInput: Codable, Equatable {
    enum Kind: String, Codable { case string, float, integer, boolean, date }
    let kind: Kind
    let format: String?
    let date_components_mask: UInt64?

    static func classify(_ value: CFTypeRef?, role: String, subrole: String?, dateMask: UInt64?) throws -> Self? {
        guard let value else {
            return [kAXTextFieldRole as String, kAXTextAreaRole as String].contains(role)
                ? .init(kind: .string, format: nil, date_components_mask: nil) : nil
        }
        let kind: Kind
        if role == kAXCheckBoxRole as String, subrole.map({ ["AXSwitch", "AXToggle"].contains($0) }) == true { kind = .boolean }
        else if CFGetTypeID(value) == CFBooleanGetTypeID() { kind = .boolean }
        else if CFGetTypeID(value) == CFNumberGetTypeID() { kind = CFNumberIsFloatType(value as! CFNumber) ? .float : .integer }
        else if CFGetTypeID(value) == CFStringGetTypeID() { kind = .string }
        else if CFGetTypeID(value) == CFDateGetTypeID() { kind = .date }
        else { return nil }
        if kind == .date {
            let mask = dateMask ?? 0xee
            let formatter = dateFormatter(mask: mask)
            guard let format = formatter.dateFormat, !format.isEmpty, format.utf8.count <= 2048 else {
                throw DesktopFailure(code: "ax_date_format_unavailable", message: "The native date field has no bounded input format")
            }
            return .init(kind: kind, format: format, date_components_mask: mask)
        }
        return .init(kind: kind, format: kind == .float || kind == .integer ? "en_US decimal" : nil, date_components_mask: nil)
    }

    static func dateFormatter(mask: UInt64) -> DateFormatter {
        let result = DateFormatter()
        switch mask {
        case 0xe0: result.dateStyle = .short; result.timeStyle = .none
        case 0x0c, 0x0e: result.dateStyle = .none; result.timeStyle = .short
        case 0xc0: result.dateFormat = "MMMM yyyy"
        default: result.dateStyle = .short; result.timeStyle = .short
        }
        return result
    }

    func convert(_ text: String, replacing current: CFTypeRef?) throws -> CFTypeRef {
        guard text.utf16.count <= 65536, !text.contains("\0") else {
            throw DesktopFailure(code: "value_too_large", message: "Editable value exceeds its native text domain")
        }
        switch kind {
        case .string: return text as CFString
        case .float, .integer:
            let formatter = NumberFormatter()
            formatter.numberStyle = .decimal
            formatter.locale = Locale(identifier: "en_US")
            guard let result = formatter.number(from: text), result.doubleValue.isFinite else {
                throw DesktopFailure(code: "ax_value_conversion_failed", message: "The value is not a finite en_US decimal number")
            }
            return result
        case .boolean:
            let lower = text.lowercased()
            let value = lower == "on" ? true : lower == "off" ? false : (text as NSString).boolValue
            return value ? kCFBooleanTrue : kCFBooleanFalse
        case .date:
            guard let current, CFGetTypeID(current) == CFDateGetTypeID(), let mask = date_components_mask else {
                throw DesktopFailure(code: "ax_value_conversion_failed", message: "The date field has no retained native date or component mask")
            }
            let formatter = Self.dateFormatter(mask: mask)
            guard formatter.dateFormat == format, let parsed = formatter.date(from: text) else {
                throw DesktopFailure(code: "ax_value_conversion_failed", message: "The value does not match the retained native date format")
            }
            let calendar = Calendar.current
            let components: [(UInt64, Calendar.Component)] = [(2,.second),(4,.minute),(8,.hour),(32,.day),(64,.month),(128,.year)]
            var result = calendar.dateComponents([.year,.month,.day,.hour,.minute,.second,.timeZone], from: current as! Date)
            for (bit, component) in components where mask & bit != 0 { result.setValue(calendar.component(component, from: parsed), for: component) }
            guard let date = calendar.date(from: result) else {
                throw DesktopFailure(code: "ax_value_conversion_failed", message: "The native date components do not form a calendar date")
            }
            return date as NSDate
        }
    }
}

struct DesktopAXNode: Codable, Equatable {
    let element_id: String
    let parent_id: String?
    let window_id: String?
    let window_unavailable_reason: String?
    let role: String
    let subrole: String?
    let title: String?
    let description: String?
    let value: String?
    let value_type: String?
    let value_input: DesktopAXValueInput?
    let enabled: Bool?
    let focused: Bool?
    let frame_screen_points: DesktopAppRect?
    let settable_attributes: [String]
    var actions: [DesktopAXAction]
    let unavailable_attributes: [String]
    var text_presentation: DesktopAXTextPresentation? = nil
}

struct DesktopAXOmission: Codable, Equatable {
    let parent_id: String?
    let reason: String
}

struct DesktopAXDisplayOmission: Codable, Equatable {
    let element_id: String
    let reason: String
}

struct DesktopAXPrimaryWindow {
    let app: DesktopAppRef
    let element: AXUIElement
    let window: DesktopAppCaptureWindow
    let incarnation: String
    let selection: String

    func matches(_ other: DesktopAXPrimaryWindow) -> Bool {
        app == other.app && CFEqual(element, other.element) && window == other.window &&
            incarnation == other.incarnation && selection == other.selection
    }
}

struct DesktopAXObservationContext: Codable, Equatable {
    let root_kind: String
    let root_element_id: String
    let menu_bar_element_id: String?
    let display_omissions: [DesktopAXDisplayOmission]
}

struct DesktopAXMenuEffect: Codable {
    let reason: String
    let transition: String
    let source_element_id: String
    let menu_bar_item_id: String
    let menu_id: String?
    let before_revision: UInt64
    let changed: Bool
}

struct DesktopAXObservation: Codable {
    let presentation: DesktopAXPresentation
    let context: DesktopAXObservationContext
    let observation_id: String
    let app: DesktopAppRef
    let revision: UInt64
    let captured_at_unix_ms: UInt64
    let mode: String
    let base_revision: UInt64?
    let nodes: [DesktopAXNode]
    let removed_ids: [String]
    let complete: Bool
    let omissions: [DesktopAXOmission]
}

struct DesktopAXEffect: Codable {
    let dispatch_state: String
    let mechanism: String
    let before_revision: UInt64
    let native_error_code: Int32
    let readback_matches: Bool?
    let readback_error: String?
}

struct DesktopAXPreparationEffect: Codable {
    let source_element_id: String
    let affected_element_id: String?
    let mechanism: String
    let name: String
    let value: Double?
    var dispatch_state: String
    var native_error_code: Int32?
    var dispatched_events: UInt32? = nil
}

@MainActor
final class DesktopAXPreparationEvidence {
    var effects: [DesktopAXPreparationEffect] = []
}

struct DesktopAXDispatchFailure: Error {
    let native_error_code: Int32
}

struct DesktopAXGuard: Codable {
    let app: DesktopAppRef
    let observation_id: String
    let revision: UInt64
    let element_id: String
}

struct DesktopAXPresentationWindow {
    let element: AXUIElement
    let id: UInt32
    let role: String
    let subrole: String?
    let modal: Bool
    let parent: UInt32?
}

struct DesktopAXPresentationInventory {
    let windows: [UInt32: DesktopAXPresentationWindow]
    let mainWindow: UInt32?
    let destroyedElements: [AXUIElement]
}

@MainActor
final class DesktopAccessibility {
    enum Action {
        case setValue(String)
        case selectText(text: String, prefix: String?, suffix: String?, selectionType: String)
        case secondaryAction(String)
    }
    final class PreparedAction {
        fileprivate let id = UUID()
        fileprivate let guardValue: DesktopAXGuard
        fileprivate let windowID: CGWindowID?
        fileprivate let action: Action
        fileprivate let convertedValue: CFTypeRef?
        fileprivate var selectionRange: NSRange?
        fileprivate var disclosureValue: Bool?
        fileprivate var menu: AXUIElement?
        fileprivate var menuTransition: String?
        var usesMenu: Bool { menuTransition != nil }
        fileprivate(set) var point: CGPoint?
        fileprivate var used = false

        fileprivate init(guardValue: DesktopAXGuard, windowID: CGWindowID?, action: Action, convertedValue: CFTypeRef?) {
            self.guardValue = guardValue
            self.windowID = windowID
            self.action = action
            self.convertedValue = convertedValue
        }
    }
    final class PreparedClick {
        fileprivate let id = UUID()
        fileprivate let guardValue: DesktopAXGuard
        fileprivate let windowID: CGWindowID?
        fileprivate let button: CGMouseButton
        fileprivate let count: Int
        fileprivate var actionName: String?
        fileprivate var menu: AXUIElement?
        var usesMenu: Bool { menu != nil }
        fileprivate(set) var point: CGPoint?
        fileprivate var used = false
        var usesAccessibility: Bool { actionName != nil }

        fileprivate init(guardValue: DesktopAXGuard, windowID: CGWindowID?, button: CGMouseButton, count: Int) {
            self.guardValue = guardValue; self.windowID = windowID; self.button = button; self.count = count
        }
    }
    final class PreparedScroll {
        fileprivate let id = UUID()
        fileprivate let guardValue: DesktopAXGuard
        fileprivate let windowID: CGWindowID
        fileprivate var frame: CGRect?
        fileprivate(set) var point: CGPoint?
        fileprivate(set) var axisExtent: CGFloat = 0
        fileprivate var used = false

        fileprivate init(guardValue: DesktopAXGuard, windowID: CGWindowID) {
            self.guardValue = guardValue; self.windowID = windowID
        }
    }
    private struct Entry {
        let element: AXUIElement
        let parent: AXUIElement?
        let node: DesktopAXNode
        let rawValue: CFTypeRef?
        var attributed: DesktopAXAttributedSource? = nil
        let visible: Bool
        let hidesActions: Bool
    }
    private struct Snapshot {
        let observation: DesktopAXObservation
        let rendered: DesktopAXRenderedTree
        let root: AXUIElement
        let menuBar: AXUIElement?
        let menuGeneration: UInt64
        let primaryWindow: DesktopAXPrimaryWindow?
        let entries: [String: Entry]
        let windows: [CGWindowID: String]
        let processID: pid_t
        var consumed: Bool
        var preparedID: UUID?
    }
    private let applications: DesktopApplications
    private var snapshots: [String: Snapshot] = [:]
    private struct PendingObservation {
        let id: String
        let previous: Snapshot?
    }
    private var pendingObservations: [String: PendingObservation] = [:]
    private var revisions: [String: UInt64] = [:]
    private var closed = false
    private var windowSPI: DesktopAppInputSPI?
    private var menuObservers: [DesktopAppRef: DesktopAXMenuObserver] = [:]
    private var callDeadline: Date?
    private var cancellationCheck: (() throws -> Void)?
    private let maxNodes = 4096
    private let maxDepth = 64
    private let maxText = 65_536

    init(applications: DesktopApplications) { self.applications = applications }

    func configureMenus(_ app: DesktopAppRef, profile: DesktopApplicationBehavior.Profile,
                        checkRead: @escaping () throws -> Void,
                        onSignal: @escaping (DesktopAXMenuObserver.Signal) -> Void) throws {
        try requireReady(); try checkRead()
        if let observer = menuObservers[app] {
            try observer.configure(profile: profile, checkRead: checkRead, onSignal: onSignal)
        } else {
            menuObservers[app] = try DesktopAXMenuObserver(application: applications.application(app),
                profile: profile, checkRead: checkRead, onSignal: onSignal)
        }
    }

    func validatePresentationBase(_ base: DesktopAXPresentationBase) throws {
        try requireReady()
        guard let previous = snapshots[base.app.app_instance_id], previous.observation.app == base.app,
              previous.observation.observation_id == base.observation_id, previous.observation.revision == base.revision,
              previous.observation.complete else {
            throw DesktopFailure(code: "diff_base_missing", message: "Display compression requires the exact retained complete application observation")
        }
        _ = try applications.application(base.app)
    }

    func primaryWindow(_ app: DesktopAppRef, windows: [DesktopAppCaptureWindow], incarnations: [String: String],
                       check: @escaping () throws -> Void) throws -> DesktopAXPrimaryWindow? {
        try begin(check); defer { end() }
        try requireReady()
        let application = try applications.application(app)
        let root = AXUIElementCreateApplication(application.processIdentifier)
        let element: AXUIElement
        let selection: String
        func window(_ value: CFTypeRef) throws -> AXUIElement {
            guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
                throw DesktopFailure(code: "invalid_ax_primary_window", message: "Native primary window attribute is not an accessibility element")
            }
            return value as! AXUIElement
        }
        if let value = try attribute(root, kAXFocusedWindowAttribute) {
            element = try window(value); selection = "ax_focused_window"
        } else if let value = try attribute(root, kAXMainWindowAttribute) {
            element = try window(value); selection = "ax_main_window"
        } else if let value = try attribute(root, kAXWindowsAttribute) {
            guard CFGetTypeID(value) == CFArrayGetTypeID(), let values = value as? [AXUIElement], values.count <= 256 else {
                throw DesktopFailure(code: "invalid_ax_primary_window", message: "Native AXWindows is not a bounded window inventory")
            }
            guard let last = values.last else { return nil }
            element = last; selection = "ax_windows_last"
        } else { return nil }
        var pid: pid_t = 0
        guard AXUIElementGetPid(element, &pid) == .success, pid == application.processIdentifier else {
            throw DesktopFailure(code: "invalid_ax_primary_window", message: "Native primary window belongs to another application")
        }
        if windowSPI == nil { windowSPI = try DesktopAppInputSPI() }
        guard let windowSPI else { throw DesktopFailure(code: "app_window_unavailable", message: "Native window identity function is unavailable") }
        let nativeID = try windowSPI.windowID(element)
        guard let descriptor = windows.first(where: { $0.app == app && $0.cg_window_id == nativeID }),
              let incarnation = incarnations[descriptor.window_id] else {
            throw DesktopFailure(code: "primary_window_unavailable", message: "Native primary window has no exact owned capture incarnation")
        }
        try check()
        let primary = DesktopAXPrimaryWindow(app: app, element: element, window: descriptor, incarnation: incarnation, selection: selection)
        try validateNativePrimary(primary)
        return primary
    }

    private func validateNativePrimary(_ primary: DesktopAXPrimaryWindow) throws {
        let application = try applications.application(primary.app)
        try timeout(primary.element)
        var pid: pid_t = 0
        guard AXUIElementGetPid(primary.element, &pid) == .success, pid == application.processIdentifier,
              let windowSPI, try windowSPI.windowID(primary.element) == primary.window.cg_window_id else {
            throw DesktopFailure(code: "primary_window_changed", message: "Retained native primary window identity is no longer available")
        }
    }

    func validateObservationContext(_ observation: DesktopAXObservation, primary: DesktopAXPrimaryWindow?,
                                    check: @escaping () throws -> Void) throws {
        try begin(check); defer { end() }
        try requireReady()
        guard let snapshot = snapshots[observation.app.app_instance_id], snapshot.observation.observation_id == observation.observation_id,
              snapshot.observation.revision == observation.revision, let menu = menuObservers[observation.app] else {
            throw DesktopFailure(code: "observation_context_changed", message: "Native observation context was replaced before publication")
        }
        try menu.check()
        let application = try applications.application(observation.app)
        let root = menu.selected?.item ?? menu.openedMenu ?? AXUIElementCreateApplication(application.processIdentifier)
        guard menu.generation == snapshot.menuGeneration, CFEqual(root, snapshot.root) else {
            throw DesktopFailure(code: "menu_context_changed", message: "Native menu or raw AX context changed during capture")
        }
        switch (primary, snapshot.primaryWindow) {
        case (nil, nil): break
        case (.some(let current), .some(let retained)) where current.matches(retained): try validateNativePrimary(current)
        default: throw DesktopFailure(code: "primary_window_changed", message: "Capture and raw observation identify different primary windows")
        }
        try check()
    }

    func observe(_ app: DesktopAppRef, windows: [DesktopAppCaptureWindow], presentationBase: DesktopAXPresentationBase?,
                 primary: DesktopAXPrimaryWindow?, profile: DesktopAXPresentationProfile, check: @escaping () throws -> Void) throws -> DesktopAXObservation {
        try begin(check); defer { end() }
        try requireReady()
        let application = try applications.application(app)
        guard let observer = menuObservers[app] else { throw DesktopFailure(code: "menu_observer_unavailable", message: "Application observation has no declared native menu owner") }
        try observer.check()
        let applicationRoot = AXUIElementCreateApplication(application.processIdentifier)
        if let primary { try validateNativePrimary(primary) }
        let root = observer.selected?.item ?? observer.openedMenu ?? applicationRoot
        let rootKind = observer.selected != nil ? "menu_bar_item" : observer.openedMenu != nil ? "menu" : "application"
        let menuGeneration = observer.generation
        try timeout(root)
        let menuBar: AXUIElement?
        if rootKind == "application", let raw = try attribute(applicationRoot, kAXMenuBarAttribute) {
            guard CFGetTypeID(raw) == AXUIElementGetTypeID(), try string(raw as! AXUIElement, kAXRoleAttribute) == kAXMenuBarRole as String else {
                throw DesktopFailure(code: "invalid_menu_bar", message: "Application AXMenuBar does not identify a native menu bar")
            }
            menuBar = raw as! AXUIElement
        } else { menuBar = nil }
        var windowIDs: [CGWindowID: String] = [:]
        for window in windows {
            guard window.app == app, window.cg_window_id != 0, windowIDs[window.cg_window_id] == nil else {
                throw DesktopFailure(code: "invalid_ax_window_inventory", message: "Accessibility window associations require the exact unique native application inventory")
            }
            windowIDs[window.cg_window_id] = window.window_id
        }
        guard pendingObservations[app.app_instance_id] == nil else {
            throw DesktopFailure(code: "observation_pending", message: "An application capture observation is awaiting its original operation result")
        }
        let previous = snapshots[app.app_instance_id]
        if let presentationBase {
            guard presentationBase.app == app else { throw DesktopFailure(code: "diff_base_foreign", message: "Display base belongs to another application") }
            try validatePresentationBase(presentationBase)
        }
        let deadline = Date().addingTimeInterval(5)
        let oldBuckets = Dictionary(grouping: Array(previous?.entries.values ?? Dictionary<String, Entry>().values), by: { CFHash($0.element) })
        func identifier(_ element: AXUIElement) -> String {
            if let old = oldBuckets[CFHash(element)]?.first(where: { CFEqual($0.element, element) }) { return old.node.element_id }
            if let selected = observer.selected {
                if CFEqual(selected.item, element) { return selected.itemID }
                if CFEqual(selected.menu, element) { return selected.menuID }
            }
            return UUID().uuidString.lowercased()
        }
        var entries: [String: Entry] = [:]
        var facts: [String: DesktopAXDisplayFacts] = [:]
        var titleElements: [String: [AXUIElement]] = [:]
        var selectedText: String?
        var ordered: [DesktopAXNode] = []
        var omissions: [DesktopAXOmission] = []
        var display: [DesktopAXDisplayOmission] = []
        var seen: [CFHashCode: [AXUIElement]] = [:]
        var stack: [(AXUIElement, AXUIElement?, String?, Int, Int?)] = []
        if let menuBar { stack.append((menuBar, nil, nil, 0, nil)) }
        stack.append((root, nil, nil, 0, nil))
        var textBytes = 0
        let shaping = observer.presentation
        while let (element, parent, parentID, depth, menuIndex) = stack.popLast() {
            try check()
            guard Date() < deadline else { omissions.append(.init(parent_id: parentID, reason: "deadline")); break }
            guard entries.count < maxNodes else { omissions.append(.init(parent_id: parentID, reason: "node_limit")); break }
            guard depth <= maxDepth else { omissions.append(.init(parent_id: parentID, reason: "depth_limit")); continue }
            let hash = CFHash(element)
            if seen[hash]?.contains(where: { CFEqual($0, element) }) == true { continue }
            seen[hash, default: []].append(element)
            try timeout(element)
            let old = oldBuckets[hash]?.first(where: { CFEqual($0.element, element) })
            let id = identifier(element)
            let (node, rawValue) = try readNode(element, id: id, parentID: parentID, previous: old?.node,
                windows: windowIDs, processID: application.processIdentifier)
            textBytes += try JSONEncoder().encode(node).count
            guard textBytes <= 4 * 1024 * 1024 else { omissions.append(.init(parent_id: parentID, reason: "text_limit")); break }
            let immediate = menuIndex != nil && node.role == "AXMenuBarItem"
            let hiddenFirst = immediate && menuIndex == 0 && shaping.omit_first_item
            let hiddenTitle = immediate && node.title.map { title in shaping.excluded_title_substrings.contains { title.contains($0) } } == true
            let visible = !hiddenFirst && !hiddenTitle
            let hidesActions = immediate && shaping.clear_immediate_actions
            entries[id] = Entry(element: element, parent: parent, node: node, rawValue: rawValue, visible: visible, hidesActions: hidesActions)
            let (nativeFacts, nativeTitles) = try displayFacts(element, node: node, rawValue: rawValue)
            facts[id] = nativeFacts; titleElements[id] = nativeTitles
            if node.focused == true { selectedText = try string(element, kAXSelectedTextAttribute) }

            if hiddenFirst { display.append(.init(element_id: id, reason: "first_menu_item")) }
            if hiddenTitle { display.append(.init(element_id: id, reason: "excluded_menu_title")) }
            if hidesActions && !node.actions.isEmpty { display.append(.init(element_id: id, reason: "immediate_menu_actions")) }
            ordered.append(node)
            if immediate {
                display.append(.init(element_id: id, reason: "immediate_menu_descendants"))
                continue
            }
            let children = try children(element, remaining: maxNodes - entries.count, omissions: &omissions, parentID: id)
            let isMenuBar = menuBar.map { CFEqual($0, element) } == true
            for (index, child) in children.enumerated().reversed() { stack.append((child, element, id, depth + 1, isMenuBar ? index : nil)) }
        }
        try check(); try observer.check()
        guard menuGeneration == observer.generation else { throw DesktopFailure(code: "menu_context_changed", message: "Native menu context changed during observation") }
        _ = try applications.application(app)
        guard let rootID = entries.first(where: { CFEqual($0.value.element, root) })?.key else {
            throw DesktopFailure(code: "ax_root_unavailable", message: "The native root could not be observed inside the finite read bound")
        }
        let barID = menuBar.flatMap { bar in entries.first(where: { CFEqual($0.value.element, bar) })?.key }
        let context = DesktopAXObservationContext(root_kind: rootKind, root_element_id: rootID, menu_bar_element_id: barID, display_omissions: display)
        let samePrimary: Bool
        switch (previous?.primaryWindow, primary) {
        case (nil, nil): samePrimary = true
        case (.some(let old), .some(let current)): samePrimary = old.matches(current)
        default: samePrimary = false
        }
        let sameContext = previous.map {
            samePrimary && CFEqual($0.root, root) && $0.menuGeneration == menuGeneration && $0.windows == windowIDs &&
                $0.observation.context == context && $0.rendered.profile == profile
        } ?? false
        let idBuckets = Dictionary(grouping: Array(entries.values), by: { CFHash($0.element) })
        for (id, targets) in titleElements {
            facts[id]?.titleTargets = targets.compactMap { target in
                idBuckets[CFHash(target)]?.first(where: { CFEqual($0.element, target) })?.node.element_id
            }
            if facts[id]?.titleTargets.count != targets.count { facts[id]?.titleTargets = [] }
        }
        let displayNodes = ordered.filter { entries[$0.element_id]?.visible == true }
        let visibleIDs = Set(displayNodes.map(\.element_id))
        let childIDs = Dictionary(grouping: displayNodes.filter { $0.parent_id != nil }, by: { $0.parent_id! })
        var incomplete = Set<String>()
        var rootIncomplete = false
        for omission in omissions {
            guard var id = omission.parent_id else { rootIncomplete = true; continue }
            while incomplete.insert(id).inserted, let parent = entries[id]?.node.parent_id { id = parent }
        }
        func displayNode(_ id: String) throws -> DesktopAXDisplayFacts {
            try check()
            guard var value = facts[id] else { throw DesktopFailure(code: "ax_presentation_identity", message: "Display node has no retained native source") }
            value.complete = !rootIncomplete && !incomplete.contains(id)
            if entries[id]?.hidesActions == true { value.actions = [] }
            value.children = try (childIDs[id] ?? []).map { try displayNode($0.element_id) }
            return value
        }
        let roots = try displayNodes.filter { $0.parent_id == nil || !visibleIDs.contains($0.parent_id!) }.map { try displayNode($0.element_id) }
        let windowTitle: String?
        if let primary { windowTitle = try string(primary.element, kAXTitleAttribute) }
        else { windowTitle = nil }
        let rendered = try DesktopAXRenderedTree.render(roots, windowTitle: windowTitle,
            applicationName: applications.info(app).display_name, selectedText: selectedText, profile: profile, check: check)
        for fact in rendered.facts {
            if let attributed = fact.attributedSource { entries[fact.id]?.attributed = attributed }
        }
        ordered = ordered.map { node in
            var result = node
            if let attributed = entries[node.element_id]?.attributed {
                result.text_presentation = DesktopAXTextPresentation(source: attributed.source, format: "attributed_markdown", text: attributed.mapped.text)
            } else if let value = node.value, node.value_type == "string" {
                result.text_presentation = DesktopAXTextPresentation(source: "ax_value", format: "plain", text: value)
            }
            return result
        }
        let requestedBase = presentationBase != nil && sameContext && omissions.isEmpty && previous?.observation.complete == true
        let presentation = try rendered.presentation(base: requestedBase ? previous?.rendered : nil,
            baseRevision: requestedBase ? presentationBase?.revision : nil)
        try check(); try observer.check()
        guard observer.generation == menuGeneration else { throw DesktopFailure(code: "menu_context_changed", message: "Native menu changed before display publication") }
        let next = (revisions[app.app_instance_id] ?? 0) + 1
        guard next <= 9_007_199_254_740_991 else { throw DesktopFailure(code: "revision_exhausted", message: "Native observation revision exhausted") }
        revisions[app.app_instance_id] = next
        let full = DesktopAXObservation(presentation: presentation, context: context, observation_id: UUID().uuidString.lowercased(), app: app, revision: next,
            captured_at_unix_ms: UInt64(Date().timeIntervalSince1970 * 1000), mode: "full", base_revision: nil,
            nodes: ordered, removed_ids: [], complete: omissions.isEmpty, omissions: omissions)
        guard try JSONEncoder().encode(full).count <= 4 * 1024 * 1024 else { throw DesktopFailure(code: "ax_presentation_limit", message: "Raw observation and native presentation exceed the owned text envelope") }
        pendingObservations[app.app_instance_id] = PendingObservation(id: full.observation_id, previous: previous)
        snapshots[app.app_instance_id] = Snapshot(observation: full, rendered: rendered, root: root, menuBar: menuBar, menuGeneration: menuGeneration,
            primaryWindow: primary, entries: entries, windows: windowIDs, processID: application.processIdentifier, consumed: false, preparedID: nil)
        return full
    }

    func commitObservation(_ observation: DesktopAXObservation) throws {
        try requireReady()
        let app = observation.app.app_instance_id
        guard pendingObservations[app]?.id == observation.observation_id,
              snapshots[app]?.observation.observation_id == observation.observation_id else {
            throw DesktopFailure(code: "observation_context_changed", message: "The original native observation was replaced before its result could be published")
        }
        pendingObservations.removeValue(forKey: app)
    }

    func discardObservation(_ observation: DesktopAXObservation) {
        let app = observation.app.app_instance_id
        guard let pending = pendingObservations[app], pending.id == observation.observation_id else { return }
        pendingObservations.removeValue(forKey: app)
        guard !closed, snapshots[app]?.observation.observation_id == observation.observation_id else { return }
        snapshots[app] = pending.previous
    }

    func prepare(_ guardValue: DesktopAXGuard, windowID: CGWindowID?, action: Action,
                 evidence: DesktopAXPreparationEvidence, check: @escaping () throws -> Void) async throws -> PreparedAction {
        try begin(check); defer { end() }
        let entry = try validated(guardValue, preparedID: nil, check: check)
        try validateWindow(entry, windowID: windowID)
        var convertedValue: CFTypeRef?
        var selection: NSRange?
        var disclosure: Bool?
        var menu: AXUIElement?
        var menuTransition: String?
        switch action {
        case .setValue(let value):
            guard value.utf16.count <= maxText else {
                throw DesktopFailure(code: "value_too_large", message: "Editable value exceeds the supported bound")
            }
            guard let input = entry.node.value_input, entry.node.settable_attributes.contains(kAXValueAttribute as String) else {
                throw DesktopFailure(code: "attribute_not_settable", message: "The exact element does not advertise a supported writable native value")
            }
            convertedValue = try input.convert(value, replacing: entry.rawValue)
        case .selectText(let text, let prefix, let suffix, let selectionType):
            guard let value = entry.node.value, entry.node.value_type == "string",
                  entry.node.settable_attributes.contains(kAXSelectedTextRangeAttribute as String) else {
                throw DesktopFailure(code: "selection_unsupported", message: "The exact editable element does not expose a writable character selection")
            }
            if let attributed = entry.attributed, let range = attributed.selection(text: text, prefix: prefix, suffix: suffix) {
                try validateAttributed(entry)
                guard ["text", "cursor_before", "cursor_after"].contains(selectionType) else { throw DesktopFailure(code: "invalid_selection", message: "Unsupported native selection type") }
                selection = selectionType == "cursor_before" ? NSRange(location: range.location, length: 0) :
                    selectionType == "cursor_after" ? NSRange(location: NSMaxRange(range), length: 0) : range
            } else {
                selection = try Self.selectionRange(value: value, text: text, prefix: prefix, suffix: suffix, selectionType: selectionType)
            }
        case .secondaryAction(let actionID):
            guard !entry.hidesActions, let declared = entry.node.actions.first(where: { $0.action_id == actionID }) else {
                throw DesktopFailure(code: "stale_action", message: "Action ID is not advertised by the exact observed element")
            }
            if declared.name == "SAIExpand" || declared.name == "SAICollapse" {
                guard let value = try disclosing(entry.element), (declared.name == "SAICollapse") == value else {
                    throw DesktopFailure(code: "stale_action", message: "Native disclosure state differs from its advertised action")
                }
                disclosure = !value
            } else if entry.node.role == "AXMenuBarItem" {
                if declared.name == "AXPick" { menu = try menuChild(entry.element); if menu != nil { menuTransition = "select" } }
                else if declared.name == "AXCancel" { menuTransition = "clear" }
            }
        }
        try check()
        let prepared = PreparedAction(guardValue: guardValue, windowID: windowID, action: action, convertedValue: convertedValue)
        prepared.selectionRange = selection; prepared.disclosureValue = disclosure
        prepared.menu = menu; prepared.menuTransition = menuTransition
        snapshots[guardValue.app.app_instance_id]?.consumed = true
        snapshots[guardValue.app.app_instance_id]?.preparedID = prepared.id
        if let selection {
            if let initial = try boundsForRange(entry.element, selection), let window = try selectionWindow(entry.element, windowID: windowID) {
                try await scrollToVisible(entry.element, frame: initial, guardValue: guardValue, evidence: evidence, check: check)
                if let frame = try boundsForRange(entry.element, selection) {
                    prepared.point = CGPoint(x: min(max(frame.maxX, window.minX), window.maxX),
                                             y: min(max(frame.maxY, window.minY), window.maxY))
                }
            }
        } else if !prepared.usesMenu, let frame = try nativeFrame(entry.element) {
            if try clippedPoint(entry.element, frame: frame) == nil {
                try await scrollToVisible(entry.element, frame: frame, guardValue: guardValue, evidence: evidence, check: check)
            }
            prepared.point = try nativeFrame(entry.element).flatMap { try clippedPoint(entry.element, frame: $0) }
        }
        _ = try validated(guardValue, preparedID: prepared.id, check: check)
        return prepared
    }

    func focusField(_ prepared: PreparedAction, clickDOMField: Bool, evidence: DesktopAXPreparationEvidence,
                    check: @escaping () throws -> Void) throws -> CGPoint? {
        try begin(check); defer { end() }
        guard !prepared.used else { throw DesktopFailure(code: "stale_action", message: "The native field action is already consumed") }
        let entry = try validated(prepared.guardValue, preparedID: prepared.id, check: check)
        try validateWindow(entry, windowID: prepared.windowID)
        guard [kAXTextFieldRole as String, kAXTextAreaRole as String, "AXDateTimeArea"].contains(entry.node.role),
              try writable(entry.element, name: kAXFocusedAttribute),
              (try attribute(entry.element, kAXFocusedAttribute) as? Bool) != true else { return nil }
        let forceClick = try clickDOMField && attribute(entry.element, "AXDOMClassList") != nil
        if !forceClick {
            try preparationEffect(entry.element, guardValue: prepared.guardValue, name: kAXFocusedAttribute,
                value: nil, action: false, evidence: evidence)
            if (try attribute(entry.element, kAXFocusedAttribute) as? Bool) == true { return nil }
        }
        let point = try nativeFrame(entry.element).flatMap { try clippedPoint(entry.element, frame: $0) }
        let preferred = ["AXPick", kAXPressAction as String].first { name in entry.node.actions.contains { $0.name == name } }
        var synthesize = preferred == nil
        if try insideWebArea(entry.element), let point { synthesize = try hitBelongsToElement(entry.element, app: prepared.guardValue.app, at: point) }
        if let preferred, !synthesize {
            try preparationEffect(entry.element, guardValue: prepared.guardValue, name: preferred,
                value: nil, action: true, evidence: evidence)
        } else {
            guard prepared.windowID != nil, let point else {
                throw DesktopFailure(code: "field_not_focusable", message: "The exact native field has no owned window point for focus preparation")
            }
            try check()
            return point
        }
        _ = try validated(prepared.guardValue, preparedID: prepared.id, check: check)
        return nil
    }

    func fieldElement(_ prepared: PreparedAction, at point: CGPoint, check: @escaping () throws -> Void) throws -> AXUIElement {
        try begin(check); defer { end() }
        guard !prepared.used else { throw DesktopFailure(code: "stale_action", message: "The native field action is already consumed") }
        let entry = try validated(prepared.guardValue, preparedID: prepared.id, check: check)
        try validateWindow(entry, windowID: prepared.windowID)
        guard let frame = try nativeFrame(entry.element), try clippedPoint(entry.element, frame: frame) == point else {
            throw DesktopFailure(code: "stale_element_geometry", message: "The exact native field point changed before focus dispatch")
        }
        return entry.element
    }

    func perform(_ prepared: PreparedAction, check: @escaping () throws -> Void) throws -> DesktopAXEffect {
        try begin(check); defer { end() }
        guard !prepared.used else { throw DesktopFailure(code: "stale_action", message: "The prepared native action has already been consumed") }
        let guardValue = prepared.guardValue
        let entry = try validated(guardValue, preparedID: prepared.id, check: check)
        try validateWindow(entry, windowID: prepared.windowID)
        try timeout(entry.element)
        prepared.used = true
        snapshots[guardValue.app.app_instance_id]?.preparedID = nil
        switch prepared.action {
        case .setValue:
            guard let value = prepared.convertedValue else { throw DesktopFailure(code: "invalid_prepared_value", message: "The native AX conversion plan is absent") }
            let result = AXUIElementSetAttributeValue(entry.element, kAXValueAttribute as CFString, value)
            try effectResult(result)
            let readback: CFTypeRef?
            var readbackError: String?
            do { readback = try attribute(entry.element, kAXValueAttribute) }
            catch { readback = nil; readbackError = String(describing: error) }
            return DesktopAXEffect(dispatch_state: "returned", mechanism: "ax_value", before_revision: guardValue.revision,
                                   native_error_code: result.rawValue, readback_matches: readbackError == nil ? readback.map { CFEqual($0, value) } == true : nil, readback_error: readbackError)
        case .selectText:
            guard let range = prepared.selectionRange else { throw DesktopFailure(code: "stale_action", message: "The prepared native character range is unavailable") }
            var nativeRange = CFRange(location: range.location, length: range.length)
            guard let nativeValue = AXValueCreate(.cfRange, &nativeRange) else {
                throw DesktopFailure(code: "selection_unavailable", message: "Cannot construct native character range")
            }
            try timeout(entry.element)
            let result = AXUIElementSetAttributeValue(entry.element, kAXSelectedTextRangeAttribute as CFString, nativeValue)
            try effectResult(result)
            let current: CFTypeRef?
            var readbackError: String?
            do { current = try attribute(entry.element, kAXSelectedTextRangeAttribute) }
            catch { current = nil; readbackError = String(describing: error) }
            var observed = CFRange(location: 0, length: 0)
            let matches: Bool
            if let current, CFGetTypeID(current) == AXValueGetTypeID(), AXValueGetValue(current as! AXValue, .cfRange, &observed) {
                matches = observed.location == nativeRange.location && observed.length == nativeRange.length
            } else { matches = false }
            return DesktopAXEffect(dispatch_state: "returned", mechanism: "ax_selection", before_revision: guardValue.revision,
                                   native_error_code: result.rawValue, readback_matches: readbackError == nil ? matches : nil, readback_error: readbackError)
        case .secondaryAction(let actionID):
            guard !prepared.usesMenu else { throw DesktopFailure(code: "invalid_app_action", message: "Logical menu selection cannot become AX dispatch") }
            if let value = prepared.disclosureValue {
                guard let current = try disclosing(entry.element), current != value else {
                    throw DesktopFailure(code: "stale_action", message: "Native disclosure changed before dispatch")
                }
                let status = AXUIElementSetAttributeValue(entry.element, "AXDisclosing" as CFString, value ? kCFBooleanTrue : kCFBooleanFalse)
                try effectResult(status)
                var readback: Bool?
                var readbackError: String?
                do { readback = try disclosing(entry.element).map { $0 == value } }
                catch { readbackError = String(describing: error) }
                return DesktopAXEffect(dispatch_state: "returned", mechanism: "ax_disclosing", before_revision: guardValue.revision,
                    native_error_code: status.rawValue, readback_matches: readback, readback_error: readbackError)
            }
            guard let action = entry.node.actions.first(where: { $0.action_id == actionID }) else {
                throw DesktopFailure(code: "stale_action", message: "The prepared native action is unavailable")
            }
            let result = AXUIElementPerformAction(entry.element, action.name as CFString)
            try effectResult(result)
            return DesktopAXEffect(dispatch_state: "returned", mechanism: "ax_action", before_revision: guardValue.revision,
                                   native_error_code: result.rawValue, readback_matches: nil, readback_error: nil)
        }
    }

    func prepareClick(_ guardValue: DesktopAXGuard, windowID: CGWindowID?, button: CGMouseButton, count: Int,
                      evidence: DesktopAXPreparationEvidence, check: @escaping () throws -> Void) async throws -> PreparedClick {
        try begin(check); defer { end() }
        guard (1...3).contains(count), [CGMouseButton.left, .right, .center].contains(button) else {
            throw DesktopFailure(code: "invalid_app_action", message: "Element click requires a supported button and click count")
        }
        let entry = try validated(guardValue, preparedID: nil, check: check)
        try validateWindow(entry, windowID: windowID)
        guard entry.node.enabled != false else { throw DesktopFailure(code: "element_disabled", message: "The exact observed element is disabled") }
        let prepared = PreparedClick(guardValue: guardValue, windowID: windowID, button: button, count: count)
        if button == .left, count == 1, entry.node.role == "AXMenuBarItem" { prepared.menu = try menuChild(entry.element) }
        snapshots[guardValue.app.app_instance_id]?.consumed = true
        snapshots[guardValue.app.app_instance_id]?.preparedID = prepared.id
        if !prepared.usesMenu, let frame = try nativeFrame(entry.element) {
            if try clippedPoint(entry.element, frame: frame) == nil {
                try await scrollToVisible(entry.element, frame: frame, guardValue: guardValue, evidence: evidence, check: check)
            }
            prepared.point = try nativeFrame(entry.element).flatMap { try clippedPoint(entry.element, frame: $0) }
        }
        _ = try validated(guardValue, preparedID: prepared.id, check: check)
        let preferred = ["AXPick", kAXPressAction as String].first { name in entry.node.actions.contains { $0.name == name } }
        if button == .left, count == 1, let preferred {
            var synthesize = false
            if try insideWebArea(entry.element), let point = prepared.point {
                synthesize = try hitBelongsToElement(entry.element, app: guardValue.app, at: point)
            }
            if !synthesize { prepared.actionName = preferred }
        }
        if prepared.actionName == nil && !prepared.usesMenu {
            guard prepared.point != nil, windowID != nil else {
                throw DesktopFailure(code: "element_not_clickable", message: "The exact element has no native click action or clipped point in an owned window")
            }
        }
        try check()
        return prepared
    }

    func clickElement(_ prepared: PreparedClick, check: @escaping () throws -> Void) throws -> AXUIElement {
        try begin(check); defer { end() }
        return try validatePreparedClick(prepared, check: check).element
    }

    func validateClick(_ prepared: PreparedClick, check: @escaping () throws -> Void) throws {
        try begin(check); defer { end() }
        _ = try validatePreparedClick(prepared, check: check)
    }

    func performClick(_ prepared: PreparedClick, check: @escaping () throws -> Void) throws -> DesktopAXEffect {
        try begin(check); defer { end() }
        let entry = try validatePreparedClick(prepared, check: check)
        guard let name = prepared.actionName else {
            throw DesktopFailure(code: "invalid_app_action", message: "The prepared element click requires native mouse delivery")
        }
        try timeout(entry.element)
        prepared.used = true
        snapshots[prepared.guardValue.app.app_instance_id]?.preparedID = nil
        let result = AXUIElementPerformAction(entry.element, name as CFString)
        try effectResult(result)
        return DesktopAXEffect(dispatch_state: "returned", mechanism: "ax_click", before_revision: prepared.guardValue.revision,
            native_error_code: result.rawValue, readback_matches: nil, readback_error: nil)
    }

    func consumeClick(_ prepared: PreparedClick, check: @escaping () throws -> Void) throws {
        try begin(check); defer { end() }
        _ = try validatePreparedClick(prepared, check: check)
        guard prepared.actionName == nil && !prepared.usesMenu else {
            throw DesktopFailure(code: "invalid_app_action", message: "A prepared native accessibility or menu action cannot become mouse input")
        }
        prepared.used = true
        snapshots[prepared.guardValue.app.app_instance_id]?.preparedID = nil
    }

    private func validatePreparedClick(_ prepared: PreparedClick, check: () throws -> Void) throws -> Entry {
        guard !prepared.used else { throw DesktopFailure(code: "stale_action", message: "The prepared native click has already been consumed") }
        let entry = try validated(prepared.guardValue, preparedID: prepared.id, check: check)
        try validateWindow(entry, windowID: prepared.windowID)
        if prepared.actionName == nil && !prepared.usesMenu {
            guard let point = prepared.point, let frame = try nativeFrame(entry.element),
                  try clippedPoint(entry.element, frame: frame) == point else {
                throw DesktopFailure(code: "stale_element_geometry", message: "The native element click point changed while preparing input")
            }
        }
        try check()
        return entry
    }

    func prepareScroll(_ guardValue: DesktopAXGuard, windowID: CGWindowID, windowBounds: CGRect, direction: String,
                       evidence: DesktopAXPreparationEvidence, check: @escaping () throws -> Void) async throws -> PreparedScroll {
        try begin(check); defer { end() }
        let entry = try validated(guardValue, preparedID: nil, check: check)
        try validateWindow(entry, windowID: windowID)
        guard entry.node.enabled != false else { throw DesktopFailure(code: "element_disabled", message: "The exact observed scroll element is disabled") }
        let prepared = PreparedScroll(guardValue: guardValue, windowID: windowID)
        snapshots[guardValue.app.app_instance_id]?.consumed = true
        snapshots[guardValue.app.app_instance_id]?.preparedID = prepared.id
        if let frame = try nativeFrame(entry.element), try clippedPoint(entry.element, frame: frame) == nil {
            try await scrollToVisible(entry.element, frame: frame, guardValue: guardValue, evidence: evidence, check: check)
        }
        prepared.frame = try nativeFrame(entry.element)
        prepared.point = try prepared.frame.flatMap { try clippedPoint(entry.element, frame: $0) }
        let bounds = prepared.frame ?? windowBounds
        prepared.axisExtent = ["up", "down"].contains(direction) ? bounds.height : bounds.width
        _ = try validated(guardValue, preparedID: prepared.id, check: check)
        try check()
        return prepared
    }

    func consumeScroll(_ prepared: PreparedScroll, check: @escaping () throws -> Void) throws {
        try begin(check); defer { end() }
        guard !prepared.used else { throw DesktopFailure(code: "stale_action", message: "The native element scroll is already consumed") }
        let entry = try validated(prepared.guardValue, preparedID: prepared.id, check: check)
        try validateWindow(entry, windowID: prepared.windowID)
        let frame = try nativeFrame(entry.element)
        let point = try frame.flatMap { try clippedPoint(entry.element, frame: $0) }
        guard frame == prepared.frame, point == prepared.point else {
            throw DesktopFailure(code: "stale_element_geometry", message: "The native page-scroll geometry changed before dispatch")
        }
        try check()
        prepared.used = true
        snapshots[prepared.guardValue.app.app_instance_id]?.preparedID = nil
    }

    func performMenu(_ prepared: PreparedAction, check: @escaping () throws -> Void) throws -> DesktopAXMenuEffect {
        try begin(check); defer { end() }
        guard !prepared.used, let transition = prepared.menuTransition else { throw DesktopFailure(code: "stale_action", message: "Native menu action is absent or consumed") }
        let entry = try validated(prepared.guardValue, preparedID: prepared.id, check: check)
        try validateWindow(entry, windowID: prepared.windowID)
        let result = try applyMenu(entry, menu: prepared.menu, transition: transition, guardValue: prepared.guardValue, check: check)
        prepared.used = true; snapshots[prepared.guardValue.app.app_instance_id]?.preparedID = nil
        return result
    }

    func performMenuClick(_ prepared: PreparedClick, check: @escaping () throws -> Void) throws -> DesktopAXMenuEffect {
        try begin(check); defer { end() }
        let entry = try validatePreparedClick(prepared, check: check)
        guard prepared.button == .left, prepared.count == 1, let menu = prepared.menu else { throw DesktopFailure(code: "invalid_app_action", message: "Click does not own a native menu selection") }
        let result = try applyMenu(entry, menu: menu, transition: "select", guardValue: prepared.guardValue, check: check)
        prepared.used = true; snapshots[prepared.guardValue.app.app_instance_id]?.preparedID = nil
        return result
    }

    private func applyMenu(_ entry: Entry, menu: AXUIElement?, transition: String, guardValue: DesktopAXGuard, check: () throws -> Void) throws -> DesktopAXMenuEffect {
        guard entry.node.role == "AXMenuBarItem", let observer = menuObservers[guardValue.app] else {
            throw DesktopFailure(code: "stale_menu", message: "Native menu selection has no retained application owner")
        }
        try check(); try observer.check()
        let old = observer.selected
        if transition == "select" {
            guard let menu, let current = try menuChild(entry.element), CFEqual(current, menu) else {
                throw DesktopFailure(code: "stale_menu", message: "The exact native menu child changed after preparation")
            }
            let menuID = snapshots[guardValue.app.app_instance_id]?.entries.first(where: { CFEqual($0.value.element, menu) })?.key
                ?? (old.flatMap { CFEqual($0.menu, menu) ? $0.menuID : nil }) ?? UUID().uuidString.lowercased()
            try observer.select(item: entry.element, itemID: entry.node.element_id, menu: menu, menuID: menuID)
            return DesktopAXMenuEffect(reason: "requested", transition: "select", source_element_id: entry.node.element_id,
                menu_bar_item_id: entry.node.element_id, menu_id: menuID, before_revision: guardValue.revision,
                changed: old.map { !CFEqual($0.item, entry.element) || !CFEqual($0.menu, menu) } ?? true)
        }
        let changed = observer.selected != nil || observer.openedMenu != nil
        try observer.clear()
        return DesktopAXMenuEffect(reason: "requested", transition: "clear", source_element_id: entry.node.element_id,
            menu_bar_item_id: entry.node.element_id, menu_id: old?.menuID, before_revision: guardValue.revision, changed: changed)
    }

    func clearMenuAfterAction(_ app: DesktopAppRef, sourceElementID: String, revision: UInt64, check: @escaping () throws -> Void) throws -> DesktopAXMenuEffect? {
        try check()
        guard let observer = menuObservers[app], let selected = observer.selected else { return nil }
        try observer.check(); try observer.clear()
        return DesktopAXMenuEffect(reason: "after_native_action", transition: "clear", source_element_id: sourceElementID,
            menu_bar_item_id: selected.itemID, menu_id: selected.menuID, before_revision: revision, changed: true)
    }

    private func menuChild(_ element: AXUIElement) throws -> AXUIElement? {
        var omissions: [DesktopAXOmission] = []
        let values = try children(element, remaining: maxNodes, omissions: &omissions, parentID: nil)
        guard omissions.isEmpty else { throw DesktopFailure(code: "menu_child_limit", message: "Native menu membership could not be completely inspected") }
        for child in values {
            if try string(child, kAXRoleAttribute) == kAXMenuRole as String { return child }
        }
        return nil
    }

    private func disclosing(_ element: AXUIElement) throws -> Bool? {
        guard let value = try attribute(element, "AXDisclosing"), try writable(element, name: "AXDisclosing") else { return nil }
        guard CFGetTypeID(value) == CFBooleanGetTypeID() else {
            throw DesktopFailure(code: "invalid_ax_disclosure", message: "Native AXDisclosing is not a boolean")
        }
        return (value as! NSNumber).boolValue
    }

    private func boundsForRange(_ element: AXUIElement, _ range: NSRange) throws -> CGRect? {
        var rawRange = CFRange(location: range.location, length: range.length)
        guard let parameter = AXValueCreate(.cfRange, &rawRange) else { throw DesktopFailure(code: "selection_unavailable", message: "Native character range cannot be represented") }
        try timeout(element)
        var raw: CFTypeRef?
        let status = AXUIElementCopyParameterizedAttributeValue(element, kAXBoundsForRangeParameterizedAttribute as CFString, parameter, &raw)
        try cancellationCheck?()
        guard status == .success else { return nil }
        guard let raw, CFGetTypeID(raw) == AXValueGetTypeID() else { throw DesktopFailure(code: "invalid_ax_geometry", message: "Native character bounds have the wrong type") }
        var bounds = CGRect.zero
        guard AXValueGetValue(raw as! AXValue, .cgRect, &bounds),
              [bounds.origin.x,bounds.origin.y,bounds.width,bounds.height].allSatisfy(\.isFinite), bounds.width >= 0, bounds.height >= 0 else {
            throw DesktopFailure(code: "invalid_ax_geometry", message: "Native character bounds are not a finite rectangle")
        }
        return bounds
    }

    private func selectionWindow(_ element: AXUIElement, windowID: CGWindowID?) throws -> CGRect? {
        guard windowID != nil, let raw = try attribute(element, kAXWindowAttribute), CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        return try nativeFrame(raw as! AXUIElement)
    }

    private func nativeFrame(_ element: AXUIElement) throws -> CGRect? {
        guard let value = try attribute(element, "AXFrame") else { return nil }
        var frame = CGRect.zero
        guard CFGetTypeID(value) == AXValueGetTypeID(), AXValueGetValue(value as! AXValue, .cgRect, &frame),
              [frame.minX, frame.minY, frame.width, frame.height].allSatisfy({ $0.isFinite }), frame.width >= 0, frame.height >= 0 else {
            throw DesktopFailure(code: "invalid_ax_geometry", message: "Native AXFrame is not a finite accessibility rectangle")
        }
        return frame.isEmpty ? nil : frame
    }

    private func parentElement(_ element: AXUIElement) throws -> AXUIElement? {
        guard let value = try attribute(element, kAXParentAttribute) else { return nil }
        guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
            throw DesktopFailure(code: "invalid_ax_parent", message: "Native accessibility parent is not an element")
        }
        return value as! AXUIElement
    }

    private func ancestorElements(_ element: AXUIElement) throws -> [AXUIElement] {
        var result: [AXUIElement] = []
        var current: AXUIElement? = element
        while let next = current {
            guard result.count < 300, !result.contains(where: { CFEqual($0, next) }) else {
                throw DesktopFailure(code: "ax_ancestry_limit", message: "Native accessibility ancestry is cyclic or exceeds its bound")
            }
            result.append(next)
            if try string(next, kAXRoleAttribute) == kAXApplicationRole as String { break }
            current = try parentElement(next)
        }
        return result
    }

    private func nativeActions(_ element: AXUIElement) throws -> [String] {
        try timeout(element)
        var raw: CFArray?
        let result = AXUIElementCopyActionNames(element, &raw)
        if result == .actionUnsupported || result == .notImplemented { return [] }
        guard result == .success, let names = raw as? [String], names.count <= 128,
              names.allSatisfy({ !$0.isEmpty && $0.utf8.count <= 2048 }) else {
            throw DesktopFailure(code: "ax_read_failed", message: "Native action list is unavailable or exceeds its bound")
        }
        return names
    }

    private func scrollContainer(_ element: AXUIElement, role: String) throws -> Bool {
        if role == kAXScrollAreaRole as String { return true }
        guard role == kAXGroupRole as String else { return false }
        let actions = try nativeActions(element)
        return ["AXScrollLeftByPage", "AXScrollRightByPage", "AXScrollUpByPage", "AXScrollDownByPage"].contains { actions.contains($0) }
    }

    private func windowBoundary(_ role: String) -> Bool {
        [kAXWindowRole as String, kAXSheetRole as String, kAXMenuRole as String, "AXPopover"].contains(role)
    }

    private func clippedPoint(_ element: AXUIElement, frame: CGRect) throws -> CGPoint? {
        var clip: CGRect?
        for ancestor in try ancestorElements(element) {
            guard let role = try string(ancestor, kAXRoleAttribute) else { return nil }
            if role == kAXApplicationRole as String { break }
            guard let bounds = try nativeFrame(ancestor) else { return nil }
            if try windowBoundary(role) || scrollContainer(ancestor, role: role) {
                clip = clip.map { $0.intersection(bounds) } ?? bounds
                if clip?.isEmpty == true || clip?.isNull == true { return nil }
            }
            if windowBoundary(role) { break }
        }
        let center = CGPoint(x: frame.midX, y: frame.midY)
        guard let clip, clip.contains(center) else { return nil }
        return center
    }

    private func insideWebArea(_ element: AXUIElement) throws -> Bool {
        for ancestor in try ancestorElements(element) {
            if try string(ancestor, kAXRoleAttribute) == "AXWebArea" { return true }
        }
        return false
    }

    private func hitBelongsToElement(_ element: AXUIElement, app: DesktopAppRef, at point: CGPoint) throws -> Bool {
        let root = AXUIElementCreateApplication(try applications.application(app).processIdentifier)
        try timeout(root)
        var hit: AXUIElement?
        let result = AXUIElementCopyElementAtPosition(root, Float(point.x), Float(point.y), &hit)
        if result == .noValue || result == .notImplemented { return false }
        guard result == .success, let hit else {
            throw DesktopFailure(code: "ax_hit_test_failed", message: "Native application hit test failed (\(result.rawValue))")
        }
        return try ancestorElements(hit).contains { CFEqual($0, element) }
    }

    private enum ScrollPreparation { case action(AXUIElement), attribute(AXUIElement), area(AXUIElement) }

    private func scrollToVisible(_ element: AXUIElement, frame: CGRect, guardValue: DesktopAXGuard,
                                 evidence: DesktopAXPreparationEvidence, check: @escaping () throws -> Void) async throws {
        var operations: [ScrollPreparation] = []
        var handled = false
        for ancestor in try ancestorElements(element) {
            guard let role = try string(ancestor, kAXRoleAttribute) else {
                throw DesktopFailure(code: "ax_role_unavailable", message: "Scroll preparation requires exact native roles")
            }
            if windowBoundary(role) || role == kAXApplicationRole as String { break }
            if !handled {
                if try nativeActions(ancestor).contains("AXScrollToVisible") {
                    operations.append(.action(ancestor)); handled = true
                } else if role == kAXCellRole as String, try writable(ancestor, name: "AXScrollToVisible") {
                    operations.append(.attribute(ancestor)); handled = true
                }
            }
            if role == kAXScrollAreaRole as String, !handled { operations.append(.area(ancestor)) }
            if try scrollContainer(ancestor, role: role) { handled = false }
        }
        let before = evidence.effects.count
        for operation in operations {
            try check()
            switch operation {
            case .action(let target):
                try preparationEffect(target, guardValue: guardValue, name: "AXScrollToVisible", value: nil, action: true, evidence: evidence)
            case .attribute(let target):
                try preparationEffect(target, guardValue: guardValue, name: "AXScrollToVisible", value: nil, action: false, evidence: evidence)
            case .area(let target):
                try scrollArea(target, target: frame, guardValue: guardValue, evidence: evidence)
            }
        }
        if evidence.effects.count > before {
            let deadline = ContinuousClock.now + .milliseconds(500)
            while let current = try nativeFrame(element), try clippedPoint(element, frame: current) == nil,
                  ContinuousClock.now < deadline {
                try await Task.sleep(for: .milliseconds(100))
                try check()
            }
        }
        try check()
    }

    private func writable(_ element: AXUIElement, name: String) throws -> Bool {
        try timeout(element)
        var writable: DarwinBoolean = false
        let status = AXUIElementIsAttributeSettable(element, name as CFString, &writable)
        if status == .attributeUnsupported { return false }
        guard status == .success else { throw DesktopFailure(code: "ax_read_failed", message: "Cannot inspect native writable attribute (\(status.rawValue))") }
        return writable.boolValue
    }

    private func preparationEffect(_ element: AXUIElement, guardValue: DesktopAXGuard, name: String, value: Double?,
                                   action: Bool, evidence: DesktopAXPreparationEvidence) throws {
        guard evidence.effects.count < 600 else { throw DesktopFailure(code: "ax_effect_limit", message: "Native scroll preparation exceeds its finite effect bound") }
        try timeout(element)
        let affected = snapshots[guardValue.app.app_instance_id]?.entries.first { CFEqual($0.value.element, element) }?.key
        evidence.effects.append(DesktopAXPreparationEffect(source_element_id: guardValue.element_id, affected_element_id: affected,
            mechanism: action ? "ax_action" : "ax_attribute", name: name, value: value, dispatch_state: "attempted", native_error_code: nil))
        let index = evidence.effects.count - 1
        let status: AXError
        if action { status = AXUIElementPerformAction(element, name as CFString) }
        else {
            let payload: CFTypeRef = value.map { NSNumber(value: $0) as CFTypeRef } ?? kCFBooleanTrue
            status = AXUIElementSetAttributeValue(element, name as CFString, payload)
        }
        evidence.effects[index].dispatch_state = "returned"
        evidence.effects[index].native_error_code = status.rawValue
        try effectResult(status)
        try cancellationCheck?()
    }

    private func contentBounds(_ scrollArea: AXUIElement) throws -> CGRect? {
        var omissions: [DesktopAXOmission] = []
        var pending = try children(scrollArea, remaining: maxNodes, omissions: &omissions, parentID: nil)
        var visited: [CFHashCode: [AXUIElement]] = [:]
        var count = 0
        var result: CGRect?
        while let element = pending.popLast() {
            guard count < maxNodes, omissions.isEmpty else { throw DesktopFailure(code: "ax_geometry_limit", message: "Scroll content bounds require a complete bounded native subtree") }
            let hash = CFHash(element)
            if visited[hash]?.contains(where: { CFEqual($0, element) }) == true { continue }
            visited[hash, default: []].append(element); count += 1
            guard let role = try string(element, kAXRoleAttribute) else { throw DesktopFailure(code: "ax_role_unavailable", message: "Scroll content element has no native role") }
            if role == kAXScrollBarRole as String { continue }
            let bounds: CGRect?
            if let frame = try nativeFrame(element) { bounds = frame }
            else if let frame = try rectangle(element) { bounds = CGRect(x: frame.x, y: frame.y, width: frame.width, height: frame.height) }
            else { bounds = nil }
            if let bounds, !bounds.isEmpty { result = result.map { $0.union(bounds) } ?? bounds }
            if try scrollContainer(element, role: role) { continue }
            pending += try children(element, remaining: maxNodes - count, omissions: &omissions, parentID: nil)
        }
        guard omissions.isEmpty else { throw DesktopFailure(code: "ax_geometry_limit", message: "Native scroll content enumeration was incomplete") }
        return result
    }

    private func scrollArea(_ area: AXUIElement, target: CGRect, guardValue: DesktopAXGuard, evidence: DesktopAXPreparationEvidence) throws {
        guard let viewport = try nativeFrame(area), !viewport.contains(target), let content = try contentBounds(area) else { return }
        for vertical in [true, false] {
            guard let raw = try attribute(area, vertical ? kAXVerticalScrollBarAttribute : kAXHorizontalScrollBarAttribute) else { continue }
            guard CFGetTypeID(raw) == AXUIElementGetTypeID() else { throw DesktopFailure(code: "invalid_ax_scrollbar", message: "Native scrollbar is not an accessibility element") }
            let bar = raw as! AXUIElement
            guard try writable(bar, name: kAXValueAttribute) else { continue }
            let minimum = vertical ? viewport.minY : viewport.minX
            let maximum = vertical ? viewport.maxY : viewport.maxX
            let contentMinimum = vertical ? content.minY : content.minX
            let extent = (vertical ? content.height : content.width) - (maximum - minimum)
            let targetMinimum = vertical ? target.minY : target.minX
            let targetMaximum = vertical ? target.maxY : target.maxX
            guard extent > 0, targetMinimum < minimum || targetMaximum > maximum else { continue }
            let delta: CGFloat
            if targetMaximum - targetMinimum > maximum - minimum {
                delta = targetMinimum < minimum ? targetMinimum - minimum : targetMaximum - maximum
            } else { delta = ((targetMinimum + targetMaximum) - (minimum + maximum)) / 2 }
            let offset: CGFloat
            if let value = try attribute(bar, kAXValueAttribute) {
                guard CFGetTypeID(value) == CFNumberGetTypeID(), let number = value as? NSNumber, number.doubleValue.isFinite else {
                    throw DesktopFailure(code: "invalid_ax_scrollbar", message: "Native scrollbar value is not finite")
                }
                offset = min(1, max(0, CGFloat(number.doubleValue))) * extent
            } else { offset = min(extent, max(0, minimum - contentMinimum)) }
            let next = Double(min(extent, max(0, offset + delta)) / extent)
            try preparationEffect(bar, guardValue: guardValue, name: kAXValueAttribute, value: next, action: false, evidence: evidence)
        }
    }

    static func selectionRange(value: String, text: String, prefix: String?, suffix: String?, selectionType: String) throws -> NSRange {
        guard !text.isEmpty, [value, text, prefix ?? "", suffix ?? ""].allSatisfy({ $0.utf16.count <= 65_536 }),
              ["text", "cursor_before", "cursor_after"].contains(selectionType) else {
            throw DesktopFailure(code: "invalid_selection", message: "Selection needs bounded exact text and a supported selection type")
        }
        let source = value as NSString
        let before = (prefix ?? "") as NSString
        let after = (suffix ?? "") as NSString
        var candidates: [NSRange] = []
        var offset = 0
        while offset < source.length {
            let found = source.range(of: text, options: [], range: NSRange(location: offset, length: source.length - offset))
            if found.location == NSNotFound { break }
            let end = NSMaxRange(found)
            if found.location >= before.length, source.length - end >= after.length,
               source.substring(with: NSRange(location: found.location - before.length, length: before.length)) == (prefix ?? ""),
               source.substring(with: NSRange(location: end, length: after.length)) == (suffix ?? "") {
                candidates.append(found)
                if candidates.count > 1 { break }
            }
            offset = found.location + 1
        }
        guard candidates.count == 1 else {
            throw DesktopFailure(code: candidates.isEmpty ? "text_not_found" : "ambiguous_text", message: "Text and context must identify exactly one character range")
        }
        let found = candidates[0]
        if selectionType == "cursor_before" { return NSRange(location: found.location, length: 0) }
        if selectionType == "cursor_after" { return NSRange(location: NSMaxRange(found), length: 0) }
        return found
    }

    func raiseWindow(_ app: DesktopAppRef, windowID: CGWindowID, check: @escaping () throws -> Void) throws {
        try begin(check); defer { end() }
        try requireReady(); try check()
        if windowSPI == nil { windowSPI = try DesktopAppInputSPI() }
        guard let windowSPI else { throw DesktopFailure(code: "pip_focus_unavailable", message: "Native window identity is unavailable") }
        let application = try applications.application(app)
        let root = AXUIElementCreateApplication(application.processIdentifier)
        try timeout(root)
        var raw: CFTypeRef?
        let status = AXUIElementCopyAttributeValue(root, kAXWindowsAttribute as CFString, &raw)
        guard status == .success, let windows = raw as? [AXUIElement], windows.count <= 256 else {
            throw DesktopFailure(code: "pip_focus_unavailable", message: "Application does not expose its exact native windows for the user focus action")
        }
        var target: AXUIElement?
        for window in windows {
            var pid: pid_t = 0
            guard AXUIElementGetPid(window, &pid) == .success, pid == application.processIdentifier else { continue }
            if try windowSPI.windowID(window) == windowID { target = window; break }
        }
        guard let target else { throw DesktopFailure(code: "pip_focus_unavailable", message: "The exact native source window is no longer accessible") }
        try check()
        guard application.activate(options: []) else {
            throw DesktopFailure(code: "pip_focus_unavailable", message: "The application rejected the explicit user focus action")
        }
        try check()
        let raised = AXUIElementPerformAction(target, kAXRaiseAction as CFString)
        guard raised == .success else { throw DesktopFailure(code: "pip_focus_unavailable", message: "The exact native source window could not be raised") }
    }

    func inputWindow(_ app: DesktopAppRef, observationID: String, revision: UInt64,
                     windowID: CGWindowID, bounds: CGRect, check: @escaping () throws -> Void) throws -> DesktopAppInputWindow {
        try begin(check); defer { end() }
        try requireReady()
        let application = try applications.application(app)
        guard let snapshot = snapshots[app.app_instance_id], !snapshot.consumed,
              snapshot.observation.app == app, snapshot.observation.observation_id == observationID,
              snapshot.observation.revision == revision, windowID != 0,
              bounds.width > 0, bounds.height > 0,
              [bounds.minX, bounds.minY, bounds.width, bounds.height].allSatisfy({ $0.isFinite }) else {
            throw DesktopFailure(code: "stale_observation", message: "Native input target requires the exact current application observation and window geometry")
        }
        try timeout(snapshot.root)
        var count: CFIndex = 0
        let counted = AXUIElementGetAttributeValueCount(snapshot.root, kAXWindowsAttribute as CFString, &count)
        guard counted == .success, count > 0, count <= 256 else {
            throw DesktopFailure(code: "ax_window_unavailable", message: "The retained application does not expose a bounded exact native window list (\(counted.rawValue))")
        }
        var values: CFArray?
        try timeout(snapshot.root)
        let read = AXUIElementCopyAttributeValues(snapshot.root, kAXWindowsAttribute as CFString, 0, count, &values)
        guard read == .success, let values, CFArrayGetCount(values) == count else {
            throw DesktopFailure(code: "ax_window_changed", message: "The retained application's native window list changed during enumeration")
        }
        if windowSPI == nil { windowSPI = try DesktopAppInputSPI() }
        guard let windowSPI else { throw DesktopFailure(code: "app_input_unavailable", message: "Native window identity is unavailable") }
        var selected: AXUIElement?
        for value in values as [AnyObject] {
            try check()
            guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
                throw DesktopFailure(code: "invalid_ax_window", message: "Application window list contains a non-element value")
            }
            let element = value as! AXUIElement
            try timeout(element)
            var pid: pid_t = 0
            guard AXUIElementGetPid(element, &pid) == .success, pid == application.processIdentifier,
                  try string(element, kAXRoleAttribute) == kAXWindowRole as String else {
                throw DesktopFailure(code: "ax_window_changed", message: "Native input window no longer belongs to the exact retained application")
            }
            guard try windowSPI.windowID(element) == windowID else { continue }
            guard selected == nil else {
                throw DesktopFailure(code: "ambiguous_ax_window", message: "Multiple native accessibility windows claim the exact capture window")
            }
            selected = element
        }
        try check()
        guard try applications.application(app).isEqual(application), let selected else {
            throw DesktopFailure(code: "ax_window_unavailable", message: "The exact captured window is absent from its retained application's accessibility windows")
        }
        guard let frame = try rectangle(selected), frame.width > 0, frame.height > 0 else {
            throw DesktopFailure(code: "ax_window_unavailable", message: "The exact accessibility window has no current native coordinate frame")
        }
        try check()
        return DesktopAppInputWindow(app: app, element: selected, windowID: windowID,
            bounds: CGRect(x: frame.x, y: frame.y, width: frame.width, height: frame.height), flipped: false)
    }

    func presentationInventory(_ app: DesktopAppRef, windowIDs: Set<UInt32>, additional: [AXUIElement],
                               check: @escaping () throws -> Void) throws -> DesktopAXPresentationInventory {
        try requireReady(); try check()
        let deadline = Date().addingTimeInterval(5)
        var destroyedElements: [AXUIElement] = []
        func timeout(_ element: AXUIElement) throws {
            try check()
            let remaining = deadline.timeIntervalSinceNow
            guard remaining > 0 else { throw DesktopFailure(code: "ax_deadline", message: "Native presentation window observation exceeded its deadline") }
            let status = AXUIElementSetMessagingTimeout(element, Float(min(0.5, remaining)))
            guard status == .success else {
                throw DesktopFailure(code: "ax_timeout_configuration_failed", message: "Cannot bound native presentation window observation (\(status.rawValue))")
            }
        }
        func attribute(_ element: AXUIElement, _ name: String) throws -> CFTypeRef? {
            try timeout(element)
            var value: CFTypeRef?
            let status = AXUIElementCopyAttributeValue(element, name as CFString, &value)
            try check()
            if status == .invalidUIElement {
                if !destroyedElements.contains(where: { CFEqual($0, element) }) { destroyedElements.append(element) }
                return nil
            }
            if status == .attributeUnsupported || status == .noValue { return nil }
            guard status == .success else {
                throw DesktopFailure(code: "ax_read_failed", message: "Cannot read native presentation window attribute \(name): \(status.rawValue)")
            }
            return value
        }
        func string(_ element: AXUIElement, _ name: String) throws -> String? {
            guard let value = try attribute(element, name) else { return nil }
            guard CFGetTypeID(value) == CFStringGetTypeID() else {
                throw DesktopFailure(code: "invalid_ax_window", message: "Native presentation window attribute is not text")
            }
            let result = value as! String
            guard result.utf16.count <= 2048 else {
                throw DesktopFailure(code: "ax_text_limit", message: "Native presentation window attribute exceeds its bound")
            }
            return result
        }
        let application = try applications.application(app)
        let root = AXUIElementCreateApplication(application.processIdentifier)
        if windowSPI == nil { windowSPI = try DesktopAppInputSPI() }
        guard let windowSPI, windowIDs.count <= 256, additional.count <= 256 else {
            throw DesktopFailure(code: "app_window_inventory_limit", message: "Native presentation window inventory exceeds its exact application bound")
        }
        func elements(_ element: AXUIElement, _ name: String) throws -> [AXUIElement] {
            guard let raw = try attribute(element, name) else { return [] }
            guard CFGetTypeID(raw) == CFArrayGetTypeID(), let array = raw as? [CFTypeRef], array.count <= 256 else {
                throw DesktopFailure(code: "invalid_ax_windows", message: "Native presentation window list is malformed or exceeds its bound")
            }
            return try array.map { value in
                guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
                    throw DesktopFailure(code: "invalid_ax_windows", message: "Native window list contains a non-element value")
                }
                return value as! AXUIElement
            }
        }
        var candidates = try elements(root, kAXWindowsAttribute) + elements(root, kAXChildrenAttribute) + additional
        var unique: [AXUIElement] = []
        var index = 0
        while index < candidates.count {
            try check()
            let element = candidates[index]; index += 1
            guard !unique.contains(where: { CFEqual($0, element) }) else { continue }
            guard unique.count < 256 else {
                throw DesktopFailure(code: "app_window_inventory_limit", message: "Native presentation window graph exceeds its bound")
            }
            unique.append(element)
            if try string(element, kAXRoleAttribute) == kAXWindowRole {
                for child in try elements(element, kAXChildrenAttribute) where try string(child, kAXRoleAttribute) == kAXSheetRole {
                    guard candidates.count < 4096 else {
                        throw DesktopFailure(code: "app_window_inventory_limit", message: "Native presentation window traversal exceeds its bound")
                    }
                    candidates.append(child)
                }
            }
        }
        var result: [UInt32: DesktopAXPresentationWindow] = [:]
        for element in unique {
            try check(); try timeout(element)
            var pid: pid_t = 0
            guard AXUIElementGetPid(element, &pid) == .success, pid == application.processIdentifier else { continue }
            let id: UInt32
            do { id = try windowSPI.windowID(element) }
            catch let failure as DesktopFailure where failure.code == "app_window_unavailable" { continue }
            guard windowIDs.contains(id), let role = try string(element, kAXRoleAttribute) else { continue }
            if let previous = result[id] {
                guard CFEqual(previous.element, element) else {
                    throw DesktopFailure(code: "ambiguous_ax_window", message: "Different native accessibility elements claim one presentation window")
                }
                continue
            }
            let subrole = try string(element, kAXSubroleAttribute)
            let rawModal = try attribute(element, kAXModalAttribute)
            guard rawModal == nil || CFGetTypeID(rawModal!) == CFBooleanGetTypeID() else {
                throw DesktopFailure(code: "invalid_ax_window", message: "Native window modal state is not boolean")
            }
            var parent: UInt32?
            var chain: [AXUIElement] = [element]
            var current = element
            for _ in 0..<300 {
                guard let raw = try attribute(current, kAXParentAttribute) else { break }
                guard CFGetTypeID(raw) == AXUIElementGetTypeID() else {
                    throw DesktopFailure(code: "invalid_ax_parent", message: "Native presentation window parent is not an accessibility element")
                }
                let next = raw as! AXUIElement
                guard !chain.contains(where: { CFEqual($0, next) }) else {
                    throw DesktopFailure(code: "invalid_ax_parent", message: "Native presentation window ancestry contains a cycle")
                }
                try timeout(next)
                var owner: pid_t = 0
                guard AXUIElementGetPid(next, &owner) == .success, owner == application.processIdentifier else { break }
                if try string(next, kAXRoleAttribute) == kAXWindowRole {
                    do {
                        let candidate = try windowSPI.windowID(next)
                        if candidate != id, windowIDs.contains(candidate) { parent = candidate }
                    } catch let failure as DesktopFailure where failure.code == "app_window_unavailable" {}
                    break
                }
                chain.append(next); current = next
            }
            result[id] = DesktopAXPresentationWindow(element: element, id: id, role: role, subrole: subrole,
                modal: (rawModal as? NSNumber)?.boolValue ?? false, parent: parent)
        }
        var main: UInt32?
        if let raw = try attribute(root, kAXMainWindowAttribute) {
            guard CFGetTypeID(raw) == AXUIElementGetTypeID() else {
                throw DesktopFailure(code: "invalid_ax_window", message: "Native main window is not an accessibility element")
            }
            let element = raw as! AXUIElement
            try timeout(element)
            var pid: pid_t = 0
            if AXUIElementGetPid(element, &pid) == .success, pid == application.processIdentifier {
                do { let id = try windowSPI.windowID(element); if result[id] != nil { main = id } }
                catch let failure as DesktopFailure where failure.code == "app_window_unavailable" {}
            }
        }
        try check()
        guard try applications.application(app).isEqual(application) else {
            throw DesktopFailure(code: "app_instance_unavailable", message: "Native presentation window owner changed during observation")
        }
        return DesktopAXPresentationInventory(windows: result, mainWindow: main, destroyedElements: destroyedElements)
    }

    func hasOpenMenu(_ app: DesktopAppRef, check: @escaping () throws -> Void) throws -> Bool {
        try requireReady(); try check()
        let application = try applications.application(app)
        _ = application
        guard let observer = menuObservers[app] else { throw DesktopFailure(code: "menu_observer_unavailable", message: "Native application menu observation is unavailable") }
        try check()
        return try observer.hasOpenMenu()
    }

    func consumeObservation(_ app: DesktopAppRef, observationID: String, revision: UInt64) throws {
        try requireReady()
        guard let snapshot = snapshots[app.app_instance_id], !snapshot.consumed,
              snapshot.observation.app == app, snapshot.observation.observation_id == observationID,
              snapshot.observation.revision == revision else {
            throw DesktopFailure(code: "stale_observation", message: "Native input cannot consume a different application observation")
        }
        snapshots[app.app_instance_id]?.consumed = true
    }

    func release(_ app: DesktopAppRef) {
        menuObservers.removeValue(forKey: app)?.close()
        snapshots.removeValue(forKey: app.app_instance_id); pendingObservations.removeValue(forKey: app.app_instance_id); revisions.removeValue(forKey: app.app_instance_id)
    }
    func close() {
        for observer in menuObservers.values { observer.close() }
        menuObservers.removeAll(); snapshots.removeAll(); pendingObservations.removeAll(); revisions.removeAll(); closed = true
    }

    private func validated(_ guardValue: DesktopAXGuard, preparedID: UUID?, check: () throws -> Void) throws -> Entry {
        try requireReady(); try check()
        _ = try applications.application(guardValue.app)
        guard let snapshot = snapshots[guardValue.app.app_instance_id],
              preparedID == nil ? !snapshot.consumed : snapshot.consumed && snapshot.preparedID == preparedID,
              snapshot.observation.app == guardValue.app,
              snapshot.observation.observation_id == guardValue.observation_id, snapshot.observation.revision == guardValue.revision,
              let entry = snapshot.entries[guardValue.element_id], entry.visible,
              let menuObserver = menuObservers[guardValue.app], menuObserver.generation == snapshot.menuGeneration else {
            throw DesktopFailure(code: "stale_observation", message: "Element does not belong to the exact current observation or its prepared native action")
        }
        var current = entry
        var depth = 0
        while let parent = current.parent {
            guard depth < maxDepth, let parentID = current.node.parent_id, let parentEntry = snapshot.entries[parentID], CFEqual(parentEntry.element, parent) else {
                throw DesktopFailure(code: "stale_element", message: "Observed accessibility ancestry is unavailable")
            }
            let liveParent = try attribute(current.element, kAXParentAttribute)
            guard let liveParent, CFGetTypeID(liveParent) == AXUIElementGetTypeID(), CFEqual(liveParent, parent) else {
                throw DesktopFailure(code: "stale_element", message: "Accessibility element changed its owning hierarchy")
            }
            current = parentEntry; depth += 1
        }
        guard CFEqual(current.element, snapshot.root) || snapshot.menuBar.map({ CFEqual($0, current.element) }) == true else {
            throw DesktopFailure(code: "stale_element", message: "Accessibility ancestry no longer reaches its exact observed native root")
        }
        try menuObserver.check()
        let (now, rawValue) = try readNode(entry.element, id: entry.node.element_id, parentID: entry.node.parent_id, previous: entry.node,
            windows: snapshot.windows, processID: snapshot.processID)
        let unchanged: Bool
        if preparedID == nil { unchanged = now == entry.node }
        else {
            let observed = entry.node
            unchanged = now.element_id == observed.element_id && now.parent_id == observed.parent_id &&
                now.window_id == observed.window_id && now.window_unavailable_reason == observed.window_unavailable_reason &&
                now.role == observed.role && now.subrole == observed.subrole && now.title == observed.title &&
                now.description == observed.description && now.value == observed.value && now.value_type == observed.value_type &&
                now.value_input == observed.value_input && now.enabled == observed.enabled && now.settable_attributes == observed.settable_attributes &&
                now.actions == observed.actions && now.unavailable_attributes.filter { $0 != kAXFocusedAttribute } == observed.unavailable_attributes.filter { $0 != kAXFocusedAttribute }
        }
        let sameValue: Bool
        switch (entry.rawValue, rawValue) {
        case (nil,nil): sameValue = true
        case (let old?, let new?): sameValue = CFEqual(old, new)
        default: sameValue = false
        }
        guard unchanged && sameValue else { throw DesktopFailure(code: "stale_observation", message: "Accessibility element changed after observation") }
        if entry.attributed != nil { try validateAttributed(entry) }
        return entry
    }

    private func validateWindow(_ entry: Entry, windowID: CGWindowID?) throws {
        guard let windowID else {
            guard entry.node.window_id == nil,
                  entry.node.window_unavailable_reason == "ax_window_attribute_unavailable",
                  entry.node.role != kAXWindowRole as String,
                  try attribute(entry.element, kAXWindowAttribute) == nil else {
                throw DesktopFailure(code: "ax_window_changed", message: "Windowless action requires native AXWindow absence on the exact observed element")
            }
            return
        }
        guard entry.node.window_id != nil, entry.node.window_unavailable_reason == nil else {
            throw DesktopFailure(code: "ax_window_unavailable", message: "The observed element has no exact application-window association")
        }
        let element: AXUIElement
        if entry.node.role == kAXWindowRole as String { element = entry.element }
        else {
            guard let raw = try attribute(entry.element, kAXWindowAttribute), CFGetTypeID(raw) == AXUIElementGetTypeID() else {
                throw DesktopFailure(code: "ax_window_unavailable", message: "The exact element has no native accessibility window identity")
            }
            element = raw as! AXUIElement
        }
        try timeout(element)
        if windowSPI == nil { windowSPI = try DesktopAppInputSPI() }
        guard let windowSPI, try windowSPI.windowID(element) == windowID else {
            throw DesktopFailure(code: "ax_window_changed", message: "The exact accessibility element does not belong to the observed native window")
        }
    }

    private func requireReady() throws {
        guard !closed else { throw DesktopFailure(code: "app_scope_closed", message: "Application accessibility scope was released") }
        guard AXIsProcessTrusted() else { throw DesktopFailure(code: "accessibility_permission_required", message: "Accessibility permission is required for app observations and actions") }
    }

    private func timeout(_ element: AXUIElement) throws {
        try cancellationCheck?()
        let remaining = callDeadline?.timeIntervalSinceNow ?? 0
        guard remaining > 0 else { throw DesktopFailure(code: "ax_deadline", message: "Accessibility operation exceeded its native deadline") }
        let result = AXUIElementSetMessagingTimeout(element, Float(min(0.5, remaining)))
        guard result == .success else { throw DesktopFailure(code: "ax_timeout_configuration_failed", message: "Cannot bound native accessibility messaging (\(result.rawValue))") }
    }

    private func begin(_ check: @escaping () throws -> Void) throws {
        guard cancellationCheck == nil else { throw DesktopFailure(code: "app_operation_busy", message: "An accessibility operation is already active") }
        try check()
        cancellationCheck = check
        callDeadline = Date().addingTimeInterval(5)
    }

    private func end() { cancellationCheck = nil; callDeadline = nil }

    private func effectResult(_ result: AXError) throws {
        guard result == .success else {
            throw DesktopAXDispatchFailure(native_error_code: result.rawValue)
        }
    }

    private func attribute(_ element: AXUIElement, _ name: String) throws -> CFTypeRef? {
        try timeout(element)
        var result: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &result)
        if error == .attributeUnsupported || error == .noValue { return nil }
        guard error == .success else { throw DesktopFailure(code: "ax_read_failed", message: "Cannot read \(name): \(error.rawValue)") }
        return result
    }

    private func string(_ element: AXUIElement, _ name: String) throws -> String? {
        guard let value = try attribute(element, name) else { return nil }
        guard CFGetTypeID(value) == CFStringGetTypeID() else { return nil }
        let text = value as! String
        guard text.utf16.count <= maxText else { throw DesktopFailure(code: "ax_text_limit", message: "Accessibility text exceeds its exact-value bound") }
        return text
    }

    private func rectangle(_ element: AXUIElement) throws -> DesktopAppRect? {
        let position = try attribute(element, kAXPositionAttribute)
        let size = try attribute(element, kAXSizeAttribute)
        var point = CGPoint.zero; var dimensions = CGSize.zero
        guard let position, let size, CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID(),
              AXValueGetValue(position as! AXValue, .cgPoint, &point), AXValueGetValue(size as! AXValue, .cgSize, &dimensions) else {
            return nil
        }
        return try DesktopAppRect(CGRect(origin: point, size: dimensions))
    }

    private func observedWindow(_ element: AXUIElement, role: String, windows: [CGWindowID: String], processID: pid_t) throws -> (String?, String?) {
        let window: AXUIElement
        if role == kAXWindowRole as String { window = element }
        else {
            guard let value = try attribute(element, kAXWindowAttribute) else { return (nil, "ax_window_attribute_unavailable") }
            guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
                throw DesktopFailure(code: "invalid_ax_window", message: "Native AXWindow attribute is not an accessibility element")
            }
            window = value as! AXUIElement
        }
        try timeout(window)
        var pid: pid_t = 0
        guard AXUIElementGetPid(window, &pid) == .success, pid == processID else { return (nil, "ax_window_owner_unavailable") }
        if windowSPI == nil { windowSPI = try DesktopAppInputSPI() }
        guard let windowSPI else { throw DesktopFailure(code: "app_window_unavailable", message: "Native window identity function is unavailable") }
        let nativeID: CGWindowID
        do { nativeID = try windowSPI.windowID(window) }
        catch let error as DesktopFailure where error.code == "app_window_unavailable" { return (nil, "native_window_id_unavailable") }
        try cancellationCheck?()
        guard let id = windows[nativeID] else { return (nil, "native_window_not_in_inventory") }
        return (id, nil)
    }

    private func writable(_ element: AXUIElement, _ name: String) throws -> Bool {
        try timeout(element)
        var value: DarwinBoolean = false
        let error = AXUIElementIsAttributeSettable(element, name as CFString, &value)
        if error == .attributeUnsupported || error == .noValue { return false }
        guard error == .success else { throw DesktopFailure(code: "ax_read_failed", message: "Cannot inspect native display attribute: \(error.rawValue)") }
        return value.boolValue
    }

    private func attributedString(_ element: AXUIElement, range: NSRange) throws -> NSAttributedString? {
        guard range.location >= 0, range.length >= 0, range.length <= maxText else {
            throw DesktopFailure(code: "invalid_ax_attributed_range", message: "Native attributed range exceeds its retained source")
        }
        var nativeRange = CFRange(location: range.location, length: range.length)
        guard let parameter = AXValueCreate(.cfRange, &nativeRange) else { throw DesktopFailure(code: "invalid_ax_attributed_range", message: "Cannot encode native character range") }
        try timeout(element)
        var result: CFTypeRef?
        let error = AXUIElementCopyParameterizedAttributeValue(element, kAXAttributedStringForRangeParameterizedAttribute as CFString, parameter, &result)
        if error == .attributeUnsupported || error == .parameterizedAttributeUnsupported || error == .noValue || error == .notImplemented { return nil }
        guard error == .success, let value = result as? NSAttributedString, value.length == range.length else {
            throw DesktopFailure(code: "invalid_ax_attributed_result", message: "Native attributed result does not match its requested range: \(error.rawValue)")
        }
        return NSAttributedString(attributedString: value)
    }

    private func validateAttributed(_ entry: Entry) throws {
        guard let retained = entry.attributed else { return }
        guard let current = try attributedString(entry.element, range: retained.range), current.isEqual(to: retained.attributed) else {
            throw DesktopFailure(code: "stale_attributed_text", message: "Native attributed source changed after its mapped observation")
        }
    }

    private func displayFacts(_ element: AXUIElement, node: DesktopAXNode, rawValue: CFTypeRef?) throws -> (DesktopAXDisplayFacts, [AXUIElement]) {
        func boolean(_ name: String) throws -> Bool? {
            guard let raw = try attribute(element, name) else { return nil }
            guard CFGetTypeID(raw) == CFBooleanGetTypeID() else { throw DesktopFailure(code: "invalid_ax_display_value", message: "Native display flag is not boolean") }
            return (raw as! NSNumber).boolValue
        }
        func strings(_ name: String) throws -> [String] {
            guard let raw = try attribute(element, name) else { return [] }
            guard let values = raw as? [String], values.count <= maxNodes, values.allSatisfy({ $0.utf16.count <= maxText }) else {
                throw DesktopFailure(code: "invalid_ax_display_value", message: "Native display string list exceeds its typed bound")
            }
            return values
        }
        func range(_ name: String) throws -> Range<Int>? {
            guard let raw = try attribute(element, name) else { return nil }
            guard CFGetTypeID(raw) == AXValueGetTypeID(), AXValueGetType(raw as! AXValue) == .cfRange else {
                throw DesktopFailure(code: "invalid_ax_display_range", message: "Native display range has the wrong type")
            }
            var value = CFRange()
            guard AXValueGetValue(raw as! AXValue, .cfRange, &value), value.location >= 0, value.length >= 0,
                  value.location <= Int.max - value.length else { throw DesktopFailure(code: "invalid_ax_display_range", message: "Native display range overflows") }
            return value.location..<(value.location + value.length)
        }
        let selectedAttribute = try boolean(kAXSelectedAttribute)
        var selected = selectedAttribute ?? false
        if selectedAttribute == nil, let traits = try attribute(element, "AXTraits") {
            guard let number = traits as? NSNumber, CFGetTypeID(traits) == CFNumberGetTypeID(), number.doubleValue.isFinite,
                  number.doubleValue >= 0, number.doubleValue.rounded(.towardZero) == number.doubleValue,
                  number.doubleValue < 18_446_744_073_709_551_616 else { throw DesktopFailure(code: "invalid_ax_traits", message: "Native AXTraits is not an unsigned mask") }
            selected = number.uint64Value & 8 != 0
        }
        let selectable = try selectedAttribute != nil && !["AXMenuItem", "AXMenuBarItem"].contains(node.role) && writable(element, kAXSelectedAttribute)
        let focusable = try node.focused != nil && writable(element, kAXFocusedAttribute)
        var titles: [AXUIElement] = []
        if let raw = try attribute(element, "AXServesAsTitleForUIElements") {
            guard let values = raw as? [AXUIElement], values.count <= maxNodes else { throw DesktopFailure(code: "invalid_ax_title_targets", message: "Native title targets are not a bounded element list") }
            titles = values
        }
        let attributed: DesktopAXAttributedInput?
        if node.role == "AXTextArea", node.value_type == "string", try string(element, "AXTextualContext") != "AXTextualContextSourceCode",
           let source = node.value, let value = try attributedString(element, range: NSRange(location: 0, length: source.utf16.count)) {
            var inline: [(AXUIElement, DesktopAXInlineElement)] = []
            var values: [Any] = []
            value.enumerateAttributes(in: NSRange(location: 0, length: value.length), options: []) { attributes, _, _ in
                for key in [kAXLinkTextAttribute, kAXAttachmentTextAttribute] { if let value = attributes[.init(rawValue: key.takeUnretainedValue() as String)] { values.append(value) } }
            }
            for value in values where CFGetTypeID(value as CFTypeRef) == AXUIElementGetTypeID() {
                let item = value as! AXUIElement
                if inline.contains(where: { CFEqual($0.0, item) }) { continue }
                inline.append((item, DesktopAXInlineElement(url: try nativeURL(item), role: try string(item, kAXRoleAttribute),
                    description: try string(item, kAXDescriptionAttribute), roleDescription: try string(item, kAXRoleDescriptionAttribute))))
            }
            attributed = DesktopAXAttributedInput(attributed: value, range: NSRange(location: 0, length: value.length), inline: inline)
        } else { attributed = nil }
        let nativeBool = rawValue.flatMap { CFGetTypeID($0) == CFBooleanGetTypeID() ? ($0 as! NSNumber).boolValue : nil }
        let disclosure = try disclosing(element)
        return try (DesktopAXDisplayFacts(id: node.element_id, role: node.role, subrole: node.subrole,
            roleDescription: string(element, kAXRoleDescriptionAttribute), title: node.title, description: node.description,
            value: node.value, nativeTitle: node.title, nativeBooleanValue: nativeBool,
            valueDescription: string(element, kAXValueDescriptionAttribute), valueTypeName: node.value_type,
            placeholder: string(element, kAXPlaceholderValueAttribute), help: string(element, kAXHelpAttribute),
            identifier: string(element, kAXIdentifierAttribute), url: nativeURL(element), domClasses: strings("AXDOMClassList"),
            actions: node.actions, selected: selected, selectable: selectable, focused: node.focused == true, focusable: focusable,
            enabled: node.enabled ?? true, disclosing: disclosure ?? false, disclosable: disclosure != nil,
            valueSettable: node.settable_attributes.contains(kAXValueAttribute),
            horizontalScrollBar: attribute(element, kAXHorizontalScrollBarAttribute) != nil,
            verticalScrollBar: attribute(element, kAXVerticalScrollBarAttribute) != nil,
            titleTargets: [], rowRange: range(kAXRowIndexRangeAttribute), columnRange: range(kAXColumnIndexRangeAttribute),
            attributed: attributed, attributedSource: nil, mapped: nil, children: [], complete: true), titles)
    }

    private func nativeURL(_ element: AXUIElement) throws -> URL? {
        guard let raw = try attribute(element, kAXURLAttribute) else { return nil }
        guard let value = raw as? URL else { throw DesktopFailure(code: "invalid_ax_url", message: "Native URL attribute is not a URL") }
        return value
    }

    private func readNode(_ element: AXUIElement, id: String, parentID: String?, previous: DesktopAXNode?,
                          windows: [CGWindowID: String], processID: pid_t) throws -> (DesktopAXNode, CFTypeRef?) {
        guard let role = try string(element, kAXRoleAttribute) else { throw DesktopFailure(code: "ax_role_unavailable", message: "Accessibility element has no role") }
        let window = try observedWindow(element, role: role, windows: windows, processID: processID)
        var unavailable: [String] = []
        func text(_ key: String) throws -> String? {
            let result = try string(element, key)
            if result == nil { unavailable.append(key as String) }
            return result
        }
        func boolean(_ key: String) throws -> Bool? {
            guard let value = try attribute(element, key), CFGetTypeID(value) == CFBooleanGetTypeID() else { unavailable.append(key as String); return nil }
            return (value as! NSNumber).boolValue
        }
        let rawValue = try attribute(element, kAXValueAttribute)
        let subrole = try text(kAXSubroleAttribute)
        var dateMask: UInt64?
        if let rawValue, CFGetTypeID(rawValue) == CFDateGetTypeID(), let rawMask = try attribute(element, "AXDateTimeComponents") {
            guard CFGetTypeID(rawMask) == CFNumberGetTypeID(), let number = rawMask as? NSNumber,
                  number.doubleValue.isFinite, number.doubleValue >= 0, number.doubleValue <= 9_007_199_254_740_991,
                  number.doubleValue.rounded(.towardZero) == number.doubleValue else {
                throw DesktopFailure(code: "invalid_ax_date_components", message: "Native AXDateTimeComponents is outside its unsigned integer domain")
            }
            dateMask = number.uint64Value
        }
        let valueInput = try DesktopAXValueInput.classify(rawValue, role: role, subrole: subrole, dateMask: dateMask)
        var value: String?
        var valueType: String?
        if let rawValue {
            if CFGetTypeID(rawValue) == CFStringGetTypeID() {
                value = rawValue as? String; valueType = "string"
                guard (value?.utf16.count ?? 0) <= maxText else { throw DesktopFailure(code: "ax_text_limit", message: "Accessibility value exceeds its exact-value bound") }
            } else if CFGetTypeID(rawValue) == CFBooleanGetTypeID() { valueType = "boolean"; value = (rawValue as! NSNumber).boolValue ? "true" : "false" }
            else if CFGetTypeID(rawValue) == CFNumberGetTypeID() {
                let number = rawValue as! NSNumber
                guard number.doubleValue.isFinite else { throw DesktopFailure(code: "invalid_ax_value", message: "Accessibility numeric value is not finite") }
                valueType = "number"; value = number.stringValue
            }
            else if CFGetTypeID(rawValue) == CFDateGetTypeID() {
                valueType = "date"
                value = DesktopAXValueInput.dateFormatter(mask: valueInput?.date_components_mask ?? 0xee).string(from: rawValue as! Date)
            }
            else { valueType = "other" }
        }
        let frame = try rectangle(element)
        var settable: [String] = []
        for name in [kAXValueAttribute, kAXSelectedTextRangeAttribute] {
            try timeout(element)
            var canSet: DarwinBoolean = false
            let error = AXUIElementIsAttributeSettable(element, name as CFString, &canSet)
            if error == .success, canSet.boolValue { settable.append(name as String) }
            else if error != .success && error != .attributeUnsupported { throw DesktopFailure(code: "ax_read_failed", message: "Cannot inspect writable attribute: \(error.rawValue)") }
        }
        var rawActions: CFArray?
        try timeout(element)
        let actionError = AXUIElementCopyActionNames(element, &rawActions)
        guard actionError == .success || actionError == .actionUnsupported || actionError == .notImplemented else {
            throw DesktopFailure(code: "ax_read_failed", message: "Cannot enumerate exact native actions: \(actionError.rawValue)")
        }
        let names = (rawActions as? [String] ?? []).sorted()
        guard names.count <= 128, names.allSatisfy({ !$0.isEmpty && $0.utf8.count <= 2048 }) else { throw DesktopFailure(code: "ax_action_limit", message: "Accessibility action list exceeds its bound") }
        var actions = try names.map { name -> DesktopAXAction in
            let actionID = previous?.actions.first(where: { $0.name == name })?.action_id ?? UUID().uuidString.lowercased()
            try timeout(element)
            var description: CFString?
            let error = AXUIElementCopyActionDescription(element, name as CFString, &description)
            guard error == .success || error == .actionUnsupported || error == .notImplemented || error == .noValue else {
                throw DesktopFailure(code: "ax_read_failed", message: "Cannot read native action description: \(error.rawValue)")
            }
            let text = description as String?
            guard (text?.utf16.count ?? 0) <= 2048 else { throw DesktopFailure(code: "ax_action_limit", message: "Native action description exceeds its bound") }
            return DesktopAXAction(action_id: actionID, name: name, description: text)
        }
        if let value = try disclosing(element) {
            let name = value ? "SAICollapse" : "SAIExpand"
            guard !names.contains(name), actions.count < 128 else { throw DesktopFailure(code: "ambiguous_ax_action", message: "Native actions collide with the disclosure attribute plan") }
            actions.append(DesktopAXAction(action_id: previous?.actions.first(where: { $0.name == name })?.action_id ?? UUID().uuidString.lowercased(),
                name: name, description: value ? "Collapse" : "Expand"))
        }
        let title = try text(kAXTitleAttribute)
        let description = try text(kAXDescriptionAttribute)
        let enabled = try boolean(kAXEnabledAttribute)
        let focused = try boolean(kAXFocusedAttribute)
        return (DesktopAXNode(element_id: id, parent_id: parentID, window_id: window.0, window_unavailable_reason: window.1,
                             role: role, subrole: subrole, title: title, description: description,
                             value: value, value_type: valueType, value_input: settable.contains(kAXValueAttribute as String) ? valueInput : nil, enabled: enabled, focused: focused, frame_screen_points: frame,
                             settable_attributes: settable.sorted(), actions: actions, unavailable_attributes: unavailable.sorted()), rawValue)
    }

    private func children(_ element: AXUIElement, remaining: Int, omissions: inout [DesktopAXOmission], parentID: String?) throws -> [AXUIElement] {
        try timeout(element)
        var count: CFIndex = 0
        let countError = AXUIElementGetAttributeValueCount(element, kAXChildrenAttribute as CFString, &count)
        if countError == .attributeUnsupported || countError == .noValue { return [] }
        guard countError == .success, count >= 0 else { throw DesktopFailure(code: "ax_read_failed", message: "Cannot enumerate accessibility children: \(countError.rawValue)") }
        let bounded = min(count, remaining)
        if count > bounded { omissions.append(.init(parent_id: parentID, reason: "child_limit")) }
        if bounded == 0 { return [] }
        var result: CFArray?
        try timeout(element)
        let error = AXUIElementCopyAttributeValues(element, kAXChildrenAttribute as CFString, 0, bounded, &result)
        guard error == .success, let result else { throw DesktopFailure(code: "ax_read_failed", message: "Cannot read accessibility children: \(error.rawValue)") }
        return try (result as [AnyObject]).map { value in
            guard CFGetTypeID(value) == AXUIElementGetTypeID() else { throw DesktopFailure(code: "invalid_ax_child", message: "Native accessibility child is not an element") }
            return value as! AXUIElement
        }
    }
}

@MainActor
final class DesktopAXMenuObserver {
    enum Signal { case opened(NSRunningApplication?), closed }
    struct Selection {
        let item: AXUIElement
        let itemID: String
        let menu: AXUIElement
        let menuID: String
    }
    private let application: NSRunningApplication
    private let root: AXUIElement
    private var observer: AXObserver?
    private var opened: [AXUIElement] = []
    private var watchedWindows: [AXUIElement] = []
    private var failure: DesktopFailure?
    private var closed = false
    private let suppressOnWindow: Bool
    let presentation: DesktopApplicationBehavior.MenuBarPresentation
    private var checkRead: () throws -> Void
    private var onSignal: (Signal) -> Void
    private var spi: DesktopAppInputSPI?
    private(set) var selected: Selection?
    private(set) var generation: UInt64 = 1
    var openedMenu: AXUIElement? { opened.last }

    init(application: NSRunningApplication, profile: DesktopApplicationBehavior.Profile,
         checkRead: @escaping () throws -> Void, onSignal: @escaping (Signal) -> Void) throws {
        self.application = application
        self.checkRead = checkRead; self.onSignal = onSignal
        suppressOnWindow = profile.suppress_menu_dismissal_on_window
        presentation = profile.menu_bar_presentation
        root = AXUIElementCreateApplication(application.processIdentifier)
        try check()
        let bounded = AXUIElementSetMessagingTimeout(root, 0.5)
        guard bounded == .success else { throw DesktopFailure(code: "menu_observer_unavailable", message: "Cannot bound native menu notification registration") }
        var observer: AXObserver?
        let status = AXObserverCreate(application.processIdentifier, { _, element, notification, context in
            guard let context else { return }
            MainActor.assumeIsolated {
                Unmanaged<DesktopAXMenuObserver>.fromOpaque(context).takeUnretainedValue().receive(element, notification: notification)
            }
        }, &observer)
        guard status == .success, let observer else {
            throw DesktopFailure(code: "menu_observer_unavailable", message: "Native application menu observation could not be created (\(status.rawValue))")
        }
        self.observer = observer
        let context = Unmanaged.passUnretained(self).toOpaque()
        for name in notifications {
            let added = AXObserverAddNotification(observer, root, name as CFString, context)
            guard added == .success else {
                close()
                throw DesktopFailure(code: "menu_observer_unavailable", message: "Native application menu notification was refused (\(added.rawValue))")
            }
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .commonModes)
    }

    private var notifications: [String] {
        [kAXMenuOpenedNotification, kAXMenuClosedNotification] + (suppressOnWindow ? [kAXWindowCreatedNotification] : [])
    }

    func configure(profile: DesktopApplicationBehavior.Profile, checkRead: @escaping () throws -> Void,
                   onSignal: @escaping (Signal) -> Void) throws {
        try checkRead()
        guard presentation == profile.menu_bar_presentation, suppressOnWindow == profile.suppress_menu_dismissal_on_window else {
            throw DesktopFailure(code: "menu_profile_changed", message: "A live native menu owner cannot change its declared behavior")
        }
        self.checkRead = checkRead; self.onSignal = onSignal
        try check()
    }

    func check() throws {
        guard !closed, !application.isTerminated, OctopusDesktopNative.parentChannel.isOpen,
              NSRunningApplication(processIdentifier: application.processIdentifier)?.isEqual(application) == true else {
            throw DesktopFailure(code: "menu_observer_closed", message: "The retained application's menu observer was released")
        }
        try checkRead()
        if let failure { throw failure }
    }

    func hasOpenMenu() throws -> Bool { try check(); return !opened.isEmpty }

    private func changed() throws {
        guard generation < 9_007_199_254_740_991 else { throw DesktopFailure(code: "menu_revision_exhausted", message: "Native menu context revision exhausted") }
        generation += 1
    }

    func select(item: AXUIElement, itemID: String, menu: AXUIElement, menuID: String) throws {
        try check()
        let same = selected.map { CFEqual($0.item, item) && CFEqual($0.menu, menu) } ?? false
        if !same { try changed() }
        selected = Selection(item: item, itemID: itemID, menu: menu, menuID: menuID)
    }

    func clear() throws {
        try check()
        if selected != nil || !opened.isEmpty { try changed() }
        selected = nil; opened.removeAll()
    }

    private func readRole(_ element: AXUIElement) throws -> String {
        let bounded = AXUIElementSetMessagingTimeout(element, 0.5)
        guard bounded == .success else { throw DesktopFailure(code: "menu_observer_unavailable", message: "Cannot bound native menu observation") }
        var value: CFTypeRef?
        let status = AXUIElementCopyAttributeValue(element, kAXRoleAttribute as CFString, &value)
        guard status == .success, let role = value as? String else {
            throw DesktopFailure(code: "menu_role_unavailable", message: "Native menu event has no current role")
        }
        return role
    }

    private func nativeMenuApplication(_ element: AXUIElement) throws -> NSRunningApplication? {
        if spi == nil { spi = try DesktopAppInputSPI() }
        guard let spi else { throw DesktopFailure(code: "menu_owner_unavailable", message: "Native menu PID source is unavailable") }
        guard let pid = try spi.actualPID(element) else { return nil }
        guard pid > 0, let current = NSRunningApplication(processIdentifier: pid), !current.isTerminated else {
            throw DesktopFailure(code: "menu_owner_unavailable", message: "Native menu process is no longer running")
        }
        return current
    }

    private func receive(_ element: AXUIElement, notification: CFString) {
        do {
            try check()
            var pid: pid_t = 0
            guard AXUIElementGetPid(element, &pid) == .success, pid == application.processIdentifier else {
                throw DesktopFailure(code: "menu_owner_changed", message: "Native menu event belongs to a different logical application")
            }
            switch notification as String {
            case kAXMenuOpenedNotification:
                guard try readRole(element) == kAXMenuRole else { throw DesktopFailure(code: "invalid_menu_event", message: "Opened native element is not a menu") }
                if !opened.contains(where: { CFEqual($0, element) }) {
                    guard opened.count < 128 else { throw DesktopFailure(code: "menu_observer_limit", message: "Native menu observations exceed their bound") }
                    try changed(); opened.append(element)
                }
                let nativeApplication = try nativeMenuApplication(element)
                try check(); onSignal(.opened(nativeApplication))
            case kAXMenuClosedNotification:
                try clear(); onSignal(.closed)
            case kAXWindowCreatedNotification:
                guard suppressOnWindow, try readRole(element) == kAXWindowRole, let observer else {
                    throw DesktopFailure(code: "invalid_menu_event", message: "Native window event is outside its declared menu behavior")
                }
                if !watchedWindows.contains(where: { CFEqual($0, element) }) {
                    guard watchedWindows.count < 128 else { throw DesktopFailure(code: "menu_observer_limit", message: "Native menu window watches exceed their bound") }
                    let result = AXObserverAddNotification(observer, element, kAXUIElementDestroyedNotification as CFString, Unmanaged.passUnretained(self).toOpaque())
                    guard result == .success else { throw DesktopFailure(code: "menu_observer_unavailable", message: "Cannot watch the exact menu window lifetime (\(result.rawValue))") }
                    watchedWindows.append(element)
                }
                let nativeApplication = try nativeMenuApplication(element)
                try check(); onSignal(.opened(nativeApplication))
            case kAXUIElementDestroyedNotification:
                guard let index = watchedWindows.firstIndex(where: { CFEqual($0, element) }), let observer else {
                    throw DesktopFailure(code: "invalid_menu_event", message: "Native destroyed element has no exact registered menu-window watch")
                }
                AXObserverRemoveNotification(observer, watchedWindows.remove(at: index), kAXUIElementDestroyedNotification as CFString)
                onSignal(.closed)
            default:
                throw DesktopFailure(code: "invalid_menu_event", message: "Native menu notification is undeclared")
            }
        } catch let error as DesktopFailure {
            failure = error; onSignal(.closed)
            if error.code == "app_scope_closed" || error.code == "menu_observer_closed" { close() }
        } catch {
            failure = DesktopFailure(code: "menu_observer_failed", message: String(describing: error)); onSignal(.closed)
        }
    }

    func close() {
        guard !closed else { return }; closed = true
        if let observer {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .commonModes)
            for name in notifications { AXObserverRemoveNotification(observer, root, name as CFString) }
            for element in watchedWindows { AXObserverRemoveNotification(observer, element, kAXUIElementDestroyedNotification as CFString) }
        }
        onSignal(.closed)
        observer = nil; opened.removeAll(); watchedWindows.removeAll(); selected = nil
    }
}
