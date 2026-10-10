import AppKit
import ApplicationServices
import Observation
import QuartzCore
import SwiftUI
import ObjectiveC
import Darwin

@MainActor
final class DesktopAppCursor: NSObject {
    enum NextInteractionTiming: Equatable { case closeEnough, finished }
    enum Activity: Equatable { case idle, loading, paused }
    enum Milestone: Equatable { case closeEnough, finished }
    enum CallbackProvenance: String { case displayLink = "display_link", synchronousCompletion = "synchronous_completion" }

    struct Snapshot {
        let window: DesktopAppInputWindow
        let windowGeneration: String
        let processID: pid_t
        let isOnScreen: Bool
        let applicationIsActive: Bool
        let menusOpen: Bool
        let windowLevel: Int
    }

    struct Wallpaper {
        let displayID: CGDirectDisplayID
        let captureGeneration: String
        let screenFrame: CGRect
        let backingScale: CGFloat
        let image: CGImage
    }

    struct Receipt {
        let app: DesktopAppRef
        let windowID: CGWindowID
        let windowGeneration: String
        let actionIdentity: UUID
        let movementIdentity: UUID
        let milestone: Milestone
        let cgScreenPoint: CGPoint
        let callbackProvenance: CallbackProvenance
        let callbackTimestamp: CFTimeInterval
    }

    enum Event {
        case presented(Receipt)
        case interrupted(actionIdentity: UUID, movementIdentity: UUID?, error: Error)
    }

    typealias Check = @MainActor (UUID) throws -> Snapshot
    typealias WallpaperCheck = @MainActor (UUID, CGDirectDisplayID, String) throws -> Void
    typealias EventHandler = @MainActor (Event) -> Void

    private let application: NSRunningApplication
    private let target: DesktopAppInputWindow
    private let windowGeneration: String
    private let check: Check
    private let checkWallpaper: WallpaperCheck
    private let onEvent: EventHandler
    private let state = DesktopCursorState()
    private let cursorWindow: DesktopCursorWindow
    private let fog: DesktopCursorFogView
    private let fadeFilter: NSObject
    private let ordering: DesktopCursorWindowOrdering
    private var displayLink: CADisplayLink?
    private let displayTarget = DesktopCursorDisplayTarget()
    private var movement: DesktopCursorMovement?
    private var continuation: CheckedContinuation<Receipt, Error>?
    private var continuationMovement: UUID?
    private var currentAction: UUID?
    private var wallpapers: [CGDirectDisplayID: Wallpaper] = [:]
    private var relativeOrigin: CGPoint?
    private static var lastGlobalOrigin: CGPoint?
    private var wantsVisible = false
    private var raisedForMotion = false
    private var released = false
    private var fadeIdentity = UUID()
    private var fadeDelegate: DesktopCursorAnimationDelegate?
    private var terminationObservation: NSKeyValueObservation?
    private var activeObservation: NSKeyValueObservation?
    private var workspaceObservation: NSObjectProtocol?

    init(application: NSRunningApplication, window: DesktopAppInputWindow,
         windowGeneration: String, check: @escaping Check,
         checkWallpaper: @escaping WallpaperCheck, onEvent: @escaping EventHandler) throws {
        guard !windowGeneration.isEmpty, !application.isTerminated, window.windowID != 0 else {
            throw DesktopFailure(code: "cursor_target_invalid", message: "Cursor presentation requires a retained application and exact native window generation")
        }
        self.application = application
        target = window
        self.windowGeneration = windowGeneration
        self.check = check
        self.checkWallpaper = checkWallpaper
        self.onEvent = onEvent
        fog = try DesktopCursorFogView(backdropGroup: UUID().uuidString)
        ordering = try DesktopCursorWindowOrdering()
        fadeFilter = try DesktopCursorFilters.make("gaussianBlur", values: ["inputRadius": 0])
        cursorWindow = DesktopCursorWindow(contentRect: .zero, styleMask: .borderless, backing: .buffered, defer: true)
        super.init()
        cursorWindow.title = "Software Cursor"
        cursorWindow.collectionBehavior = [.transient, .ignoresCycle, .canJoinAllApplications]
        cursorWindow.isReleasedWhenClosed = false
        cursorWindow.ignoresMouseEvents = true
        cursorWindow.backgroundColor = .clear
        cursorWindow.isOpaque = false
        let hosting = NSHostingView(rootView: DesktopCursorView(state: state, fog: fog))
        hosting.wantsLayer = true
        let controller = NSViewController()
        controller.view = hosting
        cursorWindow.contentViewController = controller
        hosting.layoutSubtreeIfNeeded()
        cursorWindow.setContentSize(hosting.fittingSize)
        displayTarget.owner = self
        terminationObservation = application.observe(\.isTerminated, options: [.new]) { [weak self] _, change in
            guard change.newValue == true else { return }
            Task { @MainActor [weak self] in
                self?.invalidate(DesktopFailure(code: "cursor_application_exited", message: "The cursor's retained application exited"))
            }
        }
        activeObservation = application.observe(\.isActive, options: [.new]) { [weak self] _, _ in
            Task { @MainActor [weak self] in self?.refreshFromNotification() }
        }
        workspaceObservation = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.activeSpaceDidChangeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in self?.refreshFromNotification() }
        }
    }

    deinit {
        let window = cursorWindow
        let link = displayLink
        let workspaceToken = workspaceObservation
        let termination = terminationObservation
        let active = activeObservation
        let pending = continuation
        Task { @MainActor in
            link?.invalidate()
            termination?.invalidate()
            active?.invalidate()
            if let workspaceToken { NSWorkspace.shared.notificationCenter.removeObserver(workspaceToken) }
            pending?.resume(throwing: CancellationError())
            window.orderOut(nil)
            window.contentViewController = nil
            window.close()
        }
    }

    func move(toCGScreenPoint point: CGPoint, actionIdentity: UUID,
              wallpapers samples: [Wallpaper], nextInteractionTiming: NextInteractionTiming,
              animated: Bool = true, fadeIn: Bool = true) async throws -> Receipt {
        try Task.checkCancellation()
        let screenPoint: CGPoint
        let screen: NSScreen
        let images: [CGDirectDisplayID: Wallpaper]
        do {
            _ = try validated(actionIdentity)
            screenPoint = try appKitPoint(point)
            screen = try self.screen(containing: screenPoint)
            images = try validatedWallpapers(samples, action: actionIdentity)
            try requireWallpaper(screen, images: images, action: actionIdentity)
        } catch {
            if currentAction == actionIdentity { invalidate(error) }
            throw error
        }
        let token = UUID()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { pending in
                do {
                    try Task.checkCancellation()
                    try startMovement(point: screenPoint, screen: screen,
                                      action: actionIdentity, identity: token, images: images,
                                      timing: nextInteractionTiming, animated: animated,
                                      fadeIn: fadeIn, continuation: pending)
                } catch {
                    pending.resume(throwing: error)
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.cancelMovement(token, error: CancellationError()) }
        }
    }

    func show(actionIdentity: UUID, animated: Bool = true) throws {
        let snapshot = try requireAction(actionIdentity)
        do {
            guard let relativeOrigin else {
                throw DesktopFailure(code: "cursor_position_unavailable", message: "Cursor presentation has not been positioned for this window")
            }
            let frame = try appKitRect(snapshot.window.bounds)
            cancelFade()
            wantsVisible = true
            state.presented = true
            if animated {
                cursorWindow.alphaValue = 0
                fadeFilter.setValue(7, forKey: "inputRadius")
            } else {
                cursorWindow.alphaValue = 1
                fadeFilter.setValue(0, forKey: "inputRadius")
                cursorWindow.contentView?.layer?.filters = nil
            }
            try present(origin: relativeOrigin + frame.origin, snapshot: snapshot, action: actionIdentity)
            if animated { beginFade(visible: true, action: actionIdentity) }
        } catch {
            if currentAction == actionIdentity { invalidate(error) }
            throw error
        }
    }

    func updateWallpapers(_ samples: [Wallpaper], actionIdentity: UUID) throws {
        _ = try requireAction(actionIdentity)
        do {
            let images = try validatedWallpapers(samples, action: actionIdentity)
            if let screen = cursorWindow.screen {
                try requireWallpaper(screen, images: images, action: actionIdentity)
            }
            guard currentAction == actionIdentity, !released else { throw CancellationError() }
            wallpapers = images
            try refresh(actionIdentity: actionIdentity)
        } catch {
            if currentAction == actionIdentity { invalidate(error) }
            throw error
        }
    }

    func setPresentation(pressed: Bool, activity: Activity, attached: Bool, actionIdentity: UUID) throws {
        _ = try requireAction(actionIdentity)
        state.pressed = pressed
        state.activity = activity
        state.attached = attached
    }

    func refresh(actionIdentity: UUID) throws {
        let snapshot = try requireAction(actionIdentity)
        do {
            guard wantsVisible else { return }
            if let origin = relativeOrigin {
                let frame = try appKitRect(snapshot.window.bounds)
                try present(origin: origin + frame.origin, snapshot: snapshot, action: actionIdentity)
            }
        } catch {
            if currentAction == actionIdentity { invalidate(error) }
            throw error
        }
    }

    func hide(actionIdentity: UUID, animated: Bool = true) throws {
        _ = try requireAction(actionIdentity)
        wantsVisible = false
        raisedForMotion = false
        state.presented = false
        stopMovement(error: CancellationError())
        _ = try requireAction(actionIdentity)
        guard !wantsVisible else { throw CancellationError() }
        if animated && cursorWindow.isVisible {
            beginFade(visible: false, action: actionIdentity)
        } else {
            cancelFade()
            cursorWindow.orderOut(nil)
        }
    }

    func cancel(actionIdentity: UUID) {
        guard currentAction == actionIdentity else { return }
        invalidate(CancellationError())
    }

    func invalidate(_ error: Error) {
        guard !released else { return }
        let action = currentAction
        let hadMovement = movement != nil
        cancelFade()
        wantsVisible = false
        raisedForMotion = false
        state.presented = false
        cursorWindow.orderOut(nil)
        wallpapers.removeAll()
        relativeOrigin = nil
        currentAction = nil
        stopMovement(error: error)
        if !hadMovement, let action { onEvent(.interrupted(actionIdentity: action, movementIdentity: nil, error: error)) }
    }

    func captureSnapshot(app: DesktopAppRef, windowID: CGWindowID, incarnation: String) throws -> DesktopAppPIPRenderer.CursorSnapshot? {
        guard !released, let action = currentAction, cursorWindow.isVisible else { return nil }
        let snapshot = try requireAction(action)
        guard snapshot.window.app == app, target.windowID == windowID, windowGeneration == incarnation,
              cursorWindow.windowNumber > 0, UInt32(exactly: cursorWindow.windowNumber) != nil else {
            throw DesktopFailure(code: "cursor_capture_identity", message: "Still-image cursor is not owned by the exact application window incarnation")
        }
        return DesktopAppPIPRenderer.CursorSnapshot(nativeWindow: cursorWindow, targetWindowID: target.windowID,
            targetIncarnation: windowGeneration, actionIdentity: action)
    }

    func pipSnapshot() throws -> DesktopAppPIPRenderer.CursorSnapshot? {
        guard !released, let action = currentAction, cursorWindow.isVisible else { return nil }
        _ = try requireAction(action)
        return DesktopAppPIPRenderer.CursorSnapshot(nativeWindow: cursorWindow, targetWindowID: target.windowID,
            targetIncarnation: windowGeneration, actionIdentity: action)
    }

    func release() {
        guard !released else { return }
        invalidate(DesktopFailure(code: "cursor_released", message: "Cursor presentation was released"))
        guard !released else { return }
        released = true
        terminationObservation?.invalidate()
        terminationObservation = nil
        activeObservation?.invalidate()
        activeObservation = nil
        if let workspaceObservation { NSWorkspace.shared.notificationCenter.removeObserver(workspaceObservation) }
        workspaceObservation = nil
        displayTarget.owner = nil
        cursorWindow.contentViewController = nil
        cursorWindow.close()
    }

    private func validated(_ action: UUID) throws -> Snapshot {
        guard !released, !application.isTerminated else {
            throw DesktopFailure(code: "cursor_target_unavailable", message: "Cursor application or presentation lifetime has ended")
        }
        let snapshot = try check(action)
        guard !released, !application.isTerminated, snapshot.window.app == target.app,
              snapshot.window.windowID == target.windowID,
              CFEqual(snapshot.window.element, target.element),
              snapshot.windowGeneration == windowGeneration,
              snapshot.processID == application.processIdentifier,
              snapshot.isOnScreen, snapshot.window.bounds.width > 0, snapshot.window.bounds.height > 0,
              [snapshot.window.bounds.minX, snapshot.window.bounds.minY,
               snapshot.window.bounds.width, snapshot.window.bounds.height].allSatisfy({ $0.isFinite }) else {
            throw DesktopFailure(code: "cursor_window_changed", message: "Cursor presentation no longer identifies the validated native window generation")
        }
        return snapshot
    }

    private func requireAction(_ action: UUID) throws -> Snapshot {
        guard currentAction == action else {
            throw DesktopFailure(code: "cursor_action_changed", message: "Cursor presentation belongs to another action generation")
        }
        do {
            let snapshot = try validated(action)
            guard currentAction == action else { throw CancellationError() }
            return snapshot
        } catch {
            if currentAction == action { invalidate(error) }
            throw error
        }
    }

    private func validatedWallpapers(_ samples: [Wallpaper], action: UUID) throws -> [CGDirectDisplayID: Wallpaper] {
        var images: [CGDirectDisplayID: Wallpaper] = [:]
        for sample in samples {
            guard images[sample.displayID] == nil, !sample.captureGeneration.isEmpty,
                  sample.backingScale.isFinite, sample.backingScale > 0,
                  sample.screenFrame.width > 0, sample.screenFrame.height > 0,
                  sample.image.width > 0, sample.image.height > 0 else {
                throw DesktopFailure(code: "cursor_wallpaper_invalid", message: "Cursor wallpaper requires one exact native capture per display")
            }
            try checkWallpaper(action, sample.displayID, sample.captureGeneration)
            images[sample.displayID] = sample
        }
        return images
    }

    private func requireWallpaper(_ screen: NSScreen, images: [CGDirectDisplayID: Wallpaper], action: UUID) throws {
        let id = try displayID(screen)
        guard let image = images[id], image.screenFrame == screen.frame,
              image.backingScale == screen.backingScaleFactor,
              CGFloat(image.image.width) == floor(screen.frame.width * screen.backingScaleFactor),
              CGFloat(image.image.height) == floor(screen.frame.height * screen.backingScaleFactor) else {
            throw DesktopFailure(code: "cursor_wallpaper_unavailable", message: "Fog requires a current wallpaper image for the exact native display geometry")
        }
        try checkWallpaper(action, id, image.captureGeneration)
    }

    private func startMovement(point: CGPoint, screen: NSScreen,
                               action: UUID, identity: UUID, images: [CGDirectDisplayID: Wallpaper],
                               timing: NextInteractionTiming, animated: Bool, fadeIn: Bool,
                               continuation pending: CheckedContinuation<Receipt, Error>) throws {
        let fresh = try validated(action)
        let frame = try appKitRect(fresh.window.bounds)
        cursorWindow.contentView?.layoutSubtreeIfNeeded()
        guard cursorWindow.frame.width > 0, cursorWindow.frame.height > 0 else {
            throw DesktopFailure(code: "cursor_layout_unavailable", message: "Native cursor view has no intrinsic geometry")
        }
        let hotspot = CGPoint(x: cursorWindow.frame.width * 0.5, y: cursorWindow.frame.height * 0.5)
        let end = CGPoint(x: point.x - hotspot.x, y: point.y - cursorWindow.frame.height + hotspot.y) - frame.origin
        let wasVisible = cursorWindow.isVisible && wantsVisible
        let start = wasVisible ? (relativeOrigin ?? cursorWindow.frame.origin - frame.origin)
            : (Self.lastGlobalOrigin.map { $0 - frame.origin } ?? end)
        let startHotspot = start + frame.origin + hotspot
        let startCovered = NSWindow.windowNumber(at: startHotspot, belowWindowWithWindowNumber: 0) != Int(target.windowID)
        let endCovered = NSWindow.windowNumber(at: point, belowWindowWithWindowNumber: 0) != Int(target.windowID)
        let animateMovement = animated && start != end && !(startCovered && endCovered)
        stopMovement(error: DesktopFailure(code: "cursor_movement_superseded", message: "A newer action replaced cursor movement"))
        _ = try validated(action)
        cancelFade()
        currentAction = action
        wallpapers = images
        wantsVisible = true
        state.presented = true
        raisedForMotion = animateMovement
        let bounds = screen.frame.insetBy(dx: 20, dy: 20).offsetBy(dx: -frame.minX, dy: -frame.minY)
        let next = DesktopCursorMovement(identity: identity, action: action, start: start, end: end,
                                         bounds: bounds, timing: timing, state: state)
        movement = next
        continuation = pending
        continuationMovement = identity
        relativeOrigin = start
        if !wasVisible && fadeIn {
            cursorWindow.alphaValue = 0
            fadeFilter.setValue(7, forKey: "inputRadius")
        } else {
            cursorWindow.alphaValue = 1
            fadeFilter.setValue(0, forKey: "inputRadius")
            cursorWindow.contentView?.layer?.filters = nil
        }
        do {
            try present(origin: (animateMovement ? start : end) + frame.origin, snapshot: fresh, action: action)
            if !wasVisible && fadeIn { beginFade(visible: true, action: action) }
            if animateMovement {
                let link = cursorWindow.displayLink(target: displayTarget, selector: #selector(DesktopCursorDisplayTarget.displayFrame(_:)))
                displayLink = link
                link.add(to: .main, forMode: .common)
            } else {
                relativeOrigin = end
                state.resetMotion()
                try complete(next, timestamp: CACurrentMediaTime(), provenance: .synchronousCompletion)
            }
        } catch {
            invalidate(error)
        }
    }

    fileprivate func displayFrame(_ link: CADisplayLink) {
        guard displayLink === link, let move = movement, currentAction == move.action else { return }
        do {
            let snapshot = try requireAction(move.action)
            let sample = move.advance(timestamp: link.timestamp, targetTimestamp: link.targetTimestamp, state: state)
            guard movement === move, currentAction == move.action else { return }
            let frame = try appKitRect(snapshot.window.bounds)
            relativeOrigin = sample.point
            try present(origin: sample.point + frame.origin, snapshot: snapshot, action: move.action)
            if sample.finished {
                try complete(move, timestamp: link.timestamp, provenance: .displayLink)
            } else if !move.interactionReported, move.timing == .closeEnough,
                      sample.progress >= 0.995, (sample.point - move.end).cursorLength <= 3.157 {
                move.interactionReported = true
                try emit(move, milestone: .closeEnough, timestamp: link.timestamp, provenance: .displayLink)
            }
        } catch {
            invalidate(error)
        }
    }

    private func complete(_ move: DesktopCursorMovement, timestamp: CFTimeInterval, provenance: CallbackProvenance) throws {
        guard movement === move else { return }
        let snapshot = try requireAction(move.action)
        let frame = try appKitRect(snapshot.window.bounds)
        relativeOrigin = move.end
        raisedForMotion = false
        state.resetMotion()
        try present(origin: move.end + frame.origin, snapshot: snapshot, action: move.action)
        displayLink?.invalidate()
        displayLink = nil
        try emit(move, milestone: .finished, timestamp: timestamp, provenance: provenance)
        if movement === move { movement = nil }
    }

    private func emit(_ move: DesktopCursorMovement, milestone: Milestone, timestamp: CFTimeInterval, provenance: CallbackProvenance) throws {
        guard movement === move, currentAction == move.action, wantsVisible, cursorWindow.isVisible else {
            throw DesktopFailure(code: "cursor_presentation_cancelled", message: "Cursor presentation changed before its animation callback")
        }
        _ = try requireAction(move.action)
        guard movement === move, currentAction == move.action, wantsVisible, cursorWindow.isVisible else { throw CancellationError() }
        cursorWindow.contentView?.displayIfNeeded()
        CATransaction.flush()
        let position = CGPoint(x: cursorWindow.frame.midX, y: cursorWindow.frame.midY)
        let receipt = Receipt(app: target.app, windowID: target.windowID, windowGeneration: windowGeneration,
                              actionIdentity: move.action, movementIdentity: move.identity, milestone: milestone,
                              cgScreenPoint: try appKitPoint(position), callbackProvenance: provenance, callbackTimestamp: timestamp)
        if continuationMovement == move.identity, let pending = continuation {
            continuation = nil
            continuationMovement = nil
            pending.resume(returning: receipt)
        }
        if milestone == .finished { movement = nil }
        onEvent(.presented(receipt))
    }

    private func present(origin: CGPoint, snapshot: Snapshot, action: UUID) throws {
        guard wantsVisible, currentAction == action else { throw CancellationError() }
        let center = CGPoint(x: origin.x + cursorWindow.frame.width * 0.5,
                             y: origin.y + cursorWindow.frame.height * 0.5)
        let display = try screen(containing: center)
        try requireWallpaper(display, images: wallpapers, action: action)
        guard !released, wantsVisible, currentAction == action else { throw CancellationError() }
        guard let wallpaper = wallpapers[try displayID(display)] else { throw CancellationError() }
        cursorWindow.setFrameOrigin(CGPoint(x: origin.x.rounded(), y: origin.y.rounded()))
        Self.lastGlobalOrigin = origin
        if raisedForMotion {
            let below = NSWindow.windowNumber(at: center, belowWindowWithWindowNumber: cursorWindow.windowNumber)
            if below == Int(target.windowID) { raisedForMotion = false }
        }
        if snapshot.applicationIsActive || snapshot.menusOpen || raisedForMotion {
            try ordering.raise(cursorWindow)
            cursorWindow.orderFront(nil)
        } else {
            ordering.clearOverlayState()
            cursorWindow.level = NSWindow.Level(rawValue: snapshot.windowLevel)
            cursorWindow.order(.above, relativeTo: Int(target.windowID))
        }
        fog.update(image: wallpaper.image, screen: display)
        cursorWindow.contentView?.layoutSubtreeIfNeeded()
        fog.updateGeometry()
        cursorWindow.contentView?.displayIfNeeded()
    }

    private func cancelMovement(_ identity: UUID, error: Error) {
        guard movement?.identity == identity else { return }
        invalidate(error)
    }

    private func stopMovement(error: Error) {
        let old = movement
        movement = nil
        displayLink?.invalidate()
        displayLink = nil
        state.resetMotion()
        if let pending = continuation {
            continuation = nil
            continuationMovement = nil
            pending.resume(throwing: error)
        }
        if let old { onEvent(.interrupted(actionIdentity: old.action, movementIdentity: old.identity, error: error)) }
    }

    private func cancelFade() {
        fadeIdentity = UUID()
        fadeDelegate = nil
        cursorWindow.contentView?.layer?.removeAnimation(forKey: "cursorBlurTransition")
    }

    private func beginFade(visible: Bool, action: UUID) {
        cancelFade()
        let identity = fadeIdentity
        let layer = cursorWindow.contentView?.layer
        let path = "filters.gaussianBlur.inputRadius"
        let radius = (layer?.presentation()?.value(forKeyPath: path) as? NSNumber)?.doubleValue
            ?? (fadeFilter.value(forKey: "inputRadius") as? NSNumber)?.doubleValue ?? 0
        let animation = CABasicAnimation(keyPath: path)
        animation.fromValue = radius
        animation.toValue = visible ? 0 : 7
        animation.duration = 0.44
        animation.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
        let delegate = DesktopCursorAnimationDelegate { [weak self] finished in
            guard let self, self.fadeIdentity == identity, self.currentAction == action else { return }
            self.fadeDelegate = nil
            if !visible { self.cursorWindow.orderOut(nil) }
            guard finished else { return }
            if visible { self.cursorWindow.contentView?.layer?.filters = nil }
            do { _ = try self.requireAction(action) }
            catch { self.invalidate(error) }
        }
        fadeDelegate = delegate
        animation.delegate = delegate
        layer?.filters = [fadeFilter]
        fadeFilter.setValue(visible ? 0 : 7, forKey: "inputRadius")
        layer?.add(animation, forKey: "cursorBlurTransition")
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.44
            context.timingFunction = CAMediaTimingFunction(name: .easeInEaseOut)
            cursorWindow.animator().alphaValue = visible ? 1 : 0
        }
    }

    private func refreshFromNotification() {
        guard let currentAction else { return }
        do { try refresh(actionIdentity: currentAction) }
        catch { invalidate(error) }
    }

    private func appKitPoint(_ point: CGPoint) throws -> CGPoint {
        guard point.x.isFinite, point.y.isFinite,
              let zeroScreen = NSScreen.screens.first(where: { $0.frame.contains(CGPoint.zero) }) else {
            throw DesktopFailure(code: "cursor_screen_unavailable", message: "Native display coordinates are unavailable")
        }
        return CGPoint(x: point.x, y: zeroScreen.frame.maxY - point.y)
    }

    private func appKitRect(_ rect: CGRect) throws -> CGRect {
        let origin = try appKitPoint(CGPoint(x: rect.minX, y: rect.maxY))
        return CGRect(origin: origin, size: rect.size)
    }

    private func screen(containing point: CGPoint) throws -> NSScreen {
        guard let screen = NSScreen.screens.first(where: { $0.frame.contains(point) }) else {
            throw DesktopFailure(code: "cursor_screen_unavailable", message: "Cursor presentation point is outside the native displays")
        }
        return screen
    }

    private func displayID(_ screen: NSScreen) throws -> CGDirectDisplayID {
        guard let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber else {
            throw DesktopFailure(code: "cursor_screen_unavailable", message: "Native display identity is unavailable")
        }
        return number.uint32Value
    }
}

@MainActor
private final class DesktopCursorWindow: NSWindow {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

@MainActor
private final class DesktopCursorDisplayTarget: NSObject {
    weak var owner: DesktopAppCursor?
    @objc func displayFrame(_ link: CADisplayLink) { owner?.displayFrame(link) }
}

private final class DesktopCursorAnimationDelegate: NSObject, CAAnimationDelegate {
    private let completion: @MainActor (Bool) -> Void
    init(completion: @escaping @MainActor (Bool) -> Void) { self.completion = completion }
    func animationDidStop(_ anim: CAAnimation, finished flag: Bool) {
        let completion = completion
        Task { @MainActor in completion(flag) }
    }
}

private struct DesktopCursorSpring {
    var value: SIMD2<Double>
    var target: SIMD2<Double>
    var velocity = SIMD2<Double>.zero
    var acceleration = SIMD2<Double>.zero
    let stiffness: Double
    let damping: Double
    let idleVelocity: Double
    var time: CFTimeInterval?

    init(value: SIMD2<Double>, target: SIMD2<Double>, response: Double, damping: Double, idleVelocity: Double) {
        self.value = value
        self.target = target
        stiffness = min(pow(2 * .pi / response, 2), 28800)
        self.damping = 2 * damping * sqrt(stiffness)
        self.idleVelocity = idleVelocity
    }

    mutating func advance(timestamp: CFTimeInterval, to now: CFTimeInterval) {
        var previous = time ?? timestamp
        if now - previous > 1 { previous = now - 1 / 60 }
        let dt = 1.0 / 240
        while previous < now {
            let half = velocity + acceleration * (dt * 0.5)
            value += half * dt
            acceleration = -damping * half + stiffness * (target - value)
            velocity = half + acceleration * (dt * 0.5)
            previous += dt
        }
        time = previous
    }

    var settled: Bool {
        let threshold = (target * 0.01).cursorSquared
        return max(velocity.cursorSquared, acceleration.cursorSquared) <= idleVelocity * idleVelocity
            && (threshold <= 0 || (target - value).cursorSquared <= threshold)
    }
}

@MainActor
private final class DesktopCursorMovement {
    struct Sample { let point: CGPoint; let progress: Double; let finished: Bool }
    let identity: UUID
    let action: UUID
    let start: CGPoint
    let end: CGPoint
    let timing: DesktopAppCursor.NextInteractionTiming
    var interactionReported = false
    private let curve: DesktopCursorPath?
    private var position: DesktopCursorSpring
    private var axis: DesktopCursorSpring
    private var base: DesktopCursorSpring
    private var squash: DesktopCursorSpring
    private var tilt: DesktopCursorSpring
    private let direction: SIMD2<Double>
    private let axisPivot: Double

    init(identity: UUID, action: UUID, start: CGPoint, end: CGPoint, bounds: CGRect,
         timing: DesktopAppCursor.NextInteractionTiming, state: DesktopCursorState) {
        self.identity = identity
        self.action = action
        self.start = start
        self.end = end
        self.timing = timing
        let delta = end.cursorVector - start.cursorVector
        let length = sqrt(delta.cursorSquared)
        direction = length < 0.001 ? SIMD2(1, 0) : delta / length
        let path = length > 196 ? DesktopCursorPath.choose(start: start, end: end, bounds: bounds) : nil
        curve = path
        position = DesktopCursorSpring(value: path == nil ? start.cursorVector : .zero,
                                       target: path == nil ? end.cursorVector : SIMD2(1, 0),
                                       response: path?.response ?? 0.24,
                                       damping: path == nil ? 0.84 : 0.9,
                                       idleVelocity: path == nil ? 12 : 0.01)
        var axisAngle = atan2(direction.y, direction.x) * 180 / .pi
        var pivot = 1.0
        while axisAngle > 90 { axisAngle -= 180; pivot = 1 - pivot }
        while axisAngle <= -90 { axisAngle += 180; pivot = 1 - pivot }
        axisPivot = length < 0.001 ? 0.5 : pivot
        axis = DesktopCursorSpring(value: SIMD2(state.axisAngle * 180 / .pi, 0),
                                   target: SIMD2(Self.nearest(axisAngle, to: state.axisAngle * 180 / .pi, period: 360), 0),
                                   response: 0.07, damping: 0.82, idleVelocity: 0.01)
        base = DesktopCursorSpring(value: SIMD2(Self.nearest(state.angle * 180 / .pi, to: 0, period: 360), 0),
                                   target: .zero, response: 0.09, damping: 0.86, idleVelocity: 0.01)
        squash = DesktopCursorSpring(value: SIMD2(state.yScale, 0), target: SIMD2(1, 0),
                                     response: 0.095, damping: 0.72, idleVelocity: 0.01)
        tilt = DesktopCursorSpring(value: SIMD2(state.tilt * 180 / .pi, 0), target: .zero,
                                   response: 0.055, damping: 0.76, idleVelocity: 0.01)
    }

    func advance(timestamp: CFTimeInterval, targetTimestamp time: CFTimeInterval, state: DesktopCursorState) -> Sample {
        position.advance(timestamp: timestamp, to: time)
        if let curve {
            let progress = cursorClamp(position.value.x, 0, 1)
            let sample = curve.sample(progress)
            let velocity = sample.derivative * position.velocity.x * Double(curve.segments.count)
            state.velocityX = velocity.x
            state.velocityY = velocity.y
            let tangent = sample.derivative.cursorUnit(or: DesktopCursorPath.clickDirection)
            let blend = 1 - pow(1 - cursorClamp((progress - 0.99) / 0.01, 0, 1), 3)
            let direction = (tangent * (1 - blend) + DesktopCursorPath.clickDirection * blend)
                .cursorUnit(or: DesktopCursorPath.clickDirection)
            state.angle = (atan2(-direction.y, direction.x) * 180 / .pi + 90 + 44) * .pi / 180
            return Sample(point: position.settled ? end : sample.point.cursorPoint,
                          progress: progress, finished: position.settled)
        }
        let delta = end.cursorVector - start.cursorVector
        let progress = delta.cursorSquared < 0.001 ? 1
            : cursorClamp((position.value - start.cursorVector).cursorDot(delta) / delta.cursorSquared, 0, 1)
        let envelope = cursorClamp(min(progress / 0.22, 1) * pow(max(1 - progress, 0), 0.62), 0, 1)
        squash.target = SIMD2(1 - envelope * 0.18, 0)
        tilt.target = SIMD2(cursorClamp(0.75 * direction.x + 0.62 * direction.y, -1, 1) * 76 * envelope, 0)
        axis.advance(timestamp: timestamp, to: time)
        base.advance(timestamp: timestamp, to: time)
        squash.advance(timestamp: timestamp, to: time)
        tilt.advance(timestamp: timestamp, to: time)
        state.velocityX = 0
        state.velocityY = 0
        state.angle = base.value.x * .pi / 180
        state.axisAngle = axis.value.x * .pi / 180
        state.xScale = 1 + envelope * 0.38
        state.yScale = cursorClamp(squash.value.x, 0, 1)
        state.pivot = (1 - envelope) * 0.5 + envelope * axisPivot
        state.tilt = tilt.value.x * .pi / 180
        return Sample(point: position.settled ? end : position.value.cursorPoint,
                      progress: progress, finished: position.settled)
    }

    private static func nearest(_ angle: Double, to reference: Double, period: Double) -> Double {
        var result = angle
        while result - reference > period * 0.5 { result -= period }
        while result - reference <= -period * 0.5 { result += period }
        return result
    }
}

private struct DesktopCursorPath {
    struct Segment {
        let start: SIMD2<Double>
        let first: SIMD2<Double>
        let second: SIMD2<Double>
        let end: SIMD2<Double>
        func sample(_ t: Double) -> (point: SIMD2<Double>, derivative: SIMD2<Double>) {
            let u = 1 - t
            return (start * (u * u * u) + first * (3 * u * u * t)
                    + second * (3 * u * t * t) + end * (t * t * t),
                    (first - start) * (3 * u * u) + (second - first) * (6 * u * t)
                    + (end - second) * (3 * t * t))
        }
    }
    static let clickDirection = SIMD2(sin(-44 * Double.pi / 180), cos(-44 * Double.pi / 180))
    let segments: [Segment]
    var response: Double = 0

    func sample(_ progress: Double) -> (point: SIMD2<Double>, derivative: SIMD2<Double>) {
        let scaled = cursorClamp(progress, 0, 1) * Double(segments.count)
        let index = min(Int(scaled), segments.count - 1)
        return segments[index].sample(scaled - Double(index))
    }

    private struct Metrics {
        var length = 0.0
        var energy = 0.0
        var maximumTurn = 0.0
        var totalTurn = 0.0
        var withinBounds = true
    }

    private func metrics(bounds: CGRect) -> Metrics {
        var result = Metrics()
        var previous = segments[0].start
        var previousAngle: Double?
        result.withinBounds = bounds.contains(previous.cursorPoint)
        for segment in segments {
            for index in 1...24 {
                let point = segment.sample(Double(index) / 24).point
                let delta = point - previous
                let distance = sqrt(delta.cursorSquared)
                result.length += distance
                result.withinBounds = result.withinBounds && bounds.contains(point.cursorPoint)
                if distance > 0.01 {
                    let angle = atan2(delta.y, delta.x)
                    if let old = previousAngle {
                        var turn = angle - old
                        while turn > .pi { turn -= 2 * .pi }
                        while turn < -.pi { turn += 2 * .pi }
                        result.energy += turn * turn
                        result.maximumTurn = max(result.maximumTurn, abs(turn))
                        result.totalTurn += abs(turn)
                    }
                    previousAngle = angle
                }
                previous = point
            }
        }
        return result
    }

    static func choose(start: CGPoint, end: CGPoint, bounds: CGRect) -> DesktopCursorPath {
        let start = start.cursorVector
        let end = end.cursorVector
        let delta = end - start
        let length = sqrt(delta.cursorSquared)
        let direction = delta.cursorUnit(or: SIMD2(1, 0))
        if length <= 10 {
            return DesktopCursorPath(segments: [Segment(start: start, first: start + delta / 3,
                                                       second: start + delta * (2.0 / 3), end: end)])
        }
        let handleLimit = min(640, length * 0.9)
        let startHandle = max(min(length * 0.41960295031576633, handleLimit), 48)
        let endHandle = max(min(length * 0.15, handleLimit), 48)
        let first = start + clickDirection * startHandle
        let last = end - clickDirection * endHandle
        var candidates = [DesktopCursorPath(segments: [Segment(start: start, first: first, second: last, end: end)]),
                          DesktopCursorPath(segments: [Segment(start: start, first: start + clickDirection * startHandle * 0.65,
                                                              second: end - clickDirection * endHandle * 0.65, end: end)])]
        let side = clickDirection.y * direction.x - clickDirection.x * direction.y >= 0 ? 1.0 : -1.0
        let perpendicular = SIMD2(-direction.y, direction.x) * side
        let midpoint = (start + end) * 0.5 + clickDirection * startHandle * 0.16
        let arc = cursorClamp(length * 0.2765523188064277, 50, 520)
        let flow = cursorClamp(length * 0.5783555327868779, 38, 440)
        for arcScale in [0.55, 0.8, 1.05] {
            for flowScale in [0.65, 1.0, 1.35] {
                for sign in [1.0, -1.0] {
                    let middle = midpoint + perpendicular * (arc * arcScale * sign)
                    candidates.append(DesktopCursorPath(segments: [
                        Segment(start: start, first: first, second: middle - direction * flow * flowScale, end: middle),
                        Segment(start: middle, first: middle + direction * flow * flowScale, second: last, end: end)
                    ]))
                }
            }
        }
        let measured = candidates.prefix(20).map { ($0, $0.metrics(bounds: bounds)) }
        let inBounds = measured.filter { $0.1.withinBounds }
        let eligible = inBounds.isEmpty ? measured : inBounds
        let reverse = cursorClamp((-0.08 - direction.cursorDot(clickDirection)) / 0.92, 0, 1)
        func score(_ row: (DesktopCursorPath, Metrics)) -> Double {
            let m = row.1
            return m.length + max(m.length / max(length, 1) - 1, 0) * 320 + m.energy * 140
                + m.maximumTurn * 180 + m.totalTurn * 18 + reverse * 90 + (row.0.segments.count > 1 ? 45 : 0)
        }
        let selected = eligible.dropFirst().reduce(eligible[0]) { score($1) < score($0) ? $1 : $0 }
        var path = selected.0
        let m = selected.1
        let lengthFactor = cursorClamp((m.length - 180) / 760, 0, 1)
        let detour = cursorClamp(max(m.length / max(length, 1) - 1, 0) / 0.55, 0, 1)
        let turn = cursorClamp(m.totalTurn / 4.39822971502571, 0, 1)
        let energy = cursorClamp(m.energy / 1.25, 0, 1)
        let complexity = cursorClamp(0.42 * detour + 0.38 * turn + 0.2 * energy, 0, 1)
        let hasMiddle = path.segments.count > 1
        let raw = 0.42 + 0.22 * lengthFactor + 0.12 * complexity + 0.28 * reverse + (hasMiddle ? 0.04 : 0)
        path.response = cursorClamp(raw * 0.9 * (hasMiddle ? 0.9 : 1), 0.12, 2.2)
        return path
    }
}

@MainActor
@Observable
private final class DesktopCursorState {
    var velocityX = 0.0
    var velocityY = 0.0
    var pressed = false
    var activity = DesktopAppCursor.Activity.idle
    var attached = false
    var angle = 0.0
    var xScale = 1.0
    var yScale = 1.0
    var pivot = 0.5
    var axisAngle = 0.0
    var tilt = 0.0
    var presented = false
    func resetMotion() {
        velocityX = 0
        velocityY = 0
        angle = 0
        xScale = 1
        yScale = 1
        pivot = 0.5
        axisAngle = 0
        tilt = 0
    }
}

private struct DesktopCursorShape: Shape {
    func path(in rect: CGRect) -> Path {
        func p(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: x * rect.width, y: y * rect.height) }
        var path = Path()
        path.move(to: p(0.00599, 0.15864))
        path.addCurve(to: p(0.15158, 0.00627), control1: p(-0.02364, 0.06456), control2: p(0.06169, -0.02474))
        path.addLine(to: p(0.87634, 0.25652))
        path.addCurve(to: p(0.88794, 0.48095), control1: p(0.97594, 0.29096), control2: p(0.9834, 0.43547))
        path.addLine(to: p(0.59343, 0.62108))
        path.addLine(to: p(0.45955, 0.92925))
        path.addCurve(to: p(0.2451, 0.91717), control1: p(0.41611, 1.02925), control2: p(0.27801, 1.02146))
        path.addLine(to: p(0.00599, 0.15864))
        path.closeSubpath()
        return path
    }
}

private struct DesktopCursorVelocityTransform: ViewModifier {
    let angle: Double
    let stretch: Double
    func body(content: Content) -> some View {
        content.rotationEffect(.radians(angle)).scaleEffect(x: stretch, y: 1)
            .rotationEffect(.radians(-angle)).scaleEffect(1 / stretch)
    }
}

@MainActor
private struct DesktopCursorView: View {
    let state: DesktopCursorState
    let fog: DesktopCursorFogView
    @State private var wiggle = 0.0
    @State private var wiggleGeneration = UUID()
    private let spring = Animation.interpolatingSpring(mass: 1, stiffness: 130.5, damping: 18.85, initialVelocity: 0)
    private var angle: Double { state.angle + wiggle * .pi / 180 }
    private var scale: Double { (state.attached ? 1 : 0.75) * (state.pressed ? 0.7 : 1) }
    private var velocity: DesktopCursorVelocityTransform {
        DesktopCursorVelocityTransform(angle: angle, stretch: 1 + 1.5 * cursorClamp(abs((state.velocityX + state.velocityY) * 0.5) / 3000, 0, 1))
    }

    private func cursor<V: View>(_ view: V) -> some View {
        view.rotationEffect(.radians(angle), anchor: UnitPoint(x: (9 - 1.7) / 18, y: (9 - 0.9) / 18))
            .rotationEffect(.radians(state.tilt), anchor: UnitPoint(x: 0.00599, y: 0.15864))
            .modifier(velocity).frame(width: 18, height: 18)
            .scaleEffect(scale, anchor: UnitPoint(x: 0.1, y: 0.1))
    }

    var body: some View {
        cursor(ZStack {
            DesktopCursorShape().opacity(state.activity == .loading ? 0.24 : 0.16)
            DesktopCursorShape().stroke(Color.white.opacity(0.8), lineWidth: 2)
        })
        .frame(width: 126, height: 126)
        .background(DesktopCursorFogSurface(view: fog).ignoresSafeArea())
        .opacity(state.activity == .paused ? 0.5 : 1)
        .mask(ZStack {
            cursor(DesktopCursorShape())
            Circle().modifier(velocity).frame(width: 42, height: 42).blur(radius: 12)
                .offset(x: -3, y: -2).scaleEffect(scale, anchor: UnitPoint(x: 0.25, y: 0.25))
        })
        .compositingGroup()
        .rotationEffect(.radians(-state.axisAngle))
        .scaleEffect(x: state.xScale, y: state.yScale,
                     anchor: UnitPoint(x: (54 + 18 * cursorClamp(state.pivot, 0, 1)) / 126, y: 0.5))
        .rotationEffect(.radians(state.axisAngle))
        .animation(spring, value: state.velocityX)
        .animation(spring, value: state.velocityY)
        .animation(spring, value: state.pressed)
        .animation(nil, value: state.angle)
        .animation(spring, value: state.attached)
        .animation(.easeInOut(duration: 0.2), value: state.activity)
        .animation(nil, value: state.xScale)
        .animation(nil, value: state.yScale)
        .animation(nil, value: state.pivot)
        .animation(nil, value: state.axisAngle)
        .animation(nil, value: state.tilt)
        .offset(x: 4.5, y: 4.5)
        .allowsHitTesting(false)
        .onAppear { updateWiggle() }
        .onChange(of: state.activity) { _, _ in updateWiggle() }
        .onChange(of: state.presented) { _, _ in updateWiggle() }
        .onDisappear { wiggleGeneration = UUID() }
    }

    private func updateWiggle() {
        let generation = UUID()
        wiggleGeneration = generation
        guard state.presented, state.activity == .loading else {
            withAnimation(.easeInOut(duration: 0.41)) { wiggle = 0 }
            return
        }
        let destination = abs(wiggle - 10) < abs(wiggle + 10) ? 10.0 : -10.0
        withAnimation(.easeInOut(duration: abs(destination - wiggle) / 20 * 0.684931506849315),
                      completionCriteria: .logicallyComplete) {
            wiggle = destination
        } completion: {
            guard wiggleGeneration == generation, state.presented, state.activity == .loading else { return }
            withAnimation(.easeInOut(duration: 0.684931506849315).repeatForever(autoreverses: true)) {
                wiggle = -destination
            }
        }
    }
}

@MainActor
private struct DesktopCursorFogSurface: NSViewRepresentable {
    let view: DesktopCursorFogView
    func makeNSView(context: Context) -> DesktopCursorFogView { view }
    func updateNSView(_ nsView: DesktopCursorFogView, context: Context) { nsView.updateGeometry() }
}

@MainActor
private final class DesktopCursorFogView: NSView {
    private let wallpaper = CALayer()
    private let shadowWallpaper = CALayer()
    private let shadowBackdrop = CALayer()
    private let gaussianBackdrop = CALayer()
    private let shadowSource: CALayer
    private let gaussianSource: CALayer
    private var screenFrame = CGRect.zero
    override var isFlipped: Bool { true }

    init(backdropGroup group: String) throws {
        shadowSource = try DesktopCursorFilters.backdrop(group: group + "_dynamicShadow")
        gaussianSource = try DesktopCursorFilters.backdrop(group: group + "_backdrop")
        super.init(frame: .zero)
        wantsLayer = true
        wallpaper.opacity = 0.39
        shadowWallpaper.opacity = 0.37 * 0.29
        shadowBackdrop.opacity = 0.37 * 0.59
        shadowWallpaper.filters = try DesktopCursorFilters.shadow()
        shadowBackdrop.filters = try DesktopCursorFilters.shadow()
        gaussianSource.filters = [try DesktopCursorFilters.make("gaussianBlur", values: [
            "inputRadius": 17, "inputHardEdges": true, "inputNormalizeEdges": true
        ])]
        shadowBackdrop.addSublayer(shadowSource)
        gaussianBackdrop.addSublayer(gaussianSource)
        for item in [wallpaper, shadowBackdrop, shadowWallpaper, gaussianBackdrop] { layer?.addSublayer(item) }
    }

    required init?(coder: NSCoder) { return nil }

    func update(image: CGImage, screen: NSScreen) {
        screenFrame = screen.frame
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        wallpaper.contents = image
        shadowWallpaper.contents = image
        wallpaper.contentsScale = screen.backingScaleFactor
        shadowWallpaper.contentsScale = screen.backingScaleFactor
        updateGeometry()
        CATransaction.commit()
    }

    override func layout() {
        super.layout()
        updateGeometry()
    }

    func updateGeometry() {
        guard let window, screenFrame.width > 0, screenFrame.height > 0 else { return }
        let viewFrame = convert(bounds, to: nil)
        let translation = CGAffineTransform(translationX: -viewFrame.minX - (window.frame.minX - screenFrame.minX),
                                            y: viewFrame.maxY + window.frame.minY - screenFrame.maxY)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        let frame = CGRect(origin: .zero, size: screenFrame.size)
        for item in [wallpaper, shadowWallpaper, shadowBackdrop] {
            item.setAffineTransform(.identity)
            item.frame = frame
            item.setAffineTransform(translation)
        }
        shadowSource.frame = frame
        gaussianBackdrop.frame = frame
        gaussianSource.frame = frame
        CATransaction.commit()
    }
}

private final class DesktopCursorWindowOrdering {
    private typealias Connection = @convention(c) () -> OpaquePointer?
    private typealias SetLevel = @convention(c) (OpaquePointer?, CGWindowID, Int32) -> CGError
    private let library: UnsafeMutableRawPointer
    private let connection: Connection
    private let setLevel: SetLevel
    private var overlay = false

    init() throws {
        guard let library = dlopen("/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics", RTLD_NOW | RTLD_LOCAL) else {
            throw DesktopFailure(code: "cursor_ordering_unavailable", message: "Native cursor window ordering is unavailable")
        }
        guard let connection = dlsym(library, "CGSMainConnectionID"), let level = dlsym(library, "CGSSetWindowLevel") else {
            dlclose(library)
            throw DesktopFailure(code: "cursor_ordering_unavailable", message: "Exact native cursor window ordering functions are unavailable")
        }
        self.library = library
        self.connection = unsafeBitCast(connection, to: Connection.self)
        setLevel = unsafeBitCast(level, to: SetLevel.self)
    }

    deinit { dlclose(library) }

    @MainActor
    func raise(_ window: NSWindow) throws {
        guard !overlay else { return }
        if window.windowNumber > 0 {
            guard let id = CGWindowID(exactly: window.windowNumber), let connection = connection() else {
                throw DesktopFailure(code: "cursor_ordering_unavailable", message: "The cursor's own native window connection is unavailable")
            }
            let result = setLevel(connection, id, CGWindowLevelForKey(.overlayWindow))
            guard result == .success else {
                throw DesktopFailure(code: "cursor_ordering_failed", message: "Native cursor window ordering failed (\(result.rawValue))")
            }
        }
        window.level = NSWindow.Level(rawValue: 102)
        overlay = true
    }

    func clearOverlayState() { overlay = false }
}

@MainActor
private enum DesktopCursorFilters {
    static func make(_ name: String, values: [String: Any]) throws -> NSObject {
        let selector = NSSelectorFromString("filterWithName:")
        guard let cls: AnyClass = NSClassFromString("CAFilter"),
              let method = class_getClassMethod(cls, selector) else {
            throw DesktopFailure(code: "cursor_filter_unavailable", message: "Native cursor filter creation is unavailable")
        }
        typealias Factory = @convention(c) (AnyClass, Selector, NSString) -> Unmanaged<AnyObject>?
        let create = unsafeBitCast(method_getImplementation(method), to: Factory.self)
        guard let filter = create(cls, selector, name as NSString)?.takeUnretainedValue() as? NSObject else {
            throw DesktopFailure(code: "cursor_filter_unavailable", message: "Native cursor filter \(name) is unavailable")
        }
        for (key, value) in values { filter.setValue(value, forKey: key) }
        return filter
    }

    static func shadow() throws -> [NSObject] {
        [try make("luminanceCurveMap", values: ["inputValues": [0.0, -2.0, 0.026172, 3.0], "inputAmount": 1]),
         try make("luminanceToAlpha", values: ["inputPremultipliedValues": false])]
    }

    static func backdrop(group: String) throws -> CALayer {
        guard let cls = NSClassFromString("CABackdropLayer") as? CALayer.Type else {
            throw DesktopFailure(code: "cursor_backdrop_unavailable", message: "The native cursor backdrop layer is unavailable")
        }
        let layer = cls.init()
        for name in ["setGroupName:", "setScale:", "setWindowServerAware:", "setAllowsInPlaceFiltering:"] {
            guard layer.responds(to: NSSelectorFromString(name)) else {
                throw DesktopFailure(code: "cursor_backdrop_unavailable", message: "Native cursor backdrop property \(name) is unavailable")
            }
        }
        layer.setValue(group, forKey: "groupName")
        layer.setValue(1.0, forKey: "scale")
        layer.setValue(true, forKey: "windowServerAware")
        layer.setValue(false, forKey: "allowsInPlaceFiltering")
        return layer
    }
}

private func cursorClamp(_ value: Double, _ lower: Double, _ upper: Double) -> Double {
    min(max(value, lower), upper)
}

private extension SIMD2 where Scalar == Double {
    var cursorSquared: Double { x * x + y * y }
    var cursorPoint: CGPoint { CGPoint(x: x, y: y) }
    func cursorDot(_ other: SIMD2<Double>) -> Double { x * other.x + y * other.y }
    func cursorUnit(or other: SIMD2<Double>) -> SIMD2<Double> {
        let length = sqrt(cursorSquared)
        return length < 0.001 ? other : self / length
    }
}

private extension CGPoint {
    var cursorVector: SIMD2<Double> { SIMD2(Double(x), Double(y)) }
    var cursorLength: Double { hypot(Double(x), Double(y)) }
    static func + (lhs: CGPoint, rhs: CGPoint) -> CGPoint { CGPoint(x: lhs.x + rhs.x, y: lhs.y + rhs.y) }
    static func - (lhs: CGPoint, rhs: CGPoint) -> CGPoint { CGPoint(x: lhs.x - rhs.x, y: lhs.y - rhs.y) }
}
