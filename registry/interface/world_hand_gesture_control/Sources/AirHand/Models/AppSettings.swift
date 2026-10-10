import Foundation

final class AppSettings: ObservableObject {
    @Published var selectedDeviceID: String {
        didSet { save("selectedDeviceID", selectedDeviceID) }
    }

    @Published var inputSource: String {
        didSet { save("inputSource", inputSource) }
    }

    @Published var selectedWindowID: UInt32 {
        didSet { save("selectedWindowID", Int(selectedWindowID)) }
    }

    @Published var mirrorX: Bool {
        didSet { save("mirrorX", mirrorX) }
    }

    @Published var previewMirrorX: Bool {
        didSet { save("previewMirrorX", previewMirrorX) }
    }

    @Published var invertY: Bool {
        didSet { save("invertY", invertY) }
    }

    @Published var smoothing: Double {
        didSet {
            save("smoothing", smoothing)
            markCustomPresetIfNeeded()
        }
    }

    @Published var deadZonePixels: Double {
        didSet {
            save("deadZonePixels", deadZonePixels)
            markCustomPresetIfNeeded()
        }
    }

    @Published var pointerSensitivity: Double {
        didSet {
            save("pointerSensitivity", pointerSensitivity)
            markCustomPresetIfNeeded()
        }
    }

    @Published var stabilizationRadiusPixels: Double {
        didSet {
            save("stabilizationRadiusPixels", stabilizationRadiusPixels)
            markCustomPresetIfNeeded()
        }
    }

    @Published var controlModeID: String {
        didSet {
            save("controlModeID", controlModeID)
            markCustomPresetIfNeeded()
        }
    }

    @Published var cameraMarginX: Double {
        didSet { save("cameraMarginX", cameraMarginX) }
    }

    @Published var cameraMarginY: Double {
        didSet { save("cameraMarginY", cameraMarginY) }
    }

    @Published var useCalibration: Bool {
        didSet { save("useCalibration", useCalibration) }
    }

    @Published var motionCalibrationJSON: String {
        didSet { save("motionCalibrationJSON", motionCalibrationJSON) }
    }

    @Published var motionTrainingJSON: String {
        didSet { save("motionTrainingJSON", motionTrainingJSON) }
    }

    @Published var useMotionTraining: Bool {
        didSet { save("useMotionTraining", useMotionTraining) }
    }

    @Published var calibrationRecorded: Bool {
        didSet { save("calibrationRecorded", calibrationRecorded) }
    }

    @Published var calibrationMinX: Double {
        didSet { save("calibrationMinX", calibrationMinX) }
    }

    @Published var calibrationMaxX: Double {
        didSet { save("calibrationMaxX", calibrationMaxX) }
    }

    @Published var calibrationMinY: Double {
        didSet { save("calibrationMinY", calibrationMinY) }
    }

    @Published var calibrationMaxY: Double {
        didSet { save("calibrationMaxY", calibrationMaxY) }
    }

    @Published var confidenceThreshold: Double {
        didSet {
            save("confidenceThreshold", confidenceThreshold)
            markCustomPresetIfNeeded()
        }
    }

    @Published var clickHoldSeconds: Double {
        didSet {
            save("clickHoldSeconds", clickHoldSeconds)
            markCustomPresetIfNeeded()
        }
    }

    @Published var clickCooldownSeconds: Double {
        didSet {
            save("clickCooldownSeconds", clickCooldownSeconds)
            markCustomPresetIfNeeded()
        }
    }

    @Published var scrollSensitivity: Double {
        didSet {
            save("scrollSensitivity", scrollSensitivity)
            markCustomPresetIfNeeded()
        }
    }

    @Published var invertScroll: Bool {
        didSet { save("invertScroll", invertScroll) }
    }

    @Published var tuningPresetID: String {
        didSet {
            save("tuningPresetID", tuningPresetID)
            guard !isApplyingPreset,
                  let preset = TuningPreset(rawValue: tuningPresetID),
                  let values = preset.values
            else {
                return
            }
            apply(values)
        }
    }

    @Published var startTrackingOnLaunch: Bool {
        didSet { save("startTrackingOnLaunch", startTrackingOnLaunch) }
    }

    @Published var magneticTargeting: Bool {
        didSet { save("magneticTargeting", magneticTargeting) }
    }

    @Published var magneticStrength: Double {
        didSet { save("magneticStrength", magneticStrength) }
    }

    @Published var autoGaitCompensation: Bool {
        didSet { save("autoGaitCompensation", autoGaitCompensation) }
    }

    @Published var showOverlay: Bool {
        didSet { save("showOverlay", showOverlay) }
    }

    @Published var fingerThrottle: Bool {
        didSet { save("fingerThrottle", fingerThrottle) }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let shouldApplyComfortTuning = defaults.integer(forKey: Self.key("tuningVersion")) < Self.tuningVersion
        let shouldMigrateCalibratedControl = defaults.integer(forKey: Self.key("controlStrategyVersion")) < Self.controlStrategyVersion
        let defaultTuning = TuningPreset.balanced.values

        selectedDeviceID = defaults.string(forKey: Self.key("selectedDeviceID")) ?? ""
        inputSource = defaults.string(forKey: Self.key("inputSource")) ?? InputSource.camera.rawValue
        selectedWindowID = UInt32(defaults.object(forKey: Self.key("selectedWindowID")) as? Int ?? 0)
        mirrorX = defaults.object(forKey: Self.key("mirrorX")) as? Bool ?? true
        previewMirrorX = defaults.object(forKey: Self.key("previewMirrorX")) as? Bool ?? false
        invertY = defaults.object(forKey: Self.key("invertY")) as? Bool ?? false
        smoothing = defaults.object(forKey: Self.key("smoothing")) as? Double ?? defaultTuning?.smoothing ?? 0.84
        deadZonePixels = defaults.object(forKey: Self.key("deadZonePixels")) as? Double ?? defaultTuning?.deadZonePixels ?? 11
        pointerSensitivity = defaults.object(forKey: Self.key("pointerSensitivity")) as? Double ?? defaultTuning?.pointerSensitivity ?? 0.40
        stabilizationRadiusPixels = defaults.object(forKey: Self.key("stabilizationRadiusPixels")) as? Double ?? defaultTuning?.stabilizationRadiusPixels ?? 42
        controlModeID = defaults.string(forKey: Self.key("controlModeID")) ?? defaultTuning?.controlMode.rawValue ?? ControlMode.smartWalk.rawValue
        cameraMarginX = defaults.object(forKey: Self.key("cameraMarginX")) as? Double ?? 0.08
        cameraMarginY = defaults.object(forKey: Self.key("cameraMarginY")) as? Double ?? 0.10
        useCalibration = defaults.object(forKey: Self.key("useCalibration")) as? Bool ?? false
        motionCalibrationJSON = defaults.string(forKey: Self.key("motionCalibrationJSON")) ?? ""
        motionTrainingJSON = defaults.string(forKey: Self.key("motionTrainingJSON")) ?? ""
        useMotionTraining = defaults.object(forKey: Self.key("useMotionTraining")) as? Bool ?? false
        calibrationRecorded = defaults.object(forKey: Self.key("calibrationRecorded")) as? Bool ?? false
        calibrationMinX = defaults.object(forKey: Self.key("calibrationMinX")) as? Double ?? 0.20
        calibrationMaxX = defaults.object(forKey: Self.key("calibrationMaxX")) as? Double ?? 0.80
        calibrationMinY = defaults.object(forKey: Self.key("calibrationMinY")) as? Double ?? 0.20
        calibrationMaxY = defaults.object(forKey: Self.key("calibrationMaxY")) as? Double ?? 0.80
        confidenceThreshold = defaults.object(forKey: Self.key("confidenceThreshold")) as? Double ?? defaultTuning?.confidenceThreshold ?? 0.36
        clickHoldSeconds = defaults.object(forKey: Self.key("clickHoldSeconds")) as? Double ?? defaultTuning?.clickHoldSeconds ?? 0.09
        clickCooldownSeconds = defaults.object(forKey: Self.key("clickCooldownSeconds")) as? Double ?? defaultTuning?.clickCooldownSeconds ?? 0.34
        scrollSensitivity = defaults.object(forKey: Self.key("scrollSensitivity")) as? Double ?? defaultTuning?.scrollSensitivity ?? 650
        invertScroll = defaults.object(forKey: Self.key("invertScroll")) as? Bool ?? false
        tuningPresetID = defaults.string(forKey: Self.key("tuningPresetID")) ?? TuningPreset.balanced.rawValue
        startTrackingOnLaunch = defaults.object(forKey: Self.key("startTrackingOnLaunch")) as? Bool ?? false
        magneticTargeting = defaults.object(forKey: Self.key("magneticTargeting")) as? Bool ?? true
        magneticStrength = defaults.object(forKey: Self.key("magneticStrength")) as? Double ?? 0.6
        autoGaitCompensation = defaults.object(forKey: Self.key("autoGaitCompensation")) as? Bool ?? true
        showOverlay = defaults.object(forKey: Self.key("showOverlay")) as? Bool ?? true
        fingerThrottle = defaults.object(forKey: Self.key("fingerThrottle")) as? Bool ?? true

        if shouldApplyComfortTuning {
            tuningPresetID = TuningPreset.balanced.rawValue
            apply(TuningPreset.balanced.values)
            defaults.set(Self.tuningVersion, forKey: Self.key("tuningVersion"))
        }

        if !hasCalibrationBounds {
            useCalibration = false
            calibrationRecorded = false
        }
        if !hasMotionTrainingProfile {
            useMotionTraining = false
        }

        if shouldMigrateCalibratedControl {
            if shouldUseCalibration, controlModeID == ControlMode.direct.rawValue {
                controlModeID = ControlMode.smartWalk.rawValue
                defaults.set(controlModeID, forKey: Self.key("controlModeID"))
            }
            defaults.set(Self.controlStrategyVersion, forKey: Self.key("controlStrategyVersion"))
        }
    }

    func resetTuning() {
        mirrorX = true
        previewMirrorX = false
        invertY = false
        applyPreset(.balanced)
        cameraMarginX = 0.08
        cameraMarginY = 0.10
        useCalibration = false
        motionCalibrationJSON = ""
        useMotionTraining = false
        motionTrainingJSON = ""
        calibrationRecorded = false
        calibrationMinX = 0.20
        calibrationMaxX = 0.80
        calibrationMinY = 0.20
        calibrationMaxY = 0.80
        invertScroll = false
        magneticTargeting = true
        magneticStrength = 0.6
        autoGaitCompensation = true
        fingerThrottle = true
    }

    private let defaults: UserDefaults
    private var isApplyingPreset = false
    private static let prefix = "AirHand."
    private static let tuningVersion = 8
    private static let controlStrategyVersion = 1

    private static func key(_ name: String) -> String {
        prefix + name
    }

    private func save(_ name: String, _ value: Any) {
        defaults.set(value, forKey: Self.key(name))
    }

    func applyPreset(_ preset: TuningPreset) {
        if tuningPresetID != preset.rawValue {
            tuningPresetID = preset.rawValue
            return
        }

        apply(preset.values)
    }

    private func apply(_ values: TuningValues?) {
        guard let values else { return }

        isApplyingPreset = true
        smoothing = values.smoothing
        deadZonePixels = values.deadZonePixels
        pointerSensitivity = values.pointerSensitivity
        stabilizationRadiusPixels = values.stabilizationRadiusPixels
        controlModeID = values.controlMode.rawValue
        confidenceThreshold = values.confidenceThreshold
        clickHoldSeconds = values.clickHoldSeconds
        clickCooldownSeconds = values.clickCooldownSeconds
        scrollSensitivity = values.scrollSensitivity
        isApplyingPreset = false
    }

    private func markCustomPresetIfNeeded() {
        guard !isApplyingPreset, tuningPresetID != TuningPreset.custom.rawValue else { return }
        tuningPresetID = TuningPreset.custom.rawValue
    }

    var hasCalibrationBounds: Bool {
        motionCalibrationProfile?.isSurfaceProfile == true
    }

    var shouldUseCalibration: Bool {
        useCalibration && hasCalibrationBounds
    }

    var hasMotionTrainingProfile: Bool {
        motionTrainingProfile != nil
    }

    var shouldUseMotionTraining: Bool {
        useMotionTraining && hasMotionTrainingProfile
    }

    var motionCalibrationProfile: MotionCalibrationProfile? {
        MotionCalibrationProfile(encodedString: motionCalibrationJSON)
    }

    var motionTrainingProfile: MotionTrainingProfile? {
        MotionTrainingProfile(encodedString: motionTrainingJSON)
    }

    func setMotionCalibration(_ profile: MotionCalibrationProfile) throws {
        motionCalibrationJSON = try profile.encodedString()
        calibrationRecorded = true
        useCalibration = true
    }

    func setMotionTraining(_ profile: MotionTrainingProfile) throws {
        motionTrainingJSON = try profile.encodedString()
        useMotionTraining = true
    }

    func setCalibration(minX: Double, maxX: Double, minY: Double, maxY: Double) {
        calibrationMinX = min(max(minX, 0), 1)
        calibrationMaxX = min(max(maxX, 0), 1)
        calibrationMinY = min(max(minY, 0), 1)
        calibrationMaxY = min(max(maxY, 0), 1)
        calibrationRecorded = calibrationMaxX - calibrationMinX >= 0.12
            && calibrationMaxY - calibrationMinY >= 0.12
        useCalibration = hasCalibrationBounds
    }

    func clearCalibration() {
        useCalibration = false
        motionCalibrationJSON = ""
        clearMotionTraining()
        calibrationRecorded = false
        calibrationMinX = 0.20
        calibrationMaxX = 0.80
        calibrationMinY = 0.20
        calibrationMaxY = 0.80
    }

    func clearMotionTraining() {
        useMotionTraining = false
        motionTrainingJSON = ""
    }

}

enum InputSource: String, CaseIterable, Identifiable {
    case camera
    case window
    case phone

    var id: String { rawValue }

    var label: String {
        switch self {
        case .camera: return "Camera"
        case .window: return "Window"
        case .phone: return "Phone"
        }
    }
}

struct TuningValues {
    var smoothing: Double
    var deadZonePixels: Double
    var pointerSensitivity: Double
    var stabilizationRadiusPixels: Double
    var controlMode: ControlMode
    var confidenceThreshold: Double
    var clickHoldSeconds: Double
    var clickCooldownSeconds: Double
    var scrollSensitivity: Double
}

enum ControlMode: String, CaseIterable, Identifiable {
    case smartWalk
    case direct

    var id: String { rawValue }

    var label: String {
        switch self {
        case .smartWalk: return "Smart Walk"
        case .direct: return "Direct"
        }
    }

    var detail: String {
        switch self {
        case .smartWalk:
            return "Palm acts like a soft joystick with a neutral hitbox."
        case .direct:
            return "Palm position maps directly to screen position."
        }
    }

    var message: String {
        switch self {
        case .smartWalk: return "Smart Walk"
        case .direct: return "Move with palm"
        }
    }
}

enum TuningPreset: String, CaseIterable, Identifiable {
    case treadmill
    case balanced
    case precision
    case fast
    case custom

    var id: String { rawValue }

    var label: String {
        switch self {
        case .treadmill: return "Walk"
        case .balanced: return "Balanced"
        case .precision: return "Precise"
        case .fast: return "Fast"
        case .custom: return "Custom"
        }
    }

    var detail: String {
        switch self {
        case .treadmill:
            return "Relative trackpad-style control for use while moving around."
        case .balanced:
            return "Recommended. Relative aim with flick acceleration + One Euro smoothing."
        case .precision:
            return "Steadiest absolute aim for small targets and clicks."
        case .fast:
            return "Snappy absolute aim with minimal smoothing."
        case .custom:
            return "Manual values from the sliders below."
        }
    }

    var values: TuningValues? {
        switch self {
        case .treadmill:
            return TuningValues(
                smoothing: 0.80,
                deadZonePixels: 3,
                pointerSensitivity: 0.58,
                stabilizationRadiusPixels: 12,
                controlMode: .smartWalk,
                confidenceThreshold: 0.36,
                clickHoldSeconds: 0.09,
                clickCooldownSeconds: 0.34,
                scrollSensitivity: 650
            )
        case .balanced:
            return TuningValues(
                smoothing: 0.74,
                deadZonePixels: 2,
                pointerSensitivity: 0.58,
                stabilizationRadiusPixels: 8,
                controlMode: .smartWalk,
                confidenceThreshold: 0.35,
                clickHoldSeconds: 0.08,
                clickCooldownSeconds: 0.30,
                scrollSensitivity: 850
            )
        case .precision:
            return TuningValues(
                smoothing: 0.90,
                deadZonePixels: 2,
                pointerSensitivity: 0.42,
                stabilizationRadiusPixels: 10,
                controlMode: .direct,
                confidenceThreshold: 0.40,
                clickHoldSeconds: 0.10,
                clickCooldownSeconds: 0.34,
                scrollSensitivity: 600
            )
        case .fast:
            return TuningValues(
                smoothing: 0.50,
                deadZonePixels: 1,
                pointerSensitivity: 0.95,
                stabilizationRadiusPixels: 4,
                controlMode: .direct,
                confidenceThreshold: 0.32,
                clickHoldSeconds: 0.05,
                clickCooldownSeconds: 0.24,
                scrollSensitivity: 1_100
            )
        case .custom:
            return nil
        }
    }
}
