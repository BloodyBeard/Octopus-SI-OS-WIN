import ApplicationServices
import CoreGraphics
import Foundation
import QuartzCore

protocol CursorTargetLocator: AnyObject {
    func magneticTarget(near point: CGPoint, captureRadius: CGFloat) -> CGRect?
}

final class MagneticTargeting: CursorTargetLocator {
    private let systemWide = AXUIElementCreateSystemWide()
    private let queryQueue = DispatchQueue(label: "airhand.magnet", qos: .userInteractive)
    private let stateLock = NSLock()
    private var cachedTarget: CGRect?
    private var cacheTime: TimeInterval = 0
    private var lastQueryPoint: CGPoint = .zero
    private var refreshInFlight = false

    private let cacheDuration: TimeInterval = 0.09
    private let maxTargetSize = CGSize(width: 280, height: 160)
    private let minTargetSize = CGSize(width: 6, height: 6)

    private let actionableRoles: Set<String> = [
        kAXButtonRole, kAXPopUpButtonRole, kAXMenuButtonRole, kAXMenuItemRole,
        kAXMenuBarItemRole, kAXCheckBoxRole, kAXRadioButtonRole, "AXLink",
        kAXDisclosureTriangleRole, kAXIncrementorRole
    ]

    private let excludedRoles: Set<String> = [
        kAXTextFieldRole, "AXTextArea", kAXComboBoxRole, "AXSearchField", kAXSliderRole
    ]

    init() {
        AXUIElementSetMessagingTimeout(systemWide, 0.05)
    }

    func magneticTarget(near point: CGPoint, captureRadius: CGFloat) -> CGRect? {
        let now = CACurrentMediaTime()

        stateLock.lock()
        let cached = cachedTarget
        let stale = now - cacheTime > cacheDuration || point.distance(to: lastQueryPoint) > captureRadius * 0.45
        let shouldRefresh = stale && !refreshInFlight
        if shouldRefresh {
            refreshInFlight = true
        }
        stateLock.unlock()

        if shouldRefresh {
            queryQueue.async { [weak self] in
                guard let self else { return }
                let target = self.queryNearestTarget(near: point, captureRadius: captureRadius)
                self.stateLock.lock()
                self.cachedTarget = target
                self.cacheTime = CACurrentMediaTime()
                self.lastQueryPoint = point
                self.refreshInFlight = false
                self.stateLock.unlock()
            }
        }

        return cached
    }

    private func queryNearestTarget(near point: CGPoint, captureRadius: CGFloat) -> CGRect? {
        var best: CGRect?
        var bestDistance = CGFloat.greatestFiniteMagnitude

        for sample in samplePoints(around: point, radius: captureRadius) {
            guard let element = element(at: sample),
                  let actionable = actionableAncestor(of: element),
                  let frame = frame(of: actionable)
            else {
                continue
            }

            guard frame.width >= minTargetSize.width, frame.height >= minTargetSize.height,
                  frame.width <= maxTargetSize.width, frame.height <= maxTargetSize.height
            else {
                continue
            }

            let distance = point.distance(to: frame)
            if distance <= captureRadius, distance < bestDistance {
                bestDistance = distance
                best = frame
            }
        }

        return best
    }

    private func samplePoints(around point: CGPoint, radius: CGFloat) -> [CGPoint] {
        var points = [point]
        for (ringRadius, count) in [(radius * 0.5, 8), (radius, 8)] {
            for index in 0..<count {
                let angle = CGFloat(index) / CGFloat(count) * 2 * .pi
                points.append(CGPoint(x: point.x + cos(angle) * ringRadius, y: point.y + sin(angle) * ringRadius))
            }
        }
        return points
    }

    private func element(at point: CGPoint) -> AXUIElement? {
        var element: AXUIElement?
        let result = AXUIElementCopyElementAtPosition(systemWide, Float(point.x), Float(point.y), &element)
        return result == .success ? element : nil
    }

    private func actionableAncestor(of element: AXUIElement, maxDepth: Int = 5) -> AXUIElement? {
        var current: AXUIElement? = element
        var depth = 0
        while let candidate = current, depth < maxDepth {
            if isActionable(candidate) {
                return candidate
            }
            current = parent(of: candidate)
            depth += 1
        }
        return nil
    }

    private func isActionable(_ element: AXUIElement) -> Bool {
        guard isEnabled(element) else { return false }

        let role = stringAttribute(element, kAXRoleAttribute)
        if let role, excludedRoles.contains(role) {
            return false
        }
        if let role, actionableRoles.contains(role) {
            return true
        }

        var actions: CFArray?
        if AXUIElementCopyActionNames(element, &actions) == .success,
           let names = actions as? [String],
           names.contains(kAXPressAction) {
            return true
        }
        return false
    }

    private func isEnabled(_ element: AXUIElement) -> Bool {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXEnabledAttribute as CFString, &value) == .success,
              let raw = value, CFGetTypeID(raw) == CFBooleanGetTypeID()
        else {
            return true
        }
        return CFBooleanGetValue((raw as! CFBoolean))
    }

    private func parent(of element: AXUIElement) -> AXUIElement? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXParentAttribute as CFString, &value) == .success,
              let parent = value, CFGetTypeID(parent) == AXUIElementGetTypeID()
        else {
            return nil
        }
        return (parent as! AXUIElement)
    }

    private func frame(of element: AXUIElement) -> CGRect? {
        guard let positionValue = axValue(element, kAXPositionAttribute),
              AXValueGetType(positionValue) == .cgPoint,
              let sizeValue = axValue(element, kAXSizeAttribute),
              AXValueGetType(sizeValue) == .cgSize
        else {
            return nil
        }

        var position = CGPoint.zero
        var size = CGSize.zero
        AXValueGetValue(positionValue, .cgPoint, &position)
        AXValueGetValue(sizeValue, .cgSize, &size)
        return CGRect(origin: position, size: size)
    }

    private func axValue(_ element: AXUIElement, _ attribute: String) -> AXValue? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success,
              let raw = value, CFGetTypeID(raw) == AXValueGetTypeID()
        else {
            return nil
        }
        return (raw as! AXValue)
    }

    private func stringAttribute(_ element: AXUIElement, _ attribute: String) -> String? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, attribute as CFString, &value) == .success else {
            return nil
        }
        return value as? String
    }
}
