import CoreGraphics
import QuartzCore

final class DisplayMapper {
    private let refreshInterval: TimeInterval = 2
    private var cachedDesktopRect: CGRect = .zero
    private var cachedDesktopDescription = "No active display"
    private var lastRefreshTime: TimeInterval = 0

    var desktopRect: CGRect {
        refreshDisplayCacheIfNeeded()
        return cachedDesktopRect
    }

    var desktopDescription: String {
        refreshDisplayCacheIfNeeded()
        return cachedDesktopDescription
    }

    func mapPalmToDesktop(_ palm: CGPoint, settings: AppSettings) -> CGPoint {
        let rect = desktopRect
        guard !rect.isEmpty else { return .zero }

        let normalized = normalizedControlPoint(palm, settings: settings)
        return CGPoint(
            x: rect.minX + rect.width * normalized.x,
            y: rect.minY + rect.height * normalized.y
        )
        .clamped(to: rect)
    }

    func normalizedControlPoint(_ palm: CGPoint, settings: AppSettings) -> CGPoint {
        if settings.shouldUseMotionTraining,
           let profile = settings.motionTrainingProfile {
            return profile.normalizedPoint(for: palm)
        }

        if settings.shouldUseCalibration,
           let profile = settings.motionCalibrationProfile {
            return profile.normalizedPoint(for: palm)
        }

        let normalizedX = normalize(
            palm.x,
            margin: settings.cameraMarginX,
            calibratedMin: CGFloat(settings.calibrationMinX),
            calibratedMax: CGFloat(settings.calibrationMaxX),
            useCalibration: settings.shouldUseCalibration
        )
        let normalizedY = normalize(
            palm.y,
            margin: settings.cameraMarginY,
            calibratedMin: CGFloat(settings.calibrationMinY),
            calibratedMax: CGFloat(settings.calibrationMaxY),
            useCalibration: settings.shouldUseCalibration
        )

        let x = settings.mirrorX ? 1 - normalizedX : normalizedX
        let yForScreen = settings.invertY ? normalizedY : 1 - normalizedY

        return CGPoint(x: x, y: yForScreen)
    }

    func relativeControlPoint(_ palm: CGPoint, settings: AppSettings) -> CGPoint {
        if settings.shouldUseMotionTraining,
           let profile = settings.motionTrainingProfile {
            return profile.normalizedPoint(for: palm)
        }

        if settings.shouldUseCalibration,
           let profile = settings.motionCalibrationProfile {
            return profile.relativeControlPoint(for: palm)
        }

        return normalizedControlPoint(palm, settings: settings)
    }

    private func refreshDisplayCacheIfNeeded() {
        let now = CACurrentMediaTime()
        guard cachedDesktopRect.isEmpty || now - lastRefreshTime >= refreshInterval else { return }

        var count: UInt32 = 0
        CGGetActiveDisplayList(0, nil, &count)
        var displays = Array(repeating: CGDirectDisplayID(0), count: Int(count))
        CGGetActiveDisplayList(count, &displays, &count)
        let rects = displays.map { CGDisplayBounds($0) }
        let rect = CGRect.union(rects)
        cachedDesktopRect = rect
        cachedDesktopDescription = rect.isEmpty
            ? "No active display"
            : "\(Int(rect.width)) x \(Int(rect.height)) px, origin \(Int(rect.minX)), \(Int(rect.minY))"
        lastRefreshTime = now
    }

    private func normalize(
        _ value: CGFloat,
        margin: Double,
        calibratedMin: CGFloat,
        calibratedMax: CGFloat,
        useCalibration: Bool
    ) -> CGFloat {
        if useCalibration {
            let range = max(calibratedMax - calibratedMin, 0.01)
            return min(max((value - calibratedMin) / range, 0), 1)
        }

        let safeMargin = min(max(CGFloat(margin), 0), 0.35)
        let denominator = max(1 - safeMargin * 2, 0.01)
        return min(max((value - safeMargin) / denominator, 0), 1)
    }
}
