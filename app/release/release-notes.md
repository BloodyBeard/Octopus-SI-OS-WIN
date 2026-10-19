Octopus {{version}} for macOS 14 or later ({{architecture}}). The app includes the runtime, the bundled packages, and the pinned Codex CLI it uses as its first provider; nothing else needs to be installed except the Command Line Tools for Xcode.

## Install

In Terminal:

```sh
curl -fsSL https://github.com/miuuyy/Octopus-SI-OS/releases/download/v{{version}}/install.sh | OCTOPUS_VERSION={{version}} sh
```

The installer checks the SHA-256 of the disk image against `checksums.txt` and copies `Octopus.app` to `/Applications` (or `~/Applications` when `/Applications` is not writable).

To install by hand, download `Octopus-{{version}}-macos-{{architecture}}.dmg`, drag Octopus to Applications, and before the first launch run:

```sh
xattr -dr com.apple.quarantine /Applications/Octopus.app
```

This build is ad-hoc signed and not notarized by Apple. Without that command macOS refuses to open it, and the tools Octopus copies into your Home would stay quarantined as well. Do not open Octopus from the disk image itself.

## Requirements

- macOS 14 or later.
- Command Line Tools for Xcode: `xcode-select --install`. Octopus runs `python3` and `git` from them.
- A ChatGPT account for Sign in with ChatGPT. If Codex CLI is already signed in on this Mac, Octopus uses the same sign-in in `~/.codex`.

Setup, Home location, and diagnostics: [Getting started](https://github.com/miuuyy/Octopus-SI-OS/blob/v{{version}}/docs/getting-started.md). Known problems: [Troubleshooting](https://github.com/miuuyy/Octopus-SI-OS/blob/v{{version}}/TROUBLESHOOTING.md).
