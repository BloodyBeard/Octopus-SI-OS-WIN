import AppKit
import Darwin
import SwiftUI

enum SharedAppModel {
    static let model = AppModel()
}

@main
struct AirHandApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = SharedAppModel.model

    init() {
        if CommandLine.arguments.contains("--dump-cameras") {
            CameraService.diagnosticsReport().forEach { print($0) }
            fflush(stdout)
            Darwin.exit(0)
        }

        if let fileIndex = CommandLine.arguments.firstIndex(of: "--dump-cameras-file"),
           CommandLine.arguments.indices.contains(fileIndex + 1) {
            let report = CameraService.diagnosticsReport().joined(separator: "\n")
            let path = CommandLine.arguments[fileIndex + 1]
            try? report.write(toFile: path, atomically: true, encoding: .utf8)
            Darwin.exit(0)
        }
    }

    var body: some Scene {
        WindowGroup("AirHand") {
            ContentView()
                .environmentObject(model)
                .frame(minWidth: 980, minHeight: 660)
                .onAppear {
                    model.appDidAppear()
                }
        }

        Settings {
            SettingsView()
                .environmentObject(model)
                .frame(width: 520)
        }
        .commands {
            CommandMenu("Tracking") {
                Button(model.isRunning ? "Stop Tracking" : "Start Tracking") {
                    model.toggleRunning()
                }
                .keyboardShortcut("r", modifiers: [.command])

                Button("Refresh Source") {
                    model.refreshCurrentSource()
                }
                .keyboardShortcut("r", modifiers: [.command, .shift])

                Divider()

                Button("Request Accessibility Permission") {
                    model.requestAccessibilityPermission()
                }
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        SharedAppModel.model.refreshPermissionMessage()
    }
}
