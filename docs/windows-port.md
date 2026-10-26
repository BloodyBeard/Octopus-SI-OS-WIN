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

**Compiling is not running.** Every crate type-checks for Windows with no
Windows-only warnings ([windows-warnings.md](windows-warnings.md) lists the
rest). But several subsystems still refuse at run time on Windows, by
design, until they get a Windows backend. See
[Runtime gaps on Windows](#runtime-gaps-on-windows).

Done:

| Area | Crate(s) | Windows handling |
| --- | --- | --- |
| Local IPC | `crates/octopus-local-socket` and every socket user (activation transport, run authoring, git sandbox, provider-codex, `octopus-cli`, world-terminal client, process-room viewer, Codex doctor, runtime control) | **Named pipes.** Node, and so the desktop app, can reach local sockets on Windows only as named pipes. Each socket path maps to `\\.\pipe\octopus-<first 32 hex of sha256(path)>`; the path is normalized first (no `\\?\`, `\` separators, lower case). The Rust `pipe_name`, `app/electron/localSocket.cjs` and the browser host's `native/local-socket.cjs` share one test vector. Pipes get a protected access list for the current user and SYSTEM. A client refuses a pipe whose server runs as another user (`GetNamedPipeServerProcessId`), which stands in for the socket file's owner. Pipes leave no file, so "stale socket" cleanup is a no-op. |
| Private files | `crates/octopus-private-fs` (shared) | "Owned by me, mode 0600/0700" means owned by the current user with an access list granting only that user, SYSTEM and Administrators. "No symlinks" means no reparse points. Entries it creates get a protected access list for the user and SYSTEM. It is used by the browser driver, process room, file storage, interface transport, git-sandbox records, tool-runtime generations and exec-server secrets. |
| Interface processes | `octopus-interface-transport` | Each life process runs in a **job object** that kills its whole process tree when it is stopped or when the runtime exits for any reason. This is the counterpart of the macOS process group plus the inherited ownership lock. Windows programs also get the system variables they need (`SYSTEMROOT`, `USERPROFILE`, `APPDATA`, …), with `TEMP`/`TMP` pointing at their private var dir. A descriptor that inherits `HOME` gets the profile folder. |
| Home folder | `octopus-app`, provider-codex, TUI, Codex doctor | Windows rarely sets `HOME`, so the profile folder (`USERPROFILE`) is used instead. Before this, the Octopus home fell back to a relative `.octopus`. |
| Runtime control | `octopus-app` | `octopus stop` / `restart` / status use the owner-only pipe above, and the CLI checks that the runtime serving it is the same user. |
| Process room viewer | `octopus-system-exec-env`, `app/electron/processViewer.cjs` | The live terminal view works on Windows: a pipe, an owner-only credential file and OS randomness (`getrandom`). Before this, every process room failed to form on Windows. |
| Exec-server secrets | `octopus-exec-substrate` `direct_auth` | Loaded on Windows with the same rules (no reparse points, owner-only single-link file, size bounds). Before this, loading refused outright. |
| Browser driver | `octopus-browser-session-env`, `native/electron-host.cjs` | The Rust door checks its private files with `octopus-private-fs`. The Electron host listens on the pipe, gives its host folder an owner-only access list (`icacls`), and the Rust door checks that list before trusting the folder. Node cannot set a pipe's access list, so the host's pipe keeps Windows' default list, which lets other users open it read-only. The HMAC challenge keyed by the owner-only `auth.key` still keeps them out. |
| Directory sync | `octopus_private_fs::sync_directory` and about 45 callers across the kernel, providers and rooms | Windows opens a folder only with `FILE_FLAG_BACKUP_SEMANTICS` and flushes it only through a write handle. Upstream opened folders with plain `File::open`, which fails on Windows with "Access is denied"; that broke every artifact write (confirmed on Windows). Every caller now uses the shared helper. The ledger store and git-sandbox markers, which used to skip the flush on Windows, now flush. |
| Windows sandbox | `octopus-codex-core`, `octopus-tui`, `octopus-codex-sandbox-launcher` | `crates/octopus-windows-sandbox` is a stand-in that reports the sandbox unavailable to the terminal client and debug launcher. Agent tools get the real Windows sandbox from the managed Codex CLI, pinned for `win32-x64` with its sandbox helpers. |
| Desktop driver | `octopus-system-gui-env` | Compiles; its native paths report "desktop control requires macOS". A Windows backend needs UI Automation plus `Windows.Graphics.Capture`. |
| Voice | `octopus-realtime-webrtc` | Compiles on Windows (verified on a Windows machine). |

To cross-check from Linux, point V8's build at its MSVC bindings and a
placeholder archive (`cargo check` does not link):
`RUSTY_V8_SRC_BINDING_PATH=<v8>/gen/src_binding_release_x86_64-pc-windows-msvc.rs RUSTY_V8_ARCHIVE=<empty .gz>`.

The repo's architecture guard (`crates/octopus-app/tests/architecture_dependency_guard.rs`)
lists `octopus-local-socket` and `octopus-private-fs` as passive crates that
worker-environment kinds may link. Its remaining failures predate the port.

#### Runtime gaps on Windows

These compile, but refuse at run time on Windows ("fail-closed"), so the
kernel cannot run real work on Windows yet. Most-blocking first:

| # | Gap | Where | Effect today | Windows design |
| --- | --- | --- | --- | --- |
| 1 | **Process sandbox** | `octopus-verifiers` `ProcessAuthority::prepare` | Every interface whose descriptor keeps the default `sandbox_mode: enforced` (nearly all) is refused at launch. Linux has the same gap; only macOS has a backend. | An AppContainer backend: read, write and execute roots become access-list grants to the container, and network is a capability. Or an explicit, visible opt-out for users who accept running unsandboxed. **This needs your decision.** |
| 2 | File identity and no-replace moves | `octopus-file-storage` (`FileIdentity`, `native.rs`, directory publication, reclaim), `octopus-interface-transport` `runtime_instance.rs` | File publication, registry install and runtime-instance ownership refuse. | Volume serial plus file ID (`FileIdInfo`), handle-relative opens (`NtCreateFile` with a root directory), no-replace rename (`SetFileInformationByHandle` with `FileRenameInfo`, `ReplaceIfExists = false`), and identity-checked delete. |
| 3 | Spawned-life output capture | `interface_life.rs` and `door_life.rs` `read_regular_file_beneath` | Interface outputs cannot be captured. | A no-follow, beneath-root reader (per-component reparse checks). |
| 4 | Tool-process owner lease | `octopus-process-session-tool-runtime` | Process-session tool processes refuse to spawn. | A job-object owner lease (as for interface processes). |
| 5 | Other stores | remote-exec room records, context collections (`SafeDir`), code-mode continuation (exclusive rename), memory `exchange` (`ReplaceFileW`), operator-chat task retirement, MCP OAuth randomness (`/dev/urandom`) | Each refuses its feature. | Mostly `octopus-private-fs` plus the identity and rename pieces from #2. |
| 6 | Symlinks | registry validation snapshot, Codex auth boundary | Need Developer Mode or admin on Windows. | Copy or junction fallback. |
| 7 | Git-worktree sandbox | `octopus-git-sandbox-substrate` `run_sandboxed_in_worktree` | Coder exec refuses ("no platform floor"). Linux uses bubblewrap, macOS Seatbelt. | Same decision as #1. |

The Unix-only helpers behind these refusals are gated `#[cfg(unix)]` and say
why, instead of being left as unused code on Windows.

### Desktop client (Electron)

- Packaging is macOS-only (`npm run package:mac`, `app/scripts/package-macos.cjs`).
  It needs a `package:win` path, either electron-builder or a script that
  mirrors `package-macos.cjs`. That path has to:
  - build `octopus.exe` and the room and provider executables
  - fetch the Windows Codex CLI build (`managed-cli/bootstrap.cjs`)
  - produce an NSIS or zip artifact
- Fixed: the app's local-socket connections (interface transport, readiness probes,
  runtime host, process viewer) use the Windows pipe names (`app/electron/localSocket.cjs`).
- Many app modules check privacy with POSIX mode bits (`stat.mode & 0o077`,
  `process.getuid()`), for example `localPackages.cjs`, `workspaceSetup.cjs` and
  `clientAttachments.cjs`. Node reports `0o666`/`0o777` on Windows and has no
  uid, so these checks refuse on Windows. Each needs a Windows answer, such as
  an access-list check through a native helper or relying on the owner-only
  Octopus home.
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
