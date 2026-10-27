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
| Tool-process ownership | `octopus-process-session-tool-runtime` | Each tool-process generation owns a **named job object** (`Local\octopus-tool-process-<generation>`), the counterpart of the Unix process group plus its inherited lock file. The job outlives the host while its processes run, so a restarted host finds it by name and stops it before reusing the generation; Windows has no SIGTERM for background processes, so it is terminated at once rather than asked to stop. |
| Exclusive and exchanging renames | `octopus-code-mode-room`, `octopus-system-room-env` | Code-mode records are published with a handle-relative no-replace rename (`rename_no_replace`); memory entries are replaced with `ReplaceFileW`, which leaves the files as `RENAME_EXCHANGE` does. |
| Home folder | `octopus-app`, provider-codex, TUI, Codex doctor | Windows rarely sets `HOME`, so the profile folder (`USERPROFILE`) is used instead. Before this, the Octopus home fell back to a relative `.octopus`. |
| Runtime control | `octopus-app` | `octopus stop` / `restart` / status use the owner-only pipe above, and the CLI checks that the runtime serving it is the same user. |
| Process room viewer | `octopus-system-exec-env`, `app/electron/processViewer.cjs` | The live terminal view works on Windows: a pipe, an owner-only credential file and OS randomness (`getrandom`). Before this, every process room failed to form on Windows. |
| Exec-server secrets | `octopus-exec-substrate` `direct_auth` | Loaded on Windows with the same rules (no reparse points, owner-only single-link file, size bounds). Before this, loading refused outright. |
| Browser driver | `octopus-browser-session-env`, `native/electron-host.cjs` | The Rust door checks its private files with `octopus-private-fs`. The Electron host listens on the pipe, gives its host folder an owner-only access list (`icacls`), and the Rust door checks that list before trusting the folder. Node cannot set a pipe's access list, so the host's pipe keeps Windows' default list, which lets other users open it read-only. The HMAC challenge keyed by the owner-only `auth.key` still keeps them out. |
| Directory sync | `octopus_private_fs::sync_directory` and about 45 callers across the kernel, providers and rooms | Windows opens a folder only with `FILE_FLAG_BACKUP_SEMANTICS` and flushes it only through a write handle. Upstream opened folders with plain `File::open`, which fails on Windows with "Access is denied"; that broke every artifact write (confirmed on Windows). Every caller now uses the shared helper. The ledger store and git-sandbox markers, which used to skip the flush on Windows, now flush. |
| File identity | `octopus_private_fs::identity_of_path` / `identity_of_file`; used by `octopus-ledger-store` | Windows reports no device or inode in `std::fs::Metadata`, so upstream used `0/0` and could not tell a replaced file from the original. The helper reads the volume serial number, the 128-bit file ID and NTFS's change time (the ctime counterpart) from a handle. The ledger store uses it to detect a swapped blob, confirmed failing on Windows before; file storage uses it too (next row). |
| File storage | `octopus-file-storage`, `octopus-interface-transport` `runtime_instance.rs`, operator-chat retirement and topology | File identity comes from a handle (volume serial number and file ID; 128-bit ReFS IDs are refused rather than truncated). Windows cannot lock a directory, and its file locks are mandatory, so a held file or directory is locked through a separate lock file named by its identity in `%LOCALAPPDATA%\Octopus\file-locks`: advisory like `flock`, it follows the entry rather than its name and never blocks readers. File publication and the runtime ownership lock that `octopus up` takes now work. |
| No-follow access beneath a folder | `octopus_private_fs::windows::beneath`; used by file publication (directory capture, moves, reclaim, history), context collections (`SafeDir`), tool-process private reads and snapshots, and spawned-life output capture (`interface_life.rs`, `door_life.rs`, pyecho) | The Windows counterpart of `openat(…, O_NOFOLLOW)` and its relatives. Each name is opened beneath an open folder handle (`NtCreateFile` with a root directory), so a concurrent rename cannot redirect it. A symlink, junction or other reparse point is opened as itself and refused (`ERROR_STOPPED_ON_SYMLINK`, the `ELOOP` counterpart). Also: listing (`FileFullDirectoryInfo`), `lstat`, reading a symlink's target, owner-only create of files and folders, rename without replacing (`FileRenameInformation`) or replacing in one step (POSIX semantics where supported), and delete that first checks the entry is still the expected file. Upstream returned "unsupported" for all of these on Windows. Read-only handles are flushed through a second write handle to the same file (`ReOpenFile`), and folder privacy is read from the handle's access list, since Windows has no mode bits. `canonicalize` answers in `\\?\C:\…` form, so paths are turned back into `C:\…` when that names the same file; otherwise a path given as `C:\…` never matched its own canonical form. |
| No-follow file tools | `octopus-file-system` `no_follow` (apply_patch and other no-follow reads and writes in worker rooms) | Upstream opened the whole `\??\C:\…` path with `OBJ_DONT_REPARSE`. That flag also refuses object-manager links, and the drive letter is one, so every path on a drive failed with "path contains a reparse point". The volume root is now opened normally (and must be a disk), and the rest is opened beneath it with the flag, so junctions and symlinks below the root are still refused. Named-pipe and mailslot shares are refused by name. |
| Stack depth | `octopus-tool-registry`, browser room `dispatch` | Windows gives a main thread 1 MiB of stack (Linux 8 MiB). Two debug-build hot spots were close to 2 MiB together: the first JSON-Schema meta-schema compile (deep recursion in `jsonschema`), now done on a 16 MiB thread, and the browser room's 45-arm `dispatch` (about 400 KB), now a few KB because each operation's future is built in its own frame. |
| Windows sandbox | `octopus-codex-core`, `octopus-tui`, `octopus-codex-sandbox-launcher` | `crates/octopus-windows-sandbox` is a stand-in that reports the sandbox unavailable to the terminal client and debug launcher. Agent tools get the real Windows sandbox from the managed Codex CLI, pinned for `win32-x64` with its sandbox helpers. |
| Desktop driver | `octopus-system-gui-env` | Compiles; its native paths report "desktop control requires macOS". A Windows backend needs UI Automation plus `Windows.Graphics.Capture`. |
| Voice | `octopus-realtime-webrtc` | Compiles on Windows (verified on a Windows machine). |

To cross-check from Linux, point V8's build at its MSVC bindings and a
placeholder archive (`cargo check` does not link):
`RUSTY_V8_SRC_BINDING_PATH=<v8>/gen/src_binding_release_x86_64-pc-windows-msvc.rs RUSTY_V8_ARCHIVE=<empty .gz>`.

The repo's architecture guard (`crates/octopus-app/tests/architecture_dependency_guard.rs`)
lists `octopus-local-socket` and `octopus-private-fs` as passive crates that
worker-environment kinds may link. Its remaining failures predate the port.

#### Running without a sandbox

Windows and Linux have no Octopus process sandbox yet, so processes whose
descriptors require one are refused. You can choose to run them without
one:

- **Desktop app:** Settings → General → Process sandbox → **Run without a
  sandbox**. It applies the next time the runtime starts from the app.
- **Terminal:** start the runtime with `OCTOPUS_ALLOW_UNSANDBOXED_PROCESSES=1`
  (only the exact value `1` counts).

With it on, agents' tools and helper programs run with your account's full
access: they can read, change and delete your files and reach the network.
The runtime's boot report prints a `sandbox` line that says which mode it is
in, and the first unsandboxed process logs a warning. The runtime passes the
setting to the processes it starts. Descriptors cannot set or inherit it, so
only the person who starts Octopus can turn it on. It has no effect on macOS,
which always sandboxes.

The `ocr_text_contains` verifier declares `tesseract`, so it is refused on any
machine (Linux too) where `tesseract` is not on `PATH`, even for text it would
not need OCR for. On Windows, install Tesseract and add its folder to `PATH`.

Verifier programs may be Windows executables: the verifier check accepts a PE
image on Windows, as it accepts ELF on Linux and Mach-O on macOS.

#### Runtime gaps on Windows

These compile, but refuse at run time on Windows ("fail-closed"), so the
kernel cannot run real work on Windows yet. Most-blocking first:

| # | Gap | Where | Effect today | Windows design |
| --- | --- | --- | --- | --- |
| 1 | **Process sandbox** | `octopus-verifiers` `ProcessAuthority::prepare` | Every interface whose descriptor keeps the default `sandbox_mode: enforced` (nearly all) is refused at launch, unless you opt out (see [Running without a sandbox](#running-without-a-sandbox)). Linux has the same gap; only macOS has a backend. | An AppContainer backend: read, write and execute roots become access-list grants to the container, and network is a capability. |
| 2 | Other stores | remote-exec room records, MCP OAuth randomness (`/dev/urandom`) | Each refuses its feature. | `octopus_private_fs::windows::beneath` (see above) and `getrandom`. |
| 3 | Symlinks | registry validation snapshot, Codex auth boundary | Need Developer Mode or admin on Windows. The `octopus-private-fs` folder-handle test skips its link checks when it cannot create a symlink; that skip has not yet been seen on a machine without Developer Mode. | Copy or junction fallback. |
| 4 | Git-worktree sandbox | `octopus-git-sandbox-substrate` `run_sandboxed_in_worktree` | Coder exec refuses on Windows unless you opt out. Linux uses bubblewrap, macOS Seatbelt. With the opt-out, Windows runs it through a bounded runner (timeout, output ceiling, cancellation, whole process tree killed through a job object). | Same backend as #1. |
| 5 | Time-zone database | `world_cron`, `world_memory_cron`, `world_system_review` (`timezone_database_root` declared `/usr/share/zoneinfo`) | Schedules that name a time zone cannot load it on Windows, which has no system tz files. | Python's `tzdata` package, or a tz folder shipped with Octopus, declared per platform. |

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
