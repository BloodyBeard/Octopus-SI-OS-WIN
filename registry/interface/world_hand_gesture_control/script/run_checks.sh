#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OUT="${TMPDIR:-/tmp}/airhand-checks"

FILES=()
while IFS= read -r f; do
  FILES+=("$f")
done < <(find Sources/AirHand -name '*.swift' \
  ! -name 'AirHandApp.swift' \
  ! -path '*Views*' \
  ! -name 'CameraService.swift' \
  ! -name 'WindowCaptureService.swift' \
  ! -name 'PhoneStreamService.swift' \
  ! -name 'HandPoseDetector.swift' \
  ! -name 'ManualFramingController.swift' \
  ! -name 'AppModel.swift')

echo "Compiling ${#FILES[@]} sources + spec..."
swiftc -O -parse-as-library "${FILES[@]}" Tests/CalibrationChecks/main.swift -o "$OUT"

echo "Running checks..."
"$OUT"
python3 Tests/OctopusBridgeChecks/main.py
