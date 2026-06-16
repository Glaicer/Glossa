# Glossa — AI Agent Context

---

## Coding guidelines

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:

- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:

- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:

- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:

- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:

```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

---

## What Is Glossa

Glossa is a **headless speech-to-text daemon** for **Ubuntu + GNOME + Wayland**. It captures microphone audio on a global hotkey, transcribes it via a cloud API (Groq / OpenAI), places the text into the Wayland clipboard with `wl-copy`, and pastes it into the active window with `dotool`.

**Target platform (only):** Ubuntu, GNOME, Wayland, user session.
No support for KDE, Sway, X11, macOS, or Windows.

---

## Project Layout

```
glossa/
├── Cargo.toml               # workspace root
├── rust-toolchain.toml       # stable toolchain, clippy + rustfmt
├── AGENTS.md                 # ← this file
├── build-release-tarball.sh  # packages release tarball + checksums + updater asset
├── install.sh                # interactive installer for local user setup
├── uninstall.sh              # uninstall helper for local user setup
├── update.sh                 # bootstrap updater that fetches the latest release updater
├── contrib/
│   ├── assets/sounds/        # start.wav, stop.wav cue sounds
│   ├── assets/tray/          # tray icons (light/dark themes)
│   ├── dotool/               # bundled dotool binary + vendored source
│   ├── examples/config.toml  # reference TOML config
│   ├── release/              # release-time updater template
│   └── systemd/              # user services for glossa + dotool
└── crates/
    ├── glossa-core/          # pure types, config, state, enums
    ├── glossa-app/           # state machine, orchestration, port traits
    ├── glossa-audio/         # CPAL capture, WAV I/O, silence trim, cue playback
    ├── glossa-platform-linux/# portal, tray, wl-copy, dotool, IPC, doctor
    ├── glossa-stt/           # Groq/OpenAI/compatible HTTP clients
    └── glossa-bin/           # CLI (clap), bootstrap, composition root
```

### Crate Dependency Graph

```
glossa-bin
  ├── glossa-app
  │     └── glossa-core
  ├── glossa-audio        (implements glossa-app port traits)
  │     └── glossa-core
  ├── glossa-stt          (implements glossa-app::SttClient)
  │     ├── glossa-core
  │     └── glossa-app
  ├── glossa-platform-linux (implements remaining port traits)
  │     ├── glossa-core
  │     └── glossa-app
  └── glossa-core
```

---

## Crate Responsibilities

### `glossa-core` — Domain Types

Pure data crate. **No async, no platform deps, no I/O.**

| Module | Contents |
|--------|----------|
| `config/` | `AppConfig` with 7 sections: `InputConfig`, `ControlConfig`, `ProviderConfig`, `AudioConfig`, `PasteConfig`, `UiConfig`, `LoggingConfig`. Validated on load. |
| `command.rs` | `AppCommand` enum (`StartRecording`, `StopRecording`, `ToggleRecording`, `Restart`, `Shutdown`) + `CommandOrigin` |
| `state.rs` | `AppState` enum (`Idle`, `Recording`, `Processing`, `Pasting`, `ShuttingDown`) with payload structs |
| `ids.rs` | `SessionId(Uuid)` — unique per recording cycle |
| `audio.rs` | `RecordSpec`, `AudioFormat`, `CapturedAudio` |
| `paste.rs` | `PasteMode` (`CtrlV`, `CtrlShiftV`, `ShiftInsert`) |
| `provider.rs` | `ProviderKind` (`Groq`, `OpenAi`, `OpenAiCompatible`), `ProviderConfig` |
| `status.rs` | `AppStatus` — lightweight snapshot for `glossa status` |
| `error.rs` | `CoreError` — config/state validation errors |

### `glossa-app` — State Machine & Orchestration

The heart of the application. Defines **port traits** and the **pure reducer**.

**Port traits** (in `ports/`):
- `AudioCapture` / `ActiveRecording` — start/stop microphone recording
- `SilenceTrimmer` — trim leading/trailing silence from captured audio
- `CuePlayer` — play start/stop sound effects
- `SttClient` — send audio to a transcription API
- `ClipboardWriter` — put text into the clipboard
- `PasteBackend` — emulate a keyboard paste shortcut
- `TrayPort` / `NullTrayPort` — update tray icon state
- `TempStore` — manage temp file paths and cleanup
- `CommandSource` — produce `AppCommand` values (portal, IPC)

**State machine** (in `machine/`):
- `reducer.rs`: `reduce(state, command) → Decision { next_state, actions }` — **pure function**, no I/O
- `actions.rs`: `Action` enum — side effects the actor must execute
- `guards.rs`: helper predicates for state checks

**Services** (in `services/`):
- `app_actor.rs`: `AppActor` — main `tokio::select!` loop receiving commands and internal events
- `recording_pipeline.rs`: background tasks for transcription, clipboard write, and paste
- `command_router.rs`: `AppHandle` — cloneable sender + status watcher
- `status_store.rs`: `StatusStore` — `tokio::sync::watch`-based status broadcast

### `glossa-audio` — Audio Subsystem

| Module | What it does |
|--------|-------------|
| `capture/cpal_capture.rs` | `CpalAudioCapture` implements `AudioCapture`; streams from default input device via CPAL |
| `capture/active_recording.rs` | Owns `cpal::Stream` + WAV writer thread; `stop()` flushes and returns `CapturedAudio` |
| `capture/sample_convert.rs` | Converts F32/U16 samples to I16 for hound |
| `wav/writer.rs` | `hound::WavWriter` wrapper |
| `wav/reader.rs` | WAV reading utilities |
| `trim/energy_gate.rs` | Silence detection via absolute amplitude threshold |
| `cue/rodio_player.rs` | `RodioCuePlayer` implements `CuePlayer`; plays WAV files via rodio |

### `glossa-stt` — Speech-to-Text Clients

One shared `HttpSttClient` implementing `SttClient`, with thin strategy constructors:
- `groq.rs` — Groq endpoint (`/openai/v1/audio/transcriptions`)
- `openai.rs` — OpenAI endpoint
- `compatible.rs` — generic OpenAI-compatible with configurable `base_url`
- `factory.rs` — `build_client(config) → Arc<dyn SttClient>`
- `dto.rs` — request/response DTOs

### `glossa-platform-linux` — Linux/GNOME/Wayland Adapters

| Module | What it does |
|--------|-------------|
| `portal/shortcut_source.rs` | `PortalShortcutSource` implements `CommandSource` via `ashpd` GlobalShortcuts |
| `portal/mapping.rs` | Pure `map_portal_signal_to_command(mode, signal) → Option<AppCommand>` |
| `tray/tray_port.rs` | `BestEffortTrayPort` — GTK tray icon on a dedicated thread |
| `clipboard/wl_copy.rs` | `WlCopyClipboard` implements `ClipboardWriter` |
| `paste/dotool.rs` | `DotoolPasteBackend` implements `PasteBackend` |
| `ipc/server.rs` | `IpcServer` — `tokio::net::UnixListener` JSON protocol |
| `ipc/client.rs` | Used by `glossa ctl` and `glossa status` |
| `ipc/protocol.rs` | `IpcRequest` / `IpcResponse` enums |
| `temp/xdg_temp_store.rs` | Temp files in `$XDG_RUNTIME_DIR` with per-session cleanup |
| `doctor/checks.rs` | Environment probes (Wayland, GNOME, binaries, portal, API key, etc.) |
| `shortcut_capture.rs` | GTK dialog for tray-driven shortcut rebinding |
| `updater.rs` | Finds and runs the bundled `update.sh`; supports check/install flows for CLI and tray |

### `glossa-bin` — CLI & Composition Root

- `cli.rs` — clap-derived CLI: `daemon`, `ctl {toggle, shutdown}`, `doctor`, `status`, `update`
- `bootstrap.rs` — config loading, tracing init, dependency wiring (`build_actor`, `build_tray`)
- `cmd/daemon.rs` — daemon loop: build actor → spawn IPC + portal tasks → run → restart or exit
- `cmd/ctl.rs`, `cmd/doctor.rs`, `cmd/status.rs`, `cmd/update.rs` — subcommand handlers

### Release & Install Tooling

- `install.sh` — interactive installer that downloads the latest release, installs runtime dependencies, lays down user services, and writes `~/.config/glossa/config.toml`
- `update.sh` — thin bootstrap script that fetches the release-specific updater asset and verifies its checksum before running it
- `contrib/release/glossa-update.sh` — template rendered into the version-pinned updater uploaded with each GitHub release
- `build-release-tarball.sh` — builds the release binary, assembles the release bundle, renders the updater asset, and writes checksums under `target/release/github/<version>/`
- `uninstall.sh` — removes the locally installed Glossa files and systemd user units

---

## Architecture Rules

### One State Machine, Two Command Sources

There is exactly **one recording state machine** (`AppActor`). Two independent sources feed commands into it:

1. **Portal shortcut** — XDG Desktop Portal GlobalShortcuts (`PortalShortcutSource`)
2. **CLI control** — `glossa ctl toggle` via Unix socket IPC (`IpcServer`)

Both converge on the same `mpsc::UnboundedSender<AppCommand>`. The portal obeys `[input.mode]` (toggle vs push-to-talk). The CLI always sends `ToggleRecording` regardless of config.

### State Transitions

```
Idle ──[Start/Toggle]──→ Recording ──[Stop/Toggle]──→ Processing ──→ Pasting ──→ Idle
```

During `Processing` and `Pasting`, recording commands are **ignored** and logged. `Restart` and `Shutdown` are accepted from any state.

### Pure Reducer Pattern

The state machine is a **pure function**: `reduce(state, command) → Decision { next_state, actions }`. The `AppActor` executes the `Action` list imperatively. This makes the state machine trivially testable without mocking I/O.

### Port Abstraction

All platform-specific code lives behind traits in `glossa-app::ports`. The app layer never imports `cpal`, `ashpd`, `wl-copy`, `dotool`, or `reqwest` directly. Implementations live in `glossa-audio`, `glossa-stt`, and `glossa-platform-linux`.

### Thread Model

| Thread | Runs |
|--------|------|
| Main (tokio) | `AppActor`, portal task, IPC server, STT HTTP tasks |
| Tray thread | Dedicated GTK/GLib event loop owning the `TrayIcon` |
| CPAL callback thread(s) | Deliver PCM samples into a bounded channel |
| WAV writer thread | Reads PCM from channel, writes to disk via `hound` |

The tray **must** live on its own thread because GTK requires owning the event loop.

### Daemon Restart Loop

The daemon runs in a loop in `cmd/daemon.rs`. When the actor returns `ActorExit::Restart` (e.g., after tray shortcut change), it reloads config, rebuilds the actor, and re-enters the loop. The tray (`BestEffortTrayPort`) is created once and survives restarts — only its command sender is rebound.

---

## Technology Stack

| Area | Library | Notes |
|------|---------|-------|
| Async runtime | `tokio` 1.41 | multi-thread, all features |
| Audio capture | `cpal` 0.15 | Default input device streaming |
| WAV I/O | `hound` 3.5 | Read/write WAV files |
| Audio playback | `rodio` 0.19 | Cue sounds only |
| STT HTTP | `reqwest` 0.12 | rustls-tls, multipart uploads |
| Portal shortcuts | `ashpd` 0.8 | DBus GlobalShortcuts interface |
| Tray icon | `tray-icon` 0.17 | GTK/AppIndicator on Linux |
| GTK | `gtk` 0.18 | Tray thread event loop + shortcut capture dialog |
| Config | `toml` 0.8 + `serde` 1.0 | TOML config with validation |
| CLI | `clap` 4.5 | Derive-based subcommands |
| Errors | `thiserror` 2.0 (crates), `anyhow` 1.0 (binary) | |
| Logging | `tracing` 0.1 + `tracing-subscriber` 0.3 | Structured logs, env-filter |
| IDs | `uuid` 1.11 | v4 session IDs |
| Paths | `camino` 1.1 | UTF-8 path types |

**Toolchain:** Rust stable, pinned in `rust-toolchain.toml`.

---

## Coding Conventions

### Workspace Lints (enforced)

```toml
[workspace.lints.rust]
unsafe_code = "forbid"
unused_lifetimes = "warn"
unused_qualifications = "warn"

[workspace.lints.clippy]
dbg_macro = "warn"
todo = "warn"
unwrap_used = "warn"
unnecessary_wraps = "warn"
```

All crates inherit these via `[lints] workspace = true`.

### Error Handling

- **`glossa-core`**: `CoreError` (thiserror) — config validation, state transition violations, TOML parse errors.
- **`glossa-app`**: `AppError` (thiserror) — wraps `CoreError` + I/O + opaque messages. Used by all port trait methods.
- **`glossa-bin`**: `anyhow::Result` — for CLI-level error propagation with `.context()`.
- **Never `unwrap()`** in library crates (lint-warned). Use `?` or explicit error handling.
- Sound playback and tray updates log errors but do not abort the recording cycle.

### Async Patterns

- All port traits use `#[async_trait]`.
- `AppActor::run()` uses `tokio::select!` over command channel and internal event channel.
- Background work (transcription + paste) is spawned as `tokio::spawn` tasks that report back via an internal event channel.
- CPAL audio callbacks are sync; they push samples into a `std::sync::mpsc` channel read by a sync writer thread.

### Configuration

- TOML config parsed via `serde` into `AppConfig`, then validated with `AppConfig::validate()`.
- API keys: literal, empty, or `env:VAR_NAME` syntax, resolved at runtime. Empty API keys are only valid for `openai-compatible` providers. **Never logged.**
- `work_dir = "auto"` resolves to `$XDG_RUNTIME_DIR/glossa/`.
- `socket_path = "auto"` resolves to `$XDG_RUNTIME_DIR/glossa.sock`.
- Changing portal shortcut from tray writes to `dconf`, not to `config.toml`.

### File and Module Organization

- One public type per file when the type has significant implementation.
- `mod.rs` files re-export submodules; avoid logic in `mod.rs`.
- Port traits each get their own file in `glossa-app/src/ports/`.
- Platform adapter modules mirror the port structure (e.g., `ports/clipboard.rs` ↔ `clipboard/wl_copy.rs`).

---

## Key Flows

### Toggle Recording (Portal)

1. User presses shortcut → `PortalShortcutSource` emits `ToggleRecording`
2. `AppActor` receives command, calls `reduce(Idle, ToggleRecording)`
3. Reducer returns `Decision { Recording, [SetTrayRecording, StartRecording, PlayStartCue] }`
4. Actor executes actions: tray update → start CPAL capture → cue
5. User presses again → `reduce(Recording, ToggleRecording)`
6. Reducer returns `Decision { Processing, [PlayStopCue, SetTrayProcessing, StopRecording] }`
7. Actor stops capture → spawns background pipeline: trim → STT → clipboard → paste → cleanup
8. Pipeline sends `InternalEvent` → actor transitions to `Idle`

### Push-to-Talk (Portal)

- Press → `Activated` signal → `StartRecording`
- Release → `Deactivated` signal → `StopRecording`
- Same pipeline after stop.

### CLI Toggle

```
glossa ctl toggle → IPC client → Unix socket → IpcServer → ToggleRecording → AppActor
```

Always toggle semantics. Does not read `[input.mode]`.

### CLI Update

```
glossa update → glossa-bin::cmd::update → glossa-platform-linux::updater::run_local_updater → local update.sh
```

`update.sh` downloads the release-specific updater asset, verifies checksums, installs the new bundle, and restarts `glossa.service`.

### Tray Update

1. Tray menu → "Update"
2. `TrayRuntime` calls `check_for_update()` via `glossa-platform-linux::updater`
3. GTK dialog reports "already installed" or asks for confirmation
4. On confirm, `install_update()` runs the local `update.sh install`
5. Result is reported back to the user via GTK dialog

### Daemon Restart (Tray Shortcut Change)

1. Tray menu → "Change shortcut" → GTK capture dialog
2. New accelerator written to `dconf`
3. Tray sends `AppCommand::Restart { origin: TrayMenu }`
4. Actor returns `ActorExit::Restart`
5. Daemon loop reloads config, rebuilds actor, respawns tasks

---

## External Runtime Dependencies

| Binary | Purpose | Package |
|--------|---------|---------|
| `wl-copy` | Wayland clipboard write | `wl-clipboard` |
| `dotool` | Keyboard paste emulation | `dotool` |
| `dconf` | GNOME settings (shortcut rebind) | `dconf-cli` |

The tray requires the **AppIndicator and KStatusNotifierItem Support** GNOME Shell extension. If absent, the daemon continues without tray — it does not crash.
Installer and updater scripts additionally rely on standard user-session tooling such as `systemctl`, `tar`, `sha256sum`, and `curl`/`wget`.

---

## Build & Run

```bash
cargo build                          # debug build
cargo build --release                # release build
cargo clippy --workspace             # lint all crates
cargo test --workspace               # run all tests

# Run daemon
glossa --config contrib/examples/config.toml daemon

# Control
glossa ctl toggle
glossa ctl shutdown
glossa status
glossa doctor
glossa update
```

Systemd user service: `contrib/systemd/glossa.service`
```ini
ExecStart=%h/.cargo/bin/glossa --config %h/.config/glossa/config.toml daemon
```

---

## Security Notes

- `unsafe_code` is **forbidden** workspace-wide.
- API keys are never logged; only the source (e.g., `env:GROQ_API_KEY`) appears in logs.
- Temporary audio files are cleaned up after each cycle and on daemon startup (stale file cleanup).
- IPC socket lives in `$XDG_RUNTIME_DIR` (user-only permissions).
- HTTP client uses `rustls-tls` (no OpenSSL dependency).

---

## What Is NOT Supported (TBD)

- GUI settings window
- System-wide daemon
- Multiple microphones with UI switching
- FLAC recording format (schema exists, implementation pending)
