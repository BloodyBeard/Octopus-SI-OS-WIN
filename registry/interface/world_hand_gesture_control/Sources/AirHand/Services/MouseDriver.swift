import ApplicationServices
import CoreGraphics
import Foundation
import QuartzCore

final class MouseDriver {
    private let eventSource = CGEventSource(stateID: .hidSystemState)

    var isAccessibilityTrusted: Bool {
        AXIsProcessTrusted()
    }

    var currentLocation: CGPoint {
        CGEvent(source: nil)?.location ?? .zero
    }

    func requestAccessibilityPermission() {
        let options: NSDictionary = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as NSString: true]
        _ = AXIsProcessTrustedWithOptions(options)
    }

    private var lastPressTime: TimeInterval = 0
    private var lastPressPoint = CGPoint.zero
    private var pressClickState: Int64 = 1

    func moveCursor(to point: CGPoint) {
        let target = point.roundedToPixel
        CGWarpMouseCursorPosition(target)
        postMouseEvent(type: .mouseMoved, at: target, button: .left)
    }

    func pressLeft(at point: CGPoint) {
        let target = point.roundedToPixel
        let now = CACurrentMediaTime()
        if now - lastPressTime < 0.45, target.distance(to: lastPressPoint) < 12 {
            pressClickState = min(pressClickState + 1, 3)
        } else {
            pressClickState = 1
        }
        lastPressTime = now
        lastPressPoint = target

        CGWarpMouseCursorPosition(target)
        postMouseEvent(type: .mouseMoved, at: target, button: .left)
        postMouseEvent(type: .leftMouseDown, at: target, button: .left, clickState: pressClickState)
    }

    func dragLeft(to point: CGPoint) {
        let target = point.roundedToPixel
        CGWarpMouseCursorPosition(target)
        postMouseEvent(type: .leftMouseDragged, at: target, button: .left, clickState: pressClickState)
    }

    func releaseLeft(at point: CGPoint) {
        let target = point.roundedToPixel
        postMouseEvent(type: .leftMouseUp, at: target, button: .left, clickState: pressClickState)
    }

    func rightClick(at point: CGPoint) {
        click(at: point.roundedToPixel, down: .rightMouseDown, up: .rightMouseUp, button: .right)
    }

    func switchSpace(toRight: Bool) {
        let arrowKey: CGKeyCode = toRight ? 124 : 123
        postKey(keyCode: arrowKey, flags: .maskControl)
    }

    private func postKey(keyCode: CGKeyCode, flags: CGEventFlags) {
        guard let keyDown = CGEvent(keyboardEventSource: eventSource, virtualKey: keyCode, keyDown: true),
              let keyUp = CGEvent(keyboardEventSource: eventSource, virtualKey: keyCode, keyDown: false)
        else {
            return
        }
        keyDown.flags = flags
        keyUp.flags = flags
        keyDown.post(tap: .cghidEventTap)
        keyUp.post(tap: .cghidEventTap)
    }

    func scroll(at point: CGPoint, deltaY: Int32) {
        guard deltaY != 0 else { return }
        guard let event = CGEvent(
            scrollWheelEvent2Source: nil,
            units: .pixel,
            wheelCount: 2,
            wheel1: deltaY,
            wheel2: 0,
            wheel3: 0
        ) else { return }
        event.location = point.roundedToPixel
        event.post(tap: .cghidEventTap)
    }

    private func click(at point: CGPoint, down: CGEventType, up: CGEventType, button: CGMouseButton) {
        CGWarpMouseCursorPosition(point)
        postMouseEvent(type: .mouseMoved, at: point, button: button)
        usleep(8_000)
        postMouseEvent(type: down, at: point, button: button, clickState: 1)
        usleep(30_000)
        postMouseEvent(type: up, at: point, button: button, clickState: 1)
    }

    private func postMouseEvent(type: CGEventType, at point: CGPoint, button: CGMouseButton, clickState: Int64 = 0) {
        let event = CGEvent(mouseEventSource: eventSource, mouseType: type, mouseCursorPosition: point, mouseButton: button)
        event?.setIntegerValueField(.mouseEventClickState, value: clickState)
        event?.post(tap: .cghidEventTap)
    }
}
