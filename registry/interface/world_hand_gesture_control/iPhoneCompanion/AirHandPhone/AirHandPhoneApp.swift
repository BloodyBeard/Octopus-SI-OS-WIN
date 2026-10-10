import SwiftUI

@main
struct AirHandPhoneApp: App {
    @StateObject private var streamer = AirHandPhoneStreamer()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(streamer)
        }
    }
}

