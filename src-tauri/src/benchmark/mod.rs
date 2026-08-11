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
    /// round（repetitions）1..3；round 順序 asc/desc/asc
    #[serde(default = "default_repetitions")]
    pub repetitions: u32,
    /// 已棄用：production runner 不再繫結 workload process affinity。
    /// 保留欄位供舊 session JSON 向後相容；預設 false。
    #[serde(default)]
    pub sync_workload_affinity: bool,
    #[serde(default = "default_true")]
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
    1
}
fn default_true() -> bool {
    true
}
fn default_width() -> u32 {
    640
}
fn default_height() -> u32 {
    480
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
            fullscreen: true,
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
        "--fullscreen=1".to_string(),
        "--width=640".to_string(),
        "--height=480".to_string(),
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
    /// 1% low（time-weighted）
    #[serde(default)]
    pub p1_low: Option<f64>,
    /// 0.1% low（time-weighted）
    #[serde(default)]
    pub p01_low: Option<f64>,
    /// 0.01% low（time-weighted）
    #[serde(default)]
    pub p001_low: Option<f64>,
    /// 0.005% low（time-weighted）
    #[serde(default)]
    pub p0005_low: Option<f64>,
    #[serde(default)]
    pub max_fps: Option<f64>,
    #[serde(default)]
    pub min_fps: Option<f64>,
    /// Bessel（n-1）校正的即時 FPS 標準差
    #[serde(default)]
    pub stdev_fps: Option<f64>,
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
}
