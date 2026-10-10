import AVFoundation
import Darwin
import Foundation

final class ManualFramingController {
    static let shared = ManualFramingController()

    private typealias GetDouble = @convention(c) (AnyObject) -> Double
    private typealias GetBool = @convention(c) (AnyObject) -> Bool
    private typealias GetInt = @convention(c) (AnyObject) -> Int
    private typealias SetDouble = @convention(c) (AnyObject, Double) -> Void
    private typealias SetDeviceType = @convention(c) (Int, AnyObject) -> Void
    private typealias ResetFraming = @convention(c) (AnyObject) -> Void

    private struct Symbols {
        var getZoom: GetDouble
        var minZoom: GetDouble
        var maxZoom: GetDouble
        var isDefault: GetBool
        var getDeviceType: GetInt
        var hasUltraWide: GetBool
        var hasWide: GetBool
        var setZoom: SetDouble
        var setDeviceType: SetDeviceType
        var resetFraming: ResetFraming
    }

    private enum Constants {
        static let ultraWideDeviceType = 2
    }

    private let symbols: Symbols?
    private let loadError: String?

    private init() {
        let path = "/System/Library/Frameworks/AVFoundation.framework/AVFoundation"
        guard let handle = dlopen(path, RTLD_NOW) else {
            symbols = nil
            loadError = "manual framing unavailable: dlopen failed"
            return
        }

        func load<T>(_ name: String, as type: T.Type) -> T? {
            guard let symbol = dlsym(handle, name) else {
                return nil
            }
            return unsafeBitCast(symbol, to: type)
        }

        guard
            let getZoom = load("AVControlCenterManualFramingGetVideoZoomFactor", as: GetDouble.self),
            let minZoom = load("AVControlCenterManualFramingMinAvailableVideoZoomFactor", as: GetDouble.self),
            let maxZoom = load("AVControlCenterManualFramingMaxAvailableVideoZoomFactor", as: GetDouble.self),
            let isDefault = load("AVControlCenterManualFramingIsAtDefaultForDevice", as: GetBool.self),
            let getDeviceType = load("AVControlCenterGetManualFramingDeviceType", as: GetInt.self),
            let hasUltraWide = load("AVControlCenterDeviceHasUltraWideCamera", as: GetBool.self),
            let hasWide = load("AVControlCenterDeviceHasWideCamera", as: GetBool.self),
            let setZoom = load("AVControlCenterManualFramingSetVideoZoomFactor", as: SetDouble.self),
            let setDeviceType = load("AVControlCenterSetManualFramingDeviceType", as: SetDeviceType.self),
            let resetFraming = load("AVControlCenterResetFraming", as: ResetFraming.self)
        else {
            symbols = nil
            loadError = "manual framing unavailable: AVControlCenter symbols missing"
            return
        }

        symbols = Symbols(
            getZoom: getZoom,
            minZoom: minZoom,
            maxZoom: maxZoom,
            isDefault: isDefault,
            getDeviceType: getDeviceType,
            hasUltraWide: hasUltraWide,
            hasWide: hasWide,
            setZoom: setZoom,
            setDeviceType: setDeviceType,
            resetFraming: resetFraming
        )
        loadError = nil
    }

    func forceUltraWideMinimumZoom(for device: AVCaptureDevice) -> String {
        guard shouldForceUltraWide(for: device) else {
            return "manual framing skipped"
        }

        guard let symbols else {
            return loadError ?? "manual framing unavailable"
        }

        let object = device as AnyObject
        guard symbols.hasUltraWide(object) else {
            return "manual framing skipped: no ultra-wide"
        }

        let before = state(for: object, symbols: symbols)
        symbols.setDeviceType(Constants.ultraWideDeviceType, object)
        symbols.resetFraming(object)
        let minimumZoom = symbols.minZoom(object)
        if minimumZoom > 0 {
            symbols.setZoom(object, minimumZoom)
        }
        let after = state(for: object, symbols: symbols)

        guard after.deviceType == Constants.ultraWideDeviceType, after.minimumZoom <= after.zoom + 0.001 else {
            return "manual framing failed: \(before.summary) -> \(after.summary)"
        }

        return "manual framing ultra-wide: \(before.summary) -> \(after.summary)"
    }

    func diagnostics(for device: AVCaptureDevice) -> String? {
        guard let symbols else {
            return loadError
        }

        let object = device as AnyObject
        let state = state(for: object, symbols: symbols)
        return "manualType=\(state.deviceType) manualZoom=\(format(state.zoom)) manualRange=\(format(state.minimumZoom))...\(format(state.maximumZoom)) manualDefault=\(state.isDefault) manualUltra=\(symbols.hasUltraWide(object)) manualWide=\(symbols.hasWide(object))"
    }

    private func shouldForceUltraWide(for device: AVCaptureDevice) -> Bool {
        device.localizedName.localizedCaseInsensitiveContains("iPhone")
            || device.isContinuityCamera
    }

    private func state(for object: AnyObject, symbols: Symbols) -> State {
        State(
            deviceType: symbols.getDeviceType(object),
            zoom: symbols.getZoom(object),
            minimumZoom: symbols.minZoom(object),
            maximumZoom: symbols.maxZoom(object),
            isDefault: symbols.isDefault(object)
        )
    }

    private func format(_ value: Double) -> String {
        String(format: "%.2f", value)
    }

    private struct State {
        var deviceType: Int
        var zoom: Double
        var minimumZoom: Double
        var maximumZoom: Double
        var isDefault: Bool

        var summary: String {
            "type=\(deviceType) zoom=\(String(format: "%.2f", zoom)) range=\(String(format: "%.2f", minimumZoom))...\(String(format: "%.2f", maximumZoom)) default=\(isDefault)"
        }
    }
}
