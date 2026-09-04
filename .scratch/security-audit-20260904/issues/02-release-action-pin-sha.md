# 02 — release workflow 的 mutable action ref 接觸 signing key

- 來源 findings:#34、#39、#43、#47、#50(CWE-829,severity low)
- 位置:`.github/workflows/release.yml`(tauri-action@v1 + GITHUB_TOKEN + TAURI_SIGNING_PRIVATE_KEY 同 step)

## 問題

`tauri-apps/tauri-action@v1` 是可移動 tag,不是 immutable commit SHA;上游 ref 被控制時可竊取 updater signing key 或發布有效簽章的惡意更新。

## 修法

- `uses:` 改 pin 到經審核的完整 commit SHA。
- 後續升級透過 Dependabot / 人工 review 改 SHA,不追 tag。
- (可後續)build 與 sign/upload 拆 job,只有 approval-gated environment job 拿 key — 本票只做 pin。

## 驗收

- workflow 內無 `@v1` 形式 third-party ref;release 流程實際跑一次成功。
