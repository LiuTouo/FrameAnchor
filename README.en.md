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

If prebuilt versions are available, download the NSIS installer from [GitHub Releases](https://github.com/LiuTouo/FrameAnchor/releases).

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
```

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

## Project Status

The current version is **0.1.0**. The project is still at an early stage, and APIs, configuration formats, and scheduling behavior may change in later releases.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).
