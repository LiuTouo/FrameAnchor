# 10 — portable update 缺獨立 publisher 簽章

- 來源 findings:#37、#40、#46、#49、#52(CWE-494,severity low)
- 位置:`src-tauri/src/update.rs:248-364`、`.github/workflows/release.yml:202-235`、`tauri.conf.json:47-54`
- **狀態:done 2026-09-04(首次帶簽章資產的 release 發布後,實機驗證一次更新流程)**

## 實作

- release workflow:產生 `portable-update.json`(`schema/version/asset/sha256`,綁定
  tag、ZIP 檔名與實際 hash),以 **updater 簽署 key**(`tauri signer sign`)簽署,
  上傳 `.update.json` + `.update.json.sig`;上傳步驟強制檢查簽章檔存在。
- client(`update.rs`):
  - `fetch_portable_release` 強制要求 metadata + signature 資產,缺一即拒絕(舊版
    release 無簽章 → 新 client 不更新,fail closed)。
  - `verify_portable_update`:minisign 驗章(內嵌 public key,取自 tauri.conf.json
    的 `plugins.updater.pubkey`,與 installed updater 同 keypair、單一來源)→
    metadata 綁定檢查(schema=1 / version==release / asset==下載名 / sha256==實際)。
  - 保留 `.sha256` 校驗作為縱深防禦。
- rollback 保護:既有 semver 比較(client >= latest 不更新)繼續涵蓋。
- 測試:minisign sign→verify round-trip(dev-dep 生成 throwaway keypair)、
  metadata 綁定(version/asset/hash/schema)五案、pubkey 自 config 解析。

## 已知事項

- 舊版 release(無 `.update.json`)無法被新 client 更新 — 首次帶簽章的 release
  需使用者手動安裝一次,之後恢復自動更新。
- workflow 中 `TAURI_SIGNING_PRIVATE_KEY` 需為 base64 編碼的 key 內容(與
  tauri-action 步驟同一 secret,已接入簽章步驟)。
