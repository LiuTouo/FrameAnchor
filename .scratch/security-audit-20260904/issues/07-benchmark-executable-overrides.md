# 07 — production IPC/session 的 benchmark executable override 可選未驗證 PE

- 來源 findings:#17、#22、#25、#28、#29(CWE-73,severity medium)
- 位置:`src-tauri/src/benchmark/mod.rs:114-127`(欄位)、`ipc.rs:160-168`、`manager.rs:703-768, 1501-1523`(resolver 覆寫)、`runner.rs:1782-1816`、`process_win.rs:140-149`(spawn)
- **狀態:done 2026-09-04**

## 實作

- `BenchmarkConfig.workload_exe_path` / `presentmon_path` 自 production schema 移除;
  serde 預設忽略 unknown fields,舊 session.json 帶此兩欄仍可載入(值丟棄)。
- `resolve_assets(app)` 不再接受 config,一律解析內建 `resources/benchmark`。
- runner `workload_command` 直接取 assets;`should_guard_close` 改為
  `workload == Vulkan`(Vulkan workload 恒為內建 lava-triangle)。
- 前端同步:`types.ts` 刪兩欄、`GpuTest.svelte` 不再送 null。
- 「自訂 exe 不 guard/不 resize」兩個測試隨概念移除刪除;D3D9 不 guard 測試保留。

## 驗收

- IPC 傳非 null override → serde 忽略,backend 只可能 spawn 內建路徑。
- `cargo test` 431 全過、`npm run check` 0 errors。

## 問題

`BenchmarkConfig.workloadExePath` / `presentmonPath` 是 production IPC 與 persisted session schema 欄位;resolver 直接覆寫實際要 spawn 的路徑,現有 verifier 不綁定 override。正常 UI 送 null,但 renderer compromise 或偽造 Equivalent session 即可以 administrator token 執行任意 PE。

## 修法

- 從 production IPC 與 persisted schema **移除**兩個 override 欄位;測試注入改走 `#[cfg(test)]` / test DI。
- 後端固定解析受保護 root 下的 sidecars;對實際要 spawn 的 file identity 驗證 embedded digest(與 ticket 08 同一步)。
- 舊 session.json 載入:unknown field 忽略即可(serde 預設),不需遷移。

## 驗收

- release build IPC 傳非 null override → 後端拒絕、無 child process。
- 竄改 session 兩欄觸發 Equivalent validation → spawn 前 fail closed。
