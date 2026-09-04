# 11 — fetch-benchmark-assets 下載後才從自身產生信任 digest

- 來源 findings:#38、#41、#42、#45、#51(CWE-494,severity low)
- 位置:`scripts/fetch-benchmark-assets.mjs:48-108`
- **狀態:done 2026-09-04**

## 實作

- script 內嵌 `KNOWN_GOOD` trust root(PresentMon / lava 最終產物 — 含 DPI manifest
  嵌入後 — 的固定 SHA-256)。
- 下載 + mt.exe 嵌 manifest 後、覆寫 vendored 檔與 SHA256SUMS **之前**比對;
  不符即 abort,不寫任何 manifest。上游替換無法再經 refresh 進入 release。
- digest 更新流程改為人工:升級上游版本時,同一 PR 內人工確認 + 同步更新
  KNOWN_GOOD,信任根變更必須獨立 review。

## 問題

refresh script 只檢查 curl 成功,下載後立刻對未驗證 bytes 算 SHA256 並覆寫 `SHA256SUMS` — 剛下載的 bytes 自己成為 trust root。上游 asset 被替換 + maintainer 執行 refresh 後,惡意 PE 進入所有 release 並以 administrator token 執行。

## 修法

- script 內(或獨立受審核 metadata 檔)固定 known-good SHA256 / publisher signer identity;下載後**先驗證**再 copy / 生成 manifest。
- digest 更新需獨立 review,不可由下載 script 自動升級 trust root。

## 驗收

- 模擬上游替換 + matching 新 digest fixture → refresh 在 known-good 驗證階段拒絕。
