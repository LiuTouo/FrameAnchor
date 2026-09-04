# Security audit triage — 2026-09-04

來源:Codex security scan @ `1df767b`(`report.md` / `findings.json` / `coverage.json` / `scan-manifest.json` / `results.sarif`)。
報告 52 條為重複計數;去重後 12 個 canonical findings(Surfaces 表),合併為 11 張 ticket(pe-*.mjs 的 parser-DoS 與 terminal-injection 同檔同修,合併為 `03`)。

所有 high finding 的共同前提:same-user medium-integrity attacker 可寫 TEMP/APPDATA。對常駐提升權限 app,medium→admin 跨界仍值得修,severity 不打折。

## Tickets(建議施工順序)

| Ticket | Findings | Sev | 狀態 |
| --- | --- | --- | --- |
| [01 System32 絕對路徑](issues/01-system-tool-absolute-paths.md) | #5/6/9/12/15 | high | done 2026-09-04 |
| [02 Pin release action SHA](issues/02-release-action-pin-sha.md) | #34/39/43/47/50 | low | done 2026-09-04 |
| [03 pe 診斷腳本 bounded parse + escape](issues/03-pe-scripts-bounded-parse.md) | #35/44 + #36/48 | low | done 2026-09-04 |
| [04 portable update staging race](issues/04-update-staging-race.md) | #1/4/7/10 | high | done 2026-09-04 |
| [05 HIGHEST autostart task target](issues/05-autostart-highest-target.md) | #3/8/11/13/14 | high | done 2026-09-04(決策:可寫位置一律降 LIMITED) |
| [06 GPU recovery/restore state 認證](issues/06-gpu-recovery-auth.md) | #16/24/26/30/32 | medium | done 2026-09-04 |
| [07 移除 benchmark executable override](issues/07-benchmark-executable-overrides.md) | #17/22/25/28/29 | medium | done 2026-09-04 |
| [08 benchmark sidecar trust root](issues/08-benchmark-sidecar-trust-root.md) | #19/20/21/23/33 | medium | done 2026-09-04 |
| [09 capture CSV 綁定 capture identity](issues/09-capture-csv-integrity.md) | #18/27/31 | medium | done 2026-09-04 |
| [10 portable update publisher 簽章](issues/10-portable-update-signature.md) | #37/40/46/49/52 | low | done 2026-09-04(待 release 實跑驗證) |
| [11 fetch-benchmark-assets 預信任 digest](issues/11-fetch-assets-pretrusted-digest.md) | #38/41/42/45/51 | low | done 2026-09-04 |

## 報告未列 ticket 的 open questions

- watcher 授權檢查未綁定同一 handle 之 PID-reuse window(報告 Open Questions #1)。
- `QueryFullProcessImageName` 失敗但 `PROCESS_SET_INFORMATION` 成功時 FileName 繞過 System32 排除(報告 Open Questions #2)。
  兩者報告自己標「not reportable / no reliable attacker-to-impact path」,留待有 Windows 測試環境時驗證。
