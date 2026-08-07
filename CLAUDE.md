# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

FrameAnchor is a Windows-only desktop utility that persistently applies CPU affinity and priority rules to game processes. It is a single elevated executable built with **Tauri v2**, **Svelte 5 runes**, TypeScript, Rust, and direct Win32 APIs. It also provides a tray UI, per-core usage monitoring, window-based rule creation, and Task Scheduler autostart.

`PLAN.md` is the original product specification, but the implementation has evolved beyond parts of it (notably affinity modes, watcher cadence, and shared state). When they disagree, treat the code as authoritative.

## Development commands

The repository uses npm (`package-lock.json`) and requires Windows, Node 20+, Rust 1.80+ with the MSVC toolchain, Visual Studio Build Tools, and WebView2.

```bash
npm ci                  # Install exact frontend/Tauri CLI dependencies
npm run dev             # Vite frontend only, http://localhost:1420
npm run check           # svelte-check + TypeScript checks; no ESLint/Prettier configured
npm run build           # Production frontend build to dist/
npm run tauri dev       # Full Rust + frontend development app
npm run tauri build     # Release build and NSIS installer
npm run gen-icons       # Regenerate src-tauri/icons/*
```

Rust checks and tests can be run from the repository root:

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml <test_name>  # Substring-filter one test
```

Examples of test filters include `fullpath_matches_case_insensitive`.

For built-executable DLL/export failures such as `0xC0000139`, use:

```bash
node scripts/pe-imports.mjs <exe>
node scripts/pe-exports.mjs <dll>
```

## Runtime architecture

### Startup and shared state

`src-tauri/src/main.rs` is the composition root. Startup order is intentional:

1. Release builds use the Windows GUI subsystem (no console window); debug builds retain the console.
2. Stale app-owned WebView2 processes are killed.
3. `SeDebugPrivilege` is enabled.
4. A panic hook writes `%TEMP%\frameanchor-panic.log`.
5. CPU topology and `%APPDATA%\FrameAnchor\config.json` are loaded.
6. Tauri plugins, tray, IPC commands, watcher, and usage tasks are started.

The app is always elevated through the custom manifest in `src-tauri/build.rs`. That manifest also carries the Common Controls v6 dependency and PerMonitorV2 DPI awareness; replacing it without those entries can reintroduce `TaskDialogIndirect`/`comctl32` startup failures.

Tauri manages an `Arc<AppState>` containing:

- `RwLock<Config>` for settings and rules
- startup-enumerated `Topology`
- PID-indexed applied-state and cached-handle maps
- a tokio watch channel controlling usage streaming
- an atomic quit flag used to bypass close-to-tray interception

The main window starts hidden. It is shown unless either `--minimized` or `settings.startMinimized` requests tray-only startup. The single-instance plugin wakes the existing window on a second launch.

### Background tasks

Two long-lived async tasks run on Tauri's tokio runtime:

- **Watcher (`watcher.rs`)**: one loop with a fixed 100 ms discovery pass plus a full maintenance tick at `pollIntervalMs` (backend-clamped to 200–60,000 ms, default 1,000 ms).
- **Usage sampler (`usage.rs`)**: samples `NtQuerySystemInformation(SystemProcessorPerformanceInformation)` every second and emits `usage-update`; `Dashboard.svelte` enables it only while mounted and when at least one applied-rule process is running.

The discovery pass first filters Toolhelp process names against enabled rule executable names, then performs expensive path resolution only for candidates. It opens and caches process handles as early as possible so later rule application can reuse handles acquired before anti-cheat protection tightens new handle permissions.

The full watcher tick removes dead/stale PIDs, detects PID reuse by executable name and creation time, handles deleted/disabled rules, catches processes missed by discovery, retries failures, refreshes applied state, and emits `applied-update`. `ACCESS_DENIED` retries indefinitely with a 30-second backoff; other failures get three retries.

### Rule application pipeline

The backend flow spans several modules:

1. `watcher.rs` matches the first enabled rule by normalized full path or case-insensitive file name.
2. `topology.rs` resolves the affinity specification against startup CPU topology.
3. `process.rs` performs process/thread operations and enforces the system-process blacklist.
4. `priority.rs` applies optional I/O and memory priorities as best-effort operations.
5. `commands.rs` publishes the resulting `AppliedProcess` list to the frontend and updates the tray count.

Affinity modes are `All`, `NoSmtSibling`, `PCoresOnly`, `Custom`, and `Prefer`. For selected core sets, application currently falls back through:

1. `SetProcessAffinityMask` hard affinity
2. per-thread `SetThreadIdealProcessorEx`
3. `SetProcessDefaultCpuSets`

`All` reports all logical processors without calling an affinity setter. `Prefer` takes its selected cores directly from `AffinitySpec.cores`, but the current watcher still sends them through the same hard → soft → CPU Sets fallback; do not assume it is soft-only without changing `watcher.rs`.

Only processor group 0 is supported (maximum 64 logical processors). Critical processes, PIDs below 8, and executables under `System32` are never modified. CPU priority intentionally stops at `High`; realtime priority is not exposed.

## Frontend architecture

`src/App.svelte` is a manual three-tab shell (Dashboard, Rules, Settings), not a router. On mount it loads initial state through typed wrappers in `src/lib/ipc.ts`, initializes the stores in `src/lib/stores.ts`, sets the locale, and subscribes to backend events.

The main data flow is:

```text
Svelte pages/components
  ↕ writable stores
src/lib/ipc.ts invoke wrappers + Tauri event listeners
  ↕
commands.rs / watcher.rs / usage.rs
  ↕
AppState + Win32 operations
```

Rules are edited in Svelte, saved through IPC, persisted by `config.rs`, and then reapplied by clearing the applied-state map so the watcher rebuilds it. Changing language also rebuilds the native tray menu.

## Cross-layer contracts

Several definitions are intentionally duplicated and must be updated together:

- **Rust/TypeScript models**: changes to serialized types in `src-tauri/src/model.rs`, topology output, or `watcher.rs::AppliedProcess` must be mirrored in `src/lib/types.ts`. Struct fields serialize as camelCase; enum variants serialize as PascalCase strings.
- **Affinity semantics**: `topology.rs::resolve_mask()` and frontend `src/lib/affinity.ts::resolveCores()` must select the same logical processors. The frontend copy drives picker state and dashboard highlighting.
- **IPC surface**: a new command needs a `#[tauri::command]`, registration in `main.rs`, and a typed wrapper in `src/lib/ipc.ts`. Backend events need matching frontend listener payload types.
- **Error codes**: `src-tauri/src/error.rs` returns stable code strings, not user-facing prose. Add every new code to both `src/i18n/en.json` and `src/i18n/zh-TW.json` under `errors.*`.
- **Configuration compatibility**: model fields use serde defaults so older JSON remains loadable. `config.rs` writes via a temporary file and backs up corrupt input as `config.corrupt.json`.

## Testing boundaries

Rust unit tests live in module-local `#[cfg(test)]` blocks and cover topology/mask resolution, path normalization and blacklisting, config loading/round-tripping, and watcher rule matching.

There are no frontend, integration, or E2E tests. Code that touches live processes, process handles, thread affinity, CPU Sets, the tray, Task Scheduler, or WebView2 must be verified on Windows with `npm run tauri dev` against a disposable process.

## Code conventions

- Comments and log messages are written in **繁體中文**; identifiers, type names, and error-code strings remain English.
- Win32 behavior is implemented directly with the `windows` crate rather than `sysinfo`; preserve explicit rights, handle lifetimes, and best-effort/error distinctions when changing process operations.
