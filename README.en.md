# FrameAnchor

<p align="center">
  <img src="src-tauri/icons/icon.png" width="128" alt="FrameAnchor icon">
</p>

<p align="center">
  A Windows-only CPU affinity and process priority rule manager
</p>

<p align="center">
  <a href="README.md">繁體中文</a> · <strong>English</strong>
</p>

FrameAnchor is a Windows desktop utility that continuously monitors selected games or applications and automatically applies CPU affinity, CPU priority, and optional I/O and memory priority rules when a target process starts.

It is intended for experimenting with CPU core layouts, reducing unnecessary core migrations, or separating background workloads from latency-sensitive processes. FrameAnchor does not guarantee higher average FPS; results depend on CPU topology, the game engine, background load, and Windows scheduler behavior.

## Table of Contents

- [Features](#features)
- [Affinity Modes](#affinity-modes)
- [Security and Known Limitations](#security-and-known-limitations)
- [Installation](#installation)
- [Usage](#usage)
- [Configuration and Data](#configuration-and-data)
- [GPU Benchmark (Beta)](#gpu-benchmark-beta)
- [Development and Building](#development-and-building)
- [Architecture](#architecture)
- [License](#license)

## Features

- **Persistent rules**: Once saved, rules are continuously monitored and applied in the background.
- **Fast process discovery**: A lightweight discovery pass runs every 100 ms. Full path resolution and process handle acquisition are attempted only after an executable name matches.
- **Full-path or file-name matching**: Precisely target one installation path or follow an executable whose location may change.
- **Five affinity modes**: All cores, exclude SMT siblings, P-cores only, custom cores, and a preferred-core list.
- **Process priority**: Idle, Below Normal, Normal, Above Normal, and High are supported. Realtime is intentionally unavailable.
- **Advanced priorities**: Optionally set I/O priority and memory priority.
- **CPU Dashboard**: Shows real-time system usage for each logical processor, P-core/E-core and SMT sibling information, and applied-process status.
- **Create rules from running windows**: Capture the executable path from a currently visible desktop window.
- **System tray operation**: Supports close-to-tray, start minimized, and single-instance behavior.
- **Start with Windows**: Uses Windows Task Scheduler to launch with the highest privileges at user logon.
- **Bilingual interface**: Traditional Chinese and English.
- **GPU benchmark (Beta)**: Tests GPU driver interrupt affinity per logical processor on a selected GPU, finds the best core for GPU interrupts, and can import the result into a rule draft with one click.

## Affinity Modes

| Mode | Behavior |
| --- | --- |
| `All` | Reports and uses all logical processors without calling an affinity setter. |
| `NoSmtSibling` | Selects the primary logical processor of each physical core and excludes SMT/Hyper-Threading siblings. |
| `PCoresOnly` | Selects physical cores with the highest efficiency class in the detected topology; primarily intended for Intel hybrid CPUs. |
| `Custom` | Manually select logical processors. |
| `Prefer` | Uses a manually selected core list. The current implementation still tries hard affinity, thread ideal processors, and CPU Sets in order, so this mode is not guaranteed to remain soft-only. |

For modes that restrict the core set, the backend tries the following mechanisms in order:

1. `SetProcessAffinityMask`
2. Per-thread `SetThreadIdealProcessorEx`
3. `SetProcessDefaultCpuSets`

The Dashboard displays the selected core list and the resulting application status.

## Security and Known Limitations

### Administrator Privileges

FrameAnchor uses `requireAdministrator` in its Windows manifest. Manual launches display a UAC prompt. A Task Scheduler entry created by the application can launch it with the highest privileges at logon.

### Anti-Cheat Systems

FrameAnchor only uses standard Win32 APIs. It does not install a driver, inject into target processes, or attempt to bypass anti-cheat protection. Easy Anti-Cheat, BattlEye, Vanguard, and other protected processes may reject operations with `ACCESS_DENIED`.

For an anti-cheat-protected target, you can start FrameAnchor before launching the game. FrameAnchor attempts to acquire a process handle early, but this does not guarantee that a protected process will allow changes.

### Other Limitations

- **Windows only**; development and release targets are Windows 11.
- Only **processor group 0** is supported, with a maximum of 64 logical processors.
- PIDs below 8, critical Windows processes, executables under `System32`, and FrameAnchor itself are never modified.
- The highest CPU priority is **High**. Realtime is excluded because it can make the system unresponsive.
- Full-path matching is safer. File-name matching may affect another process with the same executable name.
- A game's or application's Terms of Service may restrict external scheduling tools. Check the relevant policy before use.
- Applying a rule does not guarantee better performance. Validate changes with repeatable frame-time measurements.

## Installation

### Install from Releases

Download the latest version from [GitHub Releases](https://github.com/LiuTouo/FrameAnchor/releases). Two distribution forms are provided:

- **NSIS installer** (`FrameAnchor_X.Y.Z_x64-setup.exe`): Standard install mode. Supports automatic updates via the Tauri updater plugin.
- **Portable** (`FrameAnchor_X.Y.Z_x64-portable.zip`): Extract to any directory and run. Supports online update checking at startup and on manual request; can download a new version, ask for confirmation, replace the executable, and restart.

Every release asset includes a SHA256 checksum file (`.sha256`).

### Build from Source

#### Prerequisites

- Windows 11
- [Node.js](https://nodejs.org/) 20 or later
- [Rust](https://www.rust-lang.org/tools/install) 1.80 or later with the MSVC toolchain
- Visual Studio Build Tools with the **Desktop development with C++** workload
- Microsoft Edge WebView2 Runtime, included with Windows 11 by default

#### Build Steps

```bash
npm ci
npm run tauri build
```

The NSIS installer is written to:

```text
src-tauri/target/release/bundle/nsis/
```

## Usage

1. Launch FrameAnchor and accept the Windows UAC prompt.
2. Open the **Rules** page.
3. Select a target from the list of running windows, or create/edit an existing rule.
4. Choose an affinity mode and CPU priority. Enable advanced I/O or memory priority settings if needed.
5. Select a matching method:
   - **Full path**: Matches only the executable at the specified location.
   - **File name**: Matches the same executable name under any path.
6. Apply and save the rule.
7. Keep FrameAnchor running in the background or system tray. The rule is applied when a matching target appears.
8. Check affinity, priority, and error status on the **Dashboard**.

After FrameAnchor exits, it no longer monitors newly started processes. Settings already applied to a running process generally remain until that process exits.

## Configuration and Data

The configuration file is stored at:

```text
%APPDATA%\FrameAnchor\config.json
```

Behavior and compatibility details:

- Rules and settings are stored as JSON.
- Missing fields in older configurations receive defaults for backward compatibility.
- If the configuration cannot be parsed, the original file is copied to `config.corrupt.json`, and FrameAnchor starts with defaults.
- The data directory can be opened directly from the Settings page.
- The full background maintenance interval can be set to 0.5–5 seconds in the UI. The high-frequency discovery pass remains fixed at 100 ms.

Default settings include:

- Language: Traditional Chinese
- Start minimized: enabled
- Close to tray: enabled
- Start with Windows: disabled
- Background maintenance interval: 1 second
- Advanced priority controls: hidden

## GPU Benchmark (Beta)

FrameAnchor includes a GPU benchmark that finds the **logical processor (LP) best suited to handle the selected GPU's driver interrupts**. It cycles the GPU driver's interrupt affinity across LPs, collects frame-times with a measurement tool, and reports the best core and cores that perform poorly, which can be imported into a rule draft in one click.

### How it differs from a general CPU/GPU benchmark

- **Not a graphics benchmark**: It does not compare image quality, scenes, or FPS between GPUs. The workload is a fixed alternating black/white, uncapped, no-vsync render.
- **Not "which core runs the game fastest"**: It measures which core yields the most stable/highest frame-times when handling GPU interrupts.
- Each test pins the GPU driver interrupt affinity to a single LP; statistics include Avg/Max/Min/STDEV and 1%/0.1%/0.01%/0.005% time-weighted lows.

### Expected duration

Each tested core takes roughly:

```text
sample seconds + warm-up seconds + startup wait (5 s) + driver restart/stabilize (~14 s) + margin
```

Total is approximately `cores × rounds × per-core time`. For example 16 cores, 30 s sample, 1 round, roughly 13–15 minutes. The UI shows an estimate before starting.

### Risk warning

The test repeatedly **disables/enables the selected GPU driver** (disable/enable), which may cause:

- Several seconds of black screen
- Temporary display dropout or resolution reset
- Other workloads using the same GPU (including browser hardware acceleration) to pause

**Do not operate the computer after starting**, until the test finishes or is cancelled. The test uses a crash-safe recovery journal; even a crash mid-test restores the pre-test policy on next launch.

### Data and history

- Sessions are stored under `%APPDATA%\FrameAnchor\benchmarks\<session-uuid>\` with `session.json` and per-round `round-<round>-lp-<core>.csv` files.
- The history list shows date, GPU, API, status, best core, and disk size for each session; details can be opened or deleted (with confirmation).
- In-flight recovery journal: `%APPDATA%\FrameAnchor\benchmark-recovery.json`; the one-level restore record after an apply: `gpu-restore.json`.
- History is never deleted automatically.

### Apply and restore semantics

- A test itself **never auto-applies** anything — every session restores the GPU interrupt affinity to its pre-test state.
- After completion you must explicitly press **"Apply best core to GPU"** (with confirmation) to pin interrupt affinity to the best LP.
- **"Restore previous setting"** returns to the policy before the most recent successful apply (one-level restore record).
- Both apply and restore confirm and briefly restart the GPU driver again (possible screen flicker).

### Compatibility restriction

- Each completed session stores the **CPU fingerprint** (CPU identity + topology) and the GPU's stable PnP instance ID.
- Only sessions whose **current CPU fingerprint matches** and whose **GPU is still present** can be applied or imported; incompatible history stays viewable but apply/import is disabled with a reason.
- If the current CPU hardware no longer matches a recommendation stored on a rule, a stale-hardware warning is shown while the data is preserved.

## Development and Building

Install dependencies:

```bash
npm ci
```

Common commands:

```bash
npm run dev
# Starts only the Vite frontend at http://localhost:1420
# The Rust backend and Tauri IPC are not available

npm run tauri dev
# Starts the complete Tauri application

npm run check
# Runs svelte-check and TypeScript checks

npm run build
# Builds the frontend into dist/

npm run tauri build
# Builds the release executable and NSIS installer

npm run gen-icons
# Regenerates src-tauri/icons/*

npm run build:benchmark-assets
# Builds the D3D9 workload sidecar (Rust + Direct3D 9) and copies it into the resources dir

npm run verify:benchmark-assets
# Verifies bundled benchmark resources (SHA-256 of PresentMon/liblava and D3D9 sidecar presence)

npm run fetch:benchmark-assets
# Re-downloads PresentMon and the liblava workload and updates SHA256SUMS
```

`npm run tauri build` automatically runs the frontend build, the D3D9 sidecar build, and the resource verification in order, so the bundle always includes the bundled tools and their license notices.

Rust checks and tests:

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

The complete application and its process operations depend on Windows APIs. Changes involving live processes, affinity, priority, CPU Sets, the tray, Task Scheduler, or WebView2 still require manual verification on Windows against a disposable test process.

## Architecture

| Layer | Technology |
| --- | --- |
| Desktop framework | Tauri v2 |
| Frontend | Svelte 5 runes, TypeScript, Vite |
| Backend | Rust, tokio |
| Windows integration | `windows` crate and direct Win32 APIs |
| Internationalization | `svelte-i18n` |
| Installer | NSIS |

The runtime has two main background tasks:

- **Watcher**: A 100 ms discovery pass plus full maintenance, retries, and status updates at the configured interval.
- **Usage sampler**: Reads per-logical-processor system usage once per second while the Dashboard needs it and at least one applied process is running.

The original product specification is available in [`PLAN.md`](PLAN.md). When it differs from the current implementation, the code is authoritative.

## Release Process

Maintainers trigger automated builds and releases by pushing a semantic version tag. The CI workflow validates version consistency across all files, checks the updater signing key, runs frontend type checks and Rust tests, then builds all artifacts:

- NSIS installer (`FrameAnchor_X.Y.Z_x64-setup.exe`) with `.sha256`
- Portable ZIP (`FrameAnchor_X.Y.Z_x64-portable.zip`) with `.sha256`
- Updater `latest.json` and signature files

### Updater Signing Key Setup

Before the first release, generate an Ed25519 key pair:

```bash
npm run tauri signer generate -- -w src-tauri
```

Set the private key content as the GitHub Actions secret `TAURI_SIGNING_PRIVATE_KEY` (and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` if password-protected). Write the public key into `src-tauri/tauri.conf.json` under `plugins.updater.pubkey`, replacing the placeholder `REPLACE_ME_WITH_YOUR_PUBLIC_KEY_BASE64`.

**Never commit the private key.** The CI workflow validates that the public key has been replaced and the secret is present; builds fail with a clear error otherwise.

### Release Steps

1. Sync version numbers in `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`.
2. Commit and tag with `vX.Y.Z` format.
3. Push the tag to trigger the workflow.
4. Download assets from [GitHub Releases](https://github.com/LiuTouo/FrameAnchor/releases).

Windows binaries are **not code-signed**. Windows Defender SmartScreen may show a warning on download and first launch. This is expected and does not affect functionality.

## Project Status

The project is still at an early stage, and APIs, configuration formats, and scheduling behavior may change in later releases. Both the installer and portable editions support automatic update checking and manual updates; the version number is dynamically obtained from the built-in executable metadata.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

### Third-party notices

The GPU benchmark feature bundles and redistributes the following third-party components (each under its own license):

- **PresentMon 2.5.1** (Intel) — [MIT License](src-tauri/resources/benchmark/LICENSE-PresentMon.txt). Frame-time collection tool; verified by a fixed SHA-256 before execution.
- **liblava Vulkan workload** (`lava-triangle.exe`, distributed via valleyofdoom/AutoGpuAffinity and built on the liblava framework) — [MIT License](src-tauri/resources/benchmark/LICENSE-liblava.txt). Vulkan test workload; also SHA-256 verified before execution.
- **Direct3D 9 workload** (`d3d9-workload.exe`) — a sidecar written in Rust directly against the Win32 Direct3D 9 API by this project (see `src-tauri/d3d9-workload/`), licensed under GPL-3.0 like the project itself.

Full license texts and the SHA-256 manifest live in `src-tauri/resources/benchmark/`.
