import AppKit
import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import QuartzCore
import ScreenCaptureKit

@MainActor
final class DesktopAppPIPObservation {
    private var cancel: (() -> Void)?
    init(cancel: @escaping () -> Void) { self.cancel = cancel }
    func invalidate() { let current = cancel; cancel = nil; current?() }
}

@MainActor
final class DesktopAppPIPRenderer {
    struct Window {
        let descriptor: DesktopAppCaptureWindow
        let incarnation: String
    }
    struct Snapshot {
        let application: NSRunningApplication
        let primary: Window
        let associated: [Window]
    }
    struct CursorSnapshot {
        let nativeWindow: NSWindow
        let targetWindowID: CGWindowID
        let targetIncarnation: String
        let actionIdentity: UUID
    }
    enum Event { case geometryChanged; case sourceUnavailable(Error) }
    struct Callbacks {
        let sourceResizeChanged: (CGSize) -> Void
        let sourceResizeSettled: (CGSize) -> Void
        let sourceUnavailable: (Error) -> Void
        let diagnostic: (String, Error?) -> Void
    }
    private struct Aspect {
        let sourceSize: CGSize
        let displaySize: CGSize
        let source: String
    }
    private struct CapturePlan {
        let filter: SCContentFilter
        let configuration: SCStreamConfiguration
        let bounds: CGRect
        let displaySize: CGSize
    }
    private struct CursorKey: Equatable {
        let windowID: CGWindowID
        let displayID: CGDirectDisplayID
        let sourceRect: CGRect
        let outputSize: CGSize
    }
    private struct CursorPlan {
        let key: CursorKey
        let filter: SCContentFilter
        let configuration: SCStreamConfiguration
        let crop: CGRect
        let target: CGRect
    }
    private let application: NSRunningApplication
    private let source: Window
    private let checkSource: () throws -> Snapshot
    private let cursorSource: () throws -> CursorSnapshot?
    private let observe: (@escaping (Event) -> Void) throws -> DesktopAppPIPObservation?
    private let callbacks: Callbacks
    private let mainLayer = AVSampleBufferDisplayLayer()
    private let cursorLayer = AVSampleBufferDisplayLayer()
    private let sampleQueue = DispatchQueue(label: "octopus.desktop.pip.sample")
    private weak var root: CALayer?
    private var mainStream: (SCStream, DesktopAppPIPFrameSink)?
    private var cursorStream: (SCStream, DesktopAppPIPFrameSink)?
    private var mainGeneration: UInt64 = 0
    private var cursorGeneration: UInt64 = 0
    private var geometryGeneration: UInt64 = 0
    private var cursorUpdateGeneration: UInt64 = 0
    private var maximumDisplaySize: CGFloat
    private var presentationSize: CGSize?
    private var latestAspect: (UInt64, Aspect)?
    private var currentSourceSize: CGSize?
    private var targetBounds: CGRect?
    private var cursorPlan: CursorPlan?
    private var associatedIDs: [UInt32] = []
    private var observation: DesktopAppPIPObservation?
    private var geometryTask: Task<Void, Never>?
    private var cursorTask: Task<Void, Never>?
    private var sizeTask: Task<Void, Never>?
    private var sizeWaiter: (UUID, CheckedContinuation<CGSize, Never>)?
    private var stopped = false
    private var unavailable = false
    private var suspended = false

    init(application: NSRunningApplication, source: Window, maximumDisplaySize: CGFloat,
         check: @escaping () throws -> Snapshot, cursor: @escaping () throws -> CursorSnapshot?,
         observe: @escaping (@escaping (Event) -> Void) throws -> DesktopAppPIPObservation?, callbacks: Callbacks) throws {
        guard !application.isTerminated, application.processIdentifier > 0,
              source.descriptor.cg_window_id != 0, !source.incarnation.isEmpty else {
            throw DesktopFailure(code: "pip_source_invalid", message: "PiP requires a retained native application and exact window incarnation")
        }
        self.application = application
        self.source = source
        self.maximumDisplaySize = Self.maximum(maximumDisplaySize)
        checkSource = check
        cursorSource = cursor
        self.observe = observe
        self.callbacks = callbacks
        let snapshot = try validatedSource()
        associatedIDs = snapshot.associated.map { $0.descriptor.cg_window_id }
    }

    func start(in producerRootLayer: CALayer) async throws -> CGSize {
        guard root == nil, mainStream == nil else {
            throw DesktopFailure(code: "pip_renderer_started", message: "PiP renderer is already attached to its producer context")
        }
        _ = try validatedSource()
        root = producerRootLayer
        do {
            let plan = try await capturePlan()
            _ = try validatedSource()
            targetBounds = plan.bounds
            presentationSize = plan.displaySize
            applyLayers(size: plan.displaySize)
            try await restartMain(plan)
            _ = try validatedSource()
            observation = try observe { [weak self] event in
                guard let self, !self.stopped else { return }
                switch event {
                case .geometryChanged: self.sourceGeometryDidChange()
                case .sourceUnavailable(let error): self.invalidateSource(error)
                }
            }
            let size = await waitForSize(useCached: true, fallback: plan.displaySize)
            _ = try validatedSource()
            applyLayers(size: size)
            presentationSize = size
            await updateCursor()
            _ = try validatedSource()
            return size
        } catch {
            await stop()
            throw error
        }
    }

    func configure(in producerRootLayer: CALayer, presentationSize: CGSize, contentRevision: UInt64) async throws {
        _ = try validatedSource()
        guard root === producerRootLayer, contentRevision > 0, contentRevision <= 9_007_199_254_740_991,
              Self.positive(presentationSize) else {
            throw DesktopFailure(code: "pip_context_changed", message: "PiP layer configuration no longer identifies its retained producer context")
        }
        applyLayers(size: presentationSize)
        self.presentationSize = presentationSize
        applyCursorGeometry()
    }

    func updateAssociatedWindows() throws {
        let snapshot = try validatedSource()
        let next = snapshot.associated.map { $0.descriptor.cg_window_id }
        guard next != associatedIDs else { return }
        associatedIDs = next
        if mainStream != nil { currentSourceSize = nil; sourceGeometryDidChange() }
    }

    func cursorDidChange() {
        cursorTask?.cancel()
        cursorUpdateGeneration &+= 1
        let generation = cursorUpdateGeneration
        cursorTask = Task { [weak self] in
            guard let self, !Task.isCancelled, generation == self.cursorUpdateGeneration else { return }
            await self.updateCursor()
        }
    }

    func sourceGeometryDidChange() {
        do {
            let current = try validatedSource()
            let frame = Self.rect(current.primary.descriptor.frame_screen_points)
            let previous = targetBounds
            targetBounds = frame
            if previous != frame { cursorDidChange() }
            guard previous == nil || currentSourceSize == nil
                    || abs(previous!.width - frame.width) >= 0.5 || abs(previous!.height - frame.height) >= 0.5 else { return }
            setFrameDeliverySuspended(true)
            callbacks.sourceResizeChanged(presentationSize ?? Self.fit(frame.size, maximum: maximumDisplaySize))
            geometryTask?.cancel()
            geometryGeneration &+= 1
            let generation = geometryGeneration
            geometryTask = Task { [weak self] in
                guard let self else { return }
                do {
                    try await ContinuousClock().sleep(for: .milliseconds(240))
                    try Task.checkCancellation()
                    guard generation == self.geometryGeneration else { return }
                    _ = try self.validatedSource()
                    let plan = try await self.capturePlan()
                    try Task.checkCancellation()
                    guard generation == self.geometryGeneration else { return }
                    self.targetBounds = plan.bounds
                    try await self.restartMain(plan)
                    let size = await self.waitForSize(useCached: false, fallback: plan.displaySize)
                    try Task.checkCancellation()
                    guard generation == self.geometryGeneration else { return }
                    _ = try self.validatedSource()
                    self.callbacks.sourceResizeSettled(size)
                    await self.updateCursor()
                } catch is CancellationError { }
                catch {
                    guard generation == self.geometryGeneration, !Task.isCancelled else { return }
                    do { _ = try self.validatedSource() }
                    catch { self.invalidateSource(error); return }
                    self.setFrameDeliverySuspended(false)
                    self.callbacks.diagnostic("capture_resize_failed", error)
                }
            }
        } catch { invalidateSource(error) }
    }

    func setFrameDeliverySuspended(_ value: Bool) {
        suspended = value
        if value { mainLayer.flush() }
    }

    func invalidateSource(_ error: Error) {
        guard !stopped, !unavailable else { return }
        unavailable = true
        geometryTask?.cancel(); geometryTask = nil
        cursorTask?.cancel(); cursorTask = nil
        geometryGeneration &+= 1; mainGeneration &+= 1; cursorGeneration &+= 1; cursorUpdateGeneration &+= 1
        setFrameDeliverySuspended(true)
        resolveSize(presentationSize ?? CGSize(width: 200, height: 200))
        let previous = cursorStream
        cursorStream = nil; cursorPlan = nil
        hideCursor()
        if let previous { Task { do { try await previous.0.stopCapture() } catch { callbacks.diagnostic("cursor_stop_failed", error) } } }
        callbacks.sourceUnavailable(error)
    }

    func setMaximumDisplaySize(_ size: CGFloat) throws -> CGSize {
        _ = try validatedSource()
        maximumDisplaySize = Self.maximum(size)
        if let latest = latestAspect, latest.0 == mainGeneration { return Self.fit(latest.1.sourceSize, maximum: maximumDisplaySize) }
        if let currentSourceSize { return Self.fit(currentSourceSize, maximum: maximumDisplaySize) }
        return presentationSize ?? CGSize(width: 200, height: 200)
    }

    func stop() async {
        guard !stopped else { return }
        stopped = true
        geometryTask?.cancel(); geometryTask = nil
        cursorTask?.cancel(); cursorTask = nil
        observation?.invalidate(); observation = nil
        geometryGeneration &+= 1; mainGeneration &+= 1; cursorGeneration &+= 1; cursorUpdateGeneration &+= 1
        resolveSize(presentationSize ?? CGSize(width: 200, height: 200))
        let main = mainStream; let cursor = cursorStream
        mainStream = nil; cursorStream = nil; cursorPlan = nil; latestAspect = nil
        hideCursor()
        for stream in [main?.0, cursor?.0].compactMap({ $0 }) {
            do { try await stream.stopCapture() } catch { callbacks.diagnostic("capture_stop_failed", error) }
        }
        mainLayer.flushAndRemoveImage(); cursorLayer.flushAndRemoveImage()
        mainLayer.removeFromSuperlayer(); cursorLayer.removeFromSuperlayer(); root = nil
    }

    private func validatedSource() throws -> Snapshot {
        guard !stopped, !unavailable else { throw DesktopFailure(code: "pip_source_closed", message: "PiP source capture was withdrawn") }
        let value = try checkSource()
        guard value.application.isEqual(application), !application.isTerminated,
              value.primary.descriptor.app == source.descriptor.app,
              value.primary.descriptor.window_id == source.descriptor.window_id,
              value.primary.descriptor.cg_window_id == source.descriptor.cg_window_id,
              value.primary.incarnation == source.incarnation,
              value.associated.count <= 256 else {
            throw DesktopFailure(code: "pip_source_changed", message: "PiP source no longer matches its admitted native application-window incarnation")
        }
        var ids = Set([source.descriptor.cg_window_id])
        for window in value.associated {
            guard window.descriptor.app == source.descriptor.app, !window.incarnation.isEmpty,
                  window.descriptor.cg_window_id != 0, ids.insert(window.descriptor.cg_window_id).inserted else {
                throw DesktopFailure(code: "pip_source_changed", message: "PiP associated windows are not exact members of the retained application")
            }
        }
        return value
    }

    private func capturePlan() async throws -> CapturePlan {
        _ = try validatedSource()
        let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: false)
        let snapshot = try validatedSource()
        guard let primary = content.windows.first(where: { $0.windowID == source.descriptor.cg_window_id
            && $0.owningApplication?.processID == application.processIdentifier }) else {
            throw DesktopFailure(code: "pip_source_unavailable", message: "The exact native PiP source is absent from shareable content")
        }
        let associated = Set(snapshot.associated.map { $0.descriptor.cg_window_id })
        let extra = content.windows.filter { associated.contains($0.windowID) && $0.isOnScreen
            && $0.owningApplication?.processID == application.processIdentifier }
        let filter: SCContentFilter
        let configuration: SCStreamConfiguration
        let bounds: CGRect
        if !extra.isEmpty, let display = Self.display(in: content.displays, preferred: primary.frame, target: primary.frame) {
            bounds = extra.reduce(primary.frame) { $0.union($1.frame) }.intersection(display.frame)
            filter = SCContentFilter(display: display, including: [primary] + extra)
            configuration = try Self.configuration(size: bounds.size, scale: CGFloat(filter.pointPixelScale))
            configuration.sourceRect = bounds.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
            configuration.shouldBeOpaque = false
        } else {
            bounds = primary.frame
            filter = SCContentFilter(desktopIndependentWindow: primary)
            configuration = try Self.configuration(size: bounds.size, scale: CGFloat(filter.pointPixelScale))
        }
        _ = try validatedSource()
        return CapturePlan(filter: filter, configuration: configuration, bounds: bounds,
                           displaySize: Self.fit(bounds.size, maximum: maximumDisplaySize))
    }

    private func restartMain(_ plan: CapturePlan) async throws {
        _ = try validatedSource()
        mainGeneration &+= 1
        let generation = mainGeneration
        latestAspect = nil
        let old = mainStream
        mainStream = nil
        if let old { do { try await old.0.stopCapture() } catch { callbacks.diagnostic("capture_stop_failed", error) } }
        _ = try validatedSource()
        guard generation == mainGeneration else { throw CancellationError() }
        let sink = DesktopAppPIPFrameSink(onFrame: { [weak self] sample in
            guard let self, generation == self.mainGeneration else { return }
            self.receiveMain(sample, generation: generation)
        }, onError: { [weak self] error in
            guard let self, generation == self.mainGeneration else { return }
            self.invalidateSource(error)
        })
        let stream = SCStream(filter: plan.filter, configuration: plan.configuration, delegate: sink)
        try stream.addStreamOutput(sink, type: .screen, sampleHandlerQueue: sampleQueue)
        mainStream = (stream, sink)
        do {
            try await stream.startCapture()
            _ = try validatedSource()
            guard generation == mainGeneration, mainStream?.0 === stream else { throw CancellationError() }
        } catch {
            if mainStream?.0 === stream { mainStream = nil }
            try? await stream.stopCapture()
            throw error
        }
    }

    private func receiveMain(_ sample: CMSampleBuffer, generation: UInt64) {
        do {
            _ = try validatedSource()
            guard generation == mainGeneration, sample.isValid else { return }
            if let aspect = Self.aspect(sample, maximum: maximumDisplaySize) {
                latestAspect = (generation, aspect)
                if currentSourceSize == nil || sizeWaiter != nil {
                    currentSourceSize = aspect.sourceSize
                    resolveSize(aspect.displaySize)
                }
            }
            guard !suspended else { return }
            mainLayer.sampleBufferRenderer.enqueue(sample)
        } catch { invalidateSource(error) }
    }

    private func waitForSize(useCached: Bool, fallback: CGSize) async -> CGSize {
        if useCached, let currentSourceSize { return Self.fit(currentSourceSize, maximum: maximumDisplaySize) }
        if let latest = latestAspect, latest.0 == mainGeneration { return latest.1.displaySize }
        resolveSize(fallback)
        let id = UUID()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                sizeWaiter = (id, continuation)
                sizeTask = Task { [weak self] in
                    do { try await ContinuousClock().sleep(for: .milliseconds(500)) }
                    catch { return }
                    guard let self, self.sizeWaiter?.0 == id else { return }
                    self.resolveSize(fallback)
                }
            }
        } onCancel: {
            Task { @MainActor [weak self] in
                guard let self, self.sizeWaiter?.0 == id else { return }
                self.resolveSize(fallback)
            }
        }
    }

    private func resolveSize(_ value: CGSize) {
        let pending = sizeWaiter
        sizeWaiter = nil
        sizeTask?.cancel(); sizeTask = nil
        pending?.1.resume(returning: value)
    }

    private func updateCursor() async {
        do {
            _ = try validatedSource()
            guard let value = try cursorSource(), value.targetWindowID == source.descriptor.cg_window_id,
                  value.targetIncarnation == source.incarnation, value.nativeWindow.isVisible,
                  value.nativeWindow.windowNumber > 0 else { await stopCursor(); return }
            let token = cursorUpdateGeneration
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
            _ = try validatedSource()
            try Task.checkCancellation()
            guard token == cursorUpdateGeneration, let current = try cursorSource(),
                  current.nativeWindow === value.nativeWindow, current.actionIdentity == value.actionIdentity,
                  current.targetIncarnation == source.incarnation, current.nativeWindow.isVisible,
                  let window = content.windows.first(where: { $0.windowID == UInt32(value.nativeWindow.windowNumber)
                    && $0.owningApplication?.processID == getpid() }), let target = targetBounds,
                  let display = Self.display(in: content.displays, preferred: window.frame, target: target) else {
                await stopCursor(); return
            }
            let crop = target.intersection(display.frame)
            let filter = SCContentFilter(display: display, including: [window])
            let configuration = try Self.configuration(size: crop.size, scale: CGFloat(filter.pointPixelScale))
            configuration.shouldBeOpaque = false
            let rect = crop.offsetBy(dx: -display.frame.minX, dy: -display.frame.minY)
            configuration.sourceRect = rect
            let key = CursorKey(windowID: window.windowID, displayID: display.displayID, sourceRect: rect,
                                outputSize: CGSize(width: configuration.width, height: configuration.height))
            let plan = CursorPlan(key: key, filter: filter, configuration: configuration, crop: crop, target: target)
            let unchanged = cursorPlan?.key == key && cursorStream != nil
            cursorPlan = plan
            applyCursorGeometry()
            if unchanged { return }
            cursorGeneration &+= 1
            let generation = cursorGeneration
            let old = cursorStream
            cursorStream = nil
            if let old { do { try await old.0.stopCapture() } catch { callbacks.diagnostic("cursor_stop_failed", error) } }
            _ = try validatedSource()
            try Task.checkCancellation()
            guard token == cursorUpdateGeneration, generation == cursorGeneration else { return }
            let sink = DesktopAppPIPFrameSink(onFrame: { [weak self] sample in
                guard let self, generation == self.cursorGeneration, !self.cursorLayer.isHidden, sample.isValid else { return }
                do { _ = try self.validatedSource(); self.cursorLayer.sampleBufferRenderer.enqueue(sample) }
                catch { self.invalidateSource(error) }
            }, onError: { [weak self] error in
                guard let self, generation == self.cursorGeneration else { return }
                self.callbacks.diagnostic("cursor_capture_failed", error)
                Task { await self.stopCursor() }
            })
            let stream = SCStream(filter: filter, configuration: configuration, delegate: sink)
            try stream.addStreamOutput(sink, type: .screen, sampleHandlerQueue: sampleQueue)
            cursorStream = (stream, sink)
            do {
                try await stream.startCapture()
                _ = try validatedSource()
                guard generation == cursorGeneration, cursorStream?.0 === stream else { throw CancellationError() }
            } catch {
                if cursorStream?.0 === stream { cursorStream = nil }
                try? await stream.stopCapture()
                throw error
            }
        } catch is CancellationError { }
        catch { callbacks.diagnostic("cursor_capture_failed", error); await stopCursor() }
    }

    private func stopCursor() async {
        cursorGeneration &+= 1
        let old = cursorStream
        cursorStream = nil; cursorPlan = nil
        hideCursor()
        if let old { do { try await old.0.stopCapture() } catch { callbacks.diagnostic("cursor_stop_failed", error) } }
    }

    private func applyLayers(size: CGSize) {
        guard let root else { return }
        CATransaction.begin(); CATransaction.setDisableActions(true)
        root.frame = CGRect(origin: .zero, size: size)
        root.backgroundColor = NSColor.clear.cgColor; root.masksToBounds = true
        mainLayer.frame = root.bounds; mainLayer.videoGravity = .resizeAspect
        cursorLayer.backgroundColor = NSColor.clear.cgColor; cursorLayer.videoGravity = .resize
        if mainLayer.superlayer !== root || cursorLayer.superlayer !== root { root.sublayers = [mainLayer, cursorLayer] }
        CATransaction.commit(); CATransaction.flush()
    }

    private func applyCursorGeometry() {
        guard let parent = cursorLayer.superlayer else { return }
        guard let plan = cursorPlan, Self.positive(parent.bounds.size), Self.positive(plan.target.size), Self.positive(plan.crop.size) else { hideCursor(); return }
        let sx = parent.bounds.width / plan.target.width, sy = parent.bounds.height / plan.target.height
        CATransaction.begin(); CATransaction.setDisableActions(true)
        cursorLayer.frame = CGRect(x: (plan.crop.minX - plan.target.minX) * sx, y: (plan.target.maxY - plan.crop.maxY) * sy,
                                  width: plan.crop.width * sx, height: plan.crop.height * sy)
        cursorLayer.isHidden = false
        CATransaction.commit(); CATransaction.flush()
    }

    private func hideCursor() {
        CATransaction.begin(); CATransaction.setDisableActions(true)
        cursorLayer.isHidden = true
        CATransaction.commit(); CATransaction.flush()
    }

    private static func configuration(size: CGSize, scale: CGFloat) throws -> SCStreamConfiguration {
        let width = ceil(size.width * scale), height = ceil(size.height * scale)
        guard positive(size), scale.isFinite, scale > 0, width.isFinite, height.isFinite,
              width <= 32768, height <= 32768, width * height <= 64_000_000 else {
            throw DesktopFailure(code: "pip_capture_geometry_invalid", message: "Native PiP capture geometry exceeds its finite pixel domain")
        }
        let config = SCStreamConfiguration()
        config.width = max(1, Int(width)); config.height = max(1, Int(height))
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.showsCursor = false
        config.ignoreShadowsSingleWindow = true; config.ignoreShadowsDisplay = true
        config.queueDepth = 5; config.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        return config
    }

    private static func display(in displays: [SCDisplay], preferred: CGRect, target: CGRect) -> SCDisplay? {
        func greatest(_ rect: CGRect) -> SCDisplay? {
            var selected: SCDisplay?; var area: CGFloat = 0
            for display in displays {
                let overlap = rect.intersection(display.frame)
                let next = overlap.isNull ? 0 : overlap.width * overlap.height
                if next > area { area = next; selected = display }
            }
            return selected
        }
        if let preferred = greatest(preferred), preferred.frame.intersects(target) { return preferred }
        return greatest(target)
    }

    private static func aspect(_ sample: CMSampleBuffer, maximum: CGFloat) -> Aspect? {
        if let image = CMSampleBufferGetImageBuffer(sample) {
            let size = CGSize(width: CVPixelBufferGetWidth(image), height: CVPixelBufferGetHeight(image))
            if positive(size) { return Aspect(sourceSize: size, displaySize: fit(size, maximum: maximum), source: "sample-image-buffer") }
        }
        if let format = CMSampleBufferGetFormatDescription(sample) {
            let dimensions = CMVideoFormatDescriptionGetDimensions(format)
            let size = CGSize(width: Int(dimensions.width), height: Int(dimensions.height))
            if positive(size) { return Aspect(sourceSize: size, displaySize: fit(size, maximum: maximum), source: "sample-format-description") }
        }
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let first = attachments.first else { return nil }
        if let status = first[.status] as? NSNumber, status.intValue != 0 && status.intValue != 4 { return nil }
        for key in [SCStreamFrameInfo.contentRect, .boundingRect] {
            let rect = (first[key] as? CGRect) ?? (first[key] as? NSValue)?.rectValue
            if let rect, positive(rect.size) { return Aspect(sourceSize: rect.size, displaySize: fit(rect.size, maximum: maximum), source: key.rawValue) }
        }
        return nil
    }

    private static func positive(_ size: CGSize) -> Bool { size.width.isFinite && size.height.isFinite && size.width > 0 && size.height > 0 }
    private static func maximum(_ value: CGFloat) -> CGFloat { value.isFinite ? min(400, max(100, value.rounded())) : 200 }
    private static func fit(_ size: CGSize, maximum: CGFloat) -> CGSize {
        guard positive(size) else { return CGSize(width: 200, height: 200) }
        let scale = min(maximum / size.width, maximum / size.height)
        return CGSize(width: max(1, size.width * scale), height: max(1, size.height * scale))
    }
    private static func rect(_ value: DesktopAppRect) -> CGRect { CGRect(x: value.x, y: value.y, width: value.width, height: value.height) }
}

private final class DesktopAppPIPFrameSink: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let onFrame: @MainActor (CMSampleBuffer) -> Void
    private let onError: @MainActor (Error) -> Void
    init(onFrame: @escaping @MainActor (CMSampleBuffer) -> Void, onError: @escaping @MainActor (Error) -> Void) {
        self.onFrame = onFrame; self.onError = onError
    }
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }
        DispatchQueue.main.async { self.onFrame(sampleBuffer) }
    }
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        DispatchQueue.main.async { self.onError(error) }
    }
}
