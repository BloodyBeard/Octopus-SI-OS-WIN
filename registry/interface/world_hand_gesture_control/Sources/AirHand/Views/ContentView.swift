import CoreGraphics
import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        HStack(spacing: 0) {
            previewPane
            Divider()
            ControlPanelView()
                .frame(width: 330)
        }
        .toolbar {
            ToolbarItemGroup {
                Button {
                    model.toggleRunning()
                } label: {
                    Label(model.isRunning ? "Stop" : "Start", systemImage: model.isRunning ? "stop.fill" : "play.fill")
                }

                Button {
                    model.refreshCurrentSource()
                } label: {
                    Label("Refresh", systemImage: "arrow.clockwise")
                }
            }
        }
        .onChange(of: model.settings.inputSource) { _ in
            model.inputSourceChanged()
        }
    }

    private var previewPane: some View {
        VStack(spacing: 0) {
            ZStack(alignment: .topLeading) {
                previewContent

                LandmarkOverlay(
                    landmarks: model.snapshot.landmarks,
                    mirrorX: model.settings.previewMirrorX,
                    videoAspectRatio: model.activeVideoAspectRatio
                )

                StatusHUD(snapshot: model.snapshot, isRunning: model.isRunning)
                    .padding(14)
            }

            bottomStrip
        }
    }

    @ViewBuilder
    private var previewContent: some View {
        if InputSource(rawValue: model.settings.inputSource) == .phone,
           let image = model.phoneStream.lastFrame {
            frameImage(image)
        } else if InputSource(rawValue: model.settings.inputSource) == .camera {
            CameraPreview(session: model.camera.session, mirrorX: model.settings.previewMirrorX)
                .background(Color.black)
        } else {
            CameraPreview(session: model.camera.session, mirrorX: model.settings.previewMirrorX)
                .background(Color.black)
        }
    }

    private func frameImage(_ image: CGImage) -> some View {
        Image(decorative: image, scale: 1)
            .resizable()
            .scaledToFit()
            .scaleEffect(x: model.settings.previewMirrorX ? -1 : 1, y: 1)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color.black)
    }

    private var bottomStrip: some View {
        HStack(spacing: 16) {
            statusPill(title: "Mode", value: model.snapshot.mode.rawValue)
            statusPill(title: "Gesture", value: model.snapshot.gesture.rawValue)
            statusPill(title: "Last", value: model.snapshot.lastAction)
            statusPill(title: "Permissions", value: model.permissionMessage)
            statusPill(title: "Camera", value: model.camera.statusMessage)
            if InputSource(rawValue: model.settings.inputSource) == .phone {
                statusPill(title: "Phone", value: model.phoneStream.statusMessage)
            }
            Spacer()
        }
        .font(.caption)
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(.bar)
    }

    private func statusPill(title: String, value: String) -> some View {
        HStack(spacing: 6) {
            Text(title)
                .foregroundStyle(.secondary)
            Text(value)
                .lineLimit(1)
        }
    }
}

struct StatusHUD: View {
    var snapshot: EngineSnapshot
    var isRunning: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Circle()
                    .fill(color)
                    .frame(width: 10, height: 10)
                Text(snapshot.message)
                    .font(.headline)
            }

            Text(snapshot.desktopDescription)
                .font(.caption)
                .foregroundStyle(.secondary)

            if let cursor = snapshot.cursorPoint {
                Text("Cursor \(Int(cursor.x)), \(Int(cursor.y))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .background(.regularMaterial)
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private var color: Color {
        if !isRunning { return .secondary }
        switch snapshot.mode {
        case .pointing: return .green
        case .scrolling: return .blue
        case .searching: return .yellow
        case .unavailable: return .red
        case .stopped: return .secondary
        }
    }
}

struct ControlPanelView: View {
    @EnvironmentObject private var model: AppModel

    private var selectedSource: InputSource {
        InputSource(rawValue: model.settings.inputSource) ?? .camera
    }

    private var selectedTuningPreset: TuningPreset {
        TuningPreset(rawValue: model.settings.tuningPresetID) ?? .custom
    }

    var body: some View {
        Form {
            Section("Camera") {
                Picker("Source", selection: $model.settings.inputSource) {
                    ForEach(InputSource.allCases) { source in
                        Text(source.label).tag(source.rawValue)
                    }
                }
                .pickerStyle(.segmented)

                if selectedSource == .window {
                    Picker("Window", selection: $model.settings.selectedWindowID) {
                        if model.windowCapture.windows.isEmpty {
                            Text("No windows found").tag(UInt32(0))
                        }
                        ForEach(model.windowCapture.windows) { window in
                            Text(window.label).tag(window.id)
                        }
                    }
                } else if selectedSource == .camera {
                    Picker("Device", selection: $model.settings.selectedDeviceID) {
                        if !model.settings.selectedDeviceID.isEmpty,
                           !model.camera.devices.contains(where: { $0.uniqueID == model.settings.selectedDeviceID }) {
                            Text("Selected camera").tag(model.settings.selectedDeviceID)
                        }
                        if model.camera.devices.isEmpty {
                            Text("No camera found").tag("")
                        }
                        ForEach(model.camera.devices) { device in
                            Text("\(device.name) · \(device.kindLabel)")
                                .tag(device.uniqueID)
                        }
                    }
                }

                HStack {
                    Button {
                        model.toggleRunning()
                    } label: {
                        Label(model.isRunning ? "Stop" : "Start", systemImage: model.isRunning ? "stop.fill" : "play.fill")
                    }
                    .keyboardShortcut(.defaultAction)

                    Button {
                        model.refreshCurrentSource()
                    } label: {
                        Label("Refresh", systemImage: "arrow.clockwise")
                    }
                }

                Text(model.permissionMessage)
                    .font(.caption)
                    .foregroundStyle(model.permissionMessage.contains("missing") || model.permissionMessage.contains("denied") ? Color.red : Color.secondary)

                Text(sourceStatusText)
                    .font(.caption)
                    .foregroundStyle(sourceHasError ? Color.red : Color.secondary)
                    .textSelection(.enabled)

                if selectedSource == .camera {
                    HStack {
                        Button {
                            model.requestCameraPermission()
                        } label: {
                            Label("Camera", systemImage: "camera.fill")
                        }

                        Button {
                            model.openVideoEffectsSettings()
                        } label: {
                            Label("Effects", systemImage: "sparkles")
                        }
                    }

                    Button {
                        model.requestAccessibilityPermission()
                        model.openAccessibilityPrivacySettings()
                    } label: {
                        Label("Accessibility", systemImage: "cursorarrow.motionlines")
                    }
                } else if selectedSource == .window {
                    HStack {
                        Button {
                            model.refreshWindows()
                        } label: {
                            Label("Windows", systemImage: "macwindow")
                        }

                        Button {
                            model.openScreenRecordingPrivacySettings()
                        } label: {
                            Label("Privacy", systemImage: "lock.shield")
                        }
                    }
                }

                if let error = model.camera.errorMessage {
                    Text(error)
                        .foregroundStyle(.red)
                }

                if selectedSource == .window, let error = model.windowCapture.errorMessage {
                    Text(error)
                        .foregroundStyle(.red)
                }

                if selectedSource == .phone, let error = model.phoneStream.errorMessage {
                    Text(error)
                        .foregroundStyle(.red)
                }
            }

            Section("Motion") {
                Picker("Preset", selection: $model.settings.tuningPresetID) {
                    ForEach(TuningPreset.allCases) { preset in
                        Text(preset.label).tag(preset.rawValue)
                    }
                }
                .pickerStyle(.menu)

                Text(selectedTuningPreset.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Text(model.calibrationMessage)
                    .foregroundStyle(model.settings.hasCalibrationBounds ? .secondary : .primary)

                if model.isCalibrating {
                    ProgressView(value: model.calibrationProgress)
                    Button(role: .cancel) {
                        model.cancelCalibration()
                    } label: {
                        Label("Cancel", systemImage: "xmark.circle")
                    }
                } else {
                    Button {
                        model.startCalibration()
                    } label: {
                        Label("Train Motion", systemImage: "point.3.connected.trianglepath.dotted")
                    }

                    Button(role: .destructive) {
                        model.clearCalibration()
                    } label: {
                        Label("Clear", systemImage: "trash")
                    }
                    .disabled(!model.settings.hasCalibrationBounds)
                }

                Toggle("Use calibration", isOn: $model.settings.useCalibration)
                    .disabled(!model.settings.hasCalibrationBounds)

                Toggle("Jarvis HUD overlay", isOn: $model.settings.showOverlay)
                Toggle("Finger throttle", isOn: $model.settings.fingerThrottle)
                if model.settings.fingerThrottle {
                    Text("Index finger extension sets cursor speed: straight = fast, slightly curled = precision.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Toggle("Magnetic targeting", isOn: $model.settings.magneticTargeting)
                if model.settings.magneticTargeting {
                    Text("Cursor is gently pulled onto nearby buttons and links as your hand slows.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    SliderRow(title: "Magnet strength", value: $model.settings.magneticStrength, range: 0.10...1.0, format: "%.2f")
                }
            }

            Section("Treadmill") {
                Toggle("Auto walking compensation", isOn: $model.settings.autoGaitCompensation)

                if let walkingHz = model.snapshot.walkingHz {
                    Label(
                        String(format: "Walking detected · cancelling %.1f Hz rhythm", walkingHz),
                        systemImage: "figure.walk.motion"
                    )
                    .foregroundStyle(.green)
                } else if model.settings.autoGaitCompensation {
                    Text("Fully automatic: start walking and it locks onto your rhythm within a few seconds — no calibration needed.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            Section("Gestures") {
                GestureLegendRow(symbol: "hand.point.up.left.fill", title: "Point · index out, 3 folded", detail: "move")
                GestureLegendRow(symbol: "hand.tap.fill", title: "Fold index", detail: "click · hold + move = drag")
                GestureLegendRow(symbol: "arrow.up.arrow.down", title: "Fist up / down", detail: "scroll")
                GestureLegendRow(symbol: "rectangle.on.rectangle.angled", title: "Two fingers ✌ left / right", detail: "switch spaces")
                GestureLegendRow(symbol: "hand.raised.fill", title: "Open palm", detail: "pause")
            }

            LearningSection()
                .environmentObject(model)

            Section("Status") {
                Text("Dropped frames: \(model.droppedFrames)")
                    .foregroundStyle(.secondary)
                Button {
                    model.settings.resetTuning()
                } label: {
                    Label("Reset", systemImage: "arrow.counterclockwise")
                }
            }
        }
        .formStyle(.grouped)
        .scrollContentBackground(.hidden)
    }

    private var sourceStatusText: String {
        switch selectedSource {
        case .camera:
            return "\(model.camera.statusMessage) · \(model.camera.activeDeviceName) · \(model.camera.frameCount) frames"
        case .window:
            if let error = model.windowCapture.errorMessage {
                return error
            }
            return model.windowCapture.isRunning ? "Window capture running" : "\(model.windowCapture.windows.count) windows found"
        case .phone:
            return "\(model.phoneStream.statusMessage) · \(model.phoneStream.endpointDescription)"
        }
    }

    private var sourceHasError: Bool {
        switch selectedSource {
        case .camera:
            return model.camera.errorMessage != nil
        case .window:
            return model.windowCapture.errorMessage != nil
        case .phone:
            return model.phoneStream.errorMessage != nil
        }
    }
}

struct GestureLegendRow: View {
    var symbol: String
    var title: String
    var detail: String

    var body: some View {
        HStack {
            Image(systemName: symbol)
                .frame(width: 24)
                .foregroundStyle(.secondary)
            Text(title)
            Spacer()
            Text(detail)
                .foregroundStyle(.secondary)
        }
    }
}

struct SliderRow: View {
    var title: String
    @Binding var value: Double
    var range: ClosedRange<Double>
    var format: String

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text(title)
                Spacer()
                Text(String(format: format, value))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
            }
            Slider(value: $value, in: range)
        }
    }
}

struct CurlBars: View {
    var curls: FingerCurls

    var body: some View {
        VStack(spacing: 6) {
            bar("Thumb", curls.thumb)
            bar("Index", curls.index)
            bar("Middle", curls.middle)
            bar("Ring", curls.ring)
            bar("Little", curls.little)
        }
    }

    private func bar(_ title: String, _ value: Double) -> some View {
        HStack {
            Text(title)
                .frame(width: 52, alignment: .leading)
            ProgressView(value: value)
            Text(String(format: "%.2f", value))
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .frame(width: 38, alignment: .trailing)
        }
        .font(.caption)
    }
}

struct LearningSection: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        Section("Learning") {
            Text(model.trainingMessage)
                .foregroundStyle(.secondary)

            HStack {
                Button {
                    toggleTargetRecording()
                } label: {
                    Label(targetButtonTitle, systemImage: "record.circle")
                }

                Button {
                    toggleHandRecording()
                } label: {
                    Label(handButtonTitle, systemImage: "hand.draw")
                }
            }

            HStack {
                Button {
                    model.trainMotionFromTakes()
                } label: {
                    Label("Train", systemImage: "brain")
                }
                .disabled(model.trainingTakeCount < 3)

                Button(role: .destructive) {
                    model.clearMotionTraining()
                } label: {
                    Label("Clear", systemImage: "trash")
                }
                .disabled(!model.settings.hasMotionTrainingProfile && model.trainingTakeCount == 0)
            }

            Toggle("Use learned motion", isOn: $model.settings.useMotionTraining)
                .disabled(!model.settings.hasMotionTrainingProfile)
        }
    }

    private var targetButtonTitle: String {
        model.isRecordingTargetPath ? "Stop Cursor" : "Record Cursor"
    }

    private var handButtonTitle: String {
        model.isRecordingHandTake ? "Stop Hand" : "Record Hand"
    }

    private func toggleTargetRecording() {
        if model.isRecordingTargetPath {
            model.stopTargetPathRecording()
        } else {
            model.startTargetPathRecording()
        }
    }

    private func toggleHandRecording() {
        if model.isRecordingHandTake {
            model.stopHandTakeRecording()
        } else {
            model.startHandTakeRecording()
        }
    }
}

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel

    private var selectedTuningPreset: TuningPreset {
        TuningPreset(rawValue: model.settings.tuningPresetID) ?? .custom
    }

    private var selectedControlMode: ControlMode {
        ControlMode(rawValue: model.settings.controlModeID) ?? .smartWalk
    }

    var body: some View {
        Form {
            Section("Launch") {
                Toggle("Start tracking on launch", isOn: $model.settings.startTrackingOnLaunch)
            }

            Section("Input") {
                Toggle("Mirror pointer horizontal", isOn: $model.settings.mirrorX)
                Toggle("Mirror camera preview", isOn: $model.settings.previewMirrorX)
                Toggle("Invert vertical movement", isOn: $model.settings.invertY)
                Toggle("Invert scroll direction", isOn: $model.settings.invertScroll)
            }

            Section("Calibration") {
                Text(model.calibrationMessage)
                if model.isCalibrating {
                    ProgressView(value: model.calibrationProgress)
                    Button(role: .cancel) {
                        model.cancelCalibration()
                    } label: {
                        Label("Cancel", systemImage: "xmark.circle")
                    }
                } else {
                    Button {
                        model.startCalibration()
                    } label: {
                        Label("Train Motion", systemImage: "point.3.connected.trianglepath.dotted")
                    }
                    Button(role: .destructive) {
                        model.clearCalibration()
                    } label: {
                        Label("Clear", systemImage: "trash")
                    }
                }
                Toggle("Use calibration", isOn: $model.settings.useCalibration)
                    .disabled(!model.settings.hasCalibrationBounds)
            }

            Section("Stability") {
                Picker("Preset", selection: $model.settings.tuningPresetID) {
                    ForEach(TuningPreset.allCases) { preset in
                        Text(preset.label).tag(preset.rawValue)
                    }
                }
                .pickerStyle(.menu)

                Text(selectedTuningPreset.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                Picker("Control", selection: $model.settings.controlModeID) {
                    ForEach(ControlMode.allCases) { mode in
                        Text(mode.label).tag(mode.rawValue)
                    }
                }
                .pickerStyle(.segmented)

                Text(selectedControlMode.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)

                SliderRow(title: "Smoothness", value: $model.settings.smoothing, range: 0.10...0.90, format: "%.2f")
                SliderRow(title: "Sensitivity", value: $model.settings.pointerSensitivity, range: 0.25...1.00, format: "%.2f")
                SliderRow(title: "Stability radius", value: $model.settings.stabilizationRadiusPixels, range: 0...60, format: "%.0f px")
                SliderRow(title: "Dead zone", value: $model.settings.deadZonePixels, range: 0...16, format: "%.0f px")
                SliderRow(title: "Click hold", value: $model.settings.clickHoldSeconds, range: 0.04...0.35, format: "%.2f s")
            }
        }
        .formStyle(.grouped)
    }
}
