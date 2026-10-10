# AirHand Phone

Native iPhone companion for true ultra-wide input.

The macOS Continuity Camera path does not expose 0.5x as a settable public zoom
factor. This companion uses the iPhone camera directly:

- `AVCaptureDevice.DeviceType.builtInUltraWideCamera`
- Bonjour service discovery: `_airhand._tcp`
- TCP frames: 4-byte big-endian JPEG byte count, followed by JPEG data

Open these files as a new iOS SwiftUI app target in Xcode, set your signing
team, run it on the iPhone, then select `Phone` as the source in AirHand on Mac.

