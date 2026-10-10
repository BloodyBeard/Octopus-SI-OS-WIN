import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var streamer: AirHandPhoneStreamer

    var body: some View {
        ZStack(alignment: .bottom) {
            CameraPreview(session: streamer.session)
                .ignoresSafeArea()

            VStack(spacing: 12) {
                Text(streamer.status)
                    .font(.footnote)
                    .foregroundStyle(.white)
                    .multilineTextAlignment(.center)

                HStack(spacing: 12) {
                    Button(streamer.isRunning ? "Stop" : "Start") {
                        streamer.isRunning ? streamer.stop() : streamer.start()
                    }
                    .buttonStyle(.borderedProminent)

                    Text("Frames \(streamer.sentFrames)")
                        .font(.footnote.monospacedDigit())
                        .foregroundStyle(.white.opacity(0.85))
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity)
            .background(.black.opacity(0.45))
        }
        .onAppear {
            streamer.start()
        }
        .onDisappear {
            streamer.stop()
        }
    }
}

