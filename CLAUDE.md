# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

FrameAnchor is a Windows desktop tool for competitive gamers. It applies persistent CPU affinity and priority rules to game processes automatically — game launches, FrameAnchor detects it, applies the rule. Built with **Tauri v2**, **Svelte 5** (runes mode), and **Rust** (Win32 API). Single exe that runs as admin (requireAdministrator manifest), tray icon, optional autostart via Task Scheduler (no UAC at login).

Full spec: `PLAN.md`.

## Commands

```bash
npm run dev          # Vite dev server only (frontend hot-reload)
npm run build        # Vite production build
npm run check        # svelte-check type-checking
npm run tauri dev    # Full Tauri dev (Rust + frontend, opens Window)
npm run tauri build  # Production NSIS installer
```

Rust unit tests:
```bash
cd src-tauri; cargo test
```

Single test:
```bash
cd src-tauri; cargo test <test_name>
```

## Architecture

### Process model
- Single `FrameAnchor.exe`, manifest `requireAdministrator` → always admin
- Tauri main thread handles UI event loop
- Two long-lived tokio async tasks spawned at startup:
  - **Watcher task**: two cadences in one loop — 100ms **discovery pass** (lightweight name-only scan; new matching PID → immediately open + cache process handle + apply rule) and full tick at `pollIntervalMs` (default 1s; cleanup, retries, state read-back). Early handle acquisition is the anti-cheat strategy: EAC's `ObRegisterCallbacks` only strips rights from *newly opened* handles, so a handle opened in the first ~100ms of process life keeps working after protection attaches.
  - **Usage task**: samples `NtQuerySystemInformation(SystemProcessorPerformanceInformation)` every 1s, emits `usage-update` event. Pauses when dashboard tab not visible (controlled by `set_usage_streaming` command) — power/CPU saving
- Shared state: `Arc<AppState>` with `RwLock<Config>`, static `Topology` (enumerated once at startup), `RwLock<HashMap<u32, AppliedEntry>>` (PID → applied state), `RwLock<HashMap<u32, CachedHandle>>` (PID → early-acquired handle, kept for process lifetime)

### Backend module map (src-tauri/src/)

| Module | Role |
|---|---|
| `main.rs` | Setup, plugin registration, command registration, window event handling (closeToTray) |
| `model.rs` | Serde data types: `Config`, `Settings`, `Rule`, `AffinitySpec`, `AffinityMode`, `CpuPriority`, `IoPriority`, `MemPriority`, `AdvancedSpec` |
| `topology.rs` | CPU topology enumeration via `GetLogicalProcessorInformationEx`, SMT sibling detection, P/E-core labeling via `EfficiencyClass`. Exports `resolve_mask()` for affinity mode → u64 bitmask |
| `process.rs` | Process enumeration (Toolhelp snapshot), handle opening (`OpenProcess`), affinity set/get (`SetProcessAffinityMask`/`GetProcessAffinityMask`), priority set/get (`SetPriorityClass`/`GetPriorityClass`), soft affinity (`SetThreadIdealProcessorEx`), path normalization, orphan WebView2 cleanup (`kill_orphan_webviews`), system process blacklist |
| `priority.rs` | I/O priority via `NtSetInformationProcess(ProcessIoPriority)`, memory priority via `SetProcessInformation(ProcessMemoryPriority)`. Best-effort — failures logged but don't fail the overall rule application |
| `watcher.rs` | Rule engine: 100ms discovery pass (early handle acquisition) + polling loop, exe matching (fullPath/fileName, case-insensitive), rule application with retry (3 retries; ACCESS_DENIED = 30s backoff, anti-cheat tolerant), PID reuse detection (exe name + process creation time), handle cache purge, emits `applied-update` event |
| `usage.rs` | Per-core utilization via `NtQuerySystemInformation`, delta-based calculation, streaming on/off via tokio watch channel |
| `windows_enum.rs` | Browse dialog: `EnumWindows` → filter visible, titled, non-cloaked, non-toolwindow → `QueryFullProcessImageNameW` → icon extraction via `SHGetFileInfoW` |
| `config.rs` | `%APPDATA%\FrameAnchor\config.json`, atomic write (tmp + rename), corrupt file backup (`config.corrupt.json`), serde defaults for missing fields |
| `tray.rs` | System tray icon and context menu (show window, applied count, autostart toggle, quit), menu rebuild on language change |
| `autostart.rs` | Task Scheduler via `schtasks.exe /Create /SC ONLOGON /RL HIGHEST`, no COM |
| `commands.rs` | All `#[tauri::command]` IPC handlers, emits `applied-update` after mutations |
| `error.rs` | `thiserror` enums — `ProcessError` (with `AccessDenied` variant), `PriorityError`, `TopologyError`. Error codes as stable string keys for frontend i18n lookup |

### Frontend (src/)

- `App.svelte` — shell with left nav (Dashboard/Rules/Settings), manual tab switching (no router), initializes stores and event listeners on mount
- `pages/Dashboard.svelte` — CPU topology grid (per-core usage bars, HT/P/E badges) + applied process table
- `pages/Rules.svelte` — rule card list, new-rule-from-browse button, edit/delete
- `pages/Settings.svelte` — all settings as checkboxes/dropdowns/slider
- `components/TopologyGrid.svelte` — read-only usage grid for dashboard
- `components/AffinityPicker.svelte` — editable core checkbox grid for rule editor, preset buttons
- `components/CoreCell.svelte` — single core cell (shared by grid and picker)
- `components/RuleCard.svelte` — single rule editor card
- `components/AppliedTable.svelte` — applied processes table
- `components/BrowseDialog.svelte` — modal window picker for creating rules
- `lib/ipc.ts` — typed `invoke()` wrappers for all commands
- `lib/types.ts` — TypeScript interfaces matching Rust data model
- `lib/stores.ts` — Svelte writable stores for topology, rules, settings, applied, usage
- `i18n/zh-TW.json`, `i18n/en.json` — bilingual dictionaries, key-based (`nav.dashboard`, `errors.ACCESS_DENIED`, etc.), `svelte-i18n`

### IPC

Commands (frontend calls backend):
- `get_topology`, `list_windows`, `get_rules`, `save_rule`, `delete_rule`, `get_settings`, `save_settings`, `set_autostart`, `get_applied`, `reapply_all`, `set_usage_streaming`, `open_data_folder`

Events (backend pushes to frontend):
- `usage-update: number[]` — per-LP utilization, 1s interval when dashboard visible
- `applied-update: AppliedProcess[]` — applied process list, emitted on change

### Key design decisions

- **No `sysinfo` crate** — all Win32 is hand-written for precise control over affinity masks and topology
- **No REALTIME_PRIORITY_CLASS** — would starve system threads; `High` is the maximum exposed
- **Affinity v1**: only processor group 0 (max 64 LP), covers all mainstream gaming CPUs
- **Soft affinity (Prefer mode)**: sets thread ideal processor via `SetThreadIdealProcessorEx` instead of hard affinity mask — useful for games that react badly to hard affinity
- **Anti-cheat strategy (EAC etc.)**: open the process handle within ~100ms of process creation (100ms discovery pass), cache it for the process lifetime, and route all apply/retry through it — `ObRegisterCallbacks` only strips rights on *new* opens, so pre-protection handles stay usable. No kernel driver, no memory access — legitimate Win32 only. Requires FrameAnchor running before game launch; if the race is lost, `ACCESS_DENIED` gets 30s backoff and the UI tells the user to restart the game. `SeDebugPrivilege` enabled at startup (helps ACL-protected processes, doesn't bypass kernel callbacks)
- **Blacklist**: system processes (PID < 8, critical exe names, everything under `System32`) are never touched even if user creates a rule
- **Orphan WebView2 cleanup**: on startup, kills stale `msedgewebview2.exe` processes whose `--user-data-dir` points to this app (prevents 0x8007139F white-screen on restart after crash)
- **Console subsystem** (not `windows_subsystem="windows"`): console hidden on release via `FreeConsole()`, because windows subsystem breaks WebView2 environment creation on some machines

## Testing

Rust unit tests in `#[cfg(test)]` blocks within each module. Covers:
- `topology`: `resolve_mask` for all 4 modes × 3 fake topologies (uniform 8C, SMT 8C16T, hybrid 8P+8E), custom cores clamping, empty catch
- `process`: path normalization, blacklist checks (system PIDs, critical names, System32 paths)
- `config`: roundtrip, corrupt file backup, missing fields defaults
- `watcher`: rule matching (fullPath/fileName, extended prefix, case insensitivity)

No frontend tests. No integration/E2E tests.
