# FrameAnchor 實作計畫書

> 本文件為完整實作規格，供實作 AI 依序執行。所有產品決策已與需求方確認（見 §3），技術建議（見各節「建議」標記）可調整但需維持效能預算（§11）。

---

## 1. 專案目標與範圍

**FrameAnchor** 是 Windows 桌面工具，目標：讓競技遊戲幀數更穩定（尤其 1% low），透過持久規則自動調整遊戲進程的 CPU 排程。

**核心功能：**
- CPU 核心親和性（affinity）：綁定遊戲到指定邏輯核心，可排除 HT 虛擬核心 / E-core
- CPU 優先級（priority class）
- 持久規則：依 exe 路徑比對，遊戲啟動時自動套用
- 監控面板：每核心即時使用率、HT/P-core/E-core 標示、已套用規則的進程清單
- Browse：列出當前開啟的視窗供選擇，一鍵建立規則
- 背景 tray 常駐、可設定開機啟動（無 UAC 彈窗）

**進階功能（預設收合，見 §3 決策）：**
- I/O 優先級（主要用途：調低背景程式）
- 記憶體優先級

**明確不做（v1 範圍外）：**
- ProBalance 式動態調節
- 電源計畫切換
- GPU 排程、幀率監控（需 RTSS 級 hook）
- 超過 64 邏輯處理器的多 processor group 完整支援（見 §15）
- Kernel driver（不繞過反作弊，見 §10）

---

## 2. 技術棧

| 層 | 選擇 | 版本 | 理由 |
|---|---|---|---|
| 應用框架 | Tauri | v2（2.x 最新穩定） | 安裝包小、WebView2 Win11 內建、Rust 後端直接叫 Win32 |
| 後端 | Rust | 1.80+（stable） | 直接呼叫 Win32 API |
| Win32 綁定 | `windows` crate | 0.61+ | 官方綁定 |
| 未公開 API | `windows-sys` crate | 0.59+（Wdk feature） | NtQuerySystemInformation / NtSetInformationProcess |
| 前端框架 | Svelte | 5（runes 模式） | 無虛擬 DOM runtime，bundle 最小 |
| 建置 | Vite + TypeScript | Vite 5+ / TS 5+ | Tauri 標準 |
| i18n | `svelte-i18n` | 最新 | 雙語字典 |
| 單一實例 | `tauri-plugin-single-instance` | v2 | 官方外掛 |
| 序列化 | `serde` + `serde_json` | 最新 | config |
| ID | `uuid`（v4 feature） | 最新 | 規則 ID |
| 錯誤 | `thiserror`（後端內部）+ `anyhow`（command 邊界） | 最新 | |
| 日誌 | `tauri-plugin-log` | v2 | 寫入 %APPDATA%\FrameAnchor\logs |

**開發環境需求：** Windows 11、Rust stable、Node 20+、pnpm（或 npm）、Visual Studio Build Tools（MSVC）、WebView2（Win11 內建）。

**不引入 `sysinfo` crate：** affinity mask 與拓撲需要精確控制，全部手寫 Win32，避免抽象層失真。

---

## 3. 已確認的產品決策

需求方已拍板，實作時不得偏離：

1. 技術棧：**Tauri v2 + Rust + Svelte 5**（§2）
2. 功能範圍：**affinity + CPU priority 為主功能**；**I/O 與記憶體優先級納入為進階選項**（UI 摺疊區，預設不展開）
3. 套用方式：**持久規則 + 自動套用**（watcher 偵測到 exe 啟動即套用）
4. 權限模型：**單一 exe 常駐管理員**（manifest requireAdministrator）+ **Task Scheduler 最高權限開機啟動**（登入不跳 UAC）
5. 面板必須顯示：**每核心即時使用率、P-core/E-core 標示、已套用規則進程清單、哪些是 HT 虛擬核心**
6. 介面語言：**雙語（繁體中文 / English），預設繁體中文**

**技術建議（需求方授權決定，可推翻）：**
- 進程偵測：**輪詢**（預設 1000ms，設定可調 500–5000ms）。理由：Toolhelp snapshot 成本 <0.1ms，可靠無遺漏；WMI event 有服務依賴與遺漏風險。
- 開機啟動實作：呼叫 `schtasks.exe` CLI 而非 COM ITaskService。理由：一行指令完成，無 COM 樣板程式碼（§7.7）。
- affinity 提供四種模式預設：`全部核心` / `排除 HT 虛擬核心` / `僅 P-core`（僅混合架構顯示）/ `自訂勾選`。
- 規則只作用於使用者明確建立的目標，不提供「全域預設規則」。

---

## 4. 系統架構

```
┌────────────────────────────────────────────────────┐
│  WebView2 前端（Svelte 5）                          │
│  ┌──────────┬──────────┬───────────┐               │
│  │ 面板頁   │ 規則頁   │ 設定頁    │               │
│  │Dashboard │ Rules    │ Settings  │               │
│  └────┬─────┴────┬─────┴─────┬─────┘               │
│       │ invoke() │           │ listen()            │
├───────┼──────────┼───────────┼─────────────────────┤
│  Tauri IPC（commands / events）                     │
├───────┴────────────────────────────────────────────┤
│  Rust 後端（單一 exe，requireAdministrator）        │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────┐  │
│  │ topology.rs │  │ watcher.rs   │  │ config.rs │  │
│  │ CPU 拓撲列舉│  │ 輪詢+套用規則│  │ JSON 持久 │  │
│  └─────────────┘  └──────┬───────┘  └───────────┘  │
│  ┌─────────────┐  ┌──────┴───────┐  ┌───────────┐  │
│  │ process.rs  │  │ usage.rs     │  │ tray.rs   │  │
│  │ 開 handle/  │  │ 每核心使用率 │  │ 托盤選單  │  │
│  │ 設 affinity │  │ (1s tick)    │  │           │  │
│  └─────────────┘  └──────────────┘  └───────────┘  │
│  ┌─────────────┐  ┌──────────────┐  ┌───────────┐  │
│  │priority.rs  │  │ windows_enum │  │autostart  │  │
│  │ IO/記憶體   │  │ Browse 列舉  │  │ schtasks  │  │
│  └─────────────┘  └──────────────┘  └───────────┘  │
└────────────────────────────────────────────────────┘
```

**執行緒模型：**
- Tauri 主執行緒：UI 事件迴圈
- Tokio runtime（Tauri 內建）：兩個長駐 async task
  - **watcher task**：每 `pollIntervalMs` 列舉進程 → 比對規則 → 套用 → 維護 applied 狀態表
  - **usage task**：每 1000ms 計算每核心使用率 → emit `usage-update` event 給前端（面板頁未開啟時暫停，見 §7.5）

**共享狀態：** `Arc<RwLock<AppState>>`，`AppState` 含 config、topology（啟動時列舉一次）、applied 表（`HashMap<Pid, AppliedInfo>`）。所有 Tauri command 透過 `tauri::State` 存取。

---

## 5. 資料模型

### 5.1 config.json

路徑：`%APPDATA%\FrameAnchor\config.json`
寫入策略：**原子寫入**（先寫 `config.json.tmp`，再 `MoveFileExW` replace），避免當機損毀。

```jsonc
{
  "version": 1,
  "settings": {
    "language": "zh-TW",            // "zh-TW" | "en"
    "startWithWindows": false,      // 開機啟動（Task Scheduler）
    "startMinimized": true,         // 啟動後直接縮到 tray
    "closeToTray": true,            // 按 X 縮到 tray 而非結束
    "pollIntervalMs": 1000,         // 500–5000
    "showAdvancedPriorities": false // 規則編輯器顯示 IO/記憶體區塊
  },
  "rules": [
    {
      "id": "3f6b8c2e-....",                    // uuid v4
      "name": "VALORANT",                        // 顯示名稱，預設 exe 檔名去副檔名
      "exePath": "C:\\Games\\VALORANT\\game.exe", // 完整路徑
      "matchBy": "fullPath",                     // "fullPath" | "fileName"
      "enabled": true,
      "affinity": {
        "mode": "noSmtSibling",   // "all" | "noSmtSibling" | "pCoresOnly" | "custom"
        "cores": [0, 2, 4, 6]     // 邏輯處理器索引（LP index），僅 mode="custom" 使用
      },
      "priority": "High",         // "Idle" | "BelowNormal" | "Normal" | "AboveNormal" | "High"
      "advanced": {
        "ioPriority": null,       // null=不動 | "VeryLow" | "Low" | "Normal" | "High"
        "memoryPriority": null    // null=不動 | "VeryLow" | "Low" | "Medium" | "BelowNormal" | "Normal"
      }
    }
  ]
}
```

### 5.2 Rust 型別（`src-tauri/src/model.rs`）

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    pub settings: Settings,
    pub rules: Vec<Rule>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub language: String,
    pub start_with_windows: bool,
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub poll_interval_ms: u64,
    pub show_advanced_priorities: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub exe_path: String,
    pub match_by: MatchBy,
    pub enabled: bool,
    pub affinity: AffinitySpec,
    pub priority: CpuPriority,
    pub advanced: AdvancedSpec,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum MatchBy { FullPath, FileName }

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AffinitySpec {
    pub mode: AffinityMode,
    pub cores: Vec<u32>, // LP indices
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum AffinityMode { All, NoSmtSibling, PCoresOnly, Custom }

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum CpuPriority { Idle, BelowNormal, Normal, AboveNormal, High }

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedSpec {
    pub io_priority: Option<IoPriority>,
    pub memory_priority: Option<MemPriority>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum IoPriority { VeryLow, Low, Normal, High }

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum MemPriority { VeryLow, Low, Medium, BelowNormal, Normal }
```

### 5.3 CPU 拓撲型別（`src-tauri/src/topology.rs`）

```rust
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub logical_processors: Vec<LogicalProcessor>, // 依 LP index 排序
    pub physical_cores: Vec<PhysicalCore>,          // 依 core id 排序
    pub has_smt: bool,        // 有任何核心 >1 LP
    pub has_hybrid: bool,     // EfficiencyClass 不全相同
    pub total_lp: u32,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LogicalProcessor {
    pub index: u32,           // LP index（0..total_lp，group 0 內）
    pub core_id: u32,         // 所屬實體核心 id
    pub is_smt_sibling: bool, // true = 此核心第二條 HT 執行緒（UI 標「HT」）
    pub efficiency_class: u8, // 0 = E-core；較大 = P-core（均質 CPU 全相同）
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhysicalCore {
    pub id: u32,
    pub lp_indices: Vec<u32>, // 1 個 = 無 SMT；2 個 = 有 HT
    pub efficiency_class: u8,
    pub is_p_core: bool,      // efficiency_class == 全系統最大值
}
```

### 5.4 已套用進程型別（`src-tauri/src/watcher.rs`）

```rust
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppliedProcess {
    pub pid: u32,
    pub exe_name: String,
    pub rule_id: String,
    pub rule_name: String,
    pub affinity_ok: bool,
    pub priority_ok: bool,
    pub io_ok: Option<bool>,     // None = 規則未設定此項
    pub mem_ok: Option<bool>,
    pub error: Option<String>,   // 失敗原因（如「存取被拒（可能反作弊保護）」）
    pub applied_at: String,      // RFC3339
    pub current_cores: Vec<u32>, // 目前實際 affinity（重新查詢）
    pub current_priority: String,
}
```

---

## 6. Windows API 對照表

| 功能 | API | 所需權限/備註 |
|---|---|---|
| CPU 拓撲（SMT、P/E） | `GetLogicalProcessorInformationEx`（`RelationProcessorCore`） | 無特殊權限；`EfficiencyClass` 需 Win10 19041+ |
| 每核心使用率 | `NtQuerySystemInformation`（`SystemProcessorPerformanceInformation` = 8） | ntdll；回傳每 LP 的 Kernel/User/Idle 時間，前後取樣算差值 |
| 進程列舉 | `CreateToolhelp32Snapshot`（`TH32CS_SNAPPROCESS`）+ `Process32FirstW/NextW` | |
| exe 完整路徑 | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` + `QueryFullProcessImageNameW` | 受保護進程可能失敗 → 略過 |
| 設 affinity | `OpenProcess(PROCESS_SET_INFORMATION \| PROCESS_QUERY_INFORMATION)` + `SetProcessAffinityMask` | 僅 group 0（§15 限制） |
| 查 affinity | `GetProcessAffinityMask`（回傳 process mask + system mask） | |
| 設優先級 | `SetPriorityClass` | `HIGH_PRIORITY_CLASS` 等；**不提供 Realtime** |
| 查優先級 | `GetPriorityClass` | |
| 設 I/O 優先級 | `NtSetInformationProcess`（`ProcessIoPriority` = 33） | ntdll；值 0–3（Critical=4 系統保留不開放） |
| 查 I/O 優先級 | `NtQueryInformationProcess`（`ProcessIoPriority`） | |
| 設記憶體優先級 | `SetProcessInformation`（`ProcessMemoryPriority`） | `MEMORY_PRIORITY_INFORMATION`，值 1–5；最高只能 Normal(5) |
| 查記憶體優先級 | `GetProcessInformation`（`ProcessMemoryPriority`） | |
| 可見視窗列舉（Browse） | `EnumWindows` + `IsWindowVisible` + `GetWindowTextW` + `GetWindowThreadProcessId` + `DwmGetWindowAttribute(DWMWA_CLOAKED)` | §7.4 過濾演算法 |
| 視窗/exe 圖示 | `ExtractIconExW` 或 `SHGetFileInfoW(SHGFI_ICON)` | 轉 PNG bytes 傳前端 |
| 開機啟動 | `schtasks.exe /Create /SC ONLOGON /RL HIGHEST` | §7.7 |
| 單一實例 | `tauri-plugin-single-instance` | |
| Tray | Tauri v2 `tray-icon` feature | |

**`windows` crate features 清單（Cargo.toml）：**
```toml
[dependencies.windows]
version = "0.61"
features = [
  "Win32_Foundation",
  "Win32_System_Threading",
  "Win32_System_Kernel",
  "Win32_System_ProcessStatus",
  "Win32_System_Diagnostics_ToolHelp",
  "Win32_System_SystemInformation",
  "Win32_System_Memory",          # MEMORY_PRIORITY_INFORMATION 所在（依版本可能於 Threading）
  "Win32_UI_WindowsAndMessaging",
  "Win32_UI_Shell",
  "Win32_Graphics_Dwm",
  "Win32_Security",
]

[dependencies.windows-sys]
version = "0.59"
features = ["Wdk_System_SystemInformation", "Wdk_System_Threading", "Win32_System_Threading"]
```
> 注意：crate 版本與 feature 名稱以實作時 docs.rs 為準；`NtSetInformationProcess` 若在 `windows-sys` 對應模組找不到，fallback 為 `#[link(name="ntdll")] extern "system"` 手動宣告（§16 附錄 C 有現成寫法）。

---

## 7. 後端模組詳細設計

### 7.1 `topology.rs` — CPU 拓撲

**職責：** 啟動時呼叫一次 `GetLogicalProcessorInformationEx(RelationProcessorCore)`，建立 `Topology`（§5.3），供 UI 顯示與 affinity 模式解析。

**演算法：**
1. 兩段式呼叫：先傳 NULL buffer 取得所需長度，配置後再取資料（§16 附錄 A 有完整片段）。
2. 逐一讀 `PROCESSOR_RELATIONSHIP`：
   - `Flags == LTP_PC_SMT` → 此核心有 2 條執行緒
   - `GroupMask[0].Mask` 的 set bits → 此核心的 LP indices
   - `EfficiencyClass` → 0 通常是 E-core、較大值是 P-core
3. 組裝：LP index 由小到大編號；同一實體核心內 **mask 中最低位元的 LP 是實體執行緒，其餘標 `is_smt_sibling = true`**（UI 顯示「HT」徽章）。
4. `has_hybrid` = EfficiencyClass 不全相同；`is_p_core` = 該核心 EfficiencyClass == 全系統最大值。均質 CPU（AMD 等）所有核心 `is_p_core = true` 且 UI 隱藏 P/E 徽章。

**affinity 模式解析（`resolve_mask(spec, topology) -> u64`）：**
```rust
pub fn resolve_mask(spec: &AffinitySpec, topo: &Topology) -> u64 {
    match spec.mode {
        AffinityMode::All => (1u64 << topo.total_lp) - 1, // total_lp<64 前提
        AffinityMode::NoSmtSibling => topo.logical_processors.iter()
            .filter(|lp| !lp.is_smt_sibling)
            .fold(0, |m, lp| m | (1u64 << lp.index)),
        AffinityMode::PCoresOnly => topo.logical_processors.iter()
            .filter(|lp| topo.physical_cores[lp.core_id as usize].is_p_core)
            .fold(0, |m, lp| m | (1u64 << lp.index)),
        AffinityMode::Custom => spec.cores.iter().fold(0, |m, &i| m | (1u64 << i)),
    }
}
```
> `noSmtSibling` 但 CPU 無 SMT 時等同 All；`pCoresOnly` 但非混合架構時等同 All。解析時防呆，不得回傳 0 mask（0 會讓 `SetProcessAffinityMask` 失敗）→ 若解析結果為 0 則 fallback All。

### 7.2 `process.rs` — 進程操作

```rust
pub struct ProcessInfo { pub pid: u32, pub exe_name: String, pub exe_path: Option<String> }

/// Toolhelp snapshot 列舉全部進程；exe_path 失敗（受保護進程）時為 None
pub fn enumerate_processes() -> Vec<ProcessInfo>;

/// 開啟用於設定的 handle；失敗回傳錯誤（含 ACCESS_DENIED 判別）
pub fn open_for_set(pid: u32) -> Result<OwnedHandle, ProcessError>;

pub fn set_affinity(h: HANDLE, mask: u64) -> Result<(), ProcessError>;
pub fn get_affinity(h: HANDLE) -> Result<u64, ProcessError>; // process mask
pub fn set_priority(h: HANDLE, p: CpuPriority) -> Result<(), ProcessError>;
pub fn get_priority(h: HANDLE) -> Result<CpuPriority, ProcessError>;
```

**路徑正規化：** 比對前統一去掉 `\\?\` 前綴、統一 `\`、**大小寫不敏感**（`to_lowercase()`）。`matchBy = FileName` 時只比對檔名部分。

**優先級映射：**
```rust
match p {
    CpuPriority::Idle        => IDLE_PRIORITY_CLASS,
    CpuPriority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
    CpuPriority::Normal      => NORMAL_PRIORITY_CLASS,
    CpuPriority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
    CpuPriority::High        => HIGH_PRIORITY_CLASS,
}
```
> 刻意不提供 `REALTIME_PRIORITY_CLASS`：會讓系統層級執行緒（含滑鼠鍵盤輸入）餓死，屬危險操作。

### 7.3 `priority.rs` — I/O 與記憶體優先級（進階）

**I/O 優先級（`NtSetInformationProcess`）：**
```rust
const PROCESS_INFORMATION_CLASS_IO_PRIORITY: i32 = 33; // ProcessIoPriority
// 值：VeryLow=0, Low=1, Normal=2, High=3（4=Critical 系統保留，不開放）
pub fn set_io_priority(h: HANDLE, p: IoPriority) -> Result<(), PriorityError>;
pub fn get_io_priority(h: HANDLE) -> Result<IoPriority, PriorityError>; // NtQueryInformationProcess
```
呼叫慣例：`ntstatus >= 0` 為成功，否則包成錯誤。

**記憶體優先級（`SetProcessInformation`）：**
```rust
// MEMORY_PRIORITY_INFORMATION { MemoryPriority: u32 }
// VeryLow=1, Low=2, Medium=3, BelowNormal=4, Normal=5（無法高於 Normal）
pub fn set_memory_priority(h: HANDLE, p: MemPriority) -> Result<(), PriorityError>;
pub fn get_memory_priority(h: HANDLE) -> Result<MemPriority, PriorityError>;
```

**錯誤容忍：** 這兩項屬「盡力而為」，失敗只記錄到 `AppliedProcess.io_ok/mem_ok = Some(false)` 與 error 欄位，**不影響 affinity/priority 的整體成功判定**。

### 7.4 `windows_enum.rs` — Browse 視窗列舉

**職責：** 列出「像 alt-tab 會看到的」視窗，供使用者挑選遊戲。

**過濾演算法（每個 top-level HWND）：**
1. `IsWindowVisible` == true
2. `GetWindowTextLengthW` > 0（有標題）
3. `DwmGetWindowAttribute(DWMWA_CLOAKED)` == 0（排除隱藏的 UWP 背景視窗）
4. 排除 `WS_EX_TOOLWINDOW`（`GetWindowLongPtrW(GWL_EXSTYLE)`）
5. 排除 FrameAnchor 自己的 PID
6. `GetWindowThreadProcessId` → PID → `QueryFullProcessImageNameW` 取得 exe 路徑；取不到路徑的（受保護）仍可列出但標記「無法建立規則」並 disable

**回傳：**
```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub hwnd: u64,
    pub pid: u32,
    pub title: String,
    pub exe_name: String,
    pub exe_path: Option<String>,
    pub icon_png: Option<String>, // base64 PNG 32x32；取不到為 None，前端用預設圖
    pub already_has_rule: bool,   // 該 exe 已有規則 → 標記避免重複建立
}
```

**圖示：** `SHGetFileInfoW(exe_path, SHGFI_ICON | SHGFI_SMALLICON)` 取 HICON → 畫到 32x32 ARGB bitmap → PNG encode（用 `image` crate 或手寫最小 PNG encoder；建議 `image` crate，依賴小）。base64 後隨 command 回傳。

### 7.5 `usage.rs` — 每核心使用率

**演算法：**
1. `NtQuerySystemInformation(SystemProcessorPerformanceInformation=8)` 回傳每 LP 的 `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION { IdleTime, KernelTime, UserTime, ... }`（LARGE_INTEGER，單位 100ns）。
2. 每 1000ms 取樣，與上次取樣算差值：
   ```
   busy = ΔKernel + ΔUser          // 注意 KernelTime 已包含 IdleTime
   util = 1.0 - (ΔIdle / busy)     // clamp 0..1
   ```
3. emit Tauri event `usage-update`，payload `Vec<f32>`（index = LP index）。

**省電設計：** 追蹤面板頁是否可見（前端進入/離開面板頁時呼叫 `set_usage_streaming(bool)` command）；不可見時 usage task 暫停取樣，tray 不顯示動態數字。這是記憶體/CPU 預算的一部分。

### 7.6 `watcher.rs` — 規則引擎（核心）

**主迴圈（tokio task）：**
```
loop {
    sleep(poll_interval_ms)
    procs = enumerate_processes()
    for proc in procs:
        if proc.pid in applied_map: continue          // 已處理
        rule = match_rule(proc, config.rules)          // enabled && exe 比對
        if rule.is_none(): continue
        if is_blacklisted(proc): continue              // §10
        result = apply_rule(proc.pid, rule)            // 開 handle → affinity → priority → advanced
        applied_map.insert(proc.pid, AppliedProcess { ... })
    // 清理已結束的 PID（不在 snapshot 中 → 從 applied_map 移除）
    applied_map.retain(|pid, _| procs.contains(pid))
    emit("applied-update", applied_map.values())
}
```

**`apply_rule` 順序與重試：**
1. `open_for_set(pid)` 失敗 → 記錄 error；若是 `ACCESS_DENIED` → 標記「存取被拒（可能反作弊保護）」，**加入退避表，30 秒內不再重試**（避免每秒對 Vanguard 之類狂試）。
2. 其他失敗（如 `ERROR_INVALID_PARAMETER`）→ 最多重試 3 次（每次間隔一個 poll 週期），之後標記失敗。
3. 成功順序：affinity → priority → io（若有）→ memory（若有）。逐項記錄 ok 旗標。
4. 套用後立即重新查詢實際值填入 `current_cores` / `current_priority`（面板顯示「實際狀態」而非「期望狀態」）。

**為何需要重試：** 遊戲進程 snapshot 出現時，反作弊可能尚未初始化完成，或 exe 是 launcher 會再 spawn 真正的遊戲 exe。重試 + 依 PID 追蹤（不是依 exe 名只套一次）可涵蓋大部分情況。launcher spawn 子進程情境：子進程是不同 PID，watcher 下一輪自然會撈到並比對。

### 7.7 `autostart.rs` — 開機啟動

**用 schtasks CLI（不用 COM）：**
```rust
pub fn set_autostart(enable: bool) -> Result<(), AutoStartError> {
    let exe = std::env::current_exe()?;
    if enable {
        // /RL HIGHEST = 以最高權限執行（登入不跳 UAC）
        // /SC ONLOGON = 使用者登入時觸發
        // /F = 覆蓋已存在的同名工作
        run("schtasks", &["/Create", "/TN", "FrameAnchor", "/SC", "ONLOGON",
                          "/RL", "HIGHEST", "/TR", &format!("\"{}\" --minimized", exe.display()), "/F"])
    } else {
        run("schtasks", &["/Delete", "/TN", "FrameAnchor", "/F"])
    }
}

pub fn is_autostart_enabled() -> bool {
    // schtasks /Query /TN FrameAnchor，exit code 0 = 存在
}
```
> 不用 Registry Run key：它無法帶最高權限，開機會跳 UAC 或直接失敗。Task Scheduler 的 ONLOGON + HIGHEST 是 Process Lasso 等工具標準做法。
> 呼叫時機：設定頁 checkbox 變更即呼叫；呼叫時 app 已是管理員（manifest），所以有權建立 HIGHEST 工作。

### 7.8 `config.rs` — 設定持久化

- 啟動：讀 `%APPDATA%\FrameAnchor\config.json`；不存在 → 用預設值建立；JSON 解析失敗 → 備份為 `config.corrupt.json` 並用預設值（不得直接覆蓋使用者檔案）。
- 寫入：原子寫入（tmp + rename）。每次規則/設定變更即整檔寫入（檔案 <50KB，成本可忽略）。
- `version` 欄位預留未來遷移。

### 7.9 `tray.rs` + 主視窗行為

**Tray 選單（右鍵）：**
- `顯示面板`（左鍵點圖示同效果）
- `────────`
- `已套用 N 個規則`（disabled 純資訊項，即時更新）
- `開機自動啟動`（check item，連動 §7.7）
- `────────`
- `結束 FrameAnchor`

**主視窗行為：**
- 啟動參數 `--minimized`（Task Scheduler 帶入）→ 建立 tray 後不開主視窗。
- 關閉按鈕：`settings.closeToTray == true` → `window.hide()`；false → 真正結束。結束前 emit 確認（結束後規則不再自動套用，但已套用的 affinity 會跟隨進程直到遊戲關閉——不需還原，affinity/priority 是進程屬性，進程結束即消失）。
- 單一實例：第二個實例啟動時 → 喚醒並聚焦既有視窗後退出。

---

## 8. IPC 介面（Tauri commands & events）

### Commands（前端 `invoke()`）

| Command | 參數 | 回傳 | 說明 |
|---|---|---|---|
| `get_topology` | — | `Topology` | §5.3；前端快取即可 |
| `list_windows` | — | `Vec<WindowInfo>` | Browse 對話框開啟時呼叫 |
| `get_rules` | — | `Vec<Rule>` | |
| `save_rule` | `rule: Rule` | `Result<(), String>` | id 已存在=更新，否則新增；寫 config 並通知 watcher 重新比對 |
| `delete_rule` | `id: String` | `Result<(), String>` | 不還原已套用進程（§7.9 說明） |
| `get_settings` | — | `Settings` | |
| `save_settings` | `settings: Settings` | `Result<(), String>` | poll interval 變更即時生效（watcher 每輪重讀） |
| `set_autostart` | `enable: bool` | `Result<(), String>` | §7.7 |
| `get_applied` | — | `Vec<AppliedProcess>` | 面板頁開啟時取初始值，之後靠 event |
| `reapply_all` | — | `Result<(), String>` | 清空 applied_map，強制全部重新比對套用（規則改完後手動觸發用） |
| `set_usage_streaming` | `active: bool` | — | §7.5 省電開關 |

### Events（後端 emit）

| Event | Payload | 頻率 | 說明 |
|---|---|---|---|
| `usage-update` | `Vec<f32>` | 1s（streaming 開啟時） | 每 LP 使用率 0..1 |
| `applied-update` | `Vec<AppliedProcess>` | 變動時 | watcher 每輪有變化才 emit |

所有錯誤回傳用 `Result<T, String>`，訊息為**使用者可讀的當前語系文字**（錯誤字串在後端依 settings.language 產生，或傳 error code 由前端查 i18n——**建議後者**：後端只回 `Err("ACCESS_DENIED")` 這類代碼，前端 `t('errors.ACCESS_DENIED')` 顯示）。

---

## 9. 前端設計

### 9.1 頁面結構

單一視窗，左側窄導覽列（圖示+文字），右側內容區。三頁：`面板`（預設）、`規則`、`設定`。

```
┌──────────────────────────────────────────────────────┐
│ FrameAnchor                                          │
│ ├─────────┬──────────────────────────────────────────┤
│ │ ▣ 面板  │  （內容區）                               │
│ │ ☰ 規則  │                                          │
│ │ ⚙ 設定  │                                          │
│ └─────────┴──────────────────────────────────────────┘
```

**視窗規格：** 預設 960×640，最小 800×520，可調大小。深色主題（遊戲工具慣例），accent 色建議 `#4F8CFF`。字體：系統 UI 字體（`"Segoe UI", "Microsoft JhengHei", sans-serif`）。

### 9.2 面板頁（Dashboard）

**上半：CPU 拓撲使用率格狀圖**
```
實體核心 0 [P]    ██ LP0  45%      ░░ LP1(HT)  3%
實體核心 1 [P]    ██ LP2  61%      ░░ LP3(HT)  0%
實體核心 8 [E]    █  LP16 12%      （E-core 無 HT 只有一格）
```
- 每列 = 一個實體核心；核心內每個 LP 一格：使用率 bar（1s 更新）+ LP 編號 + 百分比。
- 徽章：`[P]` / `[E]`（僅混合架構顯示）；HT 虛擬核心那格右上角標 `HT`。
- 被任何啟用中規則 affinity 涵蓋的 LP：格框用 accent 色描邊；hover 顯示「規則：VALORANT」。
- 均質無 SMT CPU：每列一格，不顯示 HT 徽章（UI 依 `topology.has_smt` / `has_hybrid` 自動收斂）。

**下半：已套用規則的進程清單（表格）**

| 遊戲 | PID | 規則 | Affinity | 優先級 | 狀態 |
|---|---|---|---|---|---|
| game.exe | 12345 | VALORANT | LP 0–7（8 核） | High | ✔ 已套用 |
| other.exe | 23456 | Rule2 | LP 0,2,4,6 | High | ✖ 存取被拒（反作弊） |

- 空狀態：顯示「尚無符合規則的遊戲在執行」。
- 失敗列用紅色狀態 + tooltip 顯示完整錯誤。

### 9.3 規則頁（Rules）

- 頂部：`+ 新增規則（瀏覽執行中視窗）` 按鈕 → 開 Browse 對話框（§9.4）。
- 規則卡片列表，每張卡片：
  - 標題列：規則名、exe 檔名、`啟用` checkbox、刪除按鈕。
  - **Affinity 區**：四個 preset 按鈕（`全部核心` / `排除 HT 虛擬核心` / `僅 P-core` / `自訂`）+ LP 勾選格（與面板相同的拓撲格狀，但每格是 checkbox；HT/P/E 徽章一致）。preset 按鈕按下 = 切 mode；使用者手動勾選時 mode 自動變 `custom`。
  - **優先級區**：dropdown（`一般 / 高於一般 / 高 / 低於一般 / 閒置`），預設 `高`。旁邊小字警告：「『高』已足夠，勿用工作管理員設 Realtime」。
  - **進階區**（摺疊，`showAdvancedPriorities` 或卡片級展開）：I/O 優先級 dropdown（`不變更/極低/低/一般/高`）、記憶體優先級 dropdown（`不變更/極低/低/中/低於一般/一般`）。附說明：「調低背景程式優先級比調高遊戲更有效」。
  - **比對方式**：radio `完整路徑` / `僅檔名`（遊戲更新會搬 exe 路徑時用後者）。附說明「僅檔名可能誤比對同名程式」。

### 9.4 Browse 對話框

- modal，列出 `list_windows()` 結果：圖示、視窗標題、exe 名、PID。
- 每列一個 `選擇` 按鈕；已有規則的 exe 顯示「已有規則」disabled。
- 頂部搜尋框（過濾標題/exe 名）。底部「重新整理」。
- 選擇後：建立新規則（`matchBy=fullPath`、affinity `all`、priority `High`、名稱=exe 去副檔名），關閉 modal 並跳到規則頁展開該卡片讓使用者調 affinity。

### 9.5 設定頁（Settings）

全部 checkbox / 簡單控制項（需求方要求盡量勾選式）：

- ☑ 開機時自動啟動 FrameAnchor（連動 §7.7，切換立即生效）
- ☑ 啟動時最小化到系統匣
- ☑ 關閉視窗時最小化到系統匣（而非結束程式）
- 介面語言：dropdown（`繁體中文` / `English`），切換即時生效
- 背景偵測間隔：slider 0.5s–5s（預設 1s）+ 說明「較短間隔套用更快，CPU 占用略增」
- ☐ 在規則編輯器顯示進階優先級（I/O / 記憶體）
- ────
- 關於：版本號、資料夾位置連結（開啟 %APPDATA%\FrameAnchor）

### 9.6 i18n

- `src/i18n/zh-TW.json`、`src/i18n/en.json`，key 結構：`nav.dashboard`、`rules.affinityPresets.noSmtSibling`、`errors.ACCESS_DENIED` …
- `svelte-i18n` 初始化 `locale = settings.language`，fallback `zh-TW`。
- 所有 UI 字串不得 hardcode，含 tray 選單（tray 在 Rust 端：語系切換時前端呼叫 `save_settings` → 後端重建 tray menu——提供 command `rebuild_tray_menu()` 或在 save_settings 內處理）。

---

## 10. 安全防護與反作弊

**進程黑名單（永遠拒絕套用，即使使用者建立規則）：**
- PID < 8（System Idle / System / Registry 等）
- 名稱清單（大小寫不敏感）：`system`, `registry`, `memory compression`, `secure system`, `smss.exe`, `csrss.exe`, `wininit.exe`, `winlogon.exe`, `lsass.exe`, `services.exe`, `svchost.exe`, `fontdrvhost.exe`, `dwm.exe`, `explorer.exe`, `sihost.exe`, `taskhostw.exe`, `msmpeng.exe`, `frameanchor.exe`（自己）
- exe 路徑位於 `%SystemRoot%\System32` 下的進程一律拒絕

**反作弊現實（必須在 UI 與文件呈現）：**
- Vanguard（VALORANT）、EAC/BattlEye 保護模式下的遊戲會拒絕外部 `OpenProcess(PROCESS_SET_INFORMATION)` → `ACCESS_DENIED`。這是**所有**同類工具（含 Process Lasso）的共同限制。
- UI 表現：狀態欄顯示「✖ 存取被拒（可能受反作弊保護）」，tooltip 補充「此遊戲阻止外部工具調整，非 FrameAnchor 故障」。
- **不嘗試繞過**（不寫 driver、不注入）。規則頁加一行免責說明：「調整第三方程式排程可能違反部分遊戲的服務條款，請自行評估風險。」

---

## 11. 效能預算（驗收標準）

| 指標 | 目標 | 量測方式 |
|---|---|---|
| 閒置 CPU（tray 常駐、面板關閉） | < 0.5%（單核計） | 工作管理員 60 秒平均 |
| 面板開啟 CPU | < 2% | 同上 |
| 記憶體（全部 process 含 WebView2 總和） | < 120 MB | 工作管理員「詳細資料」working set |
| 規則套用延遲（遊戲 exe 出現→套用完成） | ≤ poll interval + 1s | 日誌時間戳 |
| 輪詢本身成本 | 單次 snapshot < 1ms | 日誌 debug 級別 |
| 安裝包大小 | < 15 MB | NSIS 輸出 |

手段：usage streaming 開關（§7.5）、輪詢間隔可調、前端無重型依賴（Svelte 編譯式）、icon 只在 Browse 開啟時提取。

---

## 12. 檔案結構

```
FrameAnchor/
├── PLAN.md                          ← 本文件
├── package.json
├── vite.config.ts
├── tsconfig.json
├── index.html
├── src/                             ← Svelte 前端
│   ├── App.svelte                   ← 導覽列 + 路由（手寫 tab 切換即可，不用 router 套件）
│   ├── main.ts
│   ├── pages/
│   │   ├── Dashboard.svelte
│   │   ├── Rules.svelte
│   │   └── Settings.svelte
│   ├── components/
│   │   ├── TopologyGrid.svelte      ← 唯讀使用率格（面板用）
│   │   ├── AffinityPicker.svelte    ← 可勾選格（規則編輯用，與上者共用 cell 子元件）
│   │   ├── CoreCell.svelte
│   │   ├── RuleCard.svelte
│   │   ├── AppliedTable.svelte
│   │   └── BrowseDialog.svelte
│   ├── lib/
│   │   ├── ipc.ts                   ← invoke 包裝 + 型別
│   │   ├── stores.ts                ← Svelte stores（topology/applied/rules/settings）
│   │   └── types.ts                 ← 與 §5 對應的 TS interface
│   └── i18n/
│       ├── index.ts
│       ├── zh-TW.json
│       └── en.json
└── src-tauri/
    ├── Cargo.toml
    ├── tauri.conf.json              ← manifest requireAdministrator、bundle NSIS
    ├── build.rs
    ├── icons/
    └── src/
        ├── main.rs                  ← setup、plugin 註冊、command 註冊
        ├── model.rs                 ← §5.2
        ├── topology.rs              ← §7.1
        ├── process.rs               ← §7.2
        ├── priority.rs              ← §7.3
        ├── windows_enum.rs          ← §7.4
        ├── usage.rs                 ← §7.5
        ├── watcher.rs               ← §7.6
        ├── autostart.rs             ← §7.7
        ├── config.rs                ← §7.8
        ├── tray.rs                  ← §7.9
        ├── commands.rs              ← §8 所有 #[tauri::command]
        └── error.rs                 ← thiserror 錯誤型別
```

---

## 13. 開發里程碑（依序執行，每個都有驗收）

**M0 — 腳手架**
- `npm create tauri-app`（Svelte+TS 模板）建出來能跑；加入 §2 依賴。
- `tauri.conf.json`：appId、視窗規格（§9.1）、`bundle.targets: ["nsis"]`、Windows manifest `requestedExecutionLevel = requireAdministrator`。
- 驗收：`pnpm tauri dev` 開窗；以管理員身份啟動。

**M1 — 拓撲 + 面板唯讀**
- 實作 `topology.rs`（§7.1）、`usage.rs`（§7.5）、`get_topology` command、`usage-update` event。
- 前端 `TopologyGrid` + `CoreCell` 顯示即時使用率、HT/P/E 徽章。
- 驗收：面板格數 = 邏輯處理器數；HT 徽章位置與工作管理員「邏輯處理器」圖一致；混合 CPU 上 P/E 正確（可用 HWiNFO 對照）。

**M2 — 規則 CRUD + config**
- `model.rs`、`config.rs`、規則頁卡片、`save_rule/delete_rule/get_rules`。
- 驗收：新增/編輯/刪除規則，重開 app 仍在；手改 config.json 成壞 JSON → app 用預設值且原檔被備份。

**M3 — Browse 對話框**
- `windows_enum.rs`（§7.4 含圖示提取）、`list_windows`、BrowseDialog。
- 驗收：列出所有 alt-tab 可見視窗；選擇後自動建規則；受保護視窗顯示「無法建立規則」。

**M4 — watcher 自動套用**
- `process.rs`、`watcher.rs`（§7.6）、`applied-update` event、AppliedTable。
- 驗收：對記事本建規則（自訂 affinity LP0-1、priority High）→ 開記事本 → 1–2 秒內面板出現且工作管理員確認 affinity/priority 已變。關閉記事本 → 清單消失。

**M5 — AffinityPicker + 進階優先級**
- AffinityPicker 四 preset + 勾選格；`priority.rs`（IO/mem）。
- 驗收：四 preset 產生的 mask 正確（單元測試，§14）；對測試程式設 I/O 優先級可用第三方工具（如 Process Explorer 看不到 IO 優先級 → 用 `get_io_priority` 回讀驗證）。

**M6 — Tray + 開機啟動 + 單一實例**
- `tray.rs`、`autostart.rs`、single-instance plugin、`--minimized` 參數、closeToTray。
- 驗收：勾「開機啟動」→ 工作排程器出現 FrameAnchor 工作（RL HIGHEST）；重開機後 tray 出現且無 UAC；雙擊 exe 第二次 → 聚焦既有視窗。

**M7 — i18n + 錯誤處理 + 反作弊文案**
- svelte-i18n 全量套用、tray 選單雙語重建、§10 黑名單與錯誤文案。
- 驗收：切語言全部 UI（含 tray）即時切換；對受保護進程（如開著的 anti-cheat 遊戲或 lsass 偽裝測試）顯示正確錯誤。

**M8 — 打包 + 效能驗收**
- NSIS 打包、安裝/解除安裝測試（解除安裝移除 schtasks 工作與 %APPDATA% 資料——NSIS uninstall hook）。
- 對照 §11 逐項量測。
- 驗收：乾淨 Win11 VM 安裝→設定→重開機→自動套用全流程。

---

## 14. 測試計畫

**單元測試（Rust `#[cfg(test)]`，放在各模組內）：**
- `topology::resolve_mask`：四種 mode × 三種假拓撲（無 SMT 均質 8C、SMT 8C16T、混合 8P+16E）→ mask 正確；空 custom → fallback All。
- `process::normalize_path`：`\\?\` 前綴、大小寫、正反斜線。
- `config`：讀寫循環、壞檔備份、缺欄位用 serde default。
- `watcher::match_rule`：fullPath/fileName 比對、enabled=false 跳過、黑名單攔截。

**手動測試矩陣（寫在 PR/commit 說明中逐項勾）：**

| 情境 | 預期 |
|---|---|
| 無 SMT CPU（BIOS 關 HT 或 AMD 均質） | 無 HT 徽章；noSmtSibling preset = 全部核心 |
| Intel 12–14 代混合架構 | P/E 徽章正確；pCoresOnly 只勾 P-core |
| 遊戲 launcher → 子進程才是遊戲本體 | 兩 PID 各自比對；子進程若同 exe 路徑也被套用 |
| 反作弊保護遊戲 | 錯誤狀態正確、不洗日誌（30s 退避） |
| poll interval 調 5s | 套用延遲變長但 CPU 更低 |
| 拔掉遊戲 exe（改名）後啟動 | 狀態顯示失敗，不 crash |
| 規則 matchBy=fileName | 同名不同路徑的程式也被套用（預期行為，UI 有警告） |

---

## 15. 已知限制與風險

1. **>64 邏輯處理器（多 processor group）**：`SetProcessAffinityMask` 只作用於單一 group。v1 假設 group 0、最多 64 LP（涵蓋所有主流遊戲 CPU；Threadripper Pro 等不支援）。啟動時若偵測到 >1 group → 日誌警告 + 面板顯示提示。未來可用 `SetThreadGroupAffinity` 逐執行緒或 `SetProcessDefaultCpuSetMasks` 支援。
2. **反作弊**：§10。Vanguard 類遊戲無法支援，這是產品級限制不是 bug。
3. **affinity 不是 CPU sets**：Windows 還有較軟性的 `SetProcessDefaultCpuSets`（排程器可覆寫）。v1 用硬 affinity（與 Process Lasso 相同行為）；若使用者回報某些遊戲對硬 affinity 敏感，v2 可加「CPU Sets（軟性建議）」模式。
4. **HT sibling 判定**：以 mask 最低位元 LP 當實體執行緒是業界慣例（Windows 排程器也偏好先填每核心第一條執行緒），但 Intel/AMD 未公開保證；實務上與 Ryzen Master / HWiNFO 顯示一致。
5. **WebView2 記憶體**：WebView2 是共用 runtime，工作管理員中其 process 記憶體會被算進本 app；§11 的 120MB 預算已含此現實。
6. **UAC**：手動啟動一律跳 UAC（requireAdministrator）。開機啟動透過 Task Scheduler 無彈窗。若使用者移除排程又勾回來，需重新授權——`schtasks /Create /F` 覆蓋即可。

---

## 16. 附錄：關鍵程式碼片段

### 附錄 A — CPU 拓撲列舉（`topology.rs` 核心）

```rust
use windows::Win32::System::Kernel::{
    GetLogicalProcessorInformationEx, LOGICAL_PROCESSOR_RELATIONSHIP,
    PROCESSOR_RELATIONSHIP, LTP_PC_SMT, RelationProcessorCore,
};
use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;

pub fn enumerate_topology() -> Result<Topology, TopologyError> {
    // 1) 取得所需 buffer 長度
    let mut needed: u32 = 0;
    let _ = unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut needed)
    }; // 預期失敗，needed 被填上
    if needed == 0 { return Err(TopologyError::QueryFailed); }

    // 2) 配置 buffer 正式取資料
    let mut buf = vec![0u8; needed as usize];
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr() as *mut _),
            &mut needed,
        ).map_err(|_| TopologyError::QueryFailed)?;
    }

    // 3) 走訪可變長結構鏈
    let mut cores = Vec::new();
    let mut offset = 0usize;
    while offset < needed as usize {
        let header = unsafe { &*(buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX) };
        if header.Relationship == RelationProcessorCore {
            let proc_rel = unsafe { &*(buf.as_ptr().add(offset) as *const PROCESSOR_RELATIONSHIP) };
            let mask = unsafe { proc_rel.GroupMask[0] }.Mask; // v1: 只用 group 0
            let lp_indices = mask_to_indices(mask);
            cores.push((lp_indices, proc_rel.EfficiencyClass,
                        proc_rel.Flags == LTP_PC_SMT));
        }
        offset += header.Size as usize;
    }
    // 4) 組裝 Topology（§7.1 步驟 3–4）：LP 編號、is_smt_sibling、is_p_core
    Ok(build_topology(cores))
}

fn mask_to_indices(mask: usize) -> Vec<u32> {
    (0..64).filter(|i| mask & (1usize << i) != 0).collect()
}
```

### 附錄 B — 每核心使用率（`usage.rs` 核心）

```rust
// windows-sys 路徑（實際模組以 windows-sys 文件為準）：
// windows_sys::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessorPerformanceInformation}
#[repr(C)]
struct Sppi { idle: i64, kernel: i64, user: i64, dpc: i64, interrupt: i64, interrupts: u32 }

pub fn sample_per_core(prev: &mut Vec<Sppi>) -> Vec<f32> {
    let count = num_logical_processors();
    let mut cur = vec![Sppi::zeroed(); count];
    let mut ret_len = 0u32;
    unsafe {
        NtQuerySystemInformation(
            8, // SystemProcessorPerformanceInformation
            cur.as_mut_ptr() as *mut _,
            (count * std::mem::size_of::<Sppi>()) as u32,
            &mut ret_len,
        );
    }
    let utils = cur.iter().zip(prev.iter()).map(|(c, p)| {
        let idle = c.idle - p.idle;
        let busy = (c.kernel - p.kernel) + (c.user - p.user);
        if busy <= 0 { 0.0 } else { (1.0 - idle as f64 / busy as f64).clamp(0.0, 1.0) as f32 }
    }).collect();
    *prev = cur;
    utils
}
```

### 附錄 C — I/O 優先級（`priority.rs` 核心，手動宣告 ntdll 版）

```rust
type NTSTATUS = i32;
const PROCESS_IO_PRIORITY_CLASS: i32 = 33; // ProcessIoPriority

#[link(name = "ntdll")]
extern "system" {
    fn NtSetInformationProcess(process: isize, class: i32,
                               info: *const u32, len: u32) -> NTSTATUS;
    fn NtQueryInformationProcess(process: isize, class: i32,
                                 info: *mut u32, len: u32, ret_len: *mut u32) -> NTSTATUS;
}

pub fn set_io_priority_raw(handle: isize, value: u32) -> Result<(), PriorityError> {
    let status = unsafe { NtSetInformationProcess(handle, PROCESS_IO_PRIORITY_CLASS, &value, 4) };
    if status >= 0 { Ok(()) } else { Err(PriorityError::NtStatus(status)) }
}
// value: VeryLow=0, Low=1, Normal=2, High=3
```

### 附錄 D — 記憶體優先級

```rust
use windows::Win32::System::Threading::{
    SetProcessInformation, GetProcessInformation,
    PROCESS_INFORMATION_CLASS, MEMORY_PRIORITY_INFORMATION,
};
const ProcessMemoryPriority: PROCESS_INFORMATION_CLASS = PROCESS_INFORMATION_CLASS(5);

pub fn set_memory_priority_raw(h: windows::Win32::Foundation::HANDLE, value: u32) -> windows::core::Result<()> {
    let info = MEMORY_PRIORITY_INFORMATION { MemoryPriority: value };
    unsafe {
        SetProcessInformation(h, ProcessMemoryPriority,
            &info as *const _ as *const _,
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32)
    }
}
// value: VeryLow=1, Low=2, Medium=3, BelowNormal=4, Normal=5
```
> 注意：`MEMORY_PRIORITY_INFORMATION` 與 `SetProcessInformation` 的確切模組路徑依 `windows` crate 版本微調（可能在 `Win32::System::Memory` 或 `Win32::System::Threading`），以 docs.rs 為準。

---

## 17. 名詞對照（i18n 與程式註解統一用詞）

| 英文 | 繁中 |
|---|---|
| Logical Processor (LP) | 邏輯處理器 |
| Physical Core | 實體核心 |
| SMT sibling / Hyper-Threading | HT 虛擬核心 |
| P-core / E-core | P-core（效能核心）/ E-core（效率核心） |
| Affinity | 核心親和性 |
| Priority | 優先級 |
| Rule | 規則 |
| Applied | 已套用 |
| System Tray | 系統匣 |
