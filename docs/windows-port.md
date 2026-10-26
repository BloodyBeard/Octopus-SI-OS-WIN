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
| Local IPC | `octopus-interface-activation-app-server-transport`, `octopus-run-authoring`, `octopus-git-sandbox-substrate`, `octopus-provider-codex`, `octopus-cli`, `octopus-world-terminal-client` | Interface control sockets used `tokio::net::Unix*` | Done: all use `crates/octopus-local-socket` (AF_UNIX on Windows 10 1803+ via `uds_windows`). Some test fixtures that drive fake hosts over Tokio Unix sockets are still Unix-only (`octopus-operator-chat-client` integration tests, realtime-signaling handoff tests, provider-codex native terminal handoff tests). |
| Windows sandbox | `octopus-codex-core`, `octopus-tui`, `octopus-codex-sandbox-launcher` | Upstream removed Codex's `windows-sandbox-rs` crate but kept its callers | Done: `octopus-windows-sandbox` is a stand-in that reports the sandbox unavailable to the terminal client and debug launcher. Agent tools get the real Windows sandbox from the managed Codex CLI, now pinned for `win32-x64` with its sandbox helpers. Vendoring the real crate is optional. |
| Verifiers | `octopus-verifiers` (tests) | Unix permission and inode checks | Done: the one shell-script test is Unix-only |
| Browser driver | `octopus-browser-session-env` | Owner/mode/symlink checks on its private files and the Electron host socket | Done: `src/private_fs.rs` gives each check a Windows equivalent. On Windows "owned by me, mode 0600/0700" means owned by the current user with an access list granting only that user, SYSTEM and Administrators. "No symlinks" means no reparse points, and the host socket must be an AF_UNIX reparse point. Entries it creates get a protected owner+SYSTEM access list. |
| Desktop driver | `octopus-system-gui-env` | Built on Swift / Accessibility / ScreenCaptureKit | Compiles on Windows: its native paths report "desktop control requires macOS". A Windows backend needs UI Automation plus `Windows.Graphics.Capture`. |
| Voice | `octopus-realtime-webrtc` | — | Compiles on Windows (verified on a Windows machine) |

Status: every workspace crate, including tests, type-checks for
`x86_64-pc-windows-gnu`. The `windows` CI job checks the MSVC target on real
Windows. To cross-check from Linux, point V8's build at its MSVC bindings and a
placeholder archive (`cargo check` does not link):
`RUSTY_V8_SRC_BINDING_PATH=<v8>/gen/src_binding_release_x86_64-pc-windows-msvc.rs RUSTY_V8_ARCHIVE=<empty .gz>`.

The Electron browser host must create its rendezvous folder (`host.json`,
`auth.key`, the socket) owner-only. Under `%USERPROFILE%\.octopus` the
inherited access list already qualifies. An Octopus home elsewhere needs an
owner-only access list on that folder.

### Desktop client (Electron)

- Packaging is macOS-only (`npm run package:mac`, `app/scripts/package-macos.cjs`).
  It needs a `package:win` path, either electron-builder or a script that
  mirrors `package-macos.cjs`. That path has to:
  - build `octopus.exe` and the room and provider executables
  - fetch the Windows Codex CLI build (`managed-cli/bootstrap.cjs`)
  - produce an NSIS or zip artifact
- Installing packages copies them with macOS `clonefile` (`app/electron/nativeFiles.cjs`)
  and refuses on any other OS, so setup cannot install anything on Windows or
  Linux yet. Windows needs a plain-copy fallback (or ReFS block cloning). This
  also keeps several desktop test suites (`test-client-setup`,
  `test-local-packages`, `test-cli-setup`) from running off macOS.
- Executables are spawned directly after an `fs.accessSync(..., X_OK)` check
  (`app/electron/chatgptSignIn.cjs`, `clientDictation.cjs`). On Windows that check
  always passes and `#!` scripts cannot be spawned (`EFTYPE`), so the
  managed Codex CLI path needs `.exe`/`.cmd` resolution. `test-client-provider-metadata.cjs`
  fails on Windows for the same reason: its fixture is a `#!` script.
- Fixed: `app/vite.config.ts` derived its folder from `new URL(import.meta.url).pathname`,
  which gives `/C:/...` on Windows and broke `vite build`/`vite dev`. It now uses
  `fileURLToPath`. With this and the LF fix, `npm --prefix app run build` passes on Windows.
- Llama 3.1 8B on llama.cpp (`--jinja`) returns valid tool calls (checked on Windows).
- Fixed: content-hashed files (theme sources, notices, schemas) broke on Windows
  clones with `core.autocrlf=true`. `.gitattributes` now forces LF checkouts.
- The managed Codex CLI is now pinned for `win32-x64` (`codex.exe`, `codex-code-mode-host.exe`,
  `codex-windows-sandbox-setup.exe`, `codex-command-runner.exe`), and the `windows` CI job installs it
  and checks its protocol schemas. Descriptors still name `packages/bin/codex`. Rust's process spawning
  appends `.exe` to such paths, but the Electron/Node callers and the room sandbox's exact-path
  authority checks still need to use the `.exe` name on Windows.
- The launcher `./octopus` is a POSIX shell script. Windows needs an
  `octopus.cmd` equivalent.
- `app/electron/main.cjs` already branches on `process.platform` for window
  chrome, Touch ID, and permissions. Each `darwin` branch still needs an audit
  so it has a sensible Windows fallback.
- `node-pty` and the native addon (`app/native`) need Windows prebuilds or an
  MSVC build step.

## Using any LLM

The model is chosen at first-run setup, and ChatGPT is not required.

1. On the ChatGPT screen, choose **Use another model provider** (or sign in if
   you want ChatGPT/Codex).
2. In **Capabilities**, pick the **Model provider** for Home. 29 providers are
   offered, including:
   - **Custom OpenAI-compatible**: llama.cpp `llama-server`, LM Studio, vLLM,
     LiteLLM, or any `/v1` server
   - **Ollama**, **LM Studio**, **vLLM**
   - **Anthropic**, **Claude Code**, **OpenAI**, **OpenRouter**, **Gemini**,
     **DeepSeek**, **Mistral**, **Groq**, **xAI**, and more
3. After the components install, setup asks for that provider's connection
   details (API key, server URL, model) and saves them only on this device.
   Values already present in `~/.octopus/.env` are used without asking.

Only the chosen provider is installed. Background runs created at setup (memory
consolidation, system review) use the same provider, and Agents can use any
installed provider later.

Terminal setup works the same way:

```sh
./octopus setup                     # interactive: pick provider, then enter its details
./octopus setup --non-interactive --pack codex --workspace-name "My space" \
  --provider world_provider_custom --model custom-model --effort none
```

### llama.cpp

```sh
llama-server -m your-model.gguf --port 8080 --jinja   # --jinja enables tool calling
```

Then choose **Custom OpenAI-compatible** with:

- **Server URL:** `http://localhost:8080/v1`
- **API key:** any value, e.g. `local` (llama-server ignores it unless started
  with `--api-key`)
- **Model id:** any name

Pick a model with solid tool calling, such as Qwen3, Qwen2.5-Coder, or Llama
3.1/3.3 8B+. Octopus relies heavily on tools.

### Configuring by file

The same values can live in `~/.octopus/.env` (mode 0600):

```sh
OCTOPUS_PROVIDER_CUSTOM_BASE_URL=http://localhost:8080/v1
OCTOPUS_PROVIDER_CUSTOM_API_KEY=local
OCTOPUS_PROVIDER_CUSTOM_MODEL=qwen3

OCTOPUS_PROVIDER_ANTHROPIC_API_KEY=sk-ant-...
OCTOPUS_PROVIDER_OLLAMA_MODEL=llama3.1
```

### What changed from upstream

- **Setup pack:** it offers a provider choice (`home.provider_choices`).
  Credential-based providers are verified by checking their required settings
  (`credentials_configured`) instead of a ChatGPT account.
- **Worker policy:** the Home, Task, System Review, and Client Maintenance
  workers allow every bundled provider. Upstream allowed only Codex/ChatGPT for
  these.
- **Kernel fix:** Home and tasks run on per-run copies of a provider
  (`copy_of`). Upstream never passed the settings saved for the provider to
  those copies, so API keys entered in Applications did not reach Home. Copies
  now inherit their original's saved settings, and their own values win.
- **Status:** upstream has not live-tested the non-ChatGPT providers. Report
  what works with your model.
