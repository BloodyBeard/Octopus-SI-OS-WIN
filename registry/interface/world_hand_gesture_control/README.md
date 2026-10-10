# AirHand

AirHand is a macOS prototype for controlling the cursor with an iPhone or webcam:

- open palm moves the cursor across the full combined desktop
- four upper fingers curled with thumb open triggers left click
- thumb fold, or optional pinky-only fold, triggers right click
- fist enters scroll mode; vertical palm movement scrolls until the fist opens

The app uses AVFoundation for camera input, Vision hand-pose landmarks for tracking,
and Quartz/AppKit events for cursor, click, and scroll output.

## Why this is an Octopus interface

AirHand can operate its native pointer controls locally. Its separate Octopus
boundary makes it observable and callable by a bound run:

```text
AirHand → local socket → airhand_life.py → artifact arrival → runtime
AirHand ← local socket ← airhand_life.py ← recorded outbound artifact
```

The bridge materializes declared events such as
`hand_gesture_control.gesture`, tracking, calibration, permissions, and selected
pointer effects as artifacts. In the other direction it reads its outbound
ledger movements and accepts `hand_gesture_control.command` payloads such as:

```json
{ "schema_version": 1, "command": "tracking.set", "arguments": { "enabled": true } }
```

Command results return as `hand_gesture_control.command.result` artifacts.
The interface life runs when live bindings require it. The native cursor
movement and scrolling do not make a runtime round trip on every camera frame.

The Octopus desktop client's **Settings → Gestures** is a separate local
camera-to-UI path. It uses client events for shortcuts and widget manipulation;
it does not use this bridge to switch tabs. See the
[client guide](../../../docs/client.md#experimental-interaction-and-test).

## Run

```sh
./script/build_and_run.sh
```

The run script signs the app with the local `AirHand Local Development`
codesigning identity. macOS camera and Accessibility permissions are tied to
the app's signing identity, so the script fails closed if that identity is
missing instead of falling back to ad-hoc signing.

On first launch, grant:

- Camera permission
- Accessibility permission in System Settings -> Privacy & Security -> Accessibility

For iPhone camera use, enable Continuity Camera and select the iPhone camera in the app's device picker.
On macOS 14 and newer, AirHand opts into Apple's Continuity Camera device type with
`NSCameraUseContinuityCameraDeviceType`, so the iPhone should appear as a
`Continuity` camera in the picker.

If the iPhone does not appear:

- keep Wi-Fi and Bluetooth enabled on both Mac and iPhone
- make sure both devices use the same Apple ID and Handoff is enabled
- enable Settings -> General -> AirPlay & Continuity -> Continuity Camera on iPhone
- unlock the iPhone once, then lock it and mount it in landscape
- quit iPhone Mirroring normally before using the same iPhone as a camera
- trust the Mac if connected by USB, then keep the iPhone nearby
- close other apps that may be using the iPhone camera
- press **Refresh** in AirHand after the iPhone becomes available

## Calibration

Start tracking, show an open palm, press **Record 5 seconds**, and move the palm
through the comfortable air-control rectangle. AirHand records the observed palm
range and maps that calibrated range to the full combined desktop.
