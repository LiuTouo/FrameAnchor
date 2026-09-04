# 05 — ONLOGON/HIGHEST 排程工作指向可置換的 executable

- 來源 findings:#3、#8、#11、#13、#14(CWE-732,severity high)
- 位置:`src-tauri/src/autostart.rs`(current_exe 直接寫入 `/RL HIGHEST` ONLOGON task)
- **狀態:done 2026-09-04。決策:選項 2 — 非受保護位置一律降 `/RL LIMITED`。**

## 實作

- `syspath::in_protected_program_dir()`:以 SHGetKnownFolderPath(不依賴可覆蓋的
  %ProgramFiles% 環境變數)比對 current_exe 是否位於 Program Files / Program Files (x86) 樹。
- 位於受保護樹 → `/RL HIGHEST`(原行為);否則(portable、%LOCALAPPDATA% currentUser
  安裝等一切可寫位置)→ `/RL LIMITED`,並記 warn log。
- 連 currentUser NSIS 安裝(%LOCALAPPDATA%\Programs,同樣使用者可寫)一起涵蓋,
  不只 portable。

## 已知行為變化

可寫位置啟用 autostart 時,登入會以 LIMITED token 啟動,requireAdministrator
manifest 會跳出 UAC 提示。UI 尚未提示此差異 — 有需要再加。

## 問題

autostart 工作只保存 current_exe **路徑字串**。currentUser/portable 部署中該路徑可由同帳戶 medium-integrity 程序置換;下次登入 Task Scheduler 以 HIGHEST(= 已分裂 admin token 的最高層)無 UAC 執行攻擊者 PE。持久性提權。

**本票先要產品決策**:portable(任意可寫目錄)與 HIGHEST autostart 天生矛盾。選項:
1. 建 task 前驗證 target 位於 machine-wide 受保護樹(Program Files 類,含父目錄 DACL/owner/reparse/簽章檢查),否則拒絕啟用或降 `/RL LIMITED`。
2. portable 模式完全停用 HIGHEST autostart(只留 LIMITED)。

## 修法(依決策)

- 受保護路徑檢查:檔案+所有父目錄的 DACL/owner/reparse 狀態,失敗 fail closed。
- 或 task 降 LIMITED / 拒絕啟用並在 UI 說明。

## 驗收

- 可寫目錄放 signed fixture、建 task 後置換 → 新版建 task 時即拒絕。
- Program Files 安裝與 portable 各測一次。
