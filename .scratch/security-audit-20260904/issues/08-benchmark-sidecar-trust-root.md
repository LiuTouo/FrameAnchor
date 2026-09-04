# 08 — 可寫同目錄 manifest 與 exists-only D3D9 無法保護 elevated sidecars

- 來源 findings:#19、#20、#21、#23、#33(CWE-353,severity medium)
- 位置:`src-tauri/src/benchmark/assets.rs:71-128`、`src-tauri/resources/benchmark/SHA256SUMS`、`runner.rs:1782-1816, 1898-1902`、`process_win.rs:140-149`
- **狀態:done 2026-09-04**

## 實作

- trust root 內嵌:`build.rs` 於 build 時解析 `resources/benchmark/SHA256SUMS`,
  生成 `BUILTIN_DIGESTS` 常數;`d3d9-workload.exe` 產生 per-build digest
  (`D3D9_WORKLOAD_DIGEST`,build 時檔案不存在 → None + runtime 退回存在檢查 + warn)。
- `assets::verify` 改為對內嵌 digest 逐檔比對(固定兩個內建檔 + D3D9),
  runtime 不再讀取資源樹中的 SHA256SUMS;`parse_manifest`、`AssetError::Manifest`、
  `BenchmarkAssets.manifest` 移除。
- session 開始即驗證:manager `start` / `resolve_and_verify_assets` 與 runner
  `pre_flight`(workload/ PresentMon spawn 前)兩層都呼叫 `assets::verify`。
- 測試:`verify_passes_on_vendored_resources` 以真實 vendored 檔案驗內嵌 digest;
  runner `make_assets` 改 hardlink vendored 檔(偽造內容無法通過 digest)。

## 殘餘(記錄,不阻塞)

- pre_flight 驗證到 spawn 之間仍有秒級 TOCTOU 窗口;完整關閉需 protected
  resource tree(ACL/MIC)或 verify-to-use file identity binding。攻擊者已無法
  透過改 manifest 過驗,窗口僅剩直接替換 sidecar 並命中時序,風險大幅縮小。

## 問題

PresentMon/lava 的 `SHA256SUMS` 與 binaries 共置於可寫 resource tree — manifest 是**待驗證物自己**的資料,不是 trust root;parser 也不要求固定 entries(可只列無關檔)。D3D9 workload 只做 `exists()` 檢查。同時可寫兩者的 attacker 替換 sidecar + 改 manifest 即通過驗證,benchmark 以 administrator token spawn attacker PE。

## 修法

- 把三個正式 sidecar(PresentMon / lava / D3D9)的 digest **內嵌**進受信任 main exe(build 時由 SHA256SUMS 產生 `include!` 常數或 const 陣列)。
- 驗證固定三個名稱、protected absolute path、同一 file identity;拒絕 reparse points;spawn 前最後一次驗證。
- `parse_manifest` 改為嚴格比對 embedded 集合(或整個移除 runtime manifest 讀取,只留 embedded digest)。
- 與 ticket 07 的「驗證實際 spawn identity」是同一機制,一起做省工。

## 驗收

- 同時替換 PresentMon 與 SHA256SUMS → 仍以 embedded trust root 拒絕。
- manifest 只列無關檔 → 拒絕。
- 替換 D3D9 → digest mismatch、無 child。
