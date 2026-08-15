//! 基準測試領域模型（Task 1 基礎建設）。
//! 序列化慣例：struct 欄位 camelCase、enum variant PascalCase 字串。
//! 本檔只放可序列化型別與共用工具；儲存/還原/協調分在 storage、recovery、manager。

use serde::{Deserialize, Serialize};

pub mod assets;
pub mod ipc;
pub mod manager;
pub mod metrics;
pub mod process_win;
pub mod recommend;
pub mod recovery;
pub mod runner;
pub mod storage;
pub mod window_win;

// ── GPU 基準測試領域型別 ────────────────────────────────────────────────

/// Session 狀態機
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SessionStatus {
    /// 已建立、尚未開始
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// 執行階段（progress 用）
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BenchmarkStage {
    #[default]
    Init,
    Warmup,
    Collecting,
    Finalizing,
}

/// workload 種類
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WorkloadKind {
    /// 內建 liblava Vulkan workload（lava-triangle.exe）
    #[default]
    Vulkan,
    /// 內建 Rust 編譯的 Direct3D9 workload（d3d9-workload.exe）
    D3D9,
}

/// 基準測試參數
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkConfig {
    /// 要逐一測試的候選 LP；空 = 全部支援 LP
    #[serde(default)]
    pub candidate_lps: Vec<u32>,
    #[serde(default)]
    pub gpu_instance_id: Option<String>,
    #[serde(default)]
    pub workload: WorkloadKind,
    #[serde(default = "default_warm_up_secs")]
    pub warm_up_secs: u32,
    #[serde(default = "default_sample_secs")]
    pub sample_secs: u32,
    /// 已停用：新排程固定 2 篩選 + 3..=5 確認，忽略此欄位。
    /// 保留供舊 session JSON 向後相容；預設 5。
    #[serde(default = "default_repetitions")]
    pub repetitions: u32,
    /// 已棄用：production runner 不再繫結 workload process affinity。
    /// 保留欄位供舊 session JSON 向後相容；預設 false。
    #[serde(default)]
    pub sync_workload_affinity: bool,
    #[serde(default)]
    pub fullscreen: bool,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    /// FPS cap；0 = 不限制
    #[serde(default)]
    pub fps_cap: u32,
    #[serde(default)]
    pub triple_buffer: bool,
    /// Vulkan workload 的額外參數（workload=Vulkan 時必須非空）
    #[serde(default)]
    pub vulkan_args: Vec<String>,
    /// workload exe 覆寫（測試/除錯用；None = 內建資源）
    #[serde(default)]
    pub workload_exe_path: Option<String>,
    /// PresentMon exe 覆寫（測試/除錯用）
    #[serde(default)]
    pub presentmon_path: Option<String>,
    /// 相容舊欄位：遊戲路徑（現由 workload 種類取代）
    #[serde(default)]
    pub game_path: Option<String>,
    #[serde(default)]
    pub window_title: Option<String>,
}

fn default_warm_up_secs() -> u32 {
    5
}
fn default_sample_secs() -> u32 {
    30
}
fn default_repetitions() -> u32 {
    5
}
fn default_width() -> u32 {
    1280
}
fn default_height() -> u32 {
    720
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            candidate_lps: Vec::new(),
            gpu_instance_id: None,
            workload: WorkloadKind::Vulkan,
            warm_up_secs: default_warm_up_secs(),
            sample_secs: default_sample_secs(),
            repetitions: default_repetitions(),
            sync_workload_affinity: false,
            fullscreen: false,
            width: default_width(),
            height: default_height(),
            fps_cap: 0,
            triple_buffer: false,
            vulkan_args: default_vulkan_args(),
            workload_exe_path: None,
            presentmon_path: None,
            game_path: None,
            window_title: None,
        }
    }
}

/// 內建 Vulkan workload 的預設參數（AutoGpuAffinity 內附 liblava 相容格式：
/// `--fullscreen=<0|1> --width=<n> --height=<n> --fps_cap=<n> --triple_buffering=<0|1>`）
fn default_vulkan_args() -> Vec<String> {
    vec![
        "--fullscreen=0".to_string(),
        "--width=1280".to_string(),
        "--height=720".to_string(),
        "--fps_cap=0".to_string(),
        "--triple_buffering=0".to_string(),
    ]
}

/// 單一候選 LP 的最終測試結果
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LpResult {
    #[serde(default)]
    pub lp: u32,
    #[serde(default)]
    pub avg_fps: Option<f64>,
    /// 1% low：最慢 1% 個 instantaneous FPS 的平均（frame-count）
    #[serde(default)]
    pub p1_low: Option<f64>,
    /// 0.1% low：最慢 0.1% 個 instantaneous FPS 的平均（frame-count）
    #[serde(default)]
    pub p01_low: Option<f64>,
    /// 0.01% low：最慢 0.01% 個 instantaneous FPS 的平均（frame-count）
    #[serde(default)]
    pub p001_low: Option<f64>,
    /// 0.005% low：最慢 0.005% 個 instantaneous FPS 的平均（frame-count）
    #[serde(default)]
    pub p0005_low: Option<f64>,
    /// 1% percentile：最慢 1% 分位數的 instantaneous FPS（非平均）
    #[serde(default)]
    pub p1_percentile: Option<f64>,
    /// 0.1% percentile：最慢 0.1% 分位數的 instantaneous FPS（非平均）
    #[serde(default)]
    pub p01_percentile: Option<f64>,
    /// 0.01% percentile：最慢 0.01% 分位數的 instantaneous FPS（非平均）
    #[serde(default)]
    pub p001_percentile: Option<f64>,
    /// 0.005% percentile：最慢 0.005% 分位數的 instantaneous FPS（非平均）
    #[serde(default)]
    pub p0005_percentile: Option<f64>,
    #[serde(default)]
    pub max_fps: Option<f64>,
    #[serde(default)]
    pub min_fps: Option<f64>,
    /// Bessel（n-1）校正的即時 FPS 標準差
    #[serde(default)]
    pub stdev_fps: Option<f64>,
    /// frametime MAD（中位數絕對差）正規化為 frametime 中位數的百分比（越低越穩）
    #[serde(default)]
    pub frametime_mad_pct: Option<f64>,
    /// 慢幀 spike rate：frametime 超過 2×中位數的幀佔比（百分比，越低越好）
    #[serde(default)]
    pub spike_rate_pct: Option<f64>,
    #[serde(default)]
    pub sample_count: u32,
    #[serde(default)]
    pub avg_frame_time_ms: Option<f64>,
    #[serde(default)]
    pub completed: bool,
    /// 錯誤代碼（查 i18n errors.*）
    #[serde(default)]
    pub error: Option<String>,
}

/// 執行期 progress 事件（emit `gpu-benchmark-progress`）
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkProgress {
    pub session_id: String,
    pub stage: String,
    #[serde(default)]
    pub round: Option<u32>,
    #[serde(default)]
    pub lp: Option<u32>,
    #[serde(default)]
    pub percentage: u32,
    #[serde(default)]
    pub eta_secs: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// 執行期間的原始取樣（單一 LP 單一時刻；Task 2 runner 產生）
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CoreSample {
    pub lp: u32,
    pub fps: f64,
    pub frame_time_ms: f64,
}

/// 可靠性（confidence）判定結果。僅成功的 session 由 runner 計算；
/// 舊 session.json 缺此欄位時經 `#[serde(default)]` 解讀為 `Unassessed`。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReliabilityStatus {
    /// 舊 session 或未計算（非 Completed 的 session）——尚未評估，不可套用。
    #[default]
    Unassessed,
    /// 確認證據（一致性規則 + bootstrap 穩定性區間 + 護欄）支持候選超越最小實質
    /// 效應門檻。屬小型樣本決策啟發式，非形式化顯著性。
    Passed,
    /// 確認證據落在可忽略差異帶內——僅描述「觀測到的實務等效」，非統計證明的等效。
    Equivalent,
    /// 確認證據不足以判定（一致性不足、穩定性區間橫跨門檻、確認不足、護欄倒退、
    /// 或證據不完整/無效）。
    Inconclusive,
}

/// 可靠性/信心摘要，隨 `SessionSummary` 持久化（camelCase；向後相容）。
/// 供前端顯示狀態、逐 round 勝者、候選/亞軍 LP、勝場數、複合分數優勢與
/// 護欄（Avg/1% low/spike）比較結果。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReliabilitySummary {
    #[serde(default)]
    pub status: ReliabilityStatus,
    /// 各預期 round（0..evaluated_rounds）的勝者 LP。動態長度對應 round 數，
    /// 無勝者（round 缺漏或無合格 LP）為 `None`，保留位置以區分「缺 round N」
    /// 與「僅有其餘 round 勝者」。
    #[serde(default)]
    pub per_round_winners: Vec<Option<u32>>,
    /// 穩健候選 LP（跨 round 複合分數中位數最高）
    #[serde(default)]
    pub candidate_lp: Option<u32>,
    /// 穩健亞軍 LP（同規則次高；單一 LP 時為 None）
    #[serde(default)]
    pub runner_up_lp: Option<u32>,
    /// 候選在所有預期 round 中的勝場數
    #[serde(default)]
    pub candidate_wins: u32,
    /// 舊版改善欄位（聚合結果，供既有前端沿用）：
    /// (candidate - runner_up) / runner_up * 100；不可得（缺亞軍/非有限/≤0）為 None
    #[serde(default)]
    pub avg_fps_pct: Option<f64>,
    #[serde(default)]
    pub p1_low_pct: Option<f64>,
    #[serde(default)]
    pub p01_low_pct: Option<f64>,
    /// 評估的確認 round 數（3..=5；配對測量）。舊 session 缺欄為 0。
    #[serde(default)]
    pub evaluated_rounds: u32,
    /// 已停用（新排程以一致性規則 + bootstrap 穩定性區間判定，不再用勝場門檻）；
    /// 固定 0。保留欄位供舊 session 向後相容。
    #[serde(default)]
    pub required_wins: u32,
    /// 確認證據：候選相對亞軍的配對複合分數優勢點估計（%，逐確認 round 平均）。
    /// 不可得（確認不足/無效）為 None。
    #[serde(default)]
    pub composite_advantage_pct: Option<f64>,
    /// 護欄：候選 Avg FPS 相較亞軍（確認 round 中位數，%）
    #[serde(default)]
    pub avg_fps_advantage_pct: Option<f64>,
    /// 護欄：候選 1% low 相較亞軍（確認 round 中位數，%）
    #[serde(default)]
    pub p1_low_advantage_pct: Option<f64>,
    /// 護欄：候選 spike rate 相較亞軍（絕對百分點，正 = 候選較差）
    #[serde(default)]
    pub spike_rate_delta_pp: Option<f64>,
    /// 篩選 round 數（新排程固定 2，僅用於選 finalists，不參與推論）；舊 session 缺欄為 0。
    #[serde(default)]
    pub screening_rounds: u32,
    /// 確認 round 數（3..=5，配對/區塊排序以控制時間漂移）；舊 session 缺欄為 0。
    #[serde(default)]
    pub confirmation_rounds: u32,
    /// 確認證據：bootstrap 穩定性區間下界（%，複合分數優勢）。
    /// 序列化欄位名保留 `ciLowerPct` 供向後相容；此非統計信賴區間。
    #[serde(default)]
    pub ci_lower_pct: Option<f64>,
    /// 確認證據：bootstrap 穩定性區間上界（%，複合分數優勢）。
    /// 序列化欄位名保留 `ciUpperPct` 供向後相容；此非統計信賴區間。
    #[serde(default)]
    pub ci_upper_pct: Option<f64>,
    /// 停止原因：`"passed"` / `"equivalent"` / `"inconclusive"`；舊 session 為空字串。
    #[serde(default)]
    pub stopping_reason: String,
}

/// 歷史列表用的摘要
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub status: SessionStatus,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub gpu_name: String,
    /// GPU fingerprint = 穩定 PnP instance id
    #[serde(default)]
    pub gpu_instance_id: String,
    /// CPU fingerprint：`cpu_fingerprint_with`（CPU 身分 + 拓撲），判斷相容性
    #[serde(default)]
    pub cpu_fingerprint: String,
    #[serde(default)]
    pub best_lp: Option<u32>,
    /// 可靠性/信心摘要。`#[serde(default)]` 讓沒有此欄位的舊 session.json 仍可載入，
    /// 解讀為 `ReliabilityStatus::Unassessed`（不可套用）。
    #[serde(default)]
    pub reliability: ReliabilitySummary,
    /// 嚴重 LP（avg/1%/0.1% 低於中位數 85%，或 STDEV 高於 150%）
    #[serde(default)]
    pub severe_lps: Vec<u32>,
    #[serde(default)]
    pub sample_count: u32,
    /// 整個 session 資料夾的位元組數（list/get 時即時計算）
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub config: BenchmarkConfig,
    /// 終結失敗原因（穩定錯誤代碼，查 i18n errors.*）；成功/取消為 None。
    /// `#[serde(default)]` 讓沒有此欄位的舊 session.json 仍可載入。
    #[serde(default)]
    pub error: Option<String>,
}

/// 歷史 session 的「可否套用」狀態（前端顯示用；相容性判定只存在後端）
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ApplyStatus {
    pub can_apply: bool,
    /// None = 可套用；Some(穩定錯誤代碼) 查 i18n errors.*
    #[serde(default)]
    pub reason: Option<String>,
}

/// 單一 session 完整內容（儲存於 session.json）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    pub summary: SessionSummary,
    #[serde(default)]
    pub results: Vec<LpResult>,
    #[serde(default)]
    pub samples: Vec<CoreSample>,
}

/// 執行期狀態（AppState 持有，get_benchmark_state 回傳給前端）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct BenchmarkState {
    #[serde(default)]
    pub status: SessionStatus,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub current_lp: Option<u32>,
    #[serde(default)]
    pub stage: BenchmarkStage,
    #[serde(default)]
    pub progress_pct: u32,
    #[serde(default)]
    pub elapsed_secs: u64,
    #[serde(default)]
    pub cancel_requested: bool,
    /// 啟動還原失敗 → true：封鎖新的 test/apply
    #[serde(default)]
    pub recovery_required: bool,
}

/// 儲存體資訊（get_benchmark_storage_info）
#[derive(Serialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct StorageInfo {
    pub total_bytes: u64,
    pub session_count: usize,
}

// ── CPU fingerprint ─────────────────────────────────────────────────────

/// 穩定 CPU 身分（不含時脈等易變數值）：arch/family/model/stepping。
/// 由 `GetNativeSystemInfo` 取得，無外部工具；同一顆 CPU 永不改變。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuIdentity {
    /// PROCESSOR_ARCHITECTURE（如 9 = PROCESSOR_ARCHITECTURE_AMD64）
    pub architecture: u16,
    /// wProcessorLevel（family）
    pub family: u16,
    /// wProcessorRevision 高 byte（model）
    pub model: u8,
    /// wProcessorRevision 低 byte（stepping）
    pub stepping: u8,
}

/// 偵測目前 CPU 身分（純 Win32，無外部工具）
pub fn detect_cpu_identity() -> CpuIdentity {
    use windows::Win32::System::SystemInformation::{GetNativeSystemInfo, SYSTEM_INFO};
    let mut si = SYSTEM_INFO::default();
    unsafe {
        GetNativeSystemInfo(&mut si);
        // union 欄位與 API 呼叫都需 unsafe 區塊
        CpuIdentity {
            architecture: si.Anonymous.Anonymous.wProcessorArchitecture.0,
            family: si.wProcessorLevel,
            // wProcessorRevision：高 byte = model，低 byte = stepping
            model: (si.wProcessorRevision >> 8) as u8,
            stepping: si.wProcessorRevision as u8,
        }
    }
}

/// 純函式 CPU 指紋：canonical 描述（CPU 身分 + LP 數 + 每實體核心的
/// efficiency/LP 映射）的 sha256 hex。測試注入固定 `CpuIdentity` 即為
/// 完全確定性；生產用 `cpu_fingerprint_with(&topo, &detect_cpu_identity())`。
pub fn cpu_fingerprint_with(topo: &crate::topology::Topology, cpu: &CpuIdentity) -> String {
    use sha2::{Digest, Sha256};
    let mut canonical = format!(
        "arch={};family={};model={};stepping={};lp={};",
        cpu.architecture, cpu.family, cpu.model, cpu.stepping, topo.total_lp
    );
    for core in &topo.physical_cores {
        let lps = core
            .lp_indices
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        canonical.push_str(&format!("{}:{}\n", core.efficiency_class, lps));
    }
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{build_topology, Topology};

    fn fixed_identity() -> CpuIdentity {
        CpuIdentity {
            architecture: 9, // PROCESSOR_ARCHITECTURE_AMD64
            family: 6,
            model: 183,
            stepping: 1,
        }
    }

    fn fp(t: &Topology) -> String {
        cpu_fingerprint_with(t, &fixed_identity())
    }

    fn topo_8c16t() -> Topology {
        build_topology(
            (0..8u32)
                .map(|c| (vec![c * 2, c * 2 + 1], 0, true))
                .collect(),
        )
    }

    #[test]
    fn identical_topology_same_fingerprint() {
        let a = topo_8c16t();
        let b = topo_8c16t();
        assert_eq!(fp(&a), fp(&b));
        assert_eq!(fp(&a).len(), 64); // sha256 hex
    }

    #[test]
    fn different_topology_different_fingerprint() {
        let a = topo_8c16t();
        let b = build_topology((0..8u32).map(|c| (vec![c], 0, false)).collect());
        assert_ne!(fp(&a), fp(&b));
    }

    #[test]
    fn smt_on_off_different_fingerprint() {
        let a = topo_8c16t();
        let b = build_topology((0..8u32).map(|c| (vec![c * 2], 0, true)).collect());
        assert_ne!(fp(&a), fp(&b));
    }

    #[test]
    fn different_cpu_identity_different_fingerprint() {
        let t = topo_8c16t();
        let intel = CpuIdentity {
            architecture: 9,
            family: 6,
            model: 183,
            stepping: 1,
        };
        let amd = CpuIdentity {
            architecture: 9,
            family: 25,
            model: 33,
            stepping: 2,
        };
        assert_ne!(
            cpu_fingerprint_with(&t, &intel),
            cpu_fingerprint_with(&t, &amd)
        );
    }

    #[test]
    fn fingerprint_stable_across_serialization() {
        let t = topo_8c16t();
        let fp = fp(&t);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn benchmark_state_serializes_camel_case() {
        let s = BenchmarkState {
            status: SessionStatus::Running,
            session_id: Some("x".into()),
            current_lp: Some(3),
            stage: BenchmarkStage::Collecting,
            progress_pct: 40,
            elapsed_secs: 8,
            cancel_requested: false,
            recovery_required: false,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"sessionId\""));
        assert!(json.contains("\"currentLp\""));
        assert!(json.contains("\"progressPct\""));
        assert!(json.contains("\"recoveryRequired\""));
        assert!(json.contains("\"Running\""));
        assert!(json.contains("\"Collecting\""));
    }

    /// 新 percentile 欄位以 camelCase 序列化；舊 session 缺欄反序列化為 None（向後相容）。
    #[test]
    fn lp_result_percentile_serializes_and_deserializes_backward_compat() {
        let r = LpResult {
            lp: 0,
            p1_percentile: Some(100.0),
            p01_percentile: Some(90.0),
            p001_percentile: Some(80.0),
            p0005_percentile: Some(70.0),
            ..Default::default()
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"p1Percentile\":100.0"), "json={json}");
        assert!(json.contains("\"p0005Percentile\":70.0"));
        // 舊 session 只帶 low 欄位、缺 percentile → 反序列化後 percentile 為 None
        let back: LpResult =
            serde_json::from_str(r#"{"lp":3,"avgFps":240.0,"p1Low":90.0}"#).unwrap();
        assert_eq!(back.p1_percentile, None);
        assert_eq!(back.p1_low, Some(90.0));
    }

    /// per_round_winners 以三欄固定位置（含 None）序列化，round 1 缺漏不會
    /// 被壓縮掉，能與 round 0/2 的勝者區分。
    #[test]
    fn reliability_per_round_winners_preserves_missing_positions() {
        let rel = ReliabilitySummary {
            status: ReliabilityStatus::Passed,
            per_round_winners: vec![Some(0), None, Some(2)],
            candidate_lp: Some(0),
            runner_up_lp: Some(2),
            candidate_wins: 2,
            avg_fps_pct: Some(1.0),
            p1_low_pct: Some(1.0),
            p01_low_pct: Some(1.0),
            ..Default::default()
        };
        let json = serde_json::to_string(&rel).unwrap();
        // None 序列化為 null（不是省略），固定三欄位置保留
        assert!(
            json.contains("\"perRoundWinners\":[0,null,2]"),
            "per_round_winners 應保留缺漏位置: {json}"
        );
        let back: ReliabilitySummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.per_round_winners, vec![Some(0), None, Some(2)]);
    }

    /// 舊 session 缺新欄位（evaluated_rounds/composite/護欄）→ 反序列化為預設值
    /// （0/None）不報錯；`Equivalent` 可正確序列化回讀。
    #[test]
    fn reliability_old_json_and_equivalent_roundtrip() {
        let old = r#"{"status":"Passed","perRoundWinners":[0,null,2],"candidateLp":0,"runnerUpLp":2,"candidateWins":2,"avgFpsPct":1.0,"p1LowPct":1.0,"p01LowPct":1.0}"#;
        let back: ReliabilitySummary = serde_json::from_str(old).unwrap();
        assert_eq!(back.status, ReliabilityStatus::Passed);
        assert_eq!(back.evaluated_rounds, 0);
        assert_eq!(back.required_wins, 0);
        assert_eq!(back.composite_advantage_pct, None);
        assert_eq!(back.avg_fps_advantage_pct, None);
        assert_eq!(back.p1_low_advantage_pct, None);
        assert_eq!(back.spike_rate_delta_pp, None);
        // 新排程欄位：舊 session 缺欄 → serde 預設值（0/None/空字串）
        assert_eq!(back.screening_rounds, 0);
        assert_eq!(back.confirmation_rounds, 0);
        assert_eq!(back.ci_lower_pct, None);
        assert_eq!(back.ci_upper_pct, None);
        assert_eq!(back.stopping_reason, "");

        let eq = ReliabilitySummary {
            status: ReliabilityStatus::Equivalent,
            ..Default::default()
        };
        let json = serde_json::to_string(&eq).unwrap();
        assert!(json.contains("\"status\":\"Equivalent\""), "json={json}");
        let back2: ReliabilitySummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back2.status, ReliabilityStatus::Equivalent);
    }

    /// 新排程欄位以 camelCase 序列化，並可回讀（screening/confirmation/CI/stopping）。
    #[test]
    fn reliability_new_schedule_fields_roundtrip() {
        let rel = ReliabilitySummary {
            status: ReliabilityStatus::Passed,
            screening_rounds: 2,
            confirmation_rounds: 3,
            ci_lower_pct: Some(1.5),
            ci_upper_pct: Some(4.2),
            stopping_reason: "passed".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&rel).unwrap();
        assert!(json.contains("\"screeningRounds\":2"), "json={json}");
        assert!(json.contains("\"confirmationRounds\":3"));
        assert!(json.contains("\"ciLowerPct\":1.5"));
        assert!(json.contains("\"ciUpperPct\":4.2"));
        assert!(json.contains("\"stoppingReason\":\"passed\""));
        let back: ReliabilitySummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.screening_rounds, 2);
        assert_eq!(back.confirmation_rounds, 3);
        assert_eq!(back.ci_lower_pct, Some(1.5));
        assert_eq!(back.ci_upper_pct, Some(4.2));
        assert_eq!(back.stopping_reason, "passed");
    }

    /// 舊 session 的 LpResult 缺 MAD/spike → 反序列化為 None
    #[test]
    fn lp_result_frametime_robustness_backward_compat() {
        let back: LpResult =
            serde_json::from_str(r#"{"lp":3,"avgFps":240.0,"p1Low":90.0}"#).unwrap();
        assert_eq!(back.frametime_mad_pct, None);
        assert_eq!(back.spike_rate_pct, None);
    }

    #[test]
    fn default_config_uses_product_defaults() {
        let c = BenchmarkConfig::default();
        assert!(!c.fullscreen, "產品預設應為視窗模式");
        assert_eq!(c.width, 1280);
        assert_eq!(c.height, 720);
        assert_eq!(c.width, default_width());
        assert_eq!(c.height, default_height());
        assert_eq!(c.sample_secs, 30, "產品預設取樣應為 30 秒");
        assert_eq!(c.repetitions, 5, "產品預設應為 5 round");
    }

    #[test]
    fn default_vulkan_args_match_default_config() {
        let c = BenchmarkConfig::default();
        assert_eq!(c.vulkan_args, default_vulkan_args());
        let args = default_vulkan_args();
        assert!(args.iter().any(|a| a == "--fullscreen=0"));
        assert!(args.iter().any(|a| a == "--width=1280"));
        assert!(args.iter().any(|a| a == "--height=720"));
    }

    #[test]
    fn missing_fields_get_product_defaults() {
        let c: BenchmarkConfig = serde_json::from_str("{}").unwrap();
        assert!(!c.fullscreen);
        assert_eq!(c.width, 1280);
        assert_eq!(c.height, 720);
    }

    #[test]
    fn explicit_fields_not_overwritten() {
        let c: BenchmarkConfig =
            serde_json::from_str(r#"{"fullscreen":true,"width":800,"height":600}"#).unwrap();
        assert!(c.fullscreen, "顯式 fullscreen=true 不得被覆寫");
        assert_eq!(c.width, 800);
        assert_eq!(c.height, 600);
    }
}
