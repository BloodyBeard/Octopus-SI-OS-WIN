# Windows port and model-agnostic setup

This fork of [Octopus-SI-OS](https://github.com/miuuyy/Octopus-SI-OS) has two goals:

1. Run Octopus on Windows 10/11 (x64).
2. Let the user run it on any LLM, with no ChatGPT account required.

`main` tracks upstream. Port work lands on feature branches. To pull upstream
changes in:

```sh
git remote add upstream https://github.com/miuuyy/Octopus-SI-OS.git
git fetch upstream && git merge upstream/main
```

## Where things stand

### Rust workspace (kernel, rooms, providers)

Much of the workspace already carries `cfg(windows)` code, inherited from the
vendored Codex crates. That covers the process, PTY, and shell layers and the
Windows sandbox shim. The `windows` CI workflow type-checks every crate on
`windows-latest` and lists the ones that still fail.

Already fixed:

- Runtime control (`octopus stop` / `octopus restart` / status) uses a named
  pipe on Windows. The name is derived from the Octopus home, and the default
  DACL makes it owner-only.
- `octopus-file-system` has its Windows feature flags and handle types.
- `octopus-remote-exec-room` had its dependencies declared under
  `cfg(unix)` by mistake.
- Test fixtures set the Windows-only `ProcessDriver::tty` field.

Still to do, roughly in dependency order:

| Area | Crate(s) | Problem | Plan |
| --- | --- | --- | --- |
| Local IPC | `octopus-interface-activation-app-server-transport`, `octopus-run-authoring`, `octopus-git-sandbox-substrate` | Interface control sockets use `tokio::net::Unix*` | Extract the existing AF_UNIX wrapper (`world_provider_codex/.../octopus-app-server-transport/src/uds.rs`, backed by `uds_windows`) into a shared crate and use it everywhere. Windows 10 1803+ supports AF_UNIX. |
| Windows sandbox | `octopus-codex-core`, `octopus-tui`, `octopus-codex-sandbox-launcher` | Upstream dropped Codex's `windows-sandbox-rs` crate but still references `octopus_windows_sandbox` | Vendor `codex-rs/windows-sandbox-rs` from openai/codex (Apache-2.0) as `octopus-windows-sandbox` |
| Verifiers | `octopus-verifiers` (tests) | Unix permission and inode checks | Add Windows equivalents or gate the tests |
| Browser driver | `octopus-browser-session-env` | Unix sockets, `O_NOFOLLOW`, uid/mode checks against the Electron browser host | Port the transport to the shared IPC crate and use Windows ACL checks |
| Desktop driver | `octopus-system-gui-env` | Built on Swift / Accessibility / ScreenCaptureKit | Gate on macOS for now. A Windows backend needs UI Automation plus `Windows.Graphics.Capture`. |
| Voice | `octopus-realtime-webrtc` | Builds libwebrtc | Check on CI |

### Desktop client (Electron)

- Packaging is macOS-only (`npm run package:mac`, `app/scripts/package-macos.cjs`).
  It needs a `package:win` path, either electron-builder or a script that
  mirrors `package-macos.cjs`. That path has to:
  - build `octopus.exe` and the room and provider executables
  - fetch the Windows Codex CLI build (`managed-cli/bootstrap.cjs`)
  - produce an NSIS or zip artifact
- The launcher `./octopus` is a POSIX shell script. Windows needs an
  `octopus.cmd` equivalent.
- `app/electron/main.cjs` already branches on `process.platform` for window
  chrome, Touch ID, and permissions. Each `darwin` branch still needs an audit
  so it has a sensible Windows fallback.
- `node-pty` and the native addon (`app/native`) need Windows prebuilds or an
  MSVC build step.

## Using any LLM

Every provider below already ships in `registry/interface/world_provider_*`.
All of them run through the bundled Codex runtime and a local translation
bridge (OpenAI Chat/Responses, Anthropic, Google, Azure). None of them needs a
ChatGPT login.

The providers are:

- `custom`: any OpenAI-compatible server
- `openai`, `anthropic`, `gemini`, `openrouter`
- `ollama`, `lmstudio`, `vllm`
- `groq`, `deepseek`, `mistral`, `xai`, `together`
- and more

Changes on this branch:

- The Home, Task, System Review, and Client Maintenance workers now allow
  every bundled provider in their policy ceiling. Upstream allowed only
  Codex/ChatGPT for these workers, so Home and task agents could not be
  switched to another model.

Configure a provider either way:

- **Applications** view → the provider → fill in its fields.
- `~/.octopus/.env` (mode 0600), for example:

  ```sh
  # Any OpenAI-compatible endpoint (also works for LM Studio, vLLM, LiteLLM, ...)
  OCTOPUS_PROVIDER_CUSTOM_BASE_URL=http://localhost:8000/v1
  OCTOPUS_PROVIDER_CUSTOM_API_KEY=sk-...
  OCTOPUS_PROVIDER_CUSTOM_MODEL=my-model-id

  # Anthropic
  OCTOPUS_PROVIDER_ANTHROPIC_API_KEY=sk-ant-...

  # Ollama (base URL is http://localhost:11434/v1)
  OCTOPUS_PROVIDER_OLLAMA_MODEL=llama3.1
  ```

Still to do:

1. **Make first-run sign-in optional.** `WorkspaceSetupGate.tsx` always
   renders `ChatGptSignInGate`. The setup "provider" step
   (`clientProviderMetadata.cjs` `setupOwner`, `app/cli/setup.cjs`
   `ensureSignIn`) only knows the Codex account. Credential-based providers
   need a setup owner that proves their env/config is present.
2. **Let the user choose the Home provider at setup.** The setup pack
   (`app/distribution/local-packages/packs/codex.json`) pins
   `world_provider_codex`. Add a provider choice to the pack and the setup
   overlay, plus a `--provider` flag to the CLI.
3. **Background runs.** `registry/interface/world_memory_cron/activation.json`
   targets `world_provider_codex`. It should follow the Home provider.
