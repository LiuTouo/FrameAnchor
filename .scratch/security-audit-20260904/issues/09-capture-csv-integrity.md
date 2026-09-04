# 09 — benchmark capture CSV 未綁定本次 capture identity

- 來源 findings:#18、#27、#31(CWE-345/367,severity medium)
- 位置:`src-tauri/src/benchmark/runner.rs`(run_capture、round_csvs、ranking/confirmation/final stats)、`metrics.rs`
- **狀態:done 2026-09-04**

## 實作

1. **stale 刪除失敗 = fatal**:capture 前清除舊 CSV 失敗(可能被鎖定/佔用)改為
   直接 `Err(BENCHMARK_CAPTURE_MISSING)`,不繼續 capture — 既有 shaped CSV 無法
   被當成本次 capture。
2. **PresentMon exit code 參與 success 判定**:`Exited` 且完整性通過時,若
   PresentMon exit code 非零(可取得時),capture 仍判失敗(`presentmon_exit_N`
   拒絕原因記入診斷)— 生產者未成功就不採信產物。
3. **capture identity 綁定** — 新型別 `CapturedCsv { path, sha256 }`:
   - capture 成功當下立即讀取、解析驗證並記錄內容 digest。
   - 所有下游讀取(ranking、confirmation、final stats、drift、校準)經
     `CapturedCsv::read()`:重讀檔 + digest 比對,與 capture 當下不符即
     fail closed(`BENCHMARK_CSV_INVALID`)。
   - `RoundCsvs` type alias 取代散落的 `HashMap<u32, HashMap<u32, PathBuf>>`。

   註:採「digest 綁定」而非完全 in-memory 單次讀取 — 下游多次消費的架構下,
   此法以最小侵入達成同等的 tamper-evidence;若未來要完全免 IO,可在此型別上
   快取 frames。

## 測試

- `captured_csv_detects_post_capture_replacement`:capture 後置換內容 → read 拒絕。
- `run_capture_fails_closed_when_stale_csv_cannot_be_removed`:share_mode(1) 鎖住
  stale CSV → capture 拒絕(含測試前提自檢)。
- 既有 ~40 個 runner 流程測試經 helper(`write_round_csv` 回傳 CapturedCsv)無縫
  遷移,443 tests 全過。
