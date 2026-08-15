# FrameAnchor

<p align="center">
  <img src="src-tauri/icons/icon.png" width="128" alt="FrameAnchor 圖示">
</p>

<p align="center">
  Windows 專用的 CPU 核心親和性與程序優先級規則工具
</p>

<p align="center">
  <strong>繁體中文</strong> · <a href="README.en.md">English</a>
</p>

FrameAnchor 是一款 Windows 桌面工具，會持續監控指定的遊戲或應用程式，並在目標程序啟動時自動套用 CPU 核心親和性（CPU affinity）、CPU priority，以及選用的 I/O／記憶體優先級規則。

它適合用來實驗不同 CPU 核心配置、降低不必要的核心遷移，或將背景工作負載與延遲敏感的程序分開。FrameAnchor 不保證提高平均 FPS；實際效果取決於 CPU 拓撲、遊戲引擎、背景負載與作業系統排程行為。

## 目錄

- [主要功能](#主要功能)
- [Affinity 模式](#affinity-模式)
- [安全性與已知限制](#安全性與已知限制)
- [安裝](#安裝)
- [使用方式](#使用方式)
- [設定與資料](#設定與資料)
- [GPU 基準測試（Beta）](#gpu-基準測試beta)
- [開發與建置](#開發與建置)
- [發布流程](#發布流程)
- [技術架構](#技術架構)
- [授權](#授權)

## 主要功能

- **持久規則**：規則儲存後，FrameAnchor 會在背景持續偵測並自動套用。
- **快速程序偵測**：每 100 ms 執行輕量 discovery pass，命中執行檔名稱後才解析完整路徑並嘗試取得 process handle。
- **完整路徑或檔名比對**：可精確比對單一安裝位置，或讓規則跟隨可能改變路徑的執行檔。
- **五種 affinity 模式**：支援所有核心、排除 SMT sibling、僅 P-core、自訂核心與偏好核心清單。
- **程序優先級**：支援 Idle、Below Normal、Normal、Above Normal 與 High；刻意不提供 Realtime。
- **進階優先級**：可選擇性設定 I/O priority 與 memory priority。
- **CPU Dashboard**：即時顯示每個邏輯處理器的系統使用率、P-core／E-core、SMT sibling，以及已套用程序狀態。
- **從執行中視窗建立規則**：直接從目前可見的桌面視窗取得執行檔路徑。
- **系統匣常駐**：支援關閉至系統匣、啟動時最小化與 single-instance。
- **開機啟動**：透過 Windows Task Scheduler 以最高權限在使用者登入時啟動。
- **雙語介面**：支援繁體中文與英文。
- **GPU 基準測試（Beta）**：對選定 GPU 逐邏輯處理器測試「驅動中斷親和性」的效能，找出最適合處理 GPU 中斷的核心，並可一鍵匯入為規則推薦。

## Affinity 模式

| 模式 | 行為 |
| --- | --- |
| `All` | 回報並使用所有邏輯處理器，不呼叫 affinity setter。 |
| `NoSmtSibling` | 每個實體核心只選擇主要邏輯處理器，排除 SMT／Hyper-Threading sibling。 |
| `PCoresOnly` | 只選擇系統拓撲中 efficiency class 最高的實體核心；主要用於 Intel 混合架構 CPU。 |
| `Custom` | 手動選取邏輯處理器。 |
| `Prefer` | 使用手動指定的核心清單。現行版本仍會依序嘗試硬 affinity、thread ideal processor 與 CPU Sets，因此不保證只採用軟性偏好。 |

對需要限制核心的模式，後端依序嘗試：

1. `SetProcessAffinityMask`
2. 逐執行緒 `SetThreadIdealProcessorEx`
3. `SetProcessDefaultCpuSets`

Dashboard 會顯示實際採用的核心清單與套用狀態。

## 安全性與已知限制

### 管理員權限

FrameAnchor 以 Windows manifest 的 `requireAdministrator` 執行。手動啟動時會出現 UAC 提示；透過應用程式建立的 Task Scheduler 工作可在登入時以最高權限啟動。

### 反作弊系統

FrameAnchor 只使用標準 Win32 API，不包含 driver、不注入目標程序，也不嘗試繞過反作弊保護。Easy Anti-Cheat、BattlEye、Vanguard 或其他受保護程序可能拒絕操作並回傳 `ACCESS_DENIED`。

若目標使用反作弊系統，可先啟動 FrameAnchor，再啟動遊戲。FrameAnchor 會盡早取得 process handle，但這不保證受保護程序一定允許修改。

### 其他限制

- 僅支援 **Windows**；開發與發行目標為 Windows 11。
- 目前只支援 **processor group 0**，最多 64 個邏輯處理器。
- 不修改 PID 小於 8、關鍵 Windows 程序、`System32` 下的執行檔或 FrameAnchor 自身。
- CPU priority 最高為 **High**；不提供可能造成系統無回應的 Realtime。
- 完整路徑比對較安全；僅檔名比對可能誤套用至其他同名程序。
- 遊戲或軟體的服務條款可能限制外部排程工具，使用前應自行確認。
- 修改規則不等同於效能保證；應以可重複的 frame-time 測試驗證結果。

## 安裝

### 從 Releases 安裝

從 [GitHub Releases](https://github.com/LiuTouo/FrameAnchor/releases) 下載最新版本。提供兩種發布形式：

- **NSIS 安裝程式**（`FrameAnchor_X.Y.Z_x64-setup.exe`）：標準安裝模式。支援自動更新（透過 Tauri updater plugin）。
- **可攜版**（`FrameAnchor_X.Y.Z_x64-portable.zip`）：解壓縮至任意目錄即可執行。啟動時與手動操作均支援線上檢查更新，可自動下載新版、詢問後替換執行檔並重啟。

每個發布資產均附帶 SHA256 校驗檔（`.sha256`）。

### 從原始碼建置

#### 前置需求

- Windows 11
- [Node.js](https://nodejs.org/) 20 或更新版本
- [Rust](https://www.rust-lang.org/tools/install) 1.80 或更新版本，使用 MSVC toolchain
- Visual Studio Build Tools，包含「使用 C++ 的桌面開發」工作負載
- Microsoft Edge WebView2 Runtime（Windows 11 預設已安裝）

#### 建置步驟

```bash
npm ci
npm run build:app
```

`npm run build:app` 是**本機完整桌面應用程式建置的正式指令**，以 `tauri build --no-sign` 執行，產出未簽署的 release 執行檔與 NSIS 安裝程式。已簽署的正式發布版本仍透過 `npm run tauri build` 或 GitHub release 工作流程（使用簽署 secret）產生。

NSIS 安裝程式會輸出至：

```text
src-tauri/target/release/bundle/nsis/
```

## 使用方式

1. 啟動 FrameAnchor，接受 Windows UAC 提示。
2. 開啟 **規則（Rules）** 頁面。
3. 從執行中的視窗選擇目標，或建立／編輯現有規則。
4. 選擇 affinity 模式與 CPU priority；需要時啟用進階 I/O／記憶體優先級。
5. 選擇比對方式：
   - **完整路徑**：只比對指定位置的執行檔。
   - **僅檔名**：比對任何路徑下的同名執行檔。
6. 套用並儲存規則。
7. 保持 FrameAnchor 在背景或系統匣執行。目標程序出現時，規則會自動套用。
8. 在 **Dashboard** 檢查 affinity、priority 與錯誤狀態。

FrameAnchor 結束後，不會再監控新程序；已經套用至執行中程序的設定通常會持續到該程序結束。

## 設定與資料

設定檔位於：

```text
%APPDATA%\FrameAnchor\config.json
```

相關行為：

- 規則與設定以 JSON 儲存。
- 舊版設定缺少新欄位時會使用預設值，以維持向後相容。
- 若設定檔無法解析，原檔會備份為 `config.corrupt.json`，程式改用預設設定啟動。
- 可從設定頁直接開啟資料目錄。
- 背景完整維護週期可在 UI 設為 0.5–5 秒；高頻 discovery pass 固定為 100 ms。

預設設定包括：

- 語言：繁體中文
- 啟動時最小化：開啟
- 關閉至系統匣：開啟
- 開機啟動：關閉
- 背景維護間隔：1 秒
- 進階優先級選項：隱藏

## GPU 基準測試（Beta）

FrameAnchor 內建 GPU 基準測試，目的是找出**最適合處理指定顯示卡驅動中斷**的邏輯處理器（LP）。它會逐 LP 切換 GPU 驅動的「中斷親和性」（Interrupt Affinity），用量測工具收集 frame-time，統計後標出最佳核心與表現嚴重低落的核心，並可把推薦核心集合一鍵匯入為規則草稿。

### 與一般 CPU／GPU 基準測試的區別

- **不是圖形基準測試**：不比較畫質、場景或不同顯示卡的 FPS；畫面只是固定的黑白交替、無 vsync、不設上限的 workload。
- **不是「哪顆核心跑遊戲最快」**：測的是「哪顆核心處理 GPU 中斷時，frame-time 最穩定／最高」。
- 透過每次測試將 GPU 驅動中斷親和性鎖定到單一 LP，量測對該核心的影響；統計包含 Avg／Max／Min／STDEV、1%／0.1%／0.01%／0.005% Low 與同比例的 Percentile（皆採 frame-count 最慢 N% 演算法）。

### 預期耗時

每次測試核心的耗時約為：

```text
取樣秒數 + 暖機秒數 + 啟動等待（5 秒）+ 驅動重啟與穩定（約 14 秒）+ 緩衝
```

總耗時約為 `核心數 × 輪數 × 上述每核心耗時`。例如 16 核心、取樣 30 秒、3 輪，約需 43 分鐘。開始前 UI 會顯示預估分鐘數。

### 風險警告

測試會對選定顯示卡反覆**停用／啟用驅動**（disable/enable），可能導致：

- 畫面黑屏數秒
- 顯示器暫時斷訊、解析度重置
- 其他使用同一 GPU 的工作（含瀏覽器硬體加速）暫停

**開始後請勿操作電腦**，直到測試完成或按下取消。測試本身使用可還原的 crash-safe 日誌；即使中途當機，下次啟動也會自動還原測試前策略。

### 資料與歷史

- 測試記錄儲存於 `%APPDATA%\FrameAnchor\benchmarks\<session-uuid>\`，內含 `session.json`、每輪每核心的 `round-<輪>-lp-<核心>.csv`。
- 歷史列表顯示每筆記錄的日期、GPU、API、狀態、最佳核心與磁碟大小；可檢視詳情或刪除（刪除需確認）。
- 執行中的還原日誌為 `%APPDATA%\FrameAnchor\benchmark-recovery.json`；套用後的單層還原記錄為 `gpu-restore.json`。
- 歷史預設不會自動刪除。

### 套用與還原語意

- 測試本身**不會自動套用**任何策略——每次測試結束都會把 GPU 中斷親和性還原到測試前狀態。
- 「套用」僅在結果通過可靠性門檻（Passed）時開放：需至少 3 輪，且候選核心在輪次間一致勝出、相對亞軍有足夠改善。輪數不足或結果不穩定的 session 會標示為「無法判定」（Inconclusive），不可套用。
- 完成後可在結果頁或歷史中，明確按下「套用最佳核心到 GPU」，才會把中斷親和性鎖定到該最佳 LP。
- 「還原先前設定」會回到**最近一次成功套用之前**的策略（單層還原記錄）。
- 套用／還原都需確認，且會再次短暫重啟 GPU 驅動（可能閃屏）。

### 相容性限制

- 每筆完成記錄會保存當時的 **CPU 指紋**（CPU 身分＋拓撲）與 **GPU 穩定 PnP instance ID**。
- 只有「目前 CPU 指紋一致、且該 GPU 仍存在」的完成記錄才可套用或匯入；不相容的歷史仍可檢視，但套用／匯入會被停用並說明原因。
- 若目前 CPU 硬體與儲存於規則上的推薦指紋不符，會顯示過時硬體警告，但資料保留。

## 開發與建置

安裝相依套件：

```bash
npm ci
```

常用指令：

```bash
npm run dev
# 僅啟動 Vite 前端：http://localhost:1420
# 不包含 Rust 後端與 Tauri IPC

npm run tauri dev
# 啟動完整 Tauri 應用程式

npm run check
# 執行 svelte-check 與 TypeScript 檢查

npm run build
# 建置前端至 dist/

npm run build:app
# 本機完整桌面應用程式建置（未簽署的 release 執行檔與 NSIS 安裝程式），
# 以 tauri build --no-sign 執行，不需 updater 簽署 secret

npm run tauri build
# 完整建置（含 updater 簽署）；需 TAURI_SIGNING_PRIVATE_KEY，
# 主要用於 GitHub release 工作流程

npm run gen-icons
# 重新產生 src-tauri/icons/*

npm run build:benchmark-assets
# 編譯 D3D9 workload sidecar（Rust + Direct3D 9）並複製到資源目錄

npm run verify:benchmark-assets
# 驗證內建基準測試資源（PresentMon／liblava 的 SHA-256 與 D3D9 sidecar 存在）

npm run fetch:benchmark-assets
# 重新下載 PresentMon 與 liblava workload 並更新 SHA256SUMS
```

`npm run tauri build` 會自動依序執行前端建置、D3D9 sidecar 建置與資源驗證，因此打包結果一定包含內建工具與授權聲明。

Rust 檢查與測試：

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
```

完整應用程式與程序操作依賴 Windows API。涉及 live process、affinity、priority、CPU Sets、系統匣、Task Scheduler 或 WebView2 的變更，仍需在 Windows 上使用可拋棄的測試程序進行手動驗證。

## 發布流程

維護者透過推送語意化版本標籤觸發 GitHub Actions 自動建置與發布。

### 前置設定：updater 簽署金鑰

自動更新需要一組 Ed25519 簽署金鑰。若尚未設定，請在本地執行：

```bash
npm run tauri signer generate -- -w src-tauri
```

此命令會在 `src-tauri` 目錄產生私鑰與公鑰。將私鑰內容設為 GitHub repository secret `TAURI_SIGNING_PRIVATE_KEY`；若私鑰有密碼保護，另設 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。將公鑰寫入 `src-tauri/tauri.conf.json` 中 `plugins.updater.pubkey` 欄位（取代 placeholder `REPLACE_ME_WITH_YOUR_PUBLIC_KEY_BASE64`）。

**私鑰絕對不可提交至版本控制。** CI 會在發行前驗證公鑰已替換且 secret 已設定，未設定時建置會失敗並顯示明確錯誤。

### 步驟

1. **同步版本號**

   確保以下四個檔案中的版本號一致（例如 `0.2.0`）：

   - `package.json` — `"version": "0.2.0"`
   - `package-lock.json` — 根層級 `"version": "0.2.0"`（`npm install` 會自動同步）
   - `src-tauri/Cargo.toml` — `[package]` 下的 `version = "0.2.0"`
   - `src-tauri/tauri.conf.json` — 頂層 `"version": "0.2.0"`

2. **提交並建立標籤**

   ```bash
   git add package.json package-lock.json src-tauri/Cargo.toml src-tauri/tauri.conf.json
   git commit -m "chore: bump version to 0.2.0"
   git tag -a v0.2.0 -m "v0.2.0"
   ```

   標籤必須嚴格符合 `vX.Y.Z` 格式。CI 會在建置前驗證標籤與所有檔案版本一致，不一致時會失敗並顯示明確錯誤。

3. **推送觸發建置**

   ```bash
   git push origin master
   git push origin v0.2.0
   ```

   推送標籤後，GitHub Actions 會自動執行版本驗證、簽署金鑰驗證、前端型別檢查、Rust 測試，然後建置以下資產：

   - NSIS 安裝程式（`FrameAnchor_X.Y.Z_x64-setup.exe`）與 `.sha256`
   - 可攜版 ZIP（`FrameAnchor_X.Y.Z_x64-portable.zip`）與 `.sha256`
   - updater 用 `latest.json` 與簽署檔案

4. **下載發布版本**

   建置完成後，前往 [GitHub Releases](https://github.com/LiuTouo/FrameAnchor/releases) 下載所需資產。

### 注意事項

- Windows 二進位檔**未經數位簽章**，下載及執行時 Windows Defender SmartScreen 可能顯示警告。這是預期行為，不影響程式功能。
- 可攜版與安裝版可從 About 頁面手動檢查更新；可攜版啟動時也會自動檢查。
- GitHub Actions 工作流程定義於 `.github/workflows/release.yml`。

## 技術架構

| 層 | 技術 |
| --- | --- |
| 桌面框架 | Tauri v2 |
| 前端 | Svelte 5 runes、TypeScript、Vite |
| 後端 | Rust、tokio |
| Windows 介面 | `windows` crate 與直接 Win32 API |
| 國際化 | `svelte-i18n` |
| 安裝程式 | NSIS |

執行時包含兩個主要背景工作：

- **Watcher**：100 ms discovery pass，加上依設定週期執行的完整維護、重試與狀態更新。
- **Usage sampler**：Dashboard 需要且存在已套用程序時，每秒讀取各邏輯處理器的系統使用率。

詳細的產品原始規格可參考 [`PLAN.md`](PLAN.md)，但當規格與現行程式碼不一致時，應以程式碼為準。

## 專案狀態

專案仍處於早期階段，API、設定格式與排程行為可能在後續版本調整。安裝版與可攜版均支援自動檢查更新與手動更新，版本號由執行檔內建 metadata 動態取得。

## 授權

本專案採用 [GNU General Public License v3.0](LICENSE)。

### 第三方元件聲明

GPU 基準測試功能內建並重新發布以下第三方元件（各自授權如下）：

- **PresentMon 2.5.1**（Intel 出品）— [MIT License](src-tauri/resources/benchmark/LICENSE-PresentMon.txt)。frame-time 收集工具；執行基準測試前會以固定 SHA-256 校驗。
- **liblava Vulkan workload**（`lava-triangle.exe`，由 valleyofdoom/AutoGpuAffinity 發布，使用 liblava 框架）— [MIT License](src-tauri/resources/benchmark/LICENSE-liblava.txt)。Vulkan 測試負載；執行前同樣會校驗 SHA-256。
- **Direct3D 9 workload**（`d3d9-workload.exe`）— 由本專案以 Rust 直接使用 Win32 Direct3D 9 API 撰寫的 sidecar（見 `src-tauri/d3d9-workload/`），GPL-3.0 與本專案一致。

授權全文與 SHA-256 清單存放於 `src-tauri/resources/benchmark/`。
