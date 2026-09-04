# 01 — 提升權限 host 以裸名稱啟動系統工具(untrusted search path)

- 來源 findings:#5、#6、#9、#12、#15(CWE-426,severity high)
- 位置:`src-tauri/src/autostart.rs`、`src-tauri/src/tray.rs`、`src-tauri/src/update.rs`、`src-tauri/src/commands.rs`

## 問題

提升權限程序以裸名稱 `Command::new("schtasks" / "powershell" / "explorer")` 啟動子程序。若 application dir / current dir / PATH 中較早的可寫目錄有同名 PE,會先於 System32 被解析並繼承 administrator token。`schtasks` 查詢在 tray 建立時自動觸發,每次啟動都到達。

## 修法

以 `GetSystemDirectoryW`(或等價受信任來源)組出絕對路徑:

- `schtasks.exe` → `%SystemRoot%\System32\schtasks.exe`(autostart.rs、tray.rs)
- `powershell.exe` → `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`(update.rs)
- `explorer.exe` → `%SystemRoot%\explorer.exe`(commands.rs;explorer 固定在 %SystemRoot%,不在 System32)

解析失敗即 fail closed(不退回裸名稱)。子程序 CWD 不繼承不可信目錄。

## 驗收

- `cargo check` / `cargo test` 通過。
- 人為在可寫 PATH 目錄放 `schtasks.exe` fixture,新版仍解析 System32 份本。
