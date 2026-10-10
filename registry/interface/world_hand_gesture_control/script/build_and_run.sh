#!/usr/bin/env bash
set -euo pipefail

APP_NAME="AirHand"
PRODUCT_NAME="AirHand"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIGURATION="debug"
SCRATCH_PATH="${AIRHAND_SCRATCH_PATH:-/tmp/airhand-source-build}"
BUILD_DIR="$SCRATCH_PATH/$CONFIGURATION"
DIST_DIR="$ROOT_DIR/dist"
APP_BUNDLE="$DIST_DIR/$APP_NAME.app"
STAGING_BUNDLE="$DIST_DIR/.$APP_NAME.app.staging"
CONTENTS_DIR="$APP_BUNDLE/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
INFO_PLIST="$CONTENTS_DIR/Info.plist"
STAGING_CONTENTS_DIR="$STAGING_BUNDLE/Contents"
STAGING_MACOS_DIR="$STAGING_CONTENTS_DIR/MacOS"
STAGING_INFO_PLIST="$STAGING_CONTENTS_DIR/Info.plist"
BINARY="$BUILD_DIR/$PRODUCT_NAME"
SIGN_IDENTITY="${AIRHAND_CODESIGN_IDENTITY:-AirHand Local Development}"
OCTOPUS_HOME_PATH="${OCTOPUS_HOME:-$HOME/.octopus}"
OCTOPUS_SOCKET="${AIRHAND_OCTOPUS_SOCKET:-$OCTOPUS_HOME_PATH/interfaces/world_hand_gesture_control/airhand.sock}"

usage() {
  cat <<USAGE
Usage: ./script/build_and_run.sh [--verify] [--logs] [--rebuild] [--reset-camera-permission]
USAGE
}

VERIFY=0
LOGS=0
REBUILD=0
RESET_CAMERA_PERMISSION=0

for arg in "$@"; do
  case "$arg" in
    --verify) VERIFY=1 ;;
    --logs) LOGS=1 ;;
    --rebuild) REBUILD=1 ;;
    --reset-camera-permission) RESET_CAMERA_PERMISSION=1 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $arg" >&2; usage; exit 2 ;;
  esac
done

cd "$ROOT_DIR"

if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
  /usr/bin/osascript -e "tell application \"$APP_NAME\" to quit" >/dev/null 2>&1 || true
  for _ in {1..15}; do
    pgrep -x "$APP_NAME" >/dev/null 2>&1 || break
    sleep 0.2
  done
  if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
    pkill -x "$APP_NAME" || true
    sleep 0.4
  fi
fi

if [[ "$RESET_CAMERA_PERMISSION" == "1" ]]; then
  /usr/bin/tccutil reset Camera local.airhand.app >/dev/null 2>&1 || true
fi

NEEDS_BUILD=0
if [[ "$REBUILD" == "1" || ! -x "$BINARY" || ! -x "$MACOS_DIR/$APP_NAME" ]]; then
  NEEDS_BUILD=1
elif find "$ROOT_DIR/Sources" "$ROOT_DIR/Package.swift" "$ROOT_DIR/script/build_and_run.sh" -newer "$MACOS_DIR/$APP_NAME" | grep -q .; then
  NEEDS_BUILD=1
fi

if [[ "$NEEDS_BUILD" == "1" ]]; then
  swift build --scratch-path "$SCRATCH_PATH" -c "$CONFIGURATION" --product "$PRODUCT_NAME" -Xswiftc -disable-batch-mode

  rm -rf "$STAGING_BUNDLE"
  mkdir -p "$STAGING_MACOS_DIR"
  cp "$BINARY" "$STAGING_MACOS_DIR/$APP_NAME"
  chmod +x "$STAGING_MACOS_DIR/$APP_NAME"

  cat > "$STAGING_INFO_PLIST" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>local.airhand.app</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSCameraUsageDescription</key>
    <string>AirHand uses the camera to track your palm and finger gestures for cursor control.</string>
    <key>NSCameraUseContinuityCameraDeviceType</key>
    <true/>
    <key>NSBonjourServices</key>
    <array>
        <string>_airhand._tcp</string>
    </array>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSLocalNetworkUsageDescription</key>
    <string>AirHand receives the optional ultra-wide iPhone companion stream over the local network.</string>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
PLIST

  if ! /usr/bin/security find-identity -p codesigning -v | /usr/bin/grep -F "\"$SIGN_IDENTITY\"" >/dev/null; then
    echo "Missing codesigning identity: $SIGN_IDENTITY" >&2
    echo "Create or import a stable codesigning identity before running AirHand." >&2
    exit 1
  fi

  if ! /usr/bin/perl -e 'alarm shift; exec @ARGV' 15 /usr/bin/codesign --force --deep --sign "$SIGN_IDENTITY" "$STAGING_BUNDLE" >/dev/null; then
    echo "codesign failed or timed out for identity: $SIGN_IDENTITY" >&2
    echo "The previous app bundle was left untouched." >&2
    exit 1
  fi

  if [[ -d "$APP_BUNDLE" ]]; then
    rm -rf "$CONTENTS_DIR"
    mv "$STAGING_CONTENTS_DIR" "$CONTENTS_DIR"
    rmdir "$STAGING_BUNDLE"
  else
    mv "$STAGING_BUNDLE" "$APP_BUNDLE"
  fi
else
  echo "$APP_NAME bundle is current; not rebuilding or re-signing"
fi

/usr/bin/open -n "$APP_BUNDLE" --args --octopus-socket "$OCTOPUS_SOCKET"

if [[ "$VERIFY" == "1" ]]; then
  for _ in {1..20}; do
    if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
      echo "$APP_NAME is running"
      break
    fi
    sleep 0.25
  done
  pgrep -x "$APP_NAME" >/dev/null
fi

if [[ "$LOGS" == "1" ]]; then
  /usr/bin/log stream --style compact --predicate "process == '$APP_NAME'"
fi
