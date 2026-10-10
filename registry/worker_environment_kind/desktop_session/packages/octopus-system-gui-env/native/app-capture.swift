import AppKit
import ApplicationServices
import CoreGraphics
import CoreVideo
import CryptoKit
import Darwin
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

struct DesktopAppCaptureWindow: Codable, Equatable {
    let window_id: String
    let cg_window_id: UInt32
    let app: DesktopAppRef
    let title: String?
    let frame_screen_points: DesktopAppRect
    let on_screen: Bool
    let window_layer: Int
}

struct DesktopAppImage: Codable {
    let image_id: String
    let media_type: String
    let data_base64: String
    let sha256: String
    let byte_length: Int
    let width: Int
    let height: Int
}

struct DesktopAppCaptureGeometry: Codable {
    let pixel_width: Int
    let pixel_height: Int
    let pixel_to_screen: [Double]
    let filter_content_rect: DesktopAppRect
    let point_pixel_scale: Double
}

struct DesktopAppCaptureContributor: Codable {
    let kind: String
    var window_id: String? = nil
    var window_incarnation: String? = nil
    var cg_window_id: UInt32? = nil
    var host_generation: String? = nil
}

struct DesktopAppCaptureSource: Codable {
    let primary_window_incarnation: String
    let contributors: [DesktopAppCaptureContributor]
    let crop_screen_points: DesktopAppRect
    let filter_mode: String
}

struct DesktopAppPrimaryCapture: Codable {
    let observation_id: String
    let revision: UInt64
    let source_window_id: String
    let source_window_incarnation: String
    let selection: String
    let image_id: String?

    private enum CodingKeys: String, CodingKey {
        case observation_id, revision, source_window_id, source_window_incarnation, selection, image_id
    }
    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(observation_id, forKey: .observation_id); try values.encode(revision, forKey: .revision)
        try values.encode(source_window_id, forKey: .source_window_id)
        try values.encode(source_window_incarnation, forKey: .source_window_incarnation)
        try values.encode(selection, forKey: .selection); try values.encode(image_id, forKey: .image_id)
    }
}

private final class DesktopWindowServerCapture {
    private typealias MainConnection = @convention(c) () -> OpaquePointer?
    private typealias Capture = @convention(c) (OpaquePointer?, UnsafePointer<UInt32>, Int32, UInt32, CGRect) -> Unmanaged<CFArray>?
    private let library: UnsafeMutableRawPointer
    private let graphics: UnsafeMutableRawPointer
    private let connection: OpaquePointer
    private let capture: Capture

    init() throws {
        guard let library = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW | RTLD_LOCAL) else {
            throw DesktopFailure(code: "windowserver_capture_unavailable", message: "The native window capture library is unavailable")
        }
        guard let graphics = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_NOW | RTLD_LOCAL) else {
            dlclose(library)
            throw DesktopFailure(code: "windowserver_capture_unavailable", message: "The native window server connection library is unavailable")
        }
        guard let function = dlsym(library, "SLSHWCaptureWindowListInRect"), let main = dlsym(graphics, "CGSMainConnectionID"),
              let connection = unsafeBitCast(main, to: MainConnection.self)() else {
            dlclose(graphics); dlclose(library)
            throw DesktopFailure(code: "windowserver_capture_unavailable", message: "The exact native window capture function or connection is unavailable")
        }
        self.library = library; self.graphics = graphics; self.connection = connection
        self.capture = unsafeBitCast(function, to: Capture.self)
    }

    deinit { dlclose(graphics); dlclose(library) }

    func image(windows: [UInt32], rect: CGRect) throws -> CGImage {
        guard !windows.isEmpty, windows.count <= 2, Set(windows).count == windows.count, !windows.contains(0),
              [rect.minX, rect.minY, rect.width, rect.height].allSatisfy({ $0.isFinite }), rect.width > 0, rect.height > 0 else {
            throw DesktopFailure(code: "windowserver_capture_identity", message: "Native capture requires the exact bounded primary and owned cursor window IDs")
        }
        let images = windows.withUnsafeBufferPointer { ids in
            capture(connection, ids.baseAddress!, Int32(ids.count), 0x800, rect)?.takeUnretainedValue()
        }
        guard let images = images as? [CGImage], let image = images.first else {
            throw DesktopFailure(code: "windowserver_capture_failed", message: "Native offscreen window capture returned no image")
        }
        guard image.width > 0, image.height > 0, image.width <= 32_768, image.height <= 32_768,
              image.width * image.height <= 64_000_000 else {
            throw DesktopFailure(code: "capture_geometry_unsupported", message: "Native offscreen image exceeds the original-pixel capture bound")
        }
        return image
    }
}

struct DesktopAppCapture: Codable {
    let app: DesktopAppRef
    let window: DesktopAppCaptureWindow
    let geometry: DesktopAppCaptureGeometry
    let source: DesktopAppCaptureSource
    let image: DesktopAppImage
    let captured_at_unix_ms: UInt64
}

@MainActor
final class DesktopApplicationCapture {
    private struct Entry {
        let window: SCWindow
        let descriptor: DesktopAppCaptureWindow
        let stamp: DesktopWindowStamp
    }
    private let applications: DesktopApplications
    private var windows: [String: Entry] = [:]
    private var streams: [String: (String, SCStream)] = [:]
    private var observer: DesktopWindowObserver?
    private var windowServerCapture: DesktopWindowServerCapture?
    private struct WallpaperSample {
        let app: DesktopAppRef
        let windowID: String
        let action: UUID
        let generation: String
        let screenFrame: CGRect
        let scale: CGFloat
    }
    private var wallpaperSamples: [CGDirectDisplayID: WallpaperSample] = [:]
    private var enumerating = false
    private var inventoryWaiters: [CheckedContinuation<Void, Error>] = []
    @MainActor private final class Presentation {
        let app: DesktopAppRef
        let grant: String
        let check: () throws -> Void
        let resolve: ([DesktopAppCaptureWindow], [AXUIElement]) throws -> DesktopAXPresentationInventory
        var members: [String: Entry]
        var owners: [UInt32: UInt32] = [:]
        var sources: [(String, String)] = []
        var callbacks: [UUID: (String, String, (DesktopAppPIPRenderer.Event) -> Void)] = [:]
        var observer: DesktopAXApplicationWindowObserver?
        var nativeToken: UUID?
        var refresh: Task<Void, Never>?
        var refreshNeeded = false
        var failure: Error?
        init(app: DesktopAppRef, grant: String, members: [String: Entry], check: @escaping () throws -> Void,
             resolve: @escaping ([DesktopAppCaptureWindow], [AXUIElement]) throws -> DesktopAXPresentationInventory) {
            self.app = app; self.grant = grant; self.members = members; self.check = check; self.resolve = resolve
        }
    }
    private var presentations: [DesktopAppRef: Presentation] = [:]
    private var recentWindow: [DesktopAppRef: (String, UInt64)] = [:]
    private(set) var removedWindows: [DesktopAppCaptureWindow] = []
    private var closed = false

    init(applications: DesktopApplications) { self.applications = applications }

    func inventory(_ app: DesktopAppRef, check: () throws -> Void) async throws -> [DesktopAppCaptureWindow] {
        try requireOpen(); try check()
        try await beginInventory(check: check)
        defer { endInventory() }
        let prior = windows.filter { $0.value.descriptor.app == app }
        let next = try await enumerateWindows(app, check: check)
        removedWindows = prior.filter { next[$0.key] == nil }.map { $0.value.descriptor }
        for (id, entry) in prior where next[id] == nil {
            windows.removeValue(forKey: id)
            let stopped = streams.filter { $0.value.0 == entry.descriptor.window_id }
            for (view, (_, stream)) in stopped { streams.removeValue(forKey: view); try await stream.stopCapture() }
        }
        try requireOpen(); try check()
        _ = try applications.application(app)
        for entry in next.values { try nativeObserver().validate(entry.descriptor.cg_window_id, stamp: entry.stamp) }
        windows.merge(next) { _, fresh in fresh }
        if let presentation = presentations[app] { queuePresentationRefresh(presentation) }
        return next.values.map(\.descriptor).sorted { $0.cg_window_id < $1.cg_window_id }
    }

    private func beginInventory(check: () throws -> Void) async throws {
        try requireOpen(); try check()
        if !enumerating { enumerating = true; return }
        guard inventoryWaiters.count < 256 else {
            throw DesktopFailure(code: "window_inventory_limit", message: "Pending native window enumerations exceed their bound")
        }
        try await withCheckedThrowingContinuation { inventoryWaiters.append($0) }
        do { try requireOpen(); try check(); try Task.checkCancellation() }
        catch { endInventory(); throw error }
    }

    private func endInventory() {
        if inventoryWaiters.isEmpty { enumerating = false }
        else { inventoryWaiters.removeFirst().resume() }
    }

    private func enumerateWindows(_ app: DesktopAppRef, check: () throws -> Void) async throws -> [String: Entry] {
        try requireOpen(); try check(); try requireCapturePermission()
        let application = try applications.application(app)
        let observer = try nativeObserver()
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        try requireOpen(); try check()
        guard try applications.application(app).isEqual(application) else {
            throw DesktopFailure(code: "app_instance_unavailable", message: "Application instance changed during window enumeration")
        }
        let owned = content.windows.filter { $0.owningApplication?.processID == application.processIdentifier }
        guard owned.count <= 256 else { throw DesktopFailure(code: "window_inventory_limit", message: "Application window inventory exceeds its bound") }
        let other = windows.values.filter { $0.descriptor.app != app }.map { $0.descriptor.cg_window_id }
            + presentations.values.filter { $0.app != app }.flatMap { $0.members.values.map { $0.descriptor.cg_window_id } }
        try observer.watch(Set(other + owned.map(\.windowID)))
        let stamps = try observer.snapshot()
        let confirmation = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        try requireOpen(); try check()
        guard try applications.application(app).isEqual(application) else {
            throw DesktopFailure(code: "app_instance_unavailable", message: "Application instance changed while establishing native lifecycle observation")
        }
        let confirmed = confirmation.windows.filter { $0.owningApplication?.processID == application.processIdentifier }
        guard Set(confirmed.map(\.windowID)) == Set(owned.map(\.windowID)) else {
            throw DesktopFailure(code: "window_inventory_changed", message: "Native application windows changed during exact lifecycle observation")
        }
        let prior = Array(windows.values.filter { $0.descriptor.app == app }) + Array((presentations[app]?.members ?? [:]).values)
        var next: [String: Entry] = [:]
        for window in confirmed {
            try check()
            guard let stamp = stamps[window.windowID] else {
                throw DesktopFailure(code: "window_observer_unavailable", message: "Native window was not registered for lifecycle observation")
            }
            try observer.validate(window.windowID, stamp: stamp)
            let reference = prior.first { $0.descriptor.cg_window_id == window.windowID && $0.stamp.incarnation == stamp.incarnation }?.descriptor.window_id
                ?? UUID().uuidString.lowercased()
            next[reference] = Entry(window: window, descriptor: try descriptor(window, app: app, reference: reference), stamp: stamp)
        }
        try check()
        for entry in next.values { try observer.validate(entry.descriptor.cg_window_id, stamp: entry.stamp) }
        return next
    }

    func capture(_ app: DesktopAppRef, windowID: String, format: String, cursor: DesktopAppCursor? = nil,
                 check: () throws -> Void) async throws -> DesktopAppCapture {
        try requireOpen(); try check(); try requireCapturePermission()
        let entry = try await exactWindow(app, windowID: windowID, check: check)
        try requireOpen(); try check()
        let incarnation = String(entry.stamp.incarnation)
        let retainedCursor = try cursor?.captureSnapshot(app: app, windowID: entry.descriptor.cg_window_id, incarnation: incarnation)
        let cursorID = retainedCursor.flatMap { UInt32(exactly: $0.nativeWindow.windowNumber) }
        let crop = entry.window.frame
        var contributors = [DesktopAppCaptureContributor(kind: "application_window", window_id: windowID, window_incarnation: incarnation)]
        var content: SCShareableContent?
        var cursorFrame: CGRect?
        if let cursorID {
            let discovered = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
            try requireOpen(); try check()
            try nativeObserver().validate(entry.descriptor.cg_window_id, stamp: entry.stamp)
            guard let owned = discovered.windows.first(where: { $0.windowID == cursorID && $0.owningApplication?.processID == getpid() }),
                  owned.isOnScreen, let retainedCursor, retainedCursor.nativeWindow.isVisible else {
                throw DesktopFailure(code: "cursor_capture_unavailable", message: "The exact visible native cursor window is absent from capture inventory")
            }
            cursorFrame = owned.frame; content = discovered
            contributors.append(DesktopAppCaptureContributor(kind: "owned_cursor", cg_window_id: cursorID, host_generation: app.host_generation))
        }
        let image: CGImage
        let contentRect: CGRect
        let scale: Double
        let mode: String
        try requireOpen(); try check(); try requireCapturePermission(); try nativeObserver().validate(entry.descriptor.cg_window_id, stamp: entry.stamp)
        if !entry.window.isOnScreen {
            if windowServerCapture == nil { windowServerCapture = try DesktopWindowServerCapture() }
            guard let windowServerCapture else { throw DesktopFailure(code: "windowserver_capture_unavailable", message: "Native offscreen capture owner is unavailable") }
            image = try windowServerCapture.image(windows: [entry.descriptor.cg_window_id] + (cursorID.map { [$0] } ?? []), rect: crop)
            contentRect = crop; scale = Double(image.width) / crop.width; mode = "windowserver_rect"
        } else {
            let filter: SCContentFilter
            let config: SCStreamConfiguration
            if let cursorID, let content {
                guard let primary = content.windows.first(where: { $0.windowID == entry.descriptor.cg_window_id && $0.owningApplication?.processID == entry.window.owningApplication?.processID }),
                      primary.frame == crop, primary.isOnScreen,
                      let owned = content.windows.first(where: { $0.windowID == cursorID && $0.owningApplication?.processID == getpid() }) else {
                    throw DesktopFailure(code: "capture_geometry_changed", message: "Native contributor inventory no longer contains the exact primary window")
                }
                var display: SCDisplay?
                var area: CGFloat = 0
                for candidate in content.displays {
                    let intersection = crop.intersection(candidate.frame)
                    let current = intersection.isNull ? 0 : intersection.width * intersection.height
                    if display == nil || current > area { display = candidate; area = current }
                }
                guard let display, area > 0 else {
                    throw DesktopFailure(code: "capture_display_unavailable", message: "Native primary crop does not intersect an available display")
                }
                filter = SCContentFilter(display: display, including: [primary, owned])
                config = try captureConfiguration(crop.size, scale: Double(filter.pointPixelScale))
                config.sourceRect = crop.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
                mode = "display_including_windows"
            } else {
                (filter, config) = try configuration(entry.window)
                mode = "desktop_independent_window"
            }
            config.shouldBeOpaque = true
            contentRect = filter.contentRect; scale = Double(filter.pointPixelScale)
            try requireOpen(); try check(); try requireCapturePermission()
            image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
            try requireOpen(); try check()
            guard image.width == config.width, image.height == config.height else {
                throw DesktopFailure(code: "capture_geometry_changed", message: "Native screenshot dimensions differ from the configured primary crop")
            }
        }
        try requireOpen(); try check(); try requireCapturePermission()
        let after = try await exactWindow(app, windowID: windowID, check: check)
        try requireOpen(); try check()
        guard after.descriptor == entry.descriptor, after.stamp == entry.stamp else {
            throw DesktopFailure(code: "capture_geometry_changed", message: "Window identity or geometry changed during capture")
        }
        if let retainedCursor, let cursorID, let cursor {
            let current = try cursor.captureSnapshot(app: app, windowID: entry.descriptor.cg_window_id, incarnation: incarnation)
            guard let current, current.nativeWindow === retainedCursor.nativeWindow, current.actionIdentity == retainedCursor.actionIdentity,
                  current.nativeWindow.windowNumber == Int(cursorID),
                  let info = CGWindowListCopyWindowInfo(.optionIncludingWindow, cursorID) as? [[String: Any]],
                  info.count == 1, let row = info.first, (row[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == getpid(),
                  let bounds = row[kCGWindowBounds as String] as? [String: Any],
                  let frame = CGRect(dictionaryRepresentation: bounds as CFDictionary), frame == cursorFrame else {
                throw DesktopFailure(code: "cursor_capture_changed", message: "Owned cursor identity or frame changed during the still image operation")
            }
        } else if try cursor?.captureSnapshot(app: app, windowID: entry.descriptor.cg_window_id, incarnation: incarnation) != nil {
            throw DesktopFailure(code: "cursor_capture_changed", message: "A cursor became visible after contributor selection")
        }
        let encoded = try Self.encode(image, format: format)
        let frame = try DesktopAppRect(crop)
        let geometry = DesktopAppCaptureGeometry(pixel_width: image.width, pixel_height: image.height,
            pixel_to_screen: [frame.width / Double(image.width), 0, 0, frame.height / Double(image.height), frame.x, frame.y],
            filter_content_rect: try DesktopAppRect(contentRect), point_pixel_scale: scale)
        let source = DesktopAppCaptureSource(primary_window_incarnation: incarnation, contributors: contributors,
            crop_screen_points: frame, filter_mode: mode)
        try check(); try nativeObserver().validate(entry.descriptor.cg_window_id, stamp: entry.stamp)
        return DesktopAppCapture(app: app, window: after.descriptor, geometry: geometry, source: source, image: encoded,
            captured_at_unix_ms: UInt64(Date().timeIntervalSince1970 * 1000))
    }

    func validateWindow(_ app: DesktopAppRef, windowID: String, check: () throws -> Void) async throws -> DesktopAppCaptureWindow {
        let entry = try await exactWindow(app, windowID: windowID, check: check)
        try check()
        return entry.descriptor
    }

    func validateRetainedWindow(_ app: DesktopAppRef, windowID: String) throws {
        try requireOpen()
        _ = try applications.application(app)
        guard let entry = windows[windowID], entry.descriptor.app == app else {
            throw DesktopFailure(code: "stale_window", message: "Application window is no longer retained")
        }
        try nativeObserver().validate(entry.descriptor.cg_window_id, stamp: entry.stamp)
    }

    func retainedWindowGeneration(_ app: DesktopAppRef, windowID: String) throws -> String {
        try validateRetainedWindow(app, windowID: windowID)
        guard let entry = windows[windowID] else {
            throw DesktopFailure(code: "stale_window", message: "Native window incarnation is no longer retained")
        }
        return String(entry.stamp.incarnation)
    }

    func liveWindow(_ app: DesktopAppRef, windowID: String, incarnation: String? = nil) throws -> DesktopAppPIPRenderer.Window {
        try requireOpen(); try requireCapturePermission()
        let application = try applications.application(app)
        guard let entry = windows[windowID] ?? presentations[app]?.members[windowID], entry.descriptor.app == app,
              incarnation == nil || incarnation == String(entry.stamp.incarnation) else {
            throw DesktopFailure(code: "stale_window", message: "Live capture source is not the retained application window")
        }
        let observer = try nativeObserver()
        _ = try observer.validateIncarnation(entry.descriptor.cg_window_id, incarnation: entry.stamp.incarnation)
        guard let values = CGWindowListCopyWindowInfo(.optionIncludingWindow, entry.descriptor.cg_window_id) as? [[String: Any]],
              let value = values.first(where: { ($0[kCGWindowNumber as String] as? UInt32) == entry.descriptor.cg_window_id }),
              (value[kCGWindowOwnerPID as String] as? Int32) == application.processIdentifier,
              let bounds = value[kCGWindowBounds as String] as? [String: Any],
              let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary),
              let layer = value[kCGWindowLayer as String] as? Int,
              let visible = value[kCGWindowIsOnscreen as String] as? Bool else {
            throw DesktopFailure(code: "pip_source_unavailable", message: "The exact retained native window is unavailable for live capture")
        }
        _ = try observer.validateIncarnation(entry.descriptor.cg_window_id, incarnation: entry.stamp.incarnation)
        guard try applications.application(app).isEqual(application) else {
            throw DesktopFailure(code: "app_instance_unavailable", message: "Live capture application instance changed")
        }
        return DesktopAppPIPRenderer.Window(descriptor: DesktopAppCaptureWindow(window_id: windowID,
            cg_window_id: entry.descriptor.cg_window_id, app: app, title: value[kCGWindowName as String] as? String,
            frame_screen_points: try DesktopAppRect(rect), on_screen: visible, window_layer: layer),
            incarnation: String(entry.stamp.incarnation))
    }

    func preparePresentation(_ app: DesktopAppRef, grant: String, sourceWindowID: String,
                             check: @escaping () throws -> Void,
                             resolve: @escaping ([DesktopAppCaptureWindow], [AXUIElement]) throws -> DesktopAXPresentationInventory) throws {
        try requireOpen(); try check()
        let primary = try liveWindow(app, windowID: sourceWindowID)
        let presentation: Presentation
        if let current = presentations[app], current.grant == grant, current.failure == nil {
            try current.check()
            presentation = current
        } else {
            if let previous = presentations.removeValue(forKey: app) {
                closePresentation(previous, error: DesktopFailure(code: "app_scope_closed", message: "Native application read scope was replaced"))
            }
            let members = windows.filter { $0.value.descriptor.app == app }
            presentation = Presentation(app: app, grant: grant, members: members, check: check, resolve: resolve)
            presentations[app] = presentation
            do {
                presentation.observer = try DesktopAXApplicationWindowObserver(application: applications.application(app), check: check) { [weak self, weak presentation] result in
                    guard let self, let presentation else { return }
                    switch result {
                    case .success: self.queuePresentationRefresh(presentation)
                    case .failure(let error): self.closePresentation(presentation, error: error)
                    }
                }
            } catch {
                presentations.removeValue(forKey: app); closePresentation(presentation, error: error); throw error
            }
        }
        if !presentation.sources.contains(where: { $0.0 == sourceWindowID && $0.1 == primary.incarnation }) {
            guard presentation.sources.count < 256 else {
                throw DesktopFailure(code: "pip_source_limit", message: "Native application presentation sources exceed their bound")
            }
            presentation.sources.append((sourceWindowID, primary.incarnation))
        }
        try refreshPresentationOwnership(presentation)
        queuePresentationRefresh(presentation)
        try check()
    }

    func presentationWindows(_ app: DesktopAppRef, primary: String, incarnation: String) throws -> [DesktopAppPIPRenderer.Window] {
        try requireOpen()
        guard let presentation = presentations[app] else {
            throw DesktopFailure(code: "pip_source_unavailable", message: "Native application presentation inventory is not retained")
        }
        try presentation.check()
        if let failure = presentation.failure { throw failure }
        let window = try liveWindow(app, windowID: primary, incarnation: incarnation)
        var associated: [DesktopAppPIPRenderer.Window] = []
        for entry in presentation.members.values where presentation.owners[entry.descriptor.cg_window_id] == window.descriptor.cg_window_id {
            do {
                let current = try liveWindow(app, windowID: entry.descriptor.window_id, incarnation: String(entry.stamp.incarnation))
                if current.descriptor.on_screen { associated.append(current) }
            } catch let error as DesktopFailure where ["stale_window", "pip_source_unavailable"].contains(error.code) {
                queuePresentationRefresh(presentation)
            }
        }
        try presentation.check()
        return associated.sorted { $0.descriptor.cg_window_id < $1.descriptor.cg_window_id }
    }

    func notePresentationInteraction(_ app: DesktopAppRef, windowID: String, isNormalWindow: Bool) throws {
        try validateRetainedWindow(app, windowID: windowID)
        guard let entry = windows[windowID] else { return }
        if isNormalWindow, presentations[app]?.owners[entry.descriptor.cg_window_id] == nil {
            recentWindow[app] = (windowID, entry.stamp.incarnation)
        }
        if let presentation = presentations[app] { queuePresentationRefresh(presentation) }
    }

    func observePresentation(_ app: DesktopAppRef, windowID: String, incarnation: String,
                             changed: @escaping (DesktopAppPIPRenderer.Event) -> Void) throws -> DesktopAppPIPObservation {
        guard let presentation = presentations[app], presentation.callbacks.count < 256 else {
            throw DesktopFailure(code: "pip_source_unavailable", message: "Native presentation has no bounded app read observation")
        }
        _ = try presentationWindows(app, primary: windowID, incarnation: incarnation)
        let id = UUID(); presentation.callbacks[id] = (windowID, incarnation, changed)
        return DesktopAppPIPObservation { [weak presentation] in presentation?.callbacks.removeValue(forKey: id) }
    }

    private func queuePresentationRefresh(_ presentation: Presentation) {
        guard presentation.failure == nil, presentations[presentation.app] === presentation, !closed else { return }
        presentation.refreshNeeded = true
        guard presentation.refresh == nil else { return }
        presentation.refresh = Task { [weak self, weak presentation] in
            guard let self, let presentation else { return }
            defer { presentation.refresh = nil }
            do {
                while presentation.refreshNeeded {
                    presentation.refreshNeeded = false
                    try Task.checkCancellation(); try presentation.check()
                    try await self.beginInventory(check: presentation.check)
                    let next: [String: Entry]
                    do { next = try await self.enumerateWindows(presentation.app, check: presentation.check) }
                    catch { self.endInventory(); throw error }
                    self.endInventory()
                    try Task.checkCancellation(); try presentation.check()
                    guard self.presentations[presentation.app] === presentation else {
                        throw DesktopFailure(code: "app_scope_closed", message: "Native presentation read scope changed during enumeration")
                    }
                    let previous = presentation.members
                    presentation.members = next
                    presentation.owners = presentation.owners.filter { child, parent in
                        guard let oldChild = previous.values.first(where: { $0.descriptor.cg_window_id == child }),
                              let oldParent = previous.values.first(where: { $0.descriptor.cg_window_id == parent }) else { return false }
                        return next[oldChild.descriptor.window_id]?.stamp.incarnation == oldChild.stamp.incarnation &&
                            next[oldParent.descriptor.window_id]?.stamp.incarnation == oldParent.stamp.incarnation
                    }
                    try self.refreshPresentationOwnership(presentation)
                    self.notifyPresentation(presentation)
                }
            } catch { self.closePresentation(presentation, error: error) }
        }
    }

    private func refreshPresentationOwnership(_ presentation: Presentation) throws {
        try requireOpen(); try presentation.check()
        guard let axObserver = presentation.observer else {
            throw DesktopFailure(code: "app_window_observer_unavailable", message: "Native presentation lost its application window observer")
        }
        let revision = axObserver.revision
        let inventory = try presentation.resolve(presentation.members.values.map(\.descriptor), axObserver.elements())
        try presentation.check()
        guard axObserver.revision == revision else { queuePresentationRefresh(presentation); return }
        let observer = try nativeObserver()
        try axObserver.track(inventory.windows.values.map(\.element), destroyedElements: inventory.destroyedElements)
        let stamps = try observer.snapshot()
        let live = Set(presentation.members.values.compactMap { entry -> UInt32? in
            guard let current = stamps[entry.descriptor.cg_window_id], !current.terminated,
                  current.incarnation == entry.stamp.incarnation else { return nil }
            return entry.descriptor.cg_window_id
        })
        presentation.sources.removeAll { reference, incarnation in
            guard let entry = presentation.members[reference] else { return true }
            return !live.contains(entry.descriptor.cg_window_id) || String(entry.stamp.incarnation) != incarnation
        }
        let published = presentation.sources.compactMap { presentation.members[$0.0]?.descriptor.cg_window_id }
        var recent: UInt32?
        if let (reference, incarnation) = recentWindow[presentation.app], let entry = presentation.members[reference],
           entry.stamp.incarnation == incarnation, live.contains(entry.descriptor.cg_window_id),
           let info = inventory.windows[entry.descriptor.cg_window_id], info.role == kAXWindowRole,
           info.subrole != kAXFloatingWindowSubrole, info.subrole != kAXDialogSubrole, info.parent == nil {
            recent = info.id
        }
        var native: [UInt32: [UInt32]] = [:]
        func associated(_ id: UInt32) throws -> [UInt32] {
            if let value = native[id] { return value }
            try presentation.check()
            let result = try observer.associatedWindowIDs(id).filter { live.contains($0) && inventory.windows[$0] != nil }
            native[id] = result; return result
        }
        var owners: [UInt32: UInt32] = [:]
        for info in inventory.windows.values where live.contains(info.id) {
            var parent = info.parent.flatMap { live.contains($0) && inventory.windows[$0] != nil ? $0 : nil }
            if parent == nil {
                for candidate in published where candidate != info.id {
                    if try associated(candidate).contains(info.id) { parent = candidate; break }
                }
                if parent == nil, !published.contains(info.id) {
                    parent = try associated(info.id).first { candidate in
                        guard candidate != info.id, let window = inventory.windows[candidate] else { return false }
                        return window.role == kAXWindowRole && window.subrole != kAXFloatingWindowSubrole
                    }
                }
            }
            let owner: UInt32?
            if info.role == kAXSheetRole {
                owner = parent ?? presentation.owners[info.id].flatMap { live.contains($0) ? $0 : nil }
            } else if info.role == kAXWindowRole, let parent { owner = parent }
            else if info.role == kAXWindowRole, info.subrole == kAXDialogSubrole, info.modal {
                owner = [recent, inventory.mainWindow].compactMap { $0 }.first { $0 != 0 && $0 != info.id && live.contains($0) }
            } else { owner = nil }
            if let owner, owner != 0, owner != info.id { owners[info.id] = owner }
        }
        try presentation.check()
        presentation.owners = owners
        if let token = presentation.nativeToken { observer.unsubscribe(token); presentation.nativeToken = nil }
        if !live.isEmpty {
            presentation.nativeToken = try observer.subscribe(live) { [weak self, weak presentation] in
                guard let self, let presentation else { return }
                self.notifyPresentation(presentation)
                self.queuePresentationRefresh(presentation)
            }
        }
    }

    private func notifyPresentation(_ presentation: Presentation) {
        for entry in Array(presentation.callbacks.values) {
            do { try presentation.check(); _ = try liveWindow(presentation.app, windowID: entry.0, incarnation: entry.1); entry.2(.geometryChanged) }
            catch { entry.2(.sourceUnavailable(error)) }
        }
    }

    private func closePresentation(_ presentation: Presentation, error: Error) {
        guard presentation.failure == nil else { return }
        presentation.failure = error; presentation.refresh?.cancel(); presentation.refreshNeeded = false
        presentation.observer?.close(); presentation.observer = nil
        if let observer, let token = presentation.nativeToken { observer.unsubscribe(token) }
        presentation.nativeToken = nil
        let callbacks = presentation.callbacks.values.map { $0.2 }; presentation.callbacks.removeAll()
        for callback in callbacks { callback(.sourceUnavailable(error)) }
    }

    func observeLiveWindows(_ app: DesktopAppRef, windows references: [String],
                            changed: @escaping (DesktopAppPIPRenderer.Event) -> Void) throws -> DesktopAppPIPObservation {
        try requireOpen()
        let retained = try references.map { try liveWindow(app, windowID: $0) }
        let observer = try nativeObserver()
        let token = try observer.subscribe(Set(retained.map { $0.descriptor.cg_window_id })) { [weak self] in
            guard let self else { return }
            do {
                for window in retained {
                    _ = try self.liveWindow(app, windowID: window.descriptor.window_id, incarnation: window.incarnation)
                }
                changed(.geometryChanged)
            } catch { changed(.sourceUnavailable(error)) }
        }
        let appToken = NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didTerminateApplicationNotification,
            object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    do { _ = try self.applications.application(app) }
                    catch { changed(.sourceUnavailable(error)) }
                }
            }
        return DesktopAppPIPObservation { observer.unsubscribe(token); NSWorkspace.shared.notificationCenter.removeObserver(appToken) }
    }

    func cursorWallpaper(_ app: DesktopAppRef, windowID: String, atCGScreenPoint point: CGPoint,
                         actionIdentity: UUID, check: () throws -> Void) async throws -> DesktopAppCursor.Wallpaper {
        try requireOpen(); try check(); try requireCapturePermission()
        let entry = try await exactWindow(app, windowID: windowID, check: check)
        try check()
        let screens = NSScreen.screens
        guard point.x.isFinite, point.y.isFinite, entry.window.frame.contains(point),
              let zero = screens.first(where: { $0.frame.contains(CGPoint.zero) }),
              let screen = screens.first(where: { $0.frame.contains(CGPoint(x: point.x, y: zero.frame.maxY - point.y)) }),
              let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber else {
            throw DesktopFailure(code: "cursor_display_unavailable", message: "Cursor point has no exact native display inside its retained application window")
        }
        let displayID = number.uint32Value
        let frame = screen.frame
        let scale = screen.backingScaleFactor
        let width: CGFloat = floor(frame.width * scale), height: CGFloat = floor(frame.height * scale)
        guard scale.isFinite, scale > 0, width.isFinite, height.isFinite, width >= 1, height >= 1,
              width <= 32_768, height <= 32_768, width * height <= 64_000_000 else {
            throw DesktopFailure(code: "cursor_display_unavailable", message: "Cursor wallpaper display geometry exceeds its native capture bound")
        }
        let content: SCShareableContent
        if #available(macOS 14.4, *) { content = try await SCShareableContent.currentProcess }
        else { content = try await SCShareableContent.current }
        try requireOpen(); try check(); try validateRetainedWindow(app, windowID: windowID)
        guard let display = content.displays.first(where: { $0.displayID == displayID }) else {
            throw DesktopFailure(code: "cursor_display_unavailable", message: "The exact native display is no longer capturable")
        }
        let candidates = content.windows.filter {
            $0.owningApplication?.bundleIdentifier == "com.apple.dock" &&
            $0.title?.hasPrefix("Wallpaper") == true && $0.frame.size == display.frame.size
        }
        let nativeNumbers = Set((NSWindow.windowNumbers(options: .allApplications) ?? []).map { $0.uint32Value })
        guard let wallpaper = candidates.last(where: { nativeNumbers.contains($0.windowID) }) ?? candidates.last else {
            throw DesktopFailure(code: "cursor_wallpaper_unavailable", message: "The exact display has no native wallpaper window; no other window was captured")
        }
        let filter = SCContentFilter(desktopIndependentWindow: wallpaper)
        let configuration = SCStreamConfiguration()
        configuration.width = Int(width); configuration.height = Int(height)
        configuration.showsCursor = false
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        try check()
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
        try requireOpen(); try check(); try validateRetainedWindow(app, windowID: windowID)
        guard image.width == Int(width), image.height == Int(height),
              windows[windowID]?.stamp == entry.stamp,
              let current = NSScreen.screens.first(where: {
                  ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == displayID
              }), current.frame == frame, current.backingScaleFactor == scale else {
            throw DesktopFailure(code: "cursor_wallpaper_changed", message: "Cursor display or application window changed during native wallpaper capture")
        }
        guard wallpaperSamples[displayID] != nil || wallpaperSamples.count < 16 else {
            throw DesktopFailure(code: "cursor_wallpaper_limit", message: "Native cursor wallpaper samples exceed the display bound")
        }
        let generation = UUID().uuidString.lowercased()
        try check()
        wallpaperSamples[displayID] = WallpaperSample(app: app, windowID: windowID, action: actionIdentity,
            generation: generation, screenFrame: frame, scale: scale)
        return DesktopAppCursor.Wallpaper(displayID: displayID, captureGeneration: generation,
            screenFrame: frame, backingScale: scale, image: image)
    }

    func validateCursorWallpaper(_ app: DesktopAppRef, windowID: String, actionIdentity: UUID,
                                 displayID: CGDirectDisplayID, captureGeneration: String, check: () throws -> Void) throws {
        try requireOpen(); try check(); try validateRetainedWindow(app, windowID: windowID)
        guard let sample = wallpaperSamples[displayID], sample.app == app, sample.windowID == windowID,
              sample.action == actionIdentity, sample.generation == captureGeneration,
              let screen = NSScreen.screens.first(where: {
                  ($0.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value == displayID
              }), screen.frame == sample.screenFrame, screen.backingScaleFactor == sample.scale else {
            throw DesktopFailure(code: "cursor_wallpaper_changed", message: "Cursor wallpaper does not belong to the exact retained application action and native display")
        }
        try check()
    }

    func startStream(_ app: DesktopAppRef, windowID: String, viewID: String, output: SCStreamOutput & SCStreamDelegate,
                     queue: DispatchQueue, check: () throws -> Void) async throws -> DesktopAppCaptureWindow {
        try requireOpen(); try check(); try requireCapturePermission()
        guard !viewID.isEmpty, !viewID.contains("\0") else { throw DesktopFailure(code: "invalid_view", message: "Native app stream requires an exact view identity") }
        if let existing = streams[viewID] {
            guard existing.0 == windowID else { throw DesktopFailure(code: "view_identity_changed", message: "A native app view cannot retarget an existing stream") }
            let entry = try await exactWindow(app, windowID: windowID, check: check)
            try check()
            return entry.descriptor
        }
        guard streams.count < 16 else { throw DesktopFailure(code: "view_limit", message: "Native app stream count exceeds its bound") }
        let entry = try await exactWindow(app, windowID: windowID, check: check)
        try check()
        let (filter, config) = try configuration(entry.window)
        config.minimumFrameInterval = CMTime(value: 1, timescale: 15)
        config.queueDepth = 2
        let stream = SCStream(filter: filter, configuration: config, delegate: output)
        try stream.addStreamOutput(output, type: .screen, sampleHandlerQueue: queue)
        try check()
        try await stream.startCapture()
        do {
            try requireOpen(); try check()
            let after = try await exactWindow(app, windowID: windowID, check: check)
            try check()
            guard after.descriptor == entry.descriptor, streams[viewID] == nil else {
                throw DesktopFailure(code: "capture_geometry_changed", message: "Native app view changed during attachment")
            }
            streams[viewID] = (windowID, stream)
            return after.descriptor
        } catch {
            try? await stream.stopCapture()
            throw error
        }
    }

    func stopStream(_ viewID: String) async throws {
        if let (_, stream) = streams.removeValue(forKey: viewID) { try await stream.stopCapture() }
    }

    func release(_ app: DesktopAppRef) async throws {
        recentWindow.removeValue(forKey: app)
        if let presentation = presentations.removeValue(forKey: app) {
            closePresentation(presentation, error: DesktopFailure(code: "app_scope_closed", message: "Native application read scope was released"))
        }
        wallpaperSamples = wallpaperSamples.filter { $0.value.app != app }
        let owned = Set(windows.values.filter { $0.descriptor.app == app }.map { $0.descriptor.window_id })
        let stopped = streams.filter { owned.contains($0.value.0) }
        for id in owned { windows.removeValue(forKey: id) }
        if let observer { try observer.watch(Set(windows.values.map { $0.descriptor.cg_window_id })) }
        var firstError: Error?
        for (view, (_, stream)) in stopped {
            streams.removeValue(forKey: view)
            do { try await stream.stopCapture() } catch { if firstError == nil { firstError = error } }
        }
        if let firstError { throw firstError }
    }

    func close() async {
        closed = true
        let failure = DesktopFailure(code: "app_scope_closed", message: "Native application capture was released")
        for presentation in presentations.values { closePresentation(presentation, error: failure) }
        presentations.removeAll(); recentWindow.removeAll()
        let waiters = inventoryWaiters; inventoryWaiters.removeAll()
        for waiter in waiters { waiter.resume(throwing: failure) }
        let pending = streams.values.map { $0.1 }
        streams.removeAll(); windows.removeAll(); removedWindows.removeAll(); wallpaperSamples.removeAll()
        if let observer { try? observer.close() }
        observer = nil; windowServerCapture = nil
        for stream in pending { try? await stream.stopCapture() }
    }

    private func exactWindow(_ app: DesktopAppRef, windowID: String, check: () throws -> Void) async throws -> Entry {
        try requireOpen(); try check()
        let application = try applications.application(app)
        guard let entry = windows[windowID], entry.descriptor.app == app else {
            throw DesktopFailure(code: "stale_window", message: "Window reference is not in this application's exact inventory")
        }
        let observer = try nativeObserver()
        try observer.validate(entry.descriptor.cg_window_id, stamp: entry.stamp)
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
        try requireOpen(); try check()
        try observer.validate(entry.descriptor.cg_window_id, stamp: entry.stamp)
        let current = try applications.application(app)
        guard current.isEqual(application), windows[windowID]?.descriptor == entry.descriptor,
              windows[windowID]?.stamp == entry.stamp,
              let live = content.windows.first(where: { $0.windowID == entry.descriptor.cg_window_id && $0.owningApplication?.processID == current.processIdentifier }) else {
            windows.removeValue(forKey: windowID)
            throw DesktopFailure(code: "stale_window", message: "The exact application window is no longer available")
        }
        let fresh = try descriptor(live, app: app, reference: windowID)
        guard fresh == entry.descriptor else { throw DesktopFailure(code: "capture_geometry_changed", message: "Window changed since inventory; enumerate it again") }
        try check()
        try observer.validate(entry.descriptor.cg_window_id, stamp: entry.stamp)
        return Entry(window: live, descriptor: fresh, stamp: entry.stamp)
    }

    private func nativeObserver() throws -> DesktopWindowObserver {
        if let observer { return observer }
        let created = try DesktopWindowObserver()
        observer = created
        return created
    }

    private func descriptor(_ window: SCWindow, app: DesktopAppRef, reference: String) throws -> DesktopAppCaptureWindow {
        DesktopAppCaptureWindow(window_id: reference, cg_window_id: window.windowID, app: app, title: window.title,
                                frame_screen_points: try DesktopAppRect(window.frame), on_screen: window.isOnScreen, window_layer: window.windowLayer)
    }

    private func configuration(_ window: SCWindow) throws -> (SCContentFilter, SCStreamConfiguration) {
        let filter = SCContentFilter(desktopIndependentWindow: window)
        let rect = filter.contentRect
        let scale = Double(filter.pointPixelScale)
        let width = ceil(rect.width * scale); let height = ceil(rect.height * scale)
        guard scale.isFinite, scale > 0, width.isFinite, height.isFinite, width >= 1, height >= 1,
              width <= 32_768, height <= 32_768, width * height <= 64_000_000,
              abs(rect.width - window.frame.width) < 0.01, abs(rect.height - window.frame.height) < 0.01 else {
            throw DesktopFailure(code: "capture_geometry_unsupported", message: "Native single-window capture does not expose a bounded exact coordinate domain")
        }
        return (filter, try captureConfiguration(rect.size, scale: scale))
    }

    private func captureConfiguration(_ size: CGSize, scale: Double) throws -> SCStreamConfiguration {
        let width = ceil(size.width * scale); let height = ceil(size.height * scale)
        guard scale.isFinite, scale > 0, width.isFinite, height.isFinite, width >= 1, height >= 1,
              width <= 32_768, height <= 32_768, width * height <= 64_000_000 else {
            throw DesktopFailure(code: "capture_geometry_unsupported", message: "Native screenshot crop exceeds the original-pixel bound")
        }
        let config = SCStreamConfiguration()
        config.width = Int(width); config.height = Int(height)
        config.showsCursor = false
        config.ignoreShadowsSingleWindow = true; config.ignoreShadowsDisplay = true
        config.ignoreGlobalClipSingleWindow = true
        config.scalesToFit = false
        return config
    }

    private func requireOpen() throws {
        guard !closed else { throw DesktopFailure(code: "app_scope_closed", message: "Application capture scope was released") }
    }

    private func requireCapturePermission() throws {
        guard CGPreflightScreenCaptureAccess() else { throw DesktopFailure(code: "screen_capture_permission_required", message: "App capture requires Screen Recording permission") }
    }

    private static func encode(_ image: CGImage, format: String) throws -> DesktopAppImage {
        guard format == "png" || format == "jpeg" else { throw DesktopFailure(code: "unsupported_image_format", message: "App capture supports png or jpeg") }
        let data = NSMutableData()
        let type = format == "png" ? UTType.png.identifier : UTType.jpeg.identifier
        guard let destination = CGImageDestinationCreateWithData(data, type as CFString, 1, nil) else { throw DesktopFailure(code: "image_encoding_failed", message: "Cannot create app image encoder") }
        CGImageDestinationAddImage(destination, image, format == "jpeg" ? [kCGImageDestinationLossyCompressionQuality: 0.8] as CFDictionary : nil)
        guard CGImageDestinationFinalize(destination) else { throw DesktopFailure(code: "image_encoding_failed", message: "Cannot encode app capture") }
        let bytes = data as Data
        guard bytes.count <= OctopusDesktopNative.maxMediaBytes else { throw DesktopFailure(code: "image_too_large", message: "App image exceeds the native evidence bound") }
        return DesktopAppImage(image_id: UUID().uuidString.lowercased(), media_type: "image/\(format)", data_base64: bytes.base64EncodedString(),
                               sha256: SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined(), byte_length: bytes.count, width: image.width, height: image.height)
    }
}
