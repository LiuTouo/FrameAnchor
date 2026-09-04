# 03 — pe-*.mjs 診斷腳本:無界 cstr 迴圈 + terminal control chars 直出

- 來源 findings:#35、#44(CWE-835 parser-DoS)+ #36、#48(CWE-150 terminal-output-injection),severity low
- 位置:`scripts/pe-exports.mjs`、`scripts/pe-imports.mjs`

## 問題

1. `cstr` 以 `while (buf[end] !== 0) end++` 掃 caller 指定 PE;缺 NUL 時越界索引回 `undefined`,條件永真,CPU 掛死。
2. export/import 名稱由 PE bytes 直接 ASCII decode 後 `console.log`,ESC/CSI/OSC/CR/BS 可被 terminal 解讀,偽造/抹除輸出。

## 修法

- `cstr` 改 bounded:`buf.indexOf(0, offset)`,缺 NUL 或超長即回 `<invalid>` 之類標記,不掃出界。
- 輸出前 neutralize:C0/C1、DEL、ESC、CR、LF、BS 換成 `\xNN` 可見轉義。
- PE offsets/counts 先做 bounds check(本票只做最小 cstr + escape;完整 parser 硬化不擴大)。

## 驗收

- 對缺 NUL 名稱的 PE 執行,腳本在有限時間結束且輸出含跳脫標記。
- 兩支腳本仍能正常解析合法 PE(如 `src-tauri/resources/benchmark/*.exe`)。
