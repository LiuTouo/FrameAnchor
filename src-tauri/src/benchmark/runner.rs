//! 基準測試 runner（Task 2）：單一 GPU 上逐 LP 循環切換驅動中斷親和性，
//! 用 PresentMon 收集 frametime，統計後找出最佳/嚴重 LP。
//!
//! 所有會動真實系統的依賴（backend / sleeper / process / cancel）都是注入的
//! trait，測試用 fake 跑完整流程，不碰真實 GPU 驅動重啟或真實子程序。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::codes;
use crate::gpu::{
    restore_snapshot, single_lp_mask_bytes, AffinityPolicy, GpuBackend, RegistryValueSnapshot,
    Sleep, DEVICE_POLICY_SINGLE_PROCESSOR,
};

use super::assets::{self, BenchmarkAssets};
use super::metrics::{
    competitive_score, compute_lp_result, is_competitive_eligible, median, merge_rounds,
    parse_presentmon_csv, robust_candidates, round_medians, severe_lps,
};
use super::recovery::{self, RecoveryStage};
use super::storage;
use super::window_win::WorkloadWindow;
use super::{
    cpu_fingerprint_with, BenchmarkConfig, BenchmarkProgress, CpuIdentity, LpResult,
    ReliabilityStatus, ReliabilitySummary, SessionDetail, SessionStatus, SessionSummary,
    WorkloadKind,
};

/// restart 後額外穩定時間（毫秒）：驅動中斷重新分配後等 GPU 安定
pub const RESTART_STABILIZE_MS: u64 = 5000;
/// workload 啟動後、開始收集前的固定等待（毫秒）
pub const WORKLOAD_STARTUP_MS: u64 = 5000;
/// PresentMon 等待其退出（`-timed` 自停）時的 margin（秒）：
/// 超過 `sample_secs + CAPTURE_WAIT_MARGIN_S` 仍未退出 → 視為卡住、穩定代碼失敗
pub const CAPTURE_WAIT_MARGIN_S: u64 = 15;
/// Vulkan/PresentMon 偶發會在 workload 仍正常運作時漏掉整段 Present 事件。
/// 每次都建立新的 workload 與 display-device generation，最多嘗試三次。
pub const MAX_CAPTURE_ATTEMPTS: u32 = 3;
/// 無 FPS 上限 workload 可能產生每秒上萬個 Present；v2.5.1 預設 2048，
/// 提升到 8192 降低 consumer 短暫落後時遺失事件的機率。
pub const PRESENTMON_CIRCULAR_BUFFER_SIZE: u32 = 8192;
/// 長 sleep / capture wait 的取消輪詢間隔（毫秒）。runner 以這個粒度檢查
/// cancel，避免被 5s 穩定、warmup 或 PresentMon 等待長時間阻塞。
pub const CANCEL_POLL_MS: u64 = 100;
/// workload spawn 後，等待其 top-level window 出現的上限（毫秒）。
/// 期間以 [`CANCEL_POLL_MS`] 輪詢，可被取消中斷。
pub const WORKLOAD_WINDOW_WAIT_MS: u64 = 3000;
/// 固定調適排程：篩選 round 數（所有有效候選 LP 都測前 3 round）。
pub const SCREENING_ROUNDS: u32 = 3;
/// 固定調適排程：總 round 數（3 篩選 + 2 確認）。
pub const TOTAL_ROUNDS: u32 = 5;
/// 確認 round 數（前 2 名 finalists 再測 2 round）。
pub const CONFIRMATION_ROUNDS: u32 = 2;
/// 最多 finalists 數。
pub const MAX_FINALISTS: usize = 2;

/// 子程序控制邊界。`spawn` 回傳 pid（owned handle），終結時由 runner 統一 `kill`。
pub trait ProcessRunner: Send + Sync {
    fn spawn(&self, exe: &Path, args: &[String]) -> Result<u32, String>;
    fn is_alive(&self, pid: u32) -> bool;
    fn kill(&self, pid: u32) -> Result<(), String>;
    /// 等 pid 退出，最多 timeout_ms；true=已退出，false=逾時
    fn wait_exit(&self, pid: u32, timeout_ms: u64) -> Result<bool, String>;
    /// 已退出程序的 exit code（None = 仍在執行 / 未知）。診斷用，無副作用。
    fn exit_code(&self, _pid: u32) -> Option<i32> {
        None
    }
    /// 該 pid 的 stdout/stderr bounded tail（None = 未擷取）。診斷用。
    fn output_tail(&self, _pid: u32, _max_chars: usize) -> Option<ProcessOutput> {
        None
    }
}

/// 子程序的 bounded stdout/stderr tail（診斷用）。內容有長度上限，不洩漏環境資料。
#[derive(Clone, Debug, Default)]
pub struct ProcessOutput {
    pub stdout: String,
    pub stderr: String,
}

/// 單一 capture 的診斷資料，寫入 `session_dir/diag/capture-round-<r>-lp-<lp>.json`。
/// 即使 session 失敗也保留，供 restart 後診斷「第二次 capture 無 CSV」。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDiagnostics {
    pub round: u32,
    pub lp: u32,
    /// capture attempt（1=首次，2=retry）。用於區分診斷檔名。
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    pub workload_pid: u32,
    /// PresentMon 啟動前 workload 是否還活著
    pub workload_alive_before_pm: bool,
    /// capture 結束後（PresentMon 退出後）workload 是否還活著
    pub workload_alive_after_capture: bool,
    /// workload 提早退出時的 exit code
    #[serde(default)]
    pub workload_exit_code: Option<i32>,
    #[serde(default)]
    pub workload_stdout: String,
    #[serde(default)]
    pub workload_stderr: String,
    pub presentmon_pid: u32,
    /// PresentMon 正常退出時的 exit code
    #[serde(default)]
    pub presentmon_exit_code: Option<i32>,
    /// wait_exit 是否回傳 Ok（沒 Error；timeout 也屬 Ok）
    pub wait_completed: bool,
    /// wait_exit 回傳 Ok(false)：PresentMon 逾時未退出
    pub wait_timed_out: bool,
    #[serde(default)]
    pub wait_error: Option<String>,
    #[serde(default)]
    pub presentmon_stdout: String,
    #[serde(default)]
    pub presentmon_stderr: String,
    pub csv_path: String,
    pub csv_exists: bool,
    pub csv_size_bytes: u64,
    /// PresentMon 的處理序篩選方式（固定 "process_id"）
    #[serde(default)]
    pub capture_filter_kind: String,
    /// `-process_id` 傳入的精確十進位 PID（runner 持有的 spawned workload PID）
    #[serde(default)]
    pub capture_filter_value: String,
    /// 每次 capture 的獨立 ETW session，避免與其他 PresentMon 工具互相終止。
    #[serde(default)]
    pub presentmon_session_name: String,
    /// PresentMon stderr 是否回報 ETW events lost（擷取負載過高訊號）
    #[serde(default)]
    pub etw_events_lost: bool,
    /// 本次 capture 的穩定錯誤代碼（成功為 None）
    #[serde(default)]
    pub error: Option<String>,
}

/// 每次 capture 的診斷輸出 tail 上限（bytes/stream）；避免診斷檔無限成長
pub const DIAG_OUTPUT_TAIL_CAP: usize = 4096;

fn default_attempt() -> u32 {
    1
}

/// 取消訊號：runner 只在安全階段邊界檢查
pub trait CancelSignal: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

/// 一次執行所需的全部注入依賴與狀態
pub struct RunContext {
    pub backend: Arc<dyn GpuBackend>,
    pub sleeper: Arc<dyn Sleep>,
    pub processes: Arc<dyn ProcessRunner>,
    pub cancel: Arc<dyn CancelSignal>,
    pub topo: crate::topology::Topology,
    pub cpu_identity: CpuIdentity,
    pub assets: BenchmarkAssets,
    pub storage_root: PathBuf,
    pub journal_path: PathBuf,
    pub session_id: String,
    pub config: BenchmarkConfig,
    /// 每個 progress 事件都會呼叫（manager 拿來更新 state + emit 事件）
    pub on_progress: Box<dyn FnMut(&BenchmarkProgress) + Send>,
    /// pre-flight 取得的原始策略快照（終結時還原）
    pub baseline: Option<AffinityPolicy>,
    /// 本 session 擁有的子程序 pid（終結時全部終止）
    pub owned_processes: Vec<u32>,
    /// workload 視窗調整（僅 Vulkan windowed 使用；測試注入 fake）
    pub window: Arc<dyn WorkloadWindow>,
}

/// runner 的最終結果
pub struct RunResult {
    pub status: SessionStatus,
    pub detail: SessionDetail,
    /// 錯誤代碼（Failed 時）
    pub error: Option<String>,
    pub best_lp: Option<u32>,
    pub severe_lps: Vec<u32>,
    pub recommended_cores: Vec<u32>,
    /// 終結還原失敗 → 需封鎖新的 test/apply（啟動時會重試還原）
    pub recovery_required: bool,
}

enum TerminalReason {
    Cancelled,
    Error(String),
}

/// capture wait 的結果
enum CaptureWaitOutcome {
    /// PresentMon 自行退出
    Exited,
    /// 逾時未退出
    TimedOut,
    /// 等待期間收到取消
    Cancelled,
    /// wait 本身回報錯誤
    Failed(String),
}

/// 可中斷 sleep：以 [`CANCEL_POLL_MS`] 分段睡，每段檢查 cancel。
/// 回傳 true = 已取消（sleep 被提前中斷）。
fn sleep_interruptible(ctx: &RunContext, ms: u64) -> bool {
    let mut remaining = ms;
    while remaining > 0 {
        if ctx.cancel.is_cancelled() {
            return true;
        }
        let step = remaining.min(CANCEL_POLL_MS);
        ctx.sleeper.sleep(step);
        remaining -= step;
    }
    ctx.cancel.is_cancelled()
}

/// 可中斷地等待 PresentMon 退出。以單一等待預算 `timeout_ms` 計算：每輪先
/// 非阻塞輪詢 `wait_exit(pid, 0)`（已退出即回 Exited），再以 [`CANCEL_POLL_MS`]
/// 分段睡兼作取消輪詢節奏。預算完全由累計 sleep 時間扣減，production 的
/// `wait_exit(0)` 立即回、sleeper 真的睡，故實際逾時 == `timeout_ms`（不再因
/// `wait_exit` 阻塞 + sleep 而 double）；fake sleeper（不真的睡）在有限輪數內
/// 收斂，測試不空轉。取消輪詢粒度維持 ≤ [`CANCEL_POLL_MS`]。
fn wait_capture(ctx: &RunContext, pid: u32, timeout_ms: u64) -> CaptureWaitOutcome {
    let mut remaining = timeout_ms;
    loop {
        if ctx.cancel.is_cancelled() {
            return CaptureWaitOutcome::Cancelled;
        }
        // timeout 0 = 非阻塞輪詢：只檢查一次是否已退出，不做任何等待。
        match ctx.processes.wait_exit(pid, 0) {
            Ok(true) => return CaptureWaitOutcome::Exited,
            Ok(false) => {}
            Err(e) => return CaptureWaitOutcome::Failed(e),
        }
        if remaining == 0 {
            return CaptureWaitOutcome::TimedOut;
        }
        let step = remaining.min(CANCEL_POLL_MS);
        ctx.sleeper.sleep(step);
        remaining -= step;
    }
}

/// 是否對內建 Vulkan workload 安裝關閉防護：僅限內建 lava-triangle
/// （`workload_exe_path` 為 None 才用內建資源）。D3D9 與自訂 exe 不保護。
fn should_guard_close(config: &BenchmarkConfig) -> bool {
    config.workload == WorkloadKind::Vulkan && config.workload_exe_path.is_none()
}

/// 以 [`CANCEL_POLL_MS`] 輪詢 `op` 直到它回 `Ok(true)`；`Ok(false)` 表示視窗尚未
/// 建立，在 [`WORKLOAD_WINDOW_WAIT_MS`] 預算內重試；`Err` 或預算用盡只 log warn，
/// 不影響 benchmark（保留預設狀態）。等待可被 cancel 中斷。
fn poll_window_ready(
    ctx: &RunContext,
    wl_pid: u32,
    what: &str,
    mut op: impl FnMut() -> Result<bool, String>,
) {
    let mut remaining = WORKLOAD_WINDOW_WAIT_MS;
    loop {
        match op() {
            Ok(true) => return,
            Ok(false) => {
                if remaining == 0 || ctx.cancel.is_cancelled() {
                    log::warn!("{what}：找不到 workload pid {wl_pid} 的 top-level visible window");
                    return;
                }
                let step = remaining.min(CANCEL_POLL_MS);
                ctx.sleeper.sleep(step);
                remaining -= step;
            }
            Err(e) => {
                log::warn!("{what} 失敗（pid={wl_pid}）: {e}");
                return;
            }
        }
    }
}

/// Vulkan windowed 模式：等待 workload 的 top-level window 出現並把 client
/// area 調成 config.width×height。
fn resize_workload_window(ctx: &RunContext, wl_pid: u32) {
    poll_window_ready(ctx, wl_pid, "調整 workload 視窗", || {
        ctx.window
            .find_and_resize(wl_pid, ctx.config.width, ctx.config.height)
    });
}

/// 停用 workload 視窗的關閉能力（SC_CLOSE），防使用者誤關。
fn guard_workload_window(ctx: &RunContext, wl_pid: u32) {
    poll_window_ready(ctx, wl_pid, "停用 workload 關閉鈕", || {
        ctx.window.guard_close(wl_pid)
    });
}

/// 內建 Vulkan workload：先安裝關閉防護（windowed 與 fullscreen 都做），
/// windowed 再調整 client size。
fn prepare_workload_window(ctx: &RunContext, wl_pid: u32) {
    if should_guard_close(&ctx.config) {
        guard_workload_window(ctx, wl_pid);
    }
    if ctx.config.workload == WorkloadKind::Vulkan && !ctx.config.fullscreen {
        resize_workload_window(ctx, wl_pid);
    }
}

/// 單一 (round, lp) capture 的結果控制（供 run_benchmark 兩階段排程使用）。
enum StepOutcome {
    /// capture 成功（CSV 已寫入）；繼續下一項
    Continue,
    /// 該 LP 因 MISSING/EMPTY 且已有成功 capture 而被隔離（記錄錯誤碼，session 終將 Failed）
    Isolated(String),
    /// 需立即終止（取消或錯誤）
    Break(TerminalReason),
}

/// 執行單一 (round, lp) 的完整 capture：套用單 LP 策略 → 重啟 GPU → spawn workload →
/// PresentMon 收集（含 retry）。成功回傳 Continue；MISSING/EMPTY 且已有成功 capture
/// 回傳 Isolated（隔離該 LP 並繼續）；其餘錯誤/取消回傳 Break。
#[allow(clippy::too_many_arguments)]
fn capture_step(
    ctx: &mut RunContext,
    instance: &str,
    round: u32,
    lp: u32,
    session_dir: &Path,
    round_csvs: &mut HashMap<u32, HashMap<u32, PathBuf>>,
    done: u32,
    total_tests: u32,
    detail: &SessionDetail,
) -> StepOutcome {
    let pct = (done * 100 / total_tests.max(1)).min(100);
    let eta = eta_secs(&ctx.config, total_tests, done);
    emit(ctx, detail, "applying", Some(round), Some(lp), pct, eta, None);

    // 1) 日誌 baseline（第一次變更前）+ 寫入單 LP 策略
    if let Err(e) = recovery::begin_at(&ctx.journal_path, ctx.baseline.as_ref().unwrap()) {
        return StepOutcome::Break(TerminalReason::Error(e));
    }
    let new_policy = AffinityPolicy {
        instance_id: instance.to_string(),
        device_policy: RegistryValueSnapshot::dword(DEVICE_POLICY_SINGLE_PROCESSOR),
        assignment_set_override: RegistryValueSnapshot::binary(single_lp_mask_bytes(lp)),
    };
    if let Err(_e) = ctx.backend.write_affinity_policy(&new_policy) {
        return StepOutcome::Break(TerminalReason::Error(codes::GPU_APPLY_FAILED.to_string()));
    }
    if let Err(e) = require_journal(&ctx.journal_path).and_then(|j| {
        recovery::advance_to_at(&ctx.journal_path, &j, RecoveryStage::PolicyApplied)
    }) {
        return StepOutcome::Break(TerminalReason::Error(e));
    }

    // 2) 重啟 GPU（2s/2s 由 backend 內含）+ 5s 穩定
    if let Err(_e) = ctx.backend.restart_device(instance, ctx.sleeper.as_ref()) {
        return StepOutcome::Break(TerminalReason::Error(codes::GPU_RESTART_FAILED.to_string()));
    }
    if sleep_interruptible(ctx, RESTART_STABILIZE_MS) {
        return StepOutcome::Break(TerminalReason::Cancelled);
    }
    if let Err(e) = require_journal(&ctx.journal_path).and_then(|j| {
        recovery::advance_to_at(&ctx.journal_path, &j, RecoveryStage::DeviceRestarted)
    }) {
        return StepOutcome::Break(TerminalReason::Error(e));
    }

    // 3) 啟動 workload
    emit(ctx, detail, "launching", Some(round), Some(lp), pct, eta, None);
    let (wl_exe, wl_args) = workload_command(&ctx.assets, &ctx.config);
    let wl_pid = match ctx.processes.spawn(&wl_exe, &wl_args) {
        Ok(pid) => {
            ctx.owned_processes.push(pid);
            pid
        }
        Err(e) => {
            log::warn!("workload 啟動失敗: {e}");
            return StepOutcome::Break(TerminalReason::Error(
                codes::BENCHMARK_WORKLOAD_FAILED.to_string(),
            ));
        }
    };
    // sync_workload_affinity 已棄用：production runner 絕不把 workload
    // process affinity 繫結到被測 LP。測量變因只能是 GPU interrupt affinity，
    // 將 workload 鎖在單一 LP 會導致 Vulkan present 事件全無、PresentMon
    // 無法產生 CSV（BENCHMARK_CAPTURE_MISSING）。

    // 3.5) 內建 Vulkan：安裝關閉防護；windowed 另調整 client size
    prepare_workload_window(ctx, wl_pid);

    // 4) 啟動固定等待 + 設定 warm-up
    if sleep_interruptible(
        ctx,
        WORKLOAD_STARTUP_MS + (ctx.config.warm_up_secs as u64) * 1000,
    ) {
        return StepOutcome::Break(TerminalReason::Cancelled);
    }

    // 5) PresentMon 收集 sample_secs（含 stale session 清理、逾時/輸出驗證）
    emit(ctx, detail, "collecting", Some(round), Some(lp), pct, eta, None);
    let csv = session_dir.join(format!("round-{round}-lp-{lp}.csv"));
    let mut capture_attempt: u32 = 1;
    let mut capture_result = run_capture(ctx, round, lp, wl_pid, &csv, capture_attempt);
    if ctx.cancel.is_cancelled() {
        return StepOutcome::Break(TerminalReason::Cancelled);
    }
    while capture_attempt < MAX_CAPTURE_ATTEMPTS && should_retry_capture(&capture_result) {
        log::warn!(
            "capture round-{round}-lp-{lp} attempt {capture_attempt} 失敗（{:?}），進行 retry",
            capture_result.as_ref().err()
        );
        // workload 已在 run_capture 內被 kill，確定性清理完成
        if ctx.cancel.is_cancelled() {
            return StepOutcome::Break(TerminalReason::Cancelled);
        }
        // 單純重啟 workload 無法修復 driver restart 後卡住的 Vulkan
        // device/swapchain。維持相同 affinity policy，再做一次完整 GPU
        // restart，讓 retry 建立在新的 display device generation 上。
        if let Err(e) = ctx.backend.restart_device(instance, ctx.sleeper.as_ref()) {
            log::warn!("capture retry 前 GPU 重啟失敗: {e}");
            return StepOutcome::Break(TerminalReason::Error(codes::GPU_RESTART_FAILED.to_string()));
        }
        if sleep_interruptible(ctx, RESTART_STABILIZE_MS) {
            return StepOutcome::Break(TerminalReason::Cancelled);
        }
        // 重 spawn workload（絕不設 CPU affinity）
        let (wl_exe, wl_args) = workload_command(&ctx.assets, &ctx.config);
        let wl_pid2 = match ctx.processes.spawn(&wl_exe, &wl_args) {
            Ok(pid) => {
                ctx.owned_processes.push(pid);
                pid
            }
            Err(e) => {
                log::warn!("retry workload 啟動失敗: {e}");
                return StepOutcome::Break(TerminalReason::Error(
                    codes::BENCHMARK_WORKLOAD_FAILED.to_string(),
                ));
            }
        };
        prepare_workload_window(ctx, wl_pid2);
        if sleep_interruptible(
            ctx,
            WORKLOAD_STARTUP_MS + (ctx.config.warm_up_secs as u64) * 1000,
        ) {
            return StepOutcome::Break(TerminalReason::Cancelled);
        }
        capture_attempt += 1;
        capture_result = run_capture(ctx, round, lp, wl_pid2, &csv, capture_attempt);
    }
    if ctx.cancel.is_cancelled() {
        return StepOutcome::Break(TerminalReason::Cancelled);
    }
    if let Err(e) = capture_result {
        if e == codes::BENCHMARK_CAPTURE_MISSING || e == codes::BENCHMARK_CAPTURE_EMPTY {
            if round_csvs.is_empty() {
                // 尚無任何成功 capture：第一個候選 LP 就 MISSING/EMPTY，代表
                // PresentMon/ETW 環境根本無法建立 CSV。繼續跑剩餘 LP/round 只會
                // 反覆失敗，立即終止並交由 terminal 統一 cleanup/restore。
                log::error!(
                    "capture round-{round}-lp-{lp} 經 {capture_attempt} 次嘗試仍失敗 \
                     （{e}），且尚無任何成功 capture；中止 session"
                );
                return StepOutcome::Break(TerminalReason::Error(e));
            }
            log::error!(
                "capture round-{round}-lp-{lp} 經 {capture_attempt} 次嘗試仍失敗；隔離此 LP 並繼續"
            );
            return StepOutcome::Isolated(e);
        }
        return StepOutcome::Break(TerminalReason::Error(e));
    }
    round_csvs.entry(lp).or_default().insert(round, csv);
    emit(ctx, detail, "collected", Some(round), Some(lp), pct, eta, None);
    StepOutcome::Continue
}

/// 主要入口：執行整個基準測試並回傳最終結果。
/// 固定調適排程：篩選 rounds 0..2 測所有有效候選 LP，再選出最多兩個 finalists，
/// 確認 rounds 3..4 只測 finalists。
/// 成功 / 取消 / 失敗都會：停止 owned 子程序 → 還原原始策略並重啟 GPU →
/// 還原驗證成功才清除日誌 → 原子寫入最終 session。
pub fn run_benchmark(ctx: &mut RunContext) -> RunResult {
    // 前置驗證
    let (instance, gpu_name) = match pre_flight(ctx) {
        Ok(v) => v,
        Err(e) => return abort(ctx, e),
    };
    let lps = effective_lps(&ctx.config, &ctx.topo);
    if lps.is_empty() {
        return abort(ctx, codes::BENCHMARK_INVALID_CONFIG.to_string());
    }

    // 建立 session（Running）並原子寫入
    let mut detail = SessionDetail {
        summary: SessionSummary {
            id: ctx.session_id.clone(),
            status: SessionStatus::Running,
            started_at: chrono::Local::now().to_rfc3339(),
            finished_at: None,
            gpu_name,
            gpu_instance_id: instance.clone(),
            cpu_fingerprint: cpu_fingerprint_with(&ctx.topo, &ctx.cpu_identity),
            best_lp: None,
            reliability: ReliabilitySummary::default(),
            severe_lps: Vec::new(),
            sample_count: 0,
            total_bytes: 0,
            config: ctx.config.clone(),
            error: None,
        },
        results: Vec::new(),
        samples: Vec::new(),
    };
    let _ = storage::save_session_at(&ctx.storage_root, &detail);
    emit(ctx, &detail, "starting", None, None, 0, None, None);

    let session_dir = ctx.storage_root.join(&ctx.session_id);
    // LP → round → CSV 路徑（每個 (lp, round) 最多擷取一次）
    let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
    // 計劃測試數 = N*3 + min(N,2)*2（篩選全 LP 3 round + 確認前 2 名 2 round）
    let n = lps.len() as u32;
    let total_tests = n * SCREENING_ROUNDS + n.min(MAX_FINALISTS as u32) * CONFIRMATION_ROUNDS;
    let mut done = 0u32;
    let mut reason: Option<TerminalReason> = None;
    // MISSING/EMPTY 的處置分兩類（見 capture_step 分支）：
    // - 尚無任何成功 capture → fail-fast，立即終止（PresentMon 根本無法建立 CSV）。
    // - 已有成功 capture → 單一 LP 屬可隔離的擷取故障，記錄後繼續收集其他 LP。
    let mut isolated_capture_error: Option<String> = None;

    // 篩選階段：rounds 0..SCREENING_ROUNDS，全部候選 LP。
    'screening: for round in 0..SCREENING_ROUNDS {
        for &lp in round_order(round, &lps).iter() {
            if ctx.cancel.is_cancelled() {
                reason = Some(TerminalReason::Cancelled);
                break 'screening;
            }
            done += 1;
            match capture_step(
                ctx,
                &instance,
                round,
                lp,
                &session_dir,
                &mut round_csvs,
                done,
                total_tests,
                &detail,
            ) {
                StepOutcome::Continue => {}
                StepOutcome::Isolated(e) => {
                    isolated_capture_error.get_or_insert(e);
                }
                StepOutcome::Break(r) => {
                    reason = Some(r);
                    break 'screening;
                }
            }
        }
    }

    // 篩選後選 finalists（僅當無終止原因且無隔離擷取錯誤）。隔離錯誤已使 session
    // 終將 Failed，直接跳過確認以省時。
    let mut finalists: Vec<u32> = Vec::new();
    if reason.is_none() && isolated_capture_error.is_none() {
        finalists = select_finalists(&round_csvs, SCREENING_ROUNDS);
    }

    // 確認階段：rounds SCREENING_ROUNDS..TOTAL_ROUNDS，僅 finalists（需恰好 2 名）。
    if reason.is_none()
        && isolated_capture_error.is_none()
        && finalists.len() == MAX_FINALISTS
    {
        'confirmation: for round in SCREENING_ROUNDS..TOTAL_ROUNDS {
            for &lp in round_order(round, &finalists).iter() {
                if ctx.cancel.is_cancelled() {
                    reason = Some(TerminalReason::Cancelled);
                    break 'confirmation;
                }
                done += 1;
                match capture_step(
                    ctx,
                    &instance,
                    round,
                    lp,
                    &session_dir,
                    &mut round_csvs,
                    done,
                    total_tests,
                    &detail,
                ) {
                    StepOutcome::Continue => {}
                    StepOutcome::Isolated(e) => {
                        isolated_capture_error.get_or_insert(e);
                    }
                    StepOutcome::Break(r) => {
                        reason = Some(r);
                        break 'confirmation;
                    }
                }
            }
        }
    }

    if reason.is_none() {
        if let Some(e) = isolated_capture_error {
            reason = Some(TerminalReason::Error(e));
        }
    }

    match reason {
        Some(TerminalReason::Cancelled) => {
            // 保留已收集到的部分結果
            let (partial, _) = compute_session_results(&round_csvs);
            detail.results = partial;
            detail.summary.sample_count = detail.results.iter().map(|r| r.sample_count).sum();
            terminal(
                ctx,
                detail,
                SessionStatus::Cancelled,
                None,
                None,
                Vec::new(),
                Vec::new(),
            )
        }
        Some(TerminalReason::Error(e)) => {
            // 保留已收集到的部分結果，無推薦
            let (partial, _) = compute_session_results(&round_csvs);
            detail.results = partial;
            detail.summary.sample_count = detail.results.iter().map(|r| r.sample_count).sum();
            terminal(
                ctx,
                detail,
                SessionStatus::Failed,
                Some(e),
                None,
                Vec::new(),
                Vec::new(),
            )
        }
        None => {
            // 成功：合併各 round frametime → 統計 → 最佳/嚴重 LP
            let (results, compute_err) = compute_session_results(&round_csvs);
            detail.results = results.clone();
            detail.summary.sample_count = results.iter().map(|r| r.sample_count).sum();
            if let Some(e) = compute_err {
                // 任一 LP 的 CSV 缺失/空/無效 → 失敗，保留已算出的部分結果、無推薦
                return terminal(
                    ctx,
                    detail,
                    SessionStatus::Failed,
                    Some(e),
                    None,
                    Vec::new(),
                    Vec::new(),
                );
            }
            let severe = severe_lps(&results);
            let reliability = compute_reliability(&round_csvs, &results, ctx.config.repetitions);
            // 穩健候選（跨 round 中位數分數）驅動 best_lp 與推薦，不再用聚合 dense-rank best_lp
            let best = reliability.candidate_lp;
            let recommended = best.map(|b| vec![b]).unwrap_or_default();
            detail.summary.reliability = reliability;
            terminal(
                ctx,
                detail,
                SessionStatus::Completed,
                None,
                best,
                severe,
                recommended,
            )
        }
    }
}

/// 前置驗證：資源 hash、GPU 存在、BasicDisplay 可用、設定有效。
/// 成功回傳 (gpu_instance_id, gpu_name)，並把原始策略快照存進 ctx.baseline。
fn pre_flight(ctx: &mut RunContext) -> Result<(String, String), String> {
    assets::verify(&ctx.assets).map_err(|e| {
        log::error!("基準測試資源驗證失敗: {e}");
        e.code().to_string()
    })?;

    let instance = ctx
        .config
        .gpu_instance_id
        .clone()
        .ok_or_else(|| codes::BENCHMARK_INVALID_CONFIG.to_string())?;

    let gpu_name = {
        let adapters = ctx
            .backend
            .enumerate_present_adapters()
            .map_err(|e| e.code().to_string())?;
        adapters
            .iter()
            .find(|d| d.instance_id.eq_ignore_ascii_case(&instance))
            .map(|d| d.friendly_name.clone())
            .ok_or_else(|| codes::GPU_NOT_FOUND.to_string())?
    };

    if !ctx
        .backend
        .basic_display_enabled()
        .map_err(|e| e.code().to_string())?
    {
        return Err(codes::GPU_BASIC_DISPLAY_DISABLED.to_string());
    }

    validate_config(&ctx.config, &ctx.topo)?;

    let baseline = ctx
        .backend
        .read_affinity_policy(&instance)
        .map_err(|e| e.code().to_string())?;
    ctx.baseline = Some(baseline);
    Ok((instance, gpu_name))
}

/// 設定驗證（獨立函式方便測試）
pub fn validate_config(
    config: &BenchmarkConfig,
    topo: &crate::topology::Topology,
) -> Result<(), String> {
    if config.gpu_instance_id.is_none() {
        return Err(codes::BENCHMARK_INVALID_CONFIG.to_string());
    }
    if config.sample_secs == 0 {
        return Err(codes::BENCHMARK_INVALID_CONFIG.to_string());
    }
    // 固定調適排程（3 篩選 + 2 確認）只支援 repetitions == 5；欄位保留供舊
    // session 反序列化，但新 run 只接受 5。
    if config.repetitions != TOTAL_ROUNDS {
        return Err(codes::BENCHMARK_INVALID_CONFIG.to_string());
    }
    if effective_lps(config, topo).is_empty() {
        return Err(codes::BENCHMARK_INVALID_CONFIG.to_string());
    }
    if config.workload == WorkloadKind::Vulkan && config.vulkan_args.is_empty() {
        return Err(codes::BENCHMARK_INVALID_CONFIG.to_string());
    }
    if config.width == 0 || config.height == 0 {
        return Err(codes::BENCHMARK_INVALID_CONFIG.to_string());
    }
    Ok(())
}

/// 有效 LP：候選清單 ∩ [0, min(total_lp,64))；空清單 = 全部支援 LP。
/// 一律排除 physical Core 0 的所有 LP（含 SMT sibling），避免 benchmark 干擾核心 0。
pub fn effective_lps(config: &BenchmarkConfig, topo: &crate::topology::Topology) -> Vec<u32> {
    let max_lp = topo.total_lp.min(64);
    let all: Vec<u32> = (0..max_lp).collect();
    let source = if config.candidate_lps.is_empty() {
        all
    } else {
        config.candidate_lps.clone()
    };
    // physical Core 0 的 LP index 集合（core_id 由拓撲列舉時依 index 順序指派）
    let core0_lps: Vec<u32> = topo
        .logical_processors
        .iter()
        .filter(|lp| lp.core_id == 0)
        .map(|lp| lp.index)
        .collect();
    let mut lps: Vec<u32> = source
        .into_iter()
        .filter(|&lp| lp < max_lp)
        .filter(|&lp| !core0_lps.contains(&lp))
        .collect();
    lps.sort_unstable();
    lps.dedup();
    lps
}

/// 每 round 的 LP 順序：確定性「旋轉 + 反轉」平衡排程。
/// round r 的起始位置 = `r % n`（n = LP 數），方向 = r 為奇數時遞減、偶數遞增。
/// 每個 round 都是完整排列（不省略、不重複），且相鄰 round 的起始位置與方向
/// 皆改變，避免固定順序造成的系統性偏誤。
pub fn round_order(round: u32, lps: &[u32]) -> Vec<u32> {
    let mut v = lps.to_vec();
    v.sort_unstable();
    let n = v.len();
    if n == 0 {
        return v;
    }
    let start = (round as usize) % n;
    let reverse = round % 2 == 1;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let idx = if reverse {
            (start + n - i) % n
        } else {
            (start + i) % n
        };
        out.push(v[idx]);
    }
    out
}

/// 依 workload 種類決定要啟動的 exe + args
fn workload_command(assets: &BenchmarkAssets, config: &BenchmarkConfig) -> (PathBuf, Vec<String>) {
    match config.workload {
        WorkloadKind::Vulkan => (
            config
                .workload_exe_path
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| assets.vulkan_workload.clone()),
            config.vulkan_args.clone(),
        ),
        WorkloadKind::D3D9 => (
            config
                .workload_exe_path
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| assets.d3d9_workload.clone()),
            // D3D9 workload 的 CLI 為 `<0|1>` 布林格式（false 字串會被解析成 true）
            vec![
                format!("--fullscreen={}", if config.fullscreen { 1 } else { 0 }),
                format!("--width={}", config.width),
                format!("--height={}", config.height),
                format!("--fps-cap={}", config.fps_cap),
                format!(
                    "--triple-buffer={}",
                    if config.triple_buffer { 1 } else { 0 }
                ),
            ],
        ),
    }
}

/// PresentMon 2.5.1 命令：以已 spawn 的 workload PID 精確篩選。每次使用獨立
/// ETW session；`--v1_metrics` 固定既有 MsBetweenPresents 統計語意。
///
/// 一律使用完整追蹤（不加任何 `--no_track_*`）。實測（bundled 2.5.1 + lava-triangle、
/// windowed、`-timed 3`）證明：
/// - 完整追蹤：CSV 正常建立（數 MB，exit 0，workload 存活）。
/// - `--no_track_display` 單獨：被 PresentMon 忽略並告警「display tracking is required
///   when GPU tracking is enabled」，CSV 仍建立（等於無效 fallback）。
/// - `--no_track_gpu --no_track_input --no_track_display` 三者同時停用：PresentMon
///   完全沒有 frame 來源，exit 0、workload 存活但 CSV 完全不建立（本次 regression 根因）。
///
/// 因此不存在「可恢復 capture 的 fallback 旗標」；所有 retry 都套用同一套完整追蹤命令。
fn presentmon_command(
    config: &BenchmarkConfig,
    pid: u32,
    csv: &Path,
    session_name: &str,
) -> Vec<String> {
    let args = vec![
        "--session_name".to_string(),
        session_name.to_string(),
        "--stop_existing_session".to_string(),
        "--no_console_stats".to_string(),
        "--process_id".to_string(),
        pid.to_string(),
        "--output_file".to_string(),
        csv.to_string_lossy().to_string(),
        "--timed".to_string(),
        config.sample_secs.to_string(),
        "--terminate_after_timed".to_string(),
        "--v1_metrics".to_string(),
        "--set_circular_buffer_size".to_string(),
        PRESENTMON_CIRCULAR_BUFFER_SIZE.to_string(),
    ];
    args
}

/// 執行一次 PresentMon capture 並驗證輸出。成功 → Ok(())；失敗 → Err(穩定錯誤代碼)。
///
/// 保證：
/// 1. 先刪除目標 CSV（stale 檔絕不能被當成當前輸出）。
/// 2. 等待 PresentMon 在 `sample_secs + CAPTURE_WAIT_MARGIN_S` 內自行退出
///    （`-timed` + `-terminate_after_timed`）；逾時 = 卡住 → 穩定代碼失敗，不靜默繼續。
/// 3. 任何路徑都終止 PresentMon 與 workload（確定性清理），再由 terminal 統一 reaping。
/// 4. 退出後驗證 CSV 存在且含有效 frametime；缺檔/空檔各自有穩定代碼。
/// 5. 每個路徑都把本次 capture 的診斷資料寫進 session 目錄
///    （`diag/capture-round-<r>-lp-<lp>.json`），成功與失敗都保留。
fn run_capture(
    ctx: &mut RunContext,
    round: u32,
    lp: u32,
    wl_pid: u32,
    csv: &Path,
    attempt: u32,
) -> Result<(), String> {
    let started_at = chrono::Local::now().to_rfc3339();
    let pm_session_name = format!("FrameAnchor-{}-{round}-{lp}-{attempt}", ctx.session_id);
    // PresentMon 啟動前確認 workload 是否還活著（第二次 capture 無 CSV 的關鍵判據）
    let wl_alive_before_pm = ctx.processes.is_alive(wl_pid);
    // 1) stale 輸出清除
    if let Err(e) = std::fs::remove_file(csv) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("capture 前清除舊 CSV 失敗 {}: {e}", csv.display());
        }
    }
    // 2) 啟動 PresentMon（以 -process_id 配合已 spawn 的 workload PID）
    let pm_pid = match ctx.processes.spawn(
        &ctx.assets.presentmon,
        &presentmon_command(&ctx.config, wl_pid, csv, &pm_session_name),
    ) {
        Ok(pid) => {
            ctx.owned_processes.push(pid);
            pid
        }
        Err(e) => {
            log::warn!("PresentMon 啟動失敗: {e}");
            // PresentMon 沒起來也要留診斷（workload 存活、CSV 狀態）
            let (csv_exists, csv_size) = csv_meta(csv);
            let diag = CaptureDiagnostics {
                round,
                lp,
                attempt,
                started_at: started_at.clone(),
                finished_at: Some(chrono::Local::now().to_rfc3339()),
                workload_pid: wl_pid,
                workload_alive_before_pm: wl_alive_before_pm,
                workload_alive_after_capture: ctx.processes.is_alive(wl_pid),
                csv_path: csv.to_string_lossy().to_string(),
                csv_exists,
                csv_size_bytes: csv_size,
                capture_filter_kind: "process_id".to_string(),
                capture_filter_value: wl_pid.to_string(),
                presentmon_session_name: pm_session_name,
                error: Some(codes::BENCHMARK_PRESENTMON_FAILED.to_string()),
                ..Default::default()
            };
            persist_capture_diagnostics(csv, round, lp, &diag);
            return Err(codes::BENCHMARK_PRESENTMON_FAILED.to_string());
        }
    };
    // 3) 等待 PresentMon 自停（可中斷）
    let wait = wait_capture(
        ctx,
        pm_pid,
        (ctx.config.sample_secs as u64 + CAPTURE_WAIT_MARGIN_S) * 1000,
    );
    let (wait_completed, wait_timed_out, wait_error) = match &wait {
        CaptureWaitOutcome::Exited => (true, false, None),
        CaptureWaitOutcome::TimedOut => (true, true, None),
        CaptureWaitOutcome::Cancelled => (true, false, None),
        CaptureWaitOutcome::Failed(e) => (false, false, Some(e.clone())),
    };
    // 4) 終止前先取 exit code / output tail（kill 會 reap 並丟掉 pipe 內容）
    let pm_exit_code = if wait_completed && !wait_timed_out {
        ctx.processes.exit_code(pm_pid)
    } else {
        None
    };
    let pm_out = ctx.processes.output_tail(pm_pid, DIAG_OUTPUT_TAIL_CAP);
    let wl_alive_after_capture = ctx.processes.is_alive(wl_pid);
    let wl_exit_code = if wl_alive_after_capture {
        None
    } else {
        ctx.processes.exit_code(wl_pid)
    };
    let wl_out = ctx.processes.output_tail(wl_pid, DIAG_OUTPUT_TAIL_CAP);
    // 5) 任何路徑都先終止再判結果（避免漏殺）
    let _ = ctx.processes.kill(pm_pid);
    let _ = ctx.processes.kill(wl_pid);
    ctx.owned_processes.clear();
    let etw_events_lost = pm_out
        .as_ref()
        .map(|o| stderr_has_etw_loss(&o.stderr))
        .unwrap_or(false);
    let result = match &wait {
        CaptureWaitOutcome::Failed(e) => {
            log::warn!("PresentMon wait 失敗: {e}");
            Err(codes::BENCHMARK_PRESENTMON_FAILED.to_string())
        }
        CaptureWaitOutcome::TimedOut => {
            // PresentMon 卡住：不靜默繼續，回穩定代碼；已驗證的部分結果由呼叫端保留
            log::error!(
                "PresentMon 逾時未退出（sample={}s + margin={}s），round-{round}-lp-{lp} 失敗",
                ctx.config.sample_secs,
                CAPTURE_WAIT_MARGIN_S
            );
            Err(codes::BENCHMARK_PRESENTMON_TIMEOUT.to_string())
        }
        CaptureWaitOutcome::Cancelled => {
            log::info!(
                "capture round-{round}-lp-{lp} 收到取消，提前終止 PresentMon 與 workload"
            );
            Err("cancelled".to_string())
        }
        CaptureWaitOutcome::Exited => {
            match validate_capture(csv) {
                Ok(()) => Ok(()),
                Err(_) if etw_events_lost => {
                    log::error!(
                        "PresentMon 回報 ETW events lost 且無有效 CSV（round-{round}-lp-{lp}）；\
                         擷取負載過高，中止 session"
                    );
                    Err(codes::BENCHMARK_CAPTURE_ETW_LOST.to_string())
                }
                Err(e) => Err(e),
            }
        }
    };
    // 6) 記錄診斷（成功與失敗都寫）
    let (csv_exists, csv_size) = csv_meta(csv);
    let diag = CaptureDiagnostics {
        round,
        lp,
        attempt,
        started_at,
        finished_at: Some(chrono::Local::now().to_rfc3339()),
        workload_pid: wl_pid,
        workload_alive_before_pm: wl_alive_before_pm,
        workload_alive_after_capture: wl_alive_after_capture,
        workload_exit_code: wl_exit_code,
        workload_stdout: wl_out
            .as_ref()
            .map(|o| o.stdout.clone())
            .unwrap_or_default(),
        workload_stderr: wl_out
            .as_ref()
            .map(|o| o.stderr.clone())
            .unwrap_or_default(),
        presentmon_pid: pm_pid,
        presentmon_exit_code: pm_exit_code,
        wait_completed,
        wait_timed_out,
        wait_error,
        presentmon_stdout: pm_out
            .as_ref()
            .map(|o| o.stdout.clone())
            .unwrap_or_default(),
        presentmon_stderr: pm_out
            .as_ref()
            .map(|o| o.stderr.clone())
            .unwrap_or_default(),
        csv_path: csv.to_string_lossy().to_string(),
        csv_exists,
        csv_size_bytes: csv_size,
        capture_filter_kind: "process_id".to_string(),
        capture_filter_value: wl_pid.to_string(),
        presentmon_session_name: pm_session_name,
        etw_events_lost,
        error: result.as_ref().err().cloned(),
    };
    persist_capture_diagnostics(csv, round, lp, &diag);
    result
}

/// 目標 CSV 的存在與大小（診斷用）
fn csv_meta(csv: &Path) -> (bool, u64) {
    match std::fs::metadata(csv) {
        Ok(m) => (true, m.len()),
        Err(_) => (false, 0),
    }
}

/// 把 capture 診斷寫到 `session_dir/diag/capture-round-<r>-lp-<lp>.json`。
/// best-effort：寫失敗只 log，不影響 capture 結果。
fn persist_capture_diagnostics(csv: &Path, round: u32, lp: u32, diag: &CaptureDiagnostics) {
    let Some(session_dir) = csv.parent() else {
        log::warn!("capture 診斷路徑無法解析（csv={}）", csv.display());
        return;
    };
    let diag_dir = session_dir.join("diag");
    if let Err(e) = std::fs::create_dir_all(&diag_dir) {
        log::warn!("建立診斷目錄失敗 {}: {e}", diag_dir.display());
        return;
    }
    let attempt = diag.attempt.max(1);
    let path = if attempt <= 1 {
        diag_dir.join(format!("capture-round-{round}-lp-{lp}.json"))
    } else {
        diag_dir.join(format!(
            "capture-round-{round}-lp-{lp}-attempt-{attempt}.json"
        ))
    };
    match serde_json::to_string_pretty(diag) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                log::warn!("寫入 capture 診斷失敗 {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("序列化 capture 診斷失敗: {e}"),
    }
}

/// 只有 MISSING / EMPTY 才觸發 retry；其他錯誤（PresentMon spawn/timeout/
/// invalid CSV/ETW lost/cancel/GPU restart）不盲目重試。
/// ETW lost 是擷取負載過高的結構性訊號，重試同一 LP 無意義，故刻意排除。
fn should_retry_capture(result: &Result<(), String>) -> bool {
    match result {
        Err(e) if e == codes::BENCHMARK_CAPTURE_MISSING => true,
        Err(e) if e == codes::BENCHMARK_CAPTURE_EMPTY => true,
        _ => false,
    }
}

/// 偵測 PresentMon stderr 是否回報 ETW events lost（circular buffer 溢位 /
/// 擷取負載過高）。關鍵字不分大小寫；需含 "lost" 且帶 "event"/"etw" 脈絡，
/// 涵蓋 "123 ETW events lost"、"Lost 123 ETW events"、"ETW events were lost"
/// 等變體。排除否定/零值表述（"no/not/0/without … lost"、"lost 0 …"），
/// 避免 "0 ETW events lost"、"no events were lost" 誤判為溢失。
fn stderr_has_etw_loss(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    if !s.contains("lost") || !(s.contains("event") || s.contains("etw")) {
        return false;
    }
    // 逐子句判斷：否定標記只在該子句內生效，避免前一子句的 "no/0"（或 "error code 0"
    // 之類無關 "0"）跨子句抑制真正 loss。任一子句含非零、非否定的 lost 即回報。
    s.split(['.', ';', ':', ',', '!', '?', '\n', '\r'])
        .any(clause_has_etw_loss)
}

/// 單一子句內是否回報非零、非否定的 ETW events lost。
fn clause_has_etw_loss(clause: &str) -> bool {
    if !clause.contains("lost") || !(clause.contains("event") || clause.contains("etw")) {
        return false;
    }
    const NEG: [&str; 6] = ["0", "no", "not", "none", "zero", "without"];
    let tokens: Vec<&str> = clause
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    for (i, t) in tokens.iter().enumerate() {
        if *t != "lost" {
            continue;
        }
        // 往前最多看 4 個 token：否定前綴（"no/0 ETW events lost"）即排除此 "lost"
        let negated = tokens[..i].iter().rev().take(4).any(|w| NEG.contains(w));
        if negated {
            continue;
        }
        // 後方緊接 "0"（"lost 0 events"）也是零遺失，排除
        if matches!(tokens.get(i + 1), Some(next) if *next == "0") {
            continue;
        }
        return true;
    }
    false
}

/// 驗證 capture 輸出：檔案存在且能解析出至少一個有效 frametime。
/// 缺檔 → BENCHMARK_CAPTURE_MISSING；空/無資料 → BENCHMARK_CAPTURE_EMPTY。
fn validate_capture(csv: &Path) -> Result<(), String> {
    let text = match std::fs::read_to_string(csv) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("capture 無輸出檔案 {}: {e}", csv.display());
            return Err(codes::BENCHMARK_CAPTURE_MISSING.to_string());
        }
    };
    match parse_presentmon_csv(&text) {
        Ok(frames) if !frames.is_empty() => Ok(()),
        Ok(_) | Err(_) => {
            log::warn!("capture 無有效 frametime 資料: {}", csv.display());
            Err(codes::BENCHMARK_CAPTURE_EMPTY.to_string())
        }
    }
}

/// 預估剩餘秒數（per-test：sample + warmup + 啟動 5s + 重啟 ~14s + 緩衝）
fn eta_secs(config: &BenchmarkConfig, total_tests: u32, done: u32) -> Option<u64> {
    if total_tests == 0 {
        return None;
    }
    let per_test = config.sample_secs as u64 + config.warm_up_secs as u64 + 19;
    Some((total_tests.saturating_sub(done)) as u64 * per_test)
}

/// 合併各 round CSV 並計算每 LP 指標。
/// 任一 LP 的 CSV 缺失/空/無效 → 回傳 (已算出的部分結果, Some(錯誤碼))。
fn compute_session_results(
    round_csvs: &HashMap<u32, HashMap<u32, PathBuf>>,
) -> (Vec<LpResult>, Option<String>) {
    let mut lps: Vec<u32> = round_csvs.keys().copied().collect();
    lps.sort_unstable();
    let mut out = Vec::new();
    for lp in lps {
        match compute_lp_all_rounds(lp, &round_csvs[&lp]) {
            Ok(r) => out.push(r),
            Err(e) => return (out, Some(e)),
        }
    }
    (out, None)
}

fn compute_lp_all_rounds(lp: u32, rounds: &HashMap<u32, PathBuf>) -> Result<LpResult, String> {
    let mut per_round: Vec<Vec<f64>> = Vec::new();
    let mut round_nums: Vec<u32> = rounds.keys().copied().collect();
    round_nums.sort_unstable();
    for round in round_nums {
        let csv = &rounds[&round];
        let frames = read_csv_frames(csv)?;
        per_round.push(frames);
    }
    let merged = merge_rounds(&per_round);
    compute_lp_result(lp, &merged).map_err(|e| {
        log::warn!("LP {lp} 統計失敗: {e}");
        codes::BENCHMARK_CSV_INVALID.to_string()
    })
}

/// 讀取單一 CSV 並解析 frametime（不可得 → Err；供合併與逐 round 共用）
fn read_csv_frames(csv: &Path) -> Result<Vec<f64>, String> {
    let text = match std::fs::read_to_string(csv) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("CSV 讀取失敗 {}: {e}", csv.display());
            return Err(codes::BENCHMARK_CSV_INVALID.to_string());
        }
    };
    parse_presentmon_csv(&text).map_err(|e| {
        log::warn!("CSV 解析失敗 {}: {e}", csv.display());
        codes::BENCHMARK_CSV_INVALID.to_string()
    })
}

/// 單一 (round, lp) 的 LpResult（供逐 round 勝者計算；CSV 不可讀 → None）
fn compute_lp_single_round(lp: u32, csv: &Path) -> Option<LpResult> {
    read_csv_frames(csv)
        .ok()
        .and_then(|frames| compute_lp_result(lp, &frames).ok())
}

/// 改善百分比：`(candidate - runner_up) / runner_up * 100`。
/// 亞軍不可得、非有限、或 `runner_up <= 0` → None（視為未達門檻）。
fn improvement_pct(candidate: Option<f64>, runner_up: Option<f64>) -> Option<f64> {
    let c = candidate?;
    let r = runner_up?;
    if !c.is_finite() || !r.is_finite() || r <= 0.0 {
        return None;
    }
    let pct = (c - r) / r * 100.0;
    if pct.is_finite() {
        Some(pct)
    } else {
        None
    }
}

/// 複合分數優勢門檻（%）：穩健候選的跨 round 中位分數須高於亞軍至少此值才算 Passed。
pub const COMPOSITE_ADVANTAGE_MIN_PCT: f64 = 0.5;
/// 護欄：候選 Avg FPS / 1% low 相較亞軍最多允許落後（%，負值 = 允許小幅落後）。
pub const GUARDRAIL_MAX_DEFICIT_PCT: f64 = -0.5;
/// 護欄：候選 spike rate 相較亞軍最多允許超出（絕對百分點）。
pub const SPIKE_GUARD_PP: f64 = 5.0;

/// Passed 所需勝場數 = ceil(60% × evaluated_rounds)。
pub fn required_wins(evaluated_rounds: u32) -> u32 {
    ((evaluated_rounds as f64) * 0.6).ceil() as u32
}

/// 由 per-round LpResult map 取某指標的逐 round 中位數（缺/非有限 → 略過）。
fn median_of_metric(
    per_round: &HashMap<u32, LpResult>,
    pick: fn(&LpResult) -> Option<f64>,
) -> Option<f64> {
    let vals: Vec<f64> = per_round.values().filter_map(pick).collect();
    if vals.is_empty() {
        None
    } else {
        Some(median(&vals))
    }
}

/// 逐 LP、逐 round 的 competitive-eligible 單 round 結果（僅納入 rounds 0..round_count）。
fn build_per_round(
    round_csvs: &HashMap<u32, HashMap<u32, PathBuf>>,
    round_count: u32,
) -> HashMap<u32, HashMap<u32, LpResult>> {
    let mut per_round: HashMap<u32, HashMap<u32, LpResult>> = HashMap::new();
    for (&lp, rounds) in round_csvs {
        for round in 0..round_count {
            if let Some(csv) = rounds.get(&round) {
                if let Some(r) = compute_lp_single_round(lp, csv) {
                    if is_competitive_eligible(&r) {
                        per_round.entry(lp).or_default().insert(round, r);
                    }
                }
            }
        }
    }
    per_round
}

/// 由篩選階段（rounds 0..screening_rounds）選出最多兩個 finalists。
/// 僅納入所有篩選 round 皆完整（competitive-eligible）的 LP，以既有穩健排名
/// （跨 round 中位數分數 → worst-round → 較小 LP）排序取前 2。少於兩個完整候選
/// → 回傳空（呼叫端跳過確認）。
fn select_finalists(
    round_csvs: &HashMap<u32, HashMap<u32, PathBuf>>,
    screening_rounds: u32,
) -> Vec<u32> {
    let per_round = build_per_round(round_csvs, screening_rounds);
    let mut eligible: Vec<u32> = per_round
        .iter()
        .filter(|(_, rounds)| rounds.len() as u32 == screening_rounds)
        .map(|(lp, _)| *lp)
        .collect();
    eligible.sort_unstable();
    if eligible.len() < MAX_FINALISTS {
        return Vec::new();
    }
    let mut per_lp_scores: HashMap<u32, Vec<f64>> = HashMap::new();
    for round in 0..screening_rounds {
        let round_results: Vec<LpResult> = eligible
            .iter()
            .filter_map(|lp| per_round.get(lp).and_then(|m| m.get(&round)).cloned())
            .collect();
        let med = round_medians(&round_results);
        for lp in &eligible {
            if let Some(r) = per_round.get(lp).and_then(|m| m.get(&round)) {
                if let Some(s) = competitive_score(r, &med) {
                    per_lp_scores.entry(*lp).or_default().push(s);
                }
            }
        }
    }
    let ranked = robust_candidates(
        &per_lp_scores
            .iter()
            .map(|(lp, s)| (*lp, s.clone()))
            .collect::<Vec<_>>(),
    );
    ranked.into_iter().take(MAX_FINALISTS).map(|c| c.lp).collect()
}

/// 計算可靠性摘要。`results` 為聚合（合併各 round）後每 LP 的指標（僅供舊版
/// 改善欄位）；`round_csvs` 為 LP → round → CSV，供逐 round 競爭分數與勝者；
/// `repetitions` 為本次 session 的預期 round 數。
///
/// 穩健候選 = 跨 round 競爭分數中位數最高者（平手取 worst-round 較高，再取較小
/// LP）；亞軍依同規則。勝者 = 該 round 合格 LP 中分數最高者（平手取較小 LP）。
///
/// Passed 需同時滿足：
/// 1. 至少 2 個 LP 各自在所有預期 round 皆有完整（competitive-eligible）結果，
///    且預期 round ≥5。
/// 2. 候選勝場 ≥ceil(60% × 預期 round)。
/// 3. 複合分數優勢 ≥0.5%。
/// 4. 護欄：候選 Avg FPS、1% low 相較亞軍（跨 round 中位數）各 ≥-0.5%，
///    spike rate 未超出亞軍 5 個百分點。
///
/// Equivalent = 勝場與護欄皆達標但複合優勢 <0.5%（證據完整穩定、差異過小）。
/// 其餘（round 不足、LP 不足、勝場不足、護欄倒退、證據不完整/無效）皆 Inconclusive。
fn compute_reliability(
    round_csvs: &HashMap<u32, HashMap<u32, PathBuf>>,
    results: &[LpResult],
    repetitions: u32,
) -> ReliabilitySummary {
    let expected = repetitions.max(1);

    // 逐 LP、逐 round 的單 round competitive-eligible 結果。
    let per_round = build_per_round(round_csvs, expected);

    // 合格 LP：所有預期 round 皆完整。
    let mut eligible_lps: Vec<u32> = per_round
        .iter()
        .filter(|(_, rounds)| rounds.len() as u32 == expected)
        .map(|(lp, _)| *lp)
        .collect();
    eligible_lps.sort_unstable();

    // 逐 round 競爭分數、逐 round 勝者。
    let mut per_lp_scores: HashMap<u32, Vec<f64>> = HashMap::new();
    let mut round_winners: Vec<Option<u32>> = Vec::with_capacity(expected as usize);
    for round in 0..expected {
        let round_results: Vec<LpResult> = eligible_lps
            .iter()
            .filter_map(|lp| per_round.get(lp).and_then(|m| m.get(&round)).cloned())
            .collect();
        let med = round_medians(&round_results);
        for lp in &eligible_lps {
            if let Some(r) = per_round.get(lp).and_then(|m| m.get(&round)) {
                if let Some(s) = competitive_score(r, &med) {
                    per_lp_scores.entry(*lp).or_default().push(s);
                }
            }
        }
        let winner = eligible_lps
            .iter()
            .filter_map(|lp| {
                per_round
                    .get(lp)
                    .and_then(|m| m.get(&round))
                    .and_then(|r| competitive_score(r, &med).map(|s| (*lp, s)))
            })
            .max_by(|a, b| {
                a.1.partial_cmp(&b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.0.cmp(&a.0))
            })
            .map(|(lp, _)| lp);
        round_winners.push(winner);
    }

    // 穩健候選/亞軍（跨 round 中位數分數）。
    let ranked = robust_candidates(
        &per_lp_scores
            .iter()
            .map(|(lp, s)| (*lp, s.clone()))
            .collect::<Vec<_>>(),
    );
    let candidate = ranked.first().map(|c| c.lp);
    let runner_up = ranked.get(1).map(|c| c.lp);
    let candidate_median = ranked.first().map(|c| c.median_score);
    let runner_median = ranked.get(1).map(|c| c.median_score);

    let candidate_wins = round_winners.iter().filter(|w| **w == candidate).count() as u32;
    let required = required_wins(expected);

    // 逐 round guardrail 指標（候選 vs 亞軍，跨 round 中位數）。
    let candidate_map = candidate
        .and_then(|c| per_round.get(&c).cloned())
        .unwrap_or_default();
    let runner_map = runner_up
        .and_then(|ru| per_round.get(&ru).cloned())
        .unwrap_or_default();
    let candidate_avg = median_of_metric(&candidate_map, |r| r.avg_fps);
    let runner_avg = median_of_metric(&runner_map, |r| r.avg_fps);
    let candidate_p1 = median_of_metric(&candidate_map, |r| r.p1_low);
    let runner_p1 = median_of_metric(&runner_map, |r| r.p1_low);
    let candidate_spike = median_of_metric(&candidate_map, |r| r.spike_rate_pct);
    let runner_spike = median_of_metric(&runner_map, |r| r.spike_rate_pct);

    let avg_fps_advantage_pct = improvement_pct(candidate_avg, runner_avg);
    let p1_low_advantage_pct = improvement_pct(candidate_p1, runner_p1);
    let spike_rate_delta_pp = match (candidate_spike, runner_spike) {
        (Some(c), Some(r)) => Some(c - r),
        _ => None,
    };
    let composite_advantage_pct = improvement_pct(candidate_median, runner_median);

    // 舊版改善欄位（聚合結果）。
    let candidate_res = candidate.and_then(|c| results.iter().find(|res| res.lp == c));
    let runner_up_res = runner_up.and_then(|ru| results.iter().find(|res| res.lp == ru));
    let avg_fps_pct = improvement_pct(
        candidate_res.and_then(|r| r.avg_fps),
        runner_up_res.and_then(|r| r.avg_fps),
    );
    let p1_low_pct = improvement_pct(
        candidate_res.and_then(|r| r.p1_low),
        runner_up_res.and_then(|r| r.p1_low),
    );
    let p01_low_pct = improvement_pct(
        candidate_res.and_then(|r| r.p01_low),
        runner_up_res.and_then(|r| r.p01_low),
    );

    let complete =
        eligible_lps.len() >= 2 && expected >= 5 && candidate.is_some() && runner_up.is_some();
    let wins_ok = candidate_wins >= required;
    let guardrails_ok = avg_fps_advantage_pct.is_some_and(|v| v >= GUARDRAIL_MAX_DEFICIT_PCT)
        && p1_low_advantage_pct.is_some_and(|v| v >= GUARDRAIL_MAX_DEFICIT_PCT)
        && spike_rate_delta_pp.is_some_and(|d| d <= SPIKE_GUARD_PP);

    let status = if complete {
        if wins_ok && guardrails_ok {
            if composite_advantage_pct.is_some_and(|v| v >= COMPOSITE_ADVANTAGE_MIN_PCT) {
                ReliabilityStatus::Passed
            } else {
                ReliabilityStatus::Equivalent
            }
        } else {
            ReliabilityStatus::Inconclusive
        }
    } else {
        ReliabilityStatus::Inconclusive
    };

    ReliabilitySummary {
        status,
        per_round_winners: round_winners,
        candidate_lp: candidate,
        runner_up_lp: runner_up,
        candidate_wins,
        avg_fps_pct,
        p1_low_pct,
        p01_low_pct,
        evaluated_rounds: expected,
        required_wins: required,
        composite_advantage_pct,
        avg_fps_advantage_pct,
        p1_low_advantage_pct,
        spike_rate_delta_pp,
    }
}

/// 前置失敗（尚未寫入任何策略）：直接以 Failed 收尾，不需還原
fn abort(ctx: &mut RunContext, error: String) -> RunResult {
    let detail = SessionDetail {
        summary: SessionSummary {
            id: ctx.session_id.clone(),
            status: SessionStatus::Failed,
            started_at: chrono::Local::now().to_rfc3339(),
            finished_at: Some(chrono::Local::now().to_rfc3339()),
            gpu_name: String::new(),
            gpu_instance_id: ctx.config.gpu_instance_id.clone().unwrap_or_default(),
            cpu_fingerprint: cpu_fingerprint_with(&ctx.topo, &ctx.cpu_identity),
            best_lp: None,
            reliability: ReliabilitySummary::default(),
            severe_lps: Vec::new(),
            sample_count: 0,
            total_bytes: 0,
            config: ctx.config.clone(),
            error: Some(error.clone()),
        },
        results: Vec::new(),
        samples: Vec::new(),
    };
    let _ = storage::save_session_at(&ctx.storage_root, &detail);
    emit(
        ctx,
        &detail,
        "finalizing",
        None,
        None,
        100,
        None,
        Some(error.clone()),
    );
    RunResult {
        status: SessionStatus::Failed,
        detail,
        error: Some(error),
        best_lp: None,
        severe_lps: Vec::new(),
        recommended_cores: Vec::new(),
        recovery_required: false,
    }
}

/// 終結路徑（成功/取消/失敗共用）：
/// 停止 owned 子程序 → 還原原始策略並重啟 GPU → 驗證成功才清日誌 → 原子寫入。
fn terminal(
    ctx: &mut RunContext,
    mut detail: SessionDetail,
    status: SessionStatus,
    error: Option<String>,
    best: Option<u32>,
    severe: Vec<u32>,
    recommended: Vec<u32>,
) -> RunResult {
    // 1) 停止所有 owned 子程序
    for pid in ctx.owned_processes.drain(..) {
        let _ = ctx.processes.kill(pid);
    }
    // 2) 還原原始策略（若有 baseline）+ 重啟 GPU
    let mut restored = true;
    if let Some(baseline) = &ctx.baseline {
        if let Err(e) = restore_snapshot(ctx.backend.as_ref(), ctx.sleeper.as_ref(), baseline) {
            restored = false;
            log::error!("基準測試終結還原失敗: {e}；保留還原日誌供啟動重試");
        }
    }
    // 3) 還原驗證成功才清除日誌（Task 1 崩潰還原保證不被削弱）
    if restored {
        let _ = recovery::clear_at(&ctx.journal_path);
    }
    // 4) 組最終 session 並原子寫入
    detail.summary.status = status;
    detail.summary.finished_at = Some(chrono::Local::now().to_rfc3339());
    detail.summary.best_lp = best;
    detail.summary.severe_lps = severe.clone();
    detail.summary.error = error.clone();
    let _ = storage::save_session_at(&ctx.storage_root, &detail);
    // 5) 最終 progress
    emit(
        ctx,
        &detail,
        "finalizing",
        None,
        None,
        100,
        None,
        error.clone(),
    );
    RunResult {
        status,
        detail,
        error,
        best_lp: best,
        severe_lps: severe,
        recommended_cores: recommended,
        recovery_required: !restored,
    }
}

fn require_journal(path: &Path) -> Result<recovery::RecoveryJournal, String> {
    recovery::load_from(path)?.ok_or_else(|| codes::GPU_APPLY_FAILED.to_string())
}

#[allow(clippy::too_many_arguments)]
fn emit(
    ctx: &mut RunContext,
    detail: &SessionDetail,
    stage: &str,
    round: Option<u32>,
    lp: Option<u32>,
    percentage: u32,
    eta: Option<u64>,
    error: Option<String>,
) {
    let progress = BenchmarkProgress {
        session_id: detail.summary.id.clone(),
        stage: stage.to_string(),
        round,
        lp,
        percentage,
        eta_secs: eta,
        error,
    };
    (ctx.on_progress)(&progress);
}

// ── 測試用 fake ─────────────────────────────────────────────────────────

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Mutex;

    /// 從 `round-<r>-lp-<lp>.csv` 檔名取出 (round, LP)
    fn round_lp_from_csv_path(path: &str) -> Option<(u32, u32)> {
        let stem = std::path::Path::new(path).file_stem()?.to_str()?;
        let (head, lp_str) = stem.rsplit_once("-lp-")?;
        let lp: u32 = lp_str.parse().ok()?;
        let round_str = head.rsplit_once("round-")?.1;
        let round: u32 = round_str.parse().ok()?;
        Some((round, lp))
    }

    /// 從 `round-<r>-lp-<lp>.csv` 檔名取出 LP
    fn lp_from_csv_path(path: &str) -> Option<u32> {
        let stem = std::path::Path::new(path).file_stem()?.to_str()?;
        let lp_part = stem.rsplit_once("-lp-")?.1;
        lp_part.parse().ok()
    }

    /// 記憶體模擬的 ProcessRunner：記錄 spawn/kill，
    /// 並在「PresentMon」spawn 時寫出指定的 CSV（依 program 名判斷）。
    pub struct FakeProcessRunner {
        /// spawn 紀錄：(exe name, pid, args)
        pub spawned: Mutex<Vec<(String, u32, Vec<String>)>>,
        pub killed: Mutex<Vec<u32>>,
        /// PresentMon 的 CSV 內容（寫到 -output_file）
        pub presentmon_csv: Mutex<String>,
        /// PresentMon spawn 時是否失敗（對所有 LP）
        pub fail_presentmon: AtomicBool,
        /// 指定哪些 LP 的 PresentMon spawn 失敗（依 -output_file 檔名內 -lp-<n> 判斷）
        pub fail_presentmon_lps: Mutex<std::collections::HashSet<u32>>,
        /// 指定哪些 round 的 PresentMon spawn 失敗（依 -output_file 檔名內 round-<n> 判斷）
        pub fail_presentmon_rounds: Mutex<std::collections::HashSet<u32>>,
        /// workload spawn 時是否失敗
        pub fail_workload: AtomicBool,
        /// PresentMon 的 wait_exit 是否回傳「卡住」（逾時）
        pub presentmon_timeout: AtomicBool,
        /// PresentMon spawn 時是否真的寫出 CSV（false = 不寫 → 缺檔）
        pub presentmon_write_csv: AtomicBool,
        /// 模擬 PresentMon 回報 ETW events lost：spawn 時不寫 CSV，且 stderr
        /// 含 "Lost ... ETW events"（擷取負載過高 → 應觸發不可重試的 fail-fast）
        pub presentmon_etw_loss: AtomicBool,
        /// pid → 程式名（wait_exit 需要判斷 pid 是不是 PresentMon）
        pid_name: Mutex<HashMap<u32, String>>,
        next_pid: AtomicU32,
        pub alive: Mutex<HashMap<u32, bool>>,
        /// 已退出程序的 exit code（spawn 時預設 0；診斷測試用）
        exit_codes: Mutex<HashMap<u32, i32>>,
        /// spawn 時記錄的 bounded output tail（診斷測試用）
        outputs: Mutex<HashMap<u32, ProcessOutput>>,
        /// 特定 LP 的第一個 capture attempt 行為：Missing（不寫 CSV）
        pub first_attempt_missing: Mutex<std::collections::HashSet<u32>>,
        /// 特定 LP 的第一個 capture attempt 行為：Empty（僅 header）
        pub first_attempt_empty: Mutex<std::collections::HashSet<u32>>,
        /// 特定 LP 的所有 retry attempt 也 Missing（全部嘗試都失敗）
        pub second_attempt_also_missing: Mutex<std::collections::HashSet<u32>>,
        /// per-(round, lp) → PresentMon spawn 次數（判斷第幾次 attempt）
        capture_call_count: Mutex<std::collections::HashMap<(u32, u32), u32>>,
    }

    impl FakeProcessRunner {
        pub fn new() -> Self {
            Self {
                spawned: Mutex::new(Vec::new()),
                killed: Mutex::new(Vec::new()),
                presentmon_csv: Mutex::new(String::new()),
                fail_presentmon: AtomicBool::new(false),
                fail_presentmon_lps: Mutex::new(std::collections::HashSet::new()),
                fail_presentmon_rounds: Mutex::new(std::collections::HashSet::new()),
                fail_workload: AtomicBool::new(false),
                presentmon_timeout: AtomicBool::new(false),
                presentmon_write_csv: AtomicBool::new(true),
                presentmon_etw_loss: AtomicBool::new(false),
                pid_name: Mutex::new(HashMap::new()),
                next_pid: AtomicU32::new(1000),
                alive: Mutex::new(HashMap::new()),
                exit_codes: Mutex::new(HashMap::new()),
                outputs: Mutex::new(HashMap::new()),
                first_attempt_missing: Mutex::new(std::collections::HashSet::new()),
                first_attempt_empty: Mutex::new(std::collections::HashSet::new()),
                second_attempt_also_missing: Mutex::new(std::collections::HashSet::new()),
                capture_call_count: Mutex::new(std::collections::HashMap::new()),
            }
        }

        pub fn spawn_log(&self) -> Vec<(String, u32, Vec<String>)> {
            self.spawned.lock().unwrap().clone()
        }
        pub fn killed_log(&self) -> Vec<u32> {
            self.killed.lock().unwrap().clone()
        }
    }

    impl FakeProcessRunner {
        pub fn fail_lp(&self, lp: u32) {
            self.fail_presentmon_lps.lock().unwrap().insert(lp);
        }
    }

    impl ProcessRunner for FakeProcessRunner {
        fn spawn(&self, exe: &Path, args: &[String]) -> Result<u32, String> {
            let name = exe
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let pid = self.next_pid.fetch_add(1, Ordering::SeqCst);
            self.spawned
                .lock()
                .unwrap()
                .push((name.clone(), pid, args.to_vec()));
            let out = args
                .iter()
                .position(|a| a == "--output_file")
                .and_then(|i| args.get(i + 1))
                .cloned();
            if name.contains("PresentMon") {
                // 依 -output_file 檔名判 (round, lp)，若在失敗清單則 spawn 失敗
                let lp = out
                    .as_deref()
                    .and_then(lp_from_csv_path)
                    .unwrap_or(u32::MAX);
                let round = out
                    .as_deref()
                    .and_then(round_lp_from_csv_path)
                    .map(|(r, _)| r);
                let round_fail = round
                    .map(|r| self.fail_presentmon_rounds.lock().unwrap().contains(&r))
                    .unwrap_or(false);
                if self.fail_presentmon.load(Ordering::SeqCst)
                    || self.fail_presentmon_lps.lock().unwrap().contains(&lp)
                    || round_fail
                {
                    return Err("fake: presentmon fail".into());
                }
            }
            if !name.contains("PresentMon") && self.fail_workload.load(Ordering::SeqCst) {
                return Err("fake: workload fail".into());
            }
            self.alive.lock().unwrap().insert(pid, true);
            self.pid_name.lock().unwrap().insert(pid, name.clone());
            self.exit_codes.lock().unwrap().insert(pid, 0);
            // 診斷測試用：依程式名記錄可預期的 output tail
            let etw_loss = self.presentmon_etw_loss.load(Ordering::SeqCst);
            let output = if name.contains("PresentMon") {
                if etw_loss {
                    ProcessOutput {
                        stdout: "fake-presentmon-stdout".into(),
                        stderr: "Lost 9000000 ETW events".into(),
                    }
                } else {
                    ProcessOutput {
                        stdout: "fake-presentmon-stdout".into(),
                        stderr: "fake-presentmon-stderr".into(),
                    }
                }
            } else {
                ProcessOutput {
                    stdout: "fake-workload-stdout".into(),
                    stderr: "fake-workload-stderr".into(),
                }
            };
            self.outputs.lock().unwrap().insert(pid, output);
            if name.contains("PresentMon") {
                // 模擬 PresentMon：支援 per-attempt 行為（first_attempt_missing/empty/
                // second_attempt_also_missing）。先決定要寫什麼內容，再統一寫入。
                let rl = out.as_deref().and_then(round_lp_from_csv_path);
                let (attempt, lp_opt) = if let Some((round, lp)) = rl {
                    let mut counts = self.capture_call_count.lock().unwrap();
                    let entry = counts.entry((round, lp)).or_insert(0);
                    *entry += 1;
                    (*entry, Some(lp))
                } else {
                    (1, None)
                };
                let write_full = self.presentmon_write_csv.load(Ordering::SeqCst);
                let csv_content: Option<String> = match lp_opt {
                    _ if etw_loss => None,
                    Some(lp)
                        if attempt == 1
                            && self.first_attempt_missing.lock().unwrap().contains(&lp) =>
                    {
                        None
                    }
                    Some(lp)
                        if attempt == 1
                            && self.first_attempt_empty.lock().unwrap().contains(&lp) =>
                    {
                        Some("Application,ProcessID,msBetweenPresents\n".to_string())
                    }
                    Some(lp)
                        if attempt >= 2
                            && self
                                .second_attempt_also_missing
                                .lock()
                                .unwrap()
                                .contains(&lp) =>
                    {
                        None
                    }
                    _ if !write_full => None,
                    _ => Some(self.presentmon_csv.lock().unwrap().clone()),
                };
                if let (Some(path), Some(content)) = (out, csv_content) {
                    let _ = std::fs::write(&path, content);
                }
            }
            Ok(pid)
        }

        fn is_alive(&self, pid: u32) -> bool {
            self.alive
                .lock()
                .unwrap()
                .get(&pid)
                .copied()
                .unwrap_or(false)
        }

        fn kill(&self, pid: u32) -> Result<(), String> {
            self.killed.lock().unwrap().push(pid);
            self.alive.lock().unwrap().insert(pid, false);
            Ok(())
        }

        fn wait_exit(&self, pid: u32, _timeout_ms: u64) -> Result<bool, String> {
            // 模擬 PresentMon 卡住（逾時）：回傳「未退出」
            let is_presentmon = self
                .pid_name
                .lock()
                .unwrap()
                .get(&pid)
                .map(|n| n.contains("PresentMon"))
                .unwrap_or(false);
            if is_presentmon && self.presentmon_timeout.load(Ordering::SeqCst) {
                return Ok(false);
            }
            Ok(true) // fake 立即退出
        }

        fn exit_code(&self, pid: u32) -> Option<i32> {
            self.exit_codes.lock().unwrap().get(&pid).copied()
        }

        fn output_tail(&self, pid: u32, _max_chars: usize) -> Option<ProcessOutput> {
            self.outputs.lock().unwrap().get(&pid).cloned()
        }
    }

    /// 可程式化取消的 fake
    pub struct FakeCancel {
        pub cancelled: AtomicBool,
    }

    impl FakeCancel {
        pub fn new() -> Self {
            Self {
                cancelled: AtomicBool::new(false),
            }
        }
        pub fn set(&self, v: bool) {
            self.cancelled.store(v, Ordering::SeqCst);
        }
    }

    impl CancelSignal for FakeCancel {
        fn is_cancelled(&self) -> bool {
            self.cancelled.load(Ordering::SeqCst)
        }
    }

    /// 累計 sleep 達 `trigger_after_ms` 毫秒時觸發取消（測試「執行中取消」用）。
    /// 累計毫秒與 runner 的 sleep_interruptible / wait_capture 同源，因此
    /// 可精準落在 startup/warmup/capture wait 的某一時點。
    pub struct CancelAfterSleeper {
        pub cancel: Arc<FakeCancel>,
        pub trigger_after_ms: u64,
        elapsed: Mutex<u64>,
    }

    impl CancelAfterSleeper {
        pub fn new(cancel: Arc<FakeCancel>, trigger_after_ms: u64) -> Self {
            Self {
                cancel,
                trigger_after_ms,
                elapsed: Mutex::new(0),
            }
        }

        /// 目前累計 sleep 毫秒（測試斷言「取消是否提前中斷」用）
        pub fn elapsed_ms(&self) -> u64 {
            *self.elapsed.lock().unwrap()
        }
    }

    impl Sleep for CancelAfterSleeper {
        fn sleep(&self, ms: u64) {
            let mut e = self.elapsed.lock().unwrap();
            *e += ms;
            if *e >= self.trigger_after_ms {
                self.cancel.set(true);
            }
        }
    }

    /// 記錄 resize 呼叫的 fake window（不碰真實 Win32）
    pub struct FakeWindow {
        pub calls: Mutex<Vec<(u32, u32, u32)>>,
        /// find_and_resize 回傳值：Some(Ok(true))=找到、Some(Ok(false))=未找到、
        /// Some(Err)=resize 失敗、None=預設 Ok(true)
        pub result: Mutex<Option<Result<bool, String>>>,
        /// guard_close 呼叫紀錄（pid）
        pub guard_calls: Mutex<Vec<u32>>,
        /// guard_close 回傳值：Some(Ok(true))=找到、Some(Ok(false))=未找到、
        /// Some(Err)=guard 失敗、None=預設 Ok(true)
        pub guard_result: Mutex<Option<Result<bool, String>>>,
    }

    impl FakeWindow {
        pub fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                result: Mutex::new(None),
                guard_calls: Mutex::new(Vec::new()),
                guard_result: Mutex::new(None),
            }
        }
        pub fn calls_log(&self) -> Vec<(u32, u32, u32)> {
            self.calls.lock().unwrap().clone()
        }
        pub fn guard_calls_log(&self) -> Vec<u32> {
            self.guard_calls.lock().unwrap().clone()
        }
    }

    impl WorkloadWindow for FakeWindow {
        fn find_and_resize(&self, pid: u32, width: u32, height: u32) -> Result<bool, String> {
            self.calls.lock().unwrap().push((pid, width, height));
            match self.result.lock().unwrap().clone() {
                Some(r) => r,
                None => Ok(true),
            }
        }

        fn guard_close(&self, pid: u32) -> Result<bool, String> {
            self.guard_calls.lock().unwrap().push(pid);
            match self.guard_result.lock().unwrap().clone() {
                Some(r) => r,
                None => Ok(true),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{CancelAfterSleeper, FakeCancel, FakeProcessRunner};
    use super::*;
    use crate::benchmark::assets::{self, BenchmarkAssets};
    use crate::gpu::fake::FakeBackend;
    use crate::gpu::{AffinityPolicy, GpuDevice, NoopSleeper, RegistryValueSnapshot};
    use crate::topology::{build_topology, Topology};
    use std::sync::atomic::Ordering;
    use uuid::Uuid;

    const GPU_A: &str = r"PCI\VEN_FAKE&DEV_1";

    fn fixed_identity() -> CpuIdentity {
        CpuIdentity {
            architecture: 9,
            family: 6,
            model: 183,
            stepping: 1,
        }
    }

    fn topo() -> Topology {
        build_topology((0..8u32).map(|c| (vec![c], 0, false)).collect())
    }

    fn device(instance: &str) -> GpuDevice {
        GpuDevice {
            instance_id: instance.to_string(),
            friendly_name: format!("GPU {instance}"),
        }
    }

    /// 建立可通過 assets::verify 的暫存資源
    fn make_assets(dir: &std::path::Path) -> BenchmarkAssets {
        std::fs::create_dir_all(dir).unwrap();
        let pm = dir.join(assets::PRESENTMON_FILE);
        std::fs::write(&pm, b"fake-presentmon").unwrap();
        let vk = dir.join(assets::VULKAN_WORKLOAD_FILE);
        std::fs::write(&vk, b"fake-lava").unwrap();
        std::fs::write(dir.join(assets::D3D9_WORKLOAD_FILE), b"fake-d3d9").unwrap();
        let manifest = format!(
            "{}  {}\n{}  {}\n",
            assets::file_sha256(&pm).unwrap(),
            assets::PRESENTMON_FILE,
            assets::file_sha256(&vk).unwrap(),
            assets::VULKAN_WORKLOAD_FILE,
        );
        std::fs::write(dir.join(assets::MANIFEST_FILE), manifest).unwrap();
        assets::load(dir)
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "frameanchor_runner_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn base_config() -> BenchmarkConfig {
        BenchmarkConfig {
            gpu_instance_id: Some(GPU_A.to_string()),
            workload: WorkloadKind::Vulkan,
            sample_secs: 3,
            warm_up_secs: 1,
            repetitions: 5,
            // 候選 LP 不得含 physical Core 0（測試拓撲 core 0 = LP 0）
            candidate_lps: vec![1, 2, 3],
            ..Default::default()
        }
    }

    /// 明確自訂尺寸 → workload_command 從 config.width/height 直接組出 args，
    /// 不被 product default 覆寫（D3D9 路徑直接讀欄位，最貼近序列化後的值）。
    #[test]
    fn workload_command_preserves_explicit_dimensions() {
        let dir = temp_root("wl_cmd_dims");
        let assets = make_assets(&dir);
        let config = BenchmarkConfig {
            workload: WorkloadKind::D3D9,
            fullscreen: false,
            width: 800,
            height: 600,
            ..Default::default()
        };
        let (_exe, args) = workload_command(&assets, &config);
        assert!(args.contains(&"--width=800".to_string()));
        assert!(args.contains(&"--height=600".to_string()));
        assert!(args.contains(&"--fullscreen=0".to_string()));
        assert!(!args.contains(&"--width=1280".to_string()));
        assert!(!args.contains(&"--height=720".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一組有效的 fake CSV（LP 不同 frametime）
    fn csv_for_lp(lp: u32) -> String {
        // LP 越低 fps 越高（frametime 越低）→ 讓 best_lp 可預期
        let base = 20.0 - (lp as f64) * 2.0; // LP0=20ms(50fps), LP1=18ms, LP2=16ms
        let mut s = String::from("Application,ProcessID,msBetweenPresents\n");
        for _ in 0..50 {
            s.push_str(&format!("\"w (1)\",1,{base:.1}\n"));
        }
        s
    }

    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    fn build_ctx(
        root: &std::path::Path,
        backend: Arc<dyn GpuBackend>,
        processes: Arc<dyn ProcessRunner>,
        cancel: Arc<dyn CancelSignal>,
        sleeper: Arc<dyn Sleep>,
        config: BenchmarkConfig,
        journal: &std::path::Path,
        on_progress: Option<Box<dyn FnMut(&BenchmarkProgress) + Send>>,
    ) -> RunContext {
        let session_id = Uuid::new_v4().to_string();
        let assets = make_assets(&root.join("assets"));
        RunContext {
            backend,
            sleeper,
            processes,
            cancel,
            topo: topo(),
            cpu_identity: fixed_identity(),
            assets,
            storage_root: root.join("benchmarks"),
            journal_path: journal.to_path_buf(),
            session_id,
            config,
            on_progress: on_progress.unwrap_or_else(|| Box::new(|_| {})),
            baseline: None,
            owned_processes: Vec::new(),
            window: Arc::new(fake::FakeWindow::new()),
        }
    }

    #[test]
    fn success_completes_with_best_lp_and_restores_exact_policy() {
        let root = temp_root("success");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        // 原始策略（DevicePolicy=4, override mask=0b1000 → LP 3）
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(
            result.status,
            SessionStatus::Completed,
            "err={:?}",
            result.error
        );
        assert!(result.best_lp.is_some());
        // 策略還原到原始（逐位元組）
        assert_eq!(backend.current_policy(GPU_A), baseline);
        assert!(!journal.exists(), "成功後日誌應清除");
        // session 已寫入
        let detail = storage::get_at(&ctx.storage_root, &ctx.session_id).unwrap();
        assert_eq!(detail.summary.status, SessionStatus::Completed);
        assert_eq!(detail.results.len(), 3);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_workload_affinity_true_still_skips_workload_affinity() {
        // sync_workload_affinity 已棄用：即使傳入 true，runner 也不得
        // 設定 workload affinity。ProcessRunner trait 已無 affinity API surface，
        // 完成即驗證不受影響。
        let root = temp_root("sync_skip");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.sync_workload_affinity = true; // 舊語意，但 runner 必須忽略

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(
            result.status,
            SessionStatus::Completed,
            "sync_workload_affinity=true 仍不得影響 runner；應正常完成"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn success_adaptive_merges_three_then_five_rounds() {
        // 固定調適排程：篩選 3 round 全 LP，確認 2 round 僅前 2 名 finalists。
        // 同內容 CSV → 穩健排名平手 → finalists = [1, 2]；LP3 只有 3 篩選 round。
        let root = temp_root("merge");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let config = base_config(); // repetitions=5, candidate_lps [1,2,3]

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);
        let sample_by_lp: HashMap<u32, u32> = result
            .detail
            .results
            .iter()
            .map(|r| (r.lp, r.sample_count))
            .collect();
        // finalists（LP1, LP2）合併 5 round（50×5=250）；非 finalist（LP3）合併 3 round（150）
        assert_eq!(sample_by_lp[&1], 250);
        assert_eq!(sample_by_lp[&2], 250);
        assert_eq!(sample_by_lp[&3], 150);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 可靠性（reliability）判定 ──────────────────────────────────────────

    /// 寫出一個 (round, lp) 的 CSV，全部樣本 frametime 固定（越低 FPS 越高）。
    fn write_round_csv(dir: &Path, round: u32, lp: u32, frame_ms: f64) -> PathBuf {
        let path = dir.join(format!("round-{round}-lp-{lp}.csv"));
        let mut s = String::from("Application,ProcessID,msBetweenPresents\n");
        for _ in 0..50 {
            s.push_str(&format!("\"w (1)\",1,{frame_ms:.3}\n"));
        }
        std::fs::write(&path, s).unwrap();
        path
    }

    /// 完整（completed）且四項指標齊備的 LpResult fixture
    fn lp_res(lp: u32, avg: f64, p1: f64, p01: f64, stdev: f64) -> LpResult {
        LpResult {
            lp,
            avg_fps: Some(avg),
            p1_low: Some(p1),
            p01_low: Some(p01),
            stdev_fps: Some(stdev),
            completed: true,
            ..Default::default()
        }
    }

    /// 5 round 全勝 + 複合優勢明顯 → Passed
    #[test]
    fn reliability_passed_with_5_rounds_and_effect() {
        let dir = temp_root("rel_pass");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 11.0));
        }
        let results = vec![
            lp_res(0, 100.0, 100.0, 100.0, 0.0),
            lp_res(1, 90.909, 90.909, 90.909, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 5);
        assert_eq!(rel.status, ReliabilityStatus::Passed);
        assert_eq!(rel.candidate_lp, Some(0));
        assert_eq!(rel.runner_up_lp, Some(1));
        assert_eq!(rel.candidate_wins, 5);
        assert_eq!(rel.per_round_winners, vec![Some(0); 5]);
        assert_eq!(rel.evaluated_rounds, 5);
        assert_eq!(rel.required_wins, 3);
        assert!(rel.composite_advantage_pct.unwrap() >= 0.5);
        assert!(rel.avg_fps_pct.unwrap() >= 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 7 round 中候選只勝 4 次（<ceil(60%)=5）→ Inconclusive
    #[test]
    fn reliability_inconclusive_when_insufficient_wins() {
        let dir = temp_root("rel_wins7");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..7 {
            // LP0 勝 round 0/1/2；LP1 勝 round 3/4/5/6
            let (f0, f1) = if round < 3 {
                (10.0, 11.0)
            } else {
                (11.0, 10.0)
            };
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, f0));
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, f1));
        }
        let results = vec![
            lp_res(0, 95.238, 95.238, 95.238, 0.0),
            lp_res(1, 95.238, 95.238, 95.238, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 7);
        assert_eq!(rel.status, ReliabilityStatus::Inconclusive);
        assert_eq!(rel.evaluated_rounds, 7);
        assert_eq!(rel.required_wins, 5);
        assert_eq!(rel.candidate_lp, Some(1));
        assert_eq!(rel.candidate_wins, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5 round 全勝但複合優勢 <0.5% → Equivalent（可重現、差異過小）
    #[test]
    fn reliability_equivalent_when_advantage_too_small() {
        let dir = temp_root("rel_equiv");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 10.05));
        }
        let results = vec![
            lp_res(0, 100.0, 100.0, 100.0, 0.0),
            lp_res(1, 99.502, 99.502, 99.502, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 5);
        assert_eq!(rel.status, ReliabilityStatus::Equivalent);
        assert_eq!(rel.candidate_lp, Some(0));
        assert_eq!(rel.candidate_wins, 5);
        assert!(rel.composite_advantage_pct.unwrap() < 0.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5 round 中 LP1 缺 1 個 round → Inconclusive
    #[test]
    fn reliability_inconclusive_when_missing_round() {
        let dir = temp_root("rel_missing");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
        }
        // LP1 只有 4 個 round（缺 round 4）
        for round in 0..4 {
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 12.0));
        }
        let results = vec![
            lp_res(0, 100.0, 100.0, 100.0, 0.0),
            lp_res(1, 83.333, 83.333, 83.333, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 5);
        assert_eq!(rel.status, ReliabilityStatus::Inconclusive);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 單一 LP（無亞軍）→ Inconclusive，改善百分比為 None
    #[test]
    fn reliability_inconclusive_with_single_lp() {
        let dir = temp_root("rel_one_lp");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
        }
        let results = vec![lp_res(0, 100.0, 100.0, 100.0, 0.0)];
        let rel = compute_reliability(&round_csvs, &results, 5);
        assert_eq!(rel.status, ReliabilityStatus::Inconclusive);
        assert_eq!(rel.runner_up_lp, None);
        assert!(rel.avg_fps_pct.is_none());
        assert!(rel.p1_low_pct.is_none());
        assert!(rel.p01_low_pct.is_none());
        assert!(rel.composite_advantage_pct.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 每 LP 只有 3 round（<5）→ 即使全勝也不得 Passed
    #[test]
    fn reliability_inconclusive_when_fewer_than_5_rounds() {
        let dir = temp_root("rel_3rounds");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..3 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 11.0));
        }
        let results = vec![
            lp_res(0, 100.0, 100.0, 100.0, 0.0),
            lp_res(1, 90.909, 90.909, 90.909, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 3);
        assert_eq!(rel.status, ReliabilityStatus::Inconclusive);
        assert_eq!(rel.evaluated_rounds, 3);
        assert_eq!(rel.candidate_wins, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// LP1 有 5 個 round key 但缺 round 1（0/2/3/4/5）→ 不算完整 → Inconclusive
    #[test]
    fn reliability_inconclusive_when_missing_round_one() {
        let dir = temp_root("rel_missing_r1");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
        }
        // LP1 有 5 個 key，但缺 round 1（round 5 是額外無關 key，不可補足）
        for round in [0u32, 2, 3, 4, 5] {
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 12.0));
        }

        let results = vec![
            lp_res(0, 100.0, 100.0, 100.0, 0.0),
            lp_res(1, 83.333, 83.333, 83.333, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 5);
        assert_eq!(rel.status, ReliabilityStatus::Inconclusive);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// LP1 的 round 3 CSV 無效（不可解析）→ 該 round 不完整 → Inconclusive
    #[test]
    fn reliability_inconclusive_when_invalid_csv_in_round() {
        let dir = temp_root("rel_bad_csv");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
        }
        for round in [0u32, 1, 2, 4] {
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 12.0));
        }
        let bad = dir.join("round-3-lp-1.csv");
        std::fs::write(&bad, "Application,ProcessID,msBetweenPresents\nabc,xyz\n").unwrap();
        round_csvs.entry(1).or_default().insert(3, bad);

        let results = vec![
            lp_res(0, 100.0, 100.0, 100.0, 0.0),
            lp_res(1, 83.333, 83.333, 83.333, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 5);
        assert_eq!(rel.status, ReliabilityStatus::Inconclusive);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 只測部分 round 的 LP（缺 round 4）不得成為任何 round 的勝者，
    /// 即使其聚合指標最好、且在已測 round 全勝（8ms 最快）。
    #[test]
    fn reliability_partial_lp_excluded_from_round_winner_comparisons() {
        let dir = temp_root("rel_partial");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 12.0));
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 13.0));
        }
        // LP2 只測 round 0..4（缺 round 4），且四 round 皆最佳（8ms），但不得成為勝者
        for round in 0..4 {
            round_csvs
                .entry(2)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 2, 8.0));
        }

        let results = vec![
            lp_res(0, 83.333, 83.333, 83.333, 0.0),
            lp_res(1, 76.923, 76.923, 76.923, 0.0),
            lp_res(2, 125.0, 125.0, 125.0, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, 5);
        assert_eq!(rel.status, ReliabilityStatus::Passed);
        assert_eq!(rel.candidate_lp, Some(0));
        assert!(
            !rel.per_round_winners.contains(&Some(2)),
            "部分 LP 不得成為任何 round 勝者"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancel_before_start_marks_cancelled_and_restores() {
        let root = temp_root("cancel0");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        let cancel = FakeCancel::new();
        cancel.set(true); // 一開始就取消

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Cancelled);
        assert!(processes.spawn_log().is_empty(), "取消時不該啟動 workload");
        assert_eq!(backend.current_policy(GPU_A), baseline, "策略不得被改動");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_during_lp_kills_owned_processes_and_restores() {
        let root = temp_root("cancelmid");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = Arc::new(FakeCancel::new());
        // restart 穩定（5000ms）之後、warmup（5000+1000ms）期間的 8000ms 處取消
        let sleeper = Arc::new(CancelAfterSleeper::new(cancel.clone(), 8000));

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            cancel as Arc<dyn CancelSignal>,
            sleeper.clone() as Arc<dyn Sleep>,
            base_config(),
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Cancelled);
        // 取消在 warmup 期間（11000ms 前）就被偵測，未睡滿 stabilize+warmup
        assert!(sleeper.elapsed_ms() < 11000);
        // workload 已啟動，取消後必須被終止
        assert!(processes
            .spawn_log()
            .iter()
            .any(|(n, _, _)| !n.contains("PresentMon")));
        assert!(
            !processes.killed_log().is_empty(),
            "取消時必須終止 owned 子程序"
        );
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原原始策略");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn presentmon_command_uses_process_id() {
        // PresentMon 必須以 -process_id 篩選已 spawn 的 workload PID，確保 Vulkan workload
        // 正確收集 present 事件（-process_name 在此情境不建立 CSV）
        let cfg = base_config();
        let args = presentmon_command(&cfg, 1234, Path::new("x.csv"), "test-session");
        let id_idx = args.iter().position(|a| a == "--process_id").unwrap();
        assert_eq!(args[id_idx + 1], "1234");
        assert!(
            !args.iter().any(|a| a == "--process_name"),
            "不該使用 -process_name"
        );
        // 仍有 output/timed
        assert!(args.iter().any(|a| a == "--output_file"));
        assert!(args.iter().any(|a| a == "--timed"));
    }

    #[test]
    fn presentmon_command_uses_process_id_for_d3d9() {
        let mut cfg = base_config();
        cfg.workload = WorkloadKind::D3D9;
        let args = presentmon_command(&cfg, 5678, Path::new("x.csv"), "test-session");
        let id_idx = args.iter().position(|a| a == "--process_id").unwrap();
        assert_eq!(args[id_idx + 1], "5678");
        assert!(
            !args.iter().any(|a| a == "--process_name"),
            "不該使用 -process_name"
        );
    }

    #[test]
    fn validate_config_rejects_zero_sample_secs_and_non_five_repetitions() {
        let t = topo();
        let mut c = base_config();
        c.sample_secs = 0;
        assert_eq!(
            validate_config(&c, &t).unwrap_err(),
            codes::BENCHMARK_INVALID_CONFIG
        );
        c.sample_secs = 3;
        // 固定調適排程只接受 repetitions == 5；其餘（含舊 3..=7 範圍）皆拒絕
        for bad in [2u32, 3, 4, 6, 7, 8] {
            c.repetitions = bad;
            assert_eq!(
                validate_config(&c, &t).unwrap_err(),
                codes::BENCHMARK_INVALID_CONFIG,
                "repetitions={bad} 應被拒絕"
            );
        }
        c.repetitions = 5;
        assert!(validate_config(&c, &t).is_ok());
    }

    #[test]
    fn presentmon_command_includes_stale_session_cleanup() {
        // 上游 AutoGpuAffinity 語意：先停掉殘留 ETL session，避免 stale session 卡住 capture
        let cfg = base_config();
        let args = presentmon_command(&cfg, 1234, Path::new("x.csv"), "test-session");
        assert!(
            args.iter().any(|a| a == "--stop_existing_session"),
            "必須含 -stop_existing_session"
        );
        assert!(
            args.iter().any(|a| a == "--no_console_stats"),
            "必須含 --no_console_stats"
        );
        // -terminate_after_timed 讓 PresentMon 收集完自行退出，runner 才能有界等待
        assert!(args.iter().any(|a| a == "--terminate_after_timed"));
        assert!(args
            .windows(2)
            .any(|w| { w[0] == "--session_name" && w[1] == "test-session" }));
        assert!(args.iter().any(|a| a == "--v1_metrics"));
        assert!(args.windows(2).any(|w| {
            w[0] == "--set_circular_buffer_size"
                && w[1] == PRESENTMON_CIRCULAR_BUFFER_SIZE.to_string()
        }));
    }

    /// 一律使用完整追蹤（不加任何 `--no_track_*`）。實測三者同時停用會讓
    /// PresentMon 完全沒有 frame 來源、不建立 CSV（本次 regression 根因）。
    #[test]
    fn presentmon_command_uses_full_tracking() {
        let cfg = base_config();
        let args = presentmon_command(&cfg, 1234, Path::new("x.csv"), "test-session");
        assert!(
            !args.iter().any(|a| a.starts_with("--no_track")),
            "不得停用任何追蹤（--no_track_gpu/input/display 會使 CSV 無法建立）: {args:?}"
        );
    }

    #[test]
    fn presentmon_captures_by_process_id_from_spawned_workload() {
        let root = temp_root("pmpid");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(
            result.status,
            SessionStatus::Completed,
            "err={:?}",
            result.error
        );

        let log = processes.spawn_log();
        // 依 spawn 順序：每個 PresentMon 用 -process_id 對應已 spawn 的 workload PID
        let mut workload_pids: Vec<u32> = Vec::new();
        let mut presentmon_seen = 0u32;
        for (name, pid, args) in &log {
            if name.contains("PresentMon") {
                presentmon_seen += 1;
                let id_idx = args.iter().position(|a| a == "--process_id").unwrap();
                let pm_filter_pid: u32 = args[id_idx + 1].parse().unwrap();
                assert!(
                    workload_pids.contains(&pm_filter_pid),
                    "PresentMon -process_id {pm_filter_pid} 必須對應已 spawn 的 workload PID"
                );
                assert!(
                    !args.iter().any(|a| a == "--process_name"),
                    "不該使用 -process_name"
                );
            } else {
                workload_pids.push(*pid);
            }
        }
        assert!(presentmon_seen >= 1, "至少一次 PresentMon capture");
        assert!(!workload_pids.is_empty(), "每個 round 都要啟動 workload");
        // 連續 capture 每個都新鮮有效：capture 後清理
        for wl_pid in &workload_pids {
            assert!(
                processes.killed_log().contains(wl_pid),
                "workload {wl_pid} 必須被清理"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn workload_launch_failure_fails_and_restores() {
        let root = temp_root("wlfail");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes.fail_workload.store(true, Ordering::SeqCst);
        let cancel = FakeCancel::new();

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_WORKLOAD_FAILED)
        );
        assert_eq!(
            backend.current_policy(GPU_A),
            baseline,
            "失敗後必須還原原始策略"
        );
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_csv_fails_with_partial_results_and_no_recommendation() {
        let root = temp_root("nocsvar");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // LP 1 有資料；LP 2/3 的 PresentMon spawn 失敗 → 該 LP 無 CSV
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        processes.fail_lp(2);
        processes.fail_lp(3);
        let cancel = FakeCancel::new();

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(), // candidate_lps 1,2,3
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(result.best_lp, None, "失敗不該有推薦");
        // 部分結果保留（LP 1 有完成）
        assert!(!result.detail.results.is_empty());
        // 部分 CSV 檔案保留
        let session_dir = ctx.storage_root.join(&ctx.session_id);
        let files = std::fs::read_dir(&session_dir).unwrap().count();
        assert!(files >= 1, "partial CSV 應保留");
        assert_eq!(
            backend.current_policy(GPU_A),
            baseline,
            "失敗後必須還原原始策略"
        );
        assert!(!journal.exists(), "還原成功應清日誌");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_csv_content_fails_session() {
        let root = temp_root("badcsv");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        // 垃圾 CSV（無 msBetweenPresents 欄）→ capture 驗證即失敗
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str("garbage,not,csv\n1,2,3\n");
        let cancel = FakeCancel::new();

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_EMPTY)
        );
        assert_eq!(result.best_lp, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_failure_keeps_journal_and_marks_recovery_required() {
        let root = temp_root("restorefail");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        // 終結還原的 restart 一直失敗
        backend.disable_fails.store(true, Ordering::SeqCst);

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        // 第一 LP 的 apply restart 就失敗 → 已寫入策略，還原 restart 也失敗
        assert_eq!(result.status, SessionStatus::Failed);
        assert!(result.recovery_required, "還原失敗必須要求 recovery");
        assert!(journal.exists(), "還原失敗必須保留日誌");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn progress_events_emitted_with_stages() {
        let root = temp_root("progress");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let events: std::sync::Mutex<Vec<BenchmarkProgress>> = std::sync::Mutex::new(Vec::new());
        let ev = std::sync::Arc::new(events);
        let ev_clone = ev.clone();

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            Some(Box::new(move |p| {
                ev_clone.lock().unwrap().push(p.clone());
            })),
        );
        run_benchmark(&mut ctx);
        let stages: Vec<String> = ev.lock().unwrap().iter().map(|p| p.stage.clone()).collect();
        assert!(stages.contains(&"applying".to_string()));
        assert!(stages.contains(&"collecting".to_string()));
        assert!(stages.contains(&"finalizing".to_string()));
        assert!(stages.iter().all(|s| !s.is_empty()));
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── PresentMon capture 可靠性回歸測試 ────────────────────────────────

    #[test]
    fn presentmon_timeout_fails_and_restores_with_persisted_error() {
        let root = temp_root("pmtimeout");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // PresentMon 卡住：wait_exit 一直回傳「未退出」
        processes.presentmon_timeout.store(true, Ordering::SeqCst);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_PRESENTMON_TIMEOUT)
        );
        // 卡住的 PresentMon 與 workload 都必須被終止
        assert!(!processes.killed_log().is_empty());
        // 策略還原、日誌清除
        assert_eq!(backend.current_policy(GPU_A), baseline);
        assert!(!journal.exists());
        // 失敗原因已持久化（reload 後 UI 可顯示）
        let detail = storage::get_at(&ctx.storage_root, &ctx.session_id).unwrap();
        assert_eq!(
            detail.summary.error.as_deref(),
            Some(codes::BENCHMARK_PRESENTMON_TIMEOUT)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn presentmon_no_output_file_fails() {
        let root = temp_root("nofile");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // PresentMon 正常退出但沒寫出 CSV（stale session / 依賴缺失）
        processes
            .presentmon_write_csv
            .store(false, Ordering::SeqCst);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );
        assert_eq!(result.best_lp, None, "缺檔不該有推薦");
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原策略");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn presentmon_header_only_output_fails_as_empty() {
        let root = temp_root("emptycsv");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // 只有 header、沒有任何 frametime 資料
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str("Application,ProcessID,msBetweenPresents\n");
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_EMPTY)
        );
        assert_eq!(backend.current_policy(GPU_A), baseline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 核心回歸：stale CSV（上一個 session 殘留、格式看似有效）絕不能被當成
    /// 本次 capture 的輸出。capture 前必須先清除 stale 檔，本次沒產出新檔即失敗。
    #[test]
    fn stale_csv_cannot_be_mistaken_for_fresh_output() {
        let root = temp_root("stale");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // 本次 capture 不產出任何新檔
        processes
            .presentmon_write_csv
            .store(false, Ordering::SeqCst);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        // 在目標路徑預放「看似有效」的 stale CSV（50 個 frametime）
        let session_dir = ctx.storage_root.join(&ctx.session_id);
        let csv = session_dir.join("round-0-lp-1.csv");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(&csv, csv_for_lp(0)).unwrap();

        let result = run_benchmark(&mut ctx);
        // stale 不能讓 session「成功」：沒有新鮮輸出 → 失敗
        assert_eq!(
            result.status,
            SessionStatus::Failed,
            "stale CSV 不得被當成成功輸出"
        );
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );
        // stale 檔已在 capture 前被確定性清除，不會殘留在最終 session
        assert!(!csv.exists(), "stale CSV 必須在 capture 前被刪除");
        assert_eq!(result.best_lp, None);
        assert_eq!(backend.current_policy(GPU_A), baseline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 多個連續 LP capture：每個 LP 都必須有「新鮮有效」的 CSV 才算成功；
    /// 前一個 LP 的輸出不能讓後續 LP 誤判。LP1 有效、LP3 無輸出 → 失敗且保留 LP1/LP2 部分結果。
    #[test]
    fn sequential_lp_captures_require_fresh_valid_csv_each() {
        let root = temp_root("seqfresh");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // 對所有 LP 都寫有效 CSV；但 LP3 的 PresentMon 不產出檔案
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        processes.fail_lp(3); // LP3 PresentMon spawn 失敗 → 該 LP 無新鮮 CSV
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1, 2, 3];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_PRESENTMON_FAILED)
        );
        // LP1、LP2 已驗證的部分結果保留；LP3 未完成
        assert!(
            result.detail.results.iter().all(|r| r.completed),
            "保留的部分結果必須都已完成"
        );
        assert!(result.detail.results.len() < 3, "LP3 失敗不該有結果");
        assert_eq!(result.best_lp, None);
        assert_eq!(backend.current_policy(GPU_A), baseline);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 讀取某 capture 的診斷檔
    fn read_diag(session_dir: &Path, round: u32, lp: u32) -> CaptureDiagnostics {
        let p = session_dir
            .join("diag")
            .join(format!("capture-round-{round}-lp-{lp}.json"));
        let text =
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("診斷檔讀取失敗 {p:?}: {e}"));
        serde_json::from_str(&text).unwrap()
    }

    /// 成功 capture 必須寫出診斷：workload 全程存活、PM 正常退出、CSV 存在。
    #[test]
    fn success_capture_writes_diagnostics() {
        let root = temp_root("diagok");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(
            result.status,
            SessionStatus::Completed,
            "err={:?}",
            result.error
        );

        let session_dir = ctx.storage_root.join(&ctx.session_id);
        let d = read_diag(&session_dir, 0, 1);
        assert_eq!(d.round, 0);
        assert_eq!(d.lp, 1);
        assert!(!d.started_at.is_empty(), "要有起始時間戳");
        assert!(d.finished_at.is_some(), "要有結束時間戳");
        assert_ne!(d.workload_pid, 0);
        assert!(d.workload_alive_before_pm, "啟動 PM 前 workload 必須活著");
        assert!(
            d.workload_alive_after_capture,
            "capture 後 workload 應仍活著"
        );
        assert_eq!(d.workload_exit_code, None, "存活中不該有 exit code");
        assert_ne!(d.presentmon_pid, 0);
        assert_eq!(d.presentmon_exit_code, Some(0), "PM 正常退出 exit code 0");
        assert!(d.wait_completed);
        assert!(!d.wait_timed_out);
        assert_eq!(d.wait_error, None);
        assert!(d.csv_exists, "成功 capture 必須有 CSV");
        assert!(d.csv_size_bytes > 0);
        assert_eq!(d.error, None);
        // bounded output tail 有被擷取（fake 有記錄）
        assert!(d.presentmon_stderr.contains("fake-presentmon-stderr"));
        // 診斷要記住 PresentMon 的篩選種類與值（未來 session 才看得出匹配目標）
        assert_eq!(d.capture_filter_kind, "process_id");
        assert!(
            !d.capture_filter_value.is_empty(),
            "capture_filter_value 必須記錄 PID"
        );
        let _: u32 = d
            .capture_filter_value
            .parse()
            .expect("capture_filter_value 必須為十進位 PID");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 缺檔失敗（presentmon 正常退出但沒產出 CSV）也要寫診斷，且 session.json
    /// 仍可正常讀取（診斷檔不影響既有匯入/相容性）。
    #[test]
    fn missing_output_writes_diagnostics_and_keeps_session_readable() {
        let root = temp_root("diagmiss");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_write_csv
            .store(false, Ordering::SeqCst);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );

        // 診斷檔在失敗後仍存在
        let session_dir = ctx.storage_root.join(&ctx.session_id);
        let d = read_diag(&session_dir, 0, 1);
        assert!(d.workload_alive_before_pm, "啟動 PM 前 workload 活著");
        assert!(d.workload_alive_after_capture);
        assert_eq!(d.presentmon_exit_code, Some(0), "PM 正常退出但無輸出");
        assert!(d.wait_completed);
        assert!(!d.wait_timed_out);
        assert!(!d.csv_exists, "缺檔：csv 不該存在");
        assert_eq!(d.csv_size_bytes, 0);
        assert_eq!(d.error.as_deref(), Some(codes::BENCHMARK_CAPTURE_MISSING));
        // 失敗路徑也保留篩選資訊（PM 用 -process_id 對應的 workload PID）
        assert_eq!(d.capture_filter_kind, "process_id");
        assert!(
            !d.capture_filter_value.is_empty(),
            "capture_filter_value 必須記錄 PID"
        );
        let _: u32 = d
            .capture_filter_value
            .parse()
            .expect("capture_filter_value 必須為十進位 PID");

        // session.json 仍可正常讀取（診斷檔不破壞舊相容性）
        let detail = storage::get_at(&ctx.storage_root, &ctx.session_id).unwrap();
        assert_eq!(detail.summary.status, SessionStatus::Failed);
        assert_eq!(
            detail.summary.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );
        // list 也照常（診斷檔只是多餘檔案）
        let summaries = storage::list_at(&ctx.storage_root).unwrap();
        assert!(summaries.iter().any(|s| s.id == ctx.session_id));
        // 診斷檔計入總位元組（dir_size 掃全部檔案）
        assert!(detail.summary.total_bytes > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PresentMon 逾時卡住也寫診斷：wait_timed_out=true、PM exit code 未知。
    #[test]
    fn presentmon_timeout_writes_diagnostics() {
        let root = temp_root("diagtimeout");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes.presentmon_timeout.store(true, Ordering::SeqCst);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_PRESENTMON_TIMEOUT)
        );

        let session_dir = ctx.storage_root.join(&ctx.session_id);
        let d = read_diag(&session_dir, 0, 1);
        assert!(d.wait_completed);
        assert!(d.wait_timed_out, "逾時必須記錄");
        assert_eq!(d.presentmon_exit_code, None, "未退出不該有 exit code");
        assert_eq!(
            d.error.as_deref(),
            Some(codes::BENCHMARK_PRESENTMON_TIMEOUT)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 相容性：session 資料夾裡有 diag 檔案時，get/list/delete 照常運作，
    /// 且不含 diag 的舊 session.json 仍可載入（diag 是獨立檔案，不進 schema）。
    #[test]
    fn diagnostics_files_do_not_break_old_session_compat() {
        let root = temp_root("diagcompat");
        let storage_root = root.join("benchmarks");
        let id = Uuid::new_v4().to_string();
        let detail = SessionDetail {
            summary: crate::benchmark::SessionSummary {
                id: id.clone(),
                status: SessionStatus::Failed,
                started_at: "2026-08-11T00:00:00Z".into(),
                finished_at: Some("2026-08-11T00:01:00Z".into()),
                gpu_name: "Fake GPU".into(),
                gpu_instance_id: GPU_A.to_string(),
                cpu_fingerprint: "fixture".into(),
                best_lp: None,
                reliability: ReliabilitySummary::default(),
                severe_lps: vec![],
                sample_count: 0,
                total_bytes: 0,
                config: base_config(),
                error: Some(codes::BENCHMARK_CAPTURE_MISSING.to_string()),
            },
            results: vec![],
            samples: vec![],
        };
        storage::save_session_at(&storage_root, &detail).unwrap();
        // 放診斷檔（模擬新版本寫入）
        let diag_dir = storage_root.join(&id).join("diag");
        std::fs::create_dir_all(&diag_dir).unwrap();
        std::fs::write(
            diag_dir.join("capture-round-0-lp-0.json"),
            r#"{"round":0,"lp":0,"csvExists":false}"#,
        )
        .unwrap();

        let loaded = storage::get_at(&storage_root, &id).unwrap();
        assert_eq!(loaded.summary.status, SessionStatus::Failed);
        assert_eq!(
            loaded.summary.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );
        let list = storage::list_at(&storage_root).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].total_bytes > 0, "診斷檔計入 total_bytes");
        storage::delete_at(&storage_root, &id).unwrap();
        assert!(!storage_root.join(&id).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── capture retry 回歸測試 ─────────────────────────────────────────────

    /// 第一次 capture MISSING → retry 後用新 workload PID 成功 → session Completed
    #[test]
    fn retry_missing_recovers_on_second_attempt() {
        let root = temp_root("retry_ok");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        // LP 1 第一次 missing（不寫 CSV），第二次成功
        processes.first_attempt_missing.lock().unwrap().insert(1);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(
            result.status,
            SessionStatus::Completed,
            "retry 後應成功: err={:?}",
            result.error
        );
        // 單一 LP（N=1）無確認階段 → Inconclusive，無 best_lp；此測試重點是 retry 回收
        assert_eq!(result.best_lp, None);
        // 每 round：初次套用 + capture recovery 各重啟一次；3 round 共 6 次，
        // 加上終結還原 1 次 = 7 次。
        assert_eq!(
            backend.restart_count(),
            7,
            "missing capture retry 必須先重新啟動 GPU，再建立新 workload"
        );
        // 每 round 兩個 workload PID（attempt 1 + retry）× 3 round = 6
        let log = processes.spawn_log();
        let wl_pids: Vec<u32> = log
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .map(|(_, p, _)| *p)
            .collect();
        assert_eq!(
            wl_pids.len(),
            6,
            "每 round 第一次 + retry 各一個 workload PID"
        );
        for p in &wl_pids {
            assert!(
                processes.killed_log().contains(p),
                "workload {p} 必須被清理"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 所有 capture attempt 都 MISSING → 最終失敗，無部分結果且 sample_count 為 0
    #[test]
    fn retry_missing_both_fails_cleanly() {
        let root = temp_root("retry_fail");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        // LP 1: 第一次與所有 retry 都 missing
        processes.first_attempt_missing.lock().unwrap().insert(1);
        processes
            .second_attempt_also_missing
            .lock()
            .unwrap()
            .insert(1);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );
        assert_eq!(result.best_lp, None, "失敗不該有推薦");
        assert!(
            result.detail.results.is_empty(),
            "無 LP 成功，不該有部分結果"
        );
        assert_eq!(result.detail.summary.sample_count, 0);
        // 每次 attempt 各 spawn 一個 workload
        let log = processes.spawn_log();
        let wl_pids: Vec<u32> = log
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .map(|(_, p, _)| *p)
            .collect();
        assert_eq!(
            wl_pids.len(),
            MAX_CAPTURE_ATTEMPTS as usize,
            "每次 capture attempt 各一個 workload"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 尚無任何成功 capture 時，第一個候選 LP 經所有 retry 仍 MISSING →
    /// fail-fast：立即終止 session，不再跑剩餘 LP/round，且進入 cleanup/restore。
    #[test]
    fn first_lp_missing_fails_fast_without_running_remaining_lps() {
        let root = temp_root("ff_missing");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // LP 1 所有 attempt 都 MISSING
        processes.first_attempt_missing.lock().unwrap().insert(1);
        processes
            .second_attempt_also_missing
            .lock()
            .unwrap()
            .insert(1);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1, 2, 3];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );
        assert_eq!(result.best_lp, None);
        assert!(result.detail.results.is_empty(), "無 LP 成功，不該有部分結果");
        // 只測第一個 LP（其 MAX_CAPTURE_ATTEMPTS 次），後續 LP 不得 spawn workload
        let wl_spawns = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .count();
        assert_eq!(
            wl_spawns, MAX_CAPTURE_ATTEMPTS as usize,
            "fail-fast：僅第一個 LP 的 attempts，不跑剩餘 LP"
        );
        // 進入既有 cleanup/restore
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原策略");
        assert!(!journal.exists(), "還原成功應清日誌");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 已有成功 capture（LP 1 完成）時，後續 LP（LP 2）經所有 retry 仍 MISSING →
    /// 隔離該 LP 並繼續，保留已收集的部分結果，最終 Failed 且無推薦。
    #[test]
    fn later_lp_missing_isolates_and_continues() {
        let root = temp_root("isolate_missing");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // LP 1 有資料；LP 2 所有 attempt 都 MISSING
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        processes.first_attempt_missing.lock().unwrap().insert(2);
        processes
            .second_attempt_also_missing
            .lock()
            .unwrap()
            .insert(2);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1, 2];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_MISSING)
        );
        assert_eq!(result.best_lp, None, "部分失敗不該有推薦");
        assert!(
            !result.detail.results.is_empty(),
            "已有成功 capture 應保留部分結果"
        );
        // LP 1 每 round 一次成功（3 round）+ LP 2 每 round 三次 attempt（3 round）
        let wl_spawns = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .count();
        assert_eq!(
            wl_spawns,
            3 + 3 * MAX_CAPTURE_ATTEMPTS as usize,
            "LP1 每 round 一次 + LP2 每 round 三次 attempts"
        );
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原策略");
        assert!(!journal.exists(), "還原成功應清日誌");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 第一次 capture EMPTY（僅 header）→ retry 成功
    #[test]
    fn retry_empty_recovers_on_second_attempt() {
        let root = temp_root("retry_empty");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        // full CSV for retry
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        // LP 1: 第一次 empty（header-only）
        processes.first_attempt_empty.lock().unwrap().insert(1);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(
            result.status,
            SessionStatus::Completed,
            "empty retry 後應成功: err={:?}",
            result.error
        );
        // 單一 LP（N=1）無確認階段 → Inconclusive，無 best_lp；此測試重點是 empty retry 回收
        assert_eq!(result.best_lp, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// PresentMon spawn 失敗不觸發 retry（非 MISSING/EMPTY）
    #[test]
    fn presentmon_spawn_failure_does_not_retry() {
        let root = temp_root("noretry_pmfail");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        // PresentMon spawn 失敗 → BENCHMARK_PRESENTMON_FAILED，不該 retry
        processes.fail_lp(1);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_PRESENTMON_FAILED)
        );
        // 只有一次 PresentMon spawn（沒 retry）
        let pm_spawns = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| n.contains("PresentMon"))
            .count();
        assert_eq!(pm_spawns, 1, "PresentMon spawn 失敗不該 retry");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ETW events lost 訊號的純函式偵測：大小寫、變體、誤判防護。
    #[test]
    fn stderr_etw_loss_detection() {
        // positive：數量前綴 / "Lost N" / 無數量 "were lost" / 大小寫變體
        assert!(stderr_has_etw_loss("Lost 9000000 ETW events"));
        assert!(stderr_has_etw_loss("ETW events lost"));
        assert!(stderr_has_etw_loss("123 ETW events lost"));
        assert!(stderr_has_etw_loss("Lost 123 ETW events"));
        assert!(stderr_has_etw_loss("ETW events were lost"));
        assert!(stderr_has_etw_loss("warning: events lost during capture"));
        assert!(stderr_has_etw_loss("Lost 123 events"));
        assert!(stderr_has_etw_loss("123 ETW EVENTS LOST"));
        assert!(stderr_has_etw_loss("EtW EvEnTs WeRe LoSt"));
        // negative：明確零遺失 / 否定表述不得誤判
        assert!(!stderr_has_etw_loss("0 ETW events lost"));
        assert!(!stderr_has_etw_loss("no ETW events lost"));
        assert!(!stderr_has_etw_loss("no events were lost"));
        assert!(!stderr_has_etw_loss("lost 0 events"));
        assert!(!stderr_has_etw_loss("without events lost"));
        assert!(!stderr_has_etw_loss("fake-presentmon-stderr"));
        assert!(!stderr_has_etw_loss(""));
        assert!(!stderr_has_etw_loss("etw session started"));
        assert!(!stderr_has_etw_loss("no lost here"));
        // 混合否定／肯定子句：否定只在其子句內生效，不得抑制另一子句的真正 loss
        assert!(stderr_has_etw_loss("no events were lost; 9000 ETW events lost"));
        assert!(stderr_has_etw_loss("0 events lost, 5000 ETW events lost"));
        assert!(stderr_has_etw_loss("error code 0; lost 5 ETW events"));
        assert!(stderr_has_etw_loss("lost 0 events; lost 9000 ETW events"));
        assert!(!stderr_has_etw_loss("no events lost; no ETW events lost"));
    }

    /// ETW events lost + 無 CSV → 專用錯誤、不可重試、fail-fast（後續 LP 不跑），
    /// 且進入既有 cleanup/restore（策略還原、日誌清除），診斷反映真實策略。
    #[test]
    fn etw_loss_fails_fast_and_does_not_retry() {
        let root = temp_root("etw_failfast");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes.presentmon_etw_loss.store(true, Ordering::SeqCst);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1, 2, 3];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(
            result.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_ETW_LOST)
        );
        // 不可重試：只有一次 PresentMon spawn（ETW loss 不進 retry loop）
        let pm_spawns = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| n.contains("PresentMon"))
            .count();
        assert_eq!(pm_spawns, 1, "ETW loss 不得重試");
        // fail-fast：只測第一個 LP（round 0 的 LP1），後續 LP 不得繼續
        let wl_spawns = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .count();
        assert_eq!(wl_spawns, 1, "後續 LP 不得繼續");
        assert!(
            result.detail.results.is_empty(),
            "無 LP 成功，不該有部分結果"
        );
        // 進入既有 cleanup/restore
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原策略");
        assert!(!journal.exists(), "還原成功應清日誌");
        // 診斷反映真實策略
        let session_dir = ctx.storage_root.join(&ctx.session_id);
        let d = read_diag(&session_dir, 0, 1);
        assert!(d.etw_events_lost, "診斷必須記錄 ETW loss");
        assert_eq!(
            d.error.as_deref(),
            Some(codes::BENCHMARK_CAPTURE_ETW_LOST)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// retry 期間 cancel → Cancelled，不清除已收集的部分
    #[test]
    fn cancel_during_retry_aborts_and_restores() {
        let root = temp_root("retry_cancel");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        // LP 1 第一次 missing → 觸發 retry；retry 期間 cancel
        processes.first_attempt_missing.lock().unwrap().insert(1);
        let cancel = Arc::new(FakeCancel::new());
        // 累計 13000ms 取消：落在 retry restart 穩定（11000..16000ms）期間，
        // 此時不應建立 retry workload。
        let sleeper = Arc::new(CancelAfterSleeper::new(cancel.clone(), 13000));
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            cancel as Arc<dyn CancelSignal>,
            sleeper as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Cancelled);
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原原始策略");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// capture wait（PresentMon 卡住不退出）期間取消 → 提前中斷、終止 owned、
    /// 還原策略、狀態 Cancelled。
    #[test]
    fn cancel_during_capture_wait_interrupts_and_kills() {
        let root = temp_root("cancel_capture");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        // PresentMon 卡住：wait_exit 一直 Ok(false)
        processes.presentmon_timeout.store(true, Ordering::SeqCst);
        let cancel = Arc::new(FakeCancel::new());
        // stabilize(5000) + warmup(6000) = 11000ms 後進入 capture wait；
        // 11500ms 處取消（capture 開始後 ~500ms），不該等到 18s 逾時。
        let sleeper = Arc::new(CancelAfterSleeper::new(cancel.clone(), 11500));
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            cancel as Arc<dyn CancelSignal>,
            sleeper as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Cancelled);
        assert!(!processes.killed_log().is_empty(), "取消時必須終止 owned 子程序");
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原原始策略");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Vulkan windowed → 對 workload PID 要求 client area 設成 config width×height
    #[test]
    fn windowed_vulkan_resizes_client_area_to_config() {
        let root = temp_root("win_vk");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.fullscreen = false;
        config.width = 640;
        config.height = 480;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed, "err={:?}", result.error);

        // 每次 spawn workload（含 retry）都要求 resize 成 (640, 480)
        let wl_pids: Vec<u32> = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .map(|(_, p, _)| *p)
            .collect();
        let calls = window.calls_log();
        assert!(!calls.is_empty(), "windowed Vulkan 必須呼叫 resize");
        for (pid, w, h) in &calls {
            assert!(wl_pids.contains(pid), "resize 目標必須是 spawned workload PID");
            assert_eq!(*w, 640);
            assert_eq!(*h, 480);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// fullscreen Vulkan → 不強制 resize
    #[test]
    fn fullscreen_vulkan_does_not_resize() {
        let root = temp_root("fs_vk");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.fullscreen = true;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);
        assert!(window.calls_log().is_empty(), "fullscreen Vulkan 不該 resize");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// D3D9（即使非 fullscreen）→ 不強制 resize
    #[test]
    fn d3d9_does_not_resize() {
        let root = temp_root("d3d9");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.workload = WorkloadKind::D3D9;
        config.fullscreen = false;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);
        assert!(window.calls_log().is_empty(), "D3D9 不該 resize");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 內建 Vulkan windowed → 安裝關閉防護 + resize client area
    #[test]
    fn windowed_vulkan_guards_close_and_resizes() {
        let root = temp_root("guard_win");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.fullscreen = false;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);

        let wl_pids: Vec<u32> = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .map(|(_, p, _)| *p)
            .collect();
        let guards = window.guard_calls_log();
        assert!(!guards.is_empty(), "windowed Vulkan 必須安裝關閉防護");
        for pid in &guards {
            assert!(wl_pids.contains(pid), "guard 目標必須是 spawned workload PID");
        }
        assert!(
            !window.calls_log().is_empty(),
            "windowed Vulkan 仍須 resize client area"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 內建 Vulkan fullscreen → 安裝關閉防護，但不 resize
    #[test]
    fn fullscreen_vulkan_guards_close_but_does_not_resize() {
        let root = temp_root("guard_fs");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.fullscreen = true;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);

        assert!(
            !window.guard_calls_log().is_empty(),
            "fullscreen Vulkan 也須安裝關閉防護"
        );
        assert!(
            window.calls_log().is_empty(),
            "fullscreen Vulkan 不該 resize"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// D3D9 → 不安裝關閉防護、不 resize
    #[test]
    fn d3d9_does_not_guard() {
        let root = temp_root("guard_d3d9");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.workload = WorkloadKind::D3D9;
        config.fullscreen = false;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);
        assert!(
            window.guard_calls_log().is_empty(),
            "D3D9 不該安裝關閉防護"
        );
        assert!(window.calls_log().is_empty(), "D3D9 不該 resize");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 自訂 Vulkan executable（workload_exe_path 覆寫）→ 不安裝關閉防護
    #[test]
    fn custom_vulkan_executable_does_not_guard() {
        let root = temp_root("guard_custom");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.workload_exe_path = Some("custom-lava.exe".to_string());
        config.fullscreen = true;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);
        assert!(
            window.guard_calls_log().is_empty(),
            "自訂 Vulkan exe 不該安裝關閉防護"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// guard 安裝失敗（helper 回 Err）→ 只 log warn，benchmark 仍正常完成
    #[test]
    fn guard_failure_does_not_fail_benchmark() {
        let root = temp_root("guard_fail");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let window = Arc::new(fake::FakeWindow::new());
        window
            .guard_result
            .lock()
            .unwrap()
            .replace(Err("guard 失敗".to_string()));
        let mut config = base_config();
        config.candidate_lps = vec![1];
        config.fullscreen = false;

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        ctx.window = window.clone();
        let result = run_benchmark(&mut ctx);
        assert_eq!(
            result.status,
            SessionStatus::Completed,
            "guard 失敗不得中斷 benchmark: err={:?}",
            result.error
        );
        assert!(
            !window.guard_calls_log().is_empty(),
            "guard 失敗前仍應有呼叫紀錄"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 驗證 retry 後所有新舊 workload/PresentMon PID 都被清理
    #[test]
    fn retry_cleans_up_all_pids() {
        let root = temp_root("retry_clean");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        // LP 1 第一次 missing → retry 成功
        processes.first_attempt_missing.lock().unwrap().insert(1);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);

        // 所有 spawned PIDs（workload + PresentMon）都必須被 kill
        let spawned: Vec<u32> = processes.spawn_log().iter().map(|(_, p, _)| *p).collect();
        let killed = processes.killed_log();
        for pid in &spawned {
            assert!(
                killed.contains(pid),
                "spawned PID {pid} 必須出現在 killed log"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// retry 產生兩個 attempt 的診斷檔，且 attempt 欄位正確
    #[test]
    fn retry_writes_per_attempt_diagnostics() {
        let root = temp_root("diag_retry");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        // LP 1 第一次 missing → retry 成功
        processes.first_attempt_missing.lock().unwrap().insert(1);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed);

        let session_dir = ctx.storage_root.join(&ctx.session_id);
        let diag_dir = session_dir.join("diag");

        // attempt 1 診斷檔
        let d1 = read_diag(&session_dir, 0, 1);
        assert_eq!(d1.attempt, 1);
        assert_eq!(d1.error.as_deref(), Some(codes::BENCHMARK_CAPTURE_MISSING));

        // attempt 2 診斷檔（獨立檔案）
        let d2_path = diag_dir.join("capture-round-0-lp-1-attempt-2.json");
        assert!(d2_path.exists(), "retry 診斷檔必須存在: {d2_path:?}");
        let d2_text = std::fs::read_to_string(&d2_path)
            .unwrap_or_else(|e| panic!("讀取 retry 診斷檔失敗: {e}"));
        let d2: CaptureDiagnostics =
            serde_json::from_str(&d2_text).expect("retry 診斷 JSON 解析失敗");
        assert_eq!(d2.attempt, 2);
        assert_eq!(d2.error, None, "retry 成功 error 應為 None");
        assert_ne!(
            d2.workload_pid, d1.workload_pid,
            "retry 必須用新 workload PID"
        );
        assert!(
            d2.workload_pid != 0 && d1.workload_pid != 0,
            "兩次 attempt 都應有有效 workload PID"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 部分失敗路徑的 summary.sample_count 反映已完成的 LP 結果
    #[test]
    fn partial_failure_sample_count_reflects_completed_lps() {
        let root = temp_root("partial_samples");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        backend.set_policy(AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        });
        let processes = Arc::new(FakeProcessRunner::new());
        // 所有 LP 共用一個有效 CSV（避免 csv_for_lp header 重複）
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        // 中間的 LP 2 第一次與所有 retry 都 missing；LP 3 仍應繼續完成
        processes.first_attempt_missing.lock().unwrap().insert(2);
        processes
            .second_attempt_also_missing
            .lock()
            .unwrap()
            .insert(2);
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1, 2, 3];

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);

        assert_eq!(result.status, SessionStatus::Failed);
        // LP1、LP3 各有 sample_count，證明中間 LP 失敗不會中止 session。
        assert_eq!(result.detail.results.len(), 2, "應保留 LP1, LP3 結果");
        assert_eq!(
            result
                .detail
                .results
                .iter()
                .map(|r| r.lp)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        let total_samples: u32 = result.detail.results.iter().map(|r| r.sample_count).sum();
        assert_eq!(total_samples, 300, "LP1 150 + LP3 150 = 300");
        assert_eq!(
            result.detail.summary.sample_count, total_samples,
            "summary.sample_count 必須等於已完成 LP 的 sample_count 總和"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 固定調適排程（3 篩選 + 2 確認）測試 ────────────────────────────────

    /// 由 PresentMon spawn log 統計每 LP 實際被 capture 的 round 集合。
    fn rounds_per_lp(processes: &FakeProcessRunner) -> HashMap<u32, Vec<u32>> {
        let mut map: HashMap<u32, Vec<u32>> = HashMap::new();
        for (name, _pid, args) in processes.spawn_log().iter() {
            if !name.contains("PresentMon") {
                continue;
            }
            let out = args
                .iter()
                .position(|a| a == "--output_file")
                .and_then(|i| args.get(i + 1))
                .cloned();
            let parsed = out.as_deref().and_then(|p| {
                let stem = std::path::Path::new(p).file_stem()?.to_str()?;
                let (head, lp_str) = stem.rsplit_once("-lp-")?;
                let lp: u32 = lp_str.parse().ok()?;
                let round: u32 = head.rsplit_once("round-")?.1.parse().ok()?;
                Some((round, lp))
            });
            if let Some((round, lp)) = parsed {
                map.entry(lp).or_default().push(round);
            }
        }
        for v in map.values_mut() {
            v.sort_unstable();
            v.dedup();
        }
        map
    }

    /// N=3：精確執行 N*3 + 4 = 13 次 capture，且只有前 2 名 finalists 進 rounds 3/4。
    #[test]
    fn adaptive_run_exact_counts_and_only_top_two_confirmed() {
        let root = temp_root("adaptive_counts");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let config = base_config(); // candidate_lps [1,2,3], repetitions 5

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed, "err={:?}", result.error);

        let wl_spawns = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .count();
        assert_eq!(wl_spawns, 13, "N=3 應為 3*3 + 2*2 = 13 次 capture");

        // 同內容 CSV → 穩健排名平手 → finalists = [1, 2]；LP3 只測篩選 3 round
        let rl = rounds_per_lp(&processes);
        assert_eq!(rl[&1], vec![0, 1, 2, 3, 4]);
        assert_eq!(rl[&2], vec![0, 1, 2, 3, 4]);
        assert_eq!(rl[&3], vec![0, 1, 2]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 篩選平手：同分數時依中位數 → worst-round → 較小 LP 決定，取前 2。
    #[test]
    fn select_finalists_deterministic_tie_picks_lower_lps() {
        let dir = temp_root("sel_tie");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for lp in 0..3u32 {
            for round in 0..3u32 {
                round_csvs
                    .entry(lp)
                    .or_default()
                    .insert(round, write_round_csv(&dir, round, lp, 10.0));
            }
        }
        let finalists = select_finalists(&round_csvs, SCREENING_ROUNDS);
        assert_eq!(finalists, vec![0, 1]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 篩選少於兩個完整候選 → 回傳空（呼叫端跳過確認）。
    #[test]
    fn select_finalists_empty_when_fewer_than_two_complete() {
        let dir = temp_root("sel_few");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..3u32 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
        }
        // LP1 缺 round 2 → 非完整候選
        for round in 0..2u32 {
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 11.0));
        }
        assert!(select_finalists(&round_csvs, SCREENING_ROUNDS).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非 finalist（僅 3 篩選 round）不得使成功 session 變 Inconclusive；
    /// 最終 best 由兩個 5-round finalists 比較得出。
    #[test]
    fn non_finalist_three_rounds_does_not_invalidate_success() {
        let dir = temp_root("rel_adaptive");
        let mut round_csvs: HashMap<u32, HashMap<u32, PathBuf>> = HashMap::new();
        for round in 0..5 {
            round_csvs
                .entry(0)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 0, 10.0));
            round_csvs
                .entry(1)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 1, 11.0));
        }
        // LP2 只測篩選 3 round（8ms 最快），非 finalist，不得影響最終可靠性
        for round in 0..3 {
            round_csvs
                .entry(2)
                .or_default()
                .insert(round, write_round_csv(&dir, round, 2, 8.0));
        }
        let results = vec![
            lp_res(0, 100.0, 100.0, 100.0, 0.0),
            lp_res(1, 90.909, 90.909, 90.909, 0.0),
            lp_res(2, 125.0, 125.0, 125.0, 0.0),
        ];
        let rel = compute_reliability(&round_csvs, &results, TOTAL_ROUNDS);
        assert_eq!(rel.status, ReliabilityStatus::Passed);
        assert_eq!(rel.candidate_lp, Some(0));
        assert_eq!(rel.runner_up_lp, Some(1));
        assert_eq!(rel.candidate_wins, 5);
        assert!(
            !rel.per_round_winners.contains(&Some(2)),
            "3-round 非 finalist 不得成為勝者"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// N=1：只測 3 篩選 round 即停（不浪費 2 確認 round），session Completed 但
    /// 可靠性 Inconclusive（少於兩個 finalists，無法 Passed/Equivalent）。
    #[test]
    fn single_lp_skips_confirmation_and_stays_inconclusive() {
        let root = temp_root("n1");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let mut config = base_config();
        config.candidate_lps = vec![1];

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Completed, "err={:?}", result.error);
        assert_eq!(result.best_lp, None, "單一 LP 不得有推薦");
        assert_eq!(
            result.detail.summary.reliability.status,
            ReliabilityStatus::Inconclusive
        );
        let wl_spawns = processes
            .spawn_log()
            .iter()
            .filter(|(n, _, _)| !n.contains("PresentMon"))
            .count();
        assert_eq!(wl_spawns, 3, "N=1 只測 3 篩選 round，無確認");
        assert_eq!(rounds_per_lp(&processes)[&1], vec![0, 1, 2]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 進度在兩階段過渡時單調遞增、最終達 100 且不超過 100。
    #[test]
    fn progress_monotonic_and_capped_at_100() {
        let root = temp_root("progress_adaptive");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = FakeCancel::new();
        let events: Arc<std::sync::Mutex<Vec<BenchmarkProgress>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let ev = events.clone();

        let mut ctx = build_ctx(
            &root,
            backend as Arc<dyn GpuBackend>,
            processes as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            base_config(),
            &journal,
            Some(Box::new(move |p| ev.lock().unwrap().push(p.clone()))),
        );
        run_benchmark(&mut ctx);
        let pcts: Vec<u32> = events.lock().unwrap().iter().map(|p| p.percentage).collect();
        assert!(!pcts.is_empty());
        assert!(pcts.iter().all(|&p| p <= 100), "進度不得超過 100");
        assert_eq!(*pcts.last().unwrap(), 100, "最終進度應達 100");
        for w in pcts.windows(2) {
            assert!(w[0] <= w[1], "進度不得倒退: {pcts:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 確認階段（round 3 起）收到取消 → Cancelled 並還原。
    #[test]
    fn cancel_during_confirmation_aborts_and_restores() {
        let root = temp_root("cancel_confirm");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        let cancel = Arc::new(FakeCancel::new());
        let cancel2 = cancel.clone();
        let config = base_config(); // candidate_lps [1,2,3]

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            cancel as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            Some(Box::new(move |p| {
                // 進入確認階段（round 3）即觸發取消
                if p.round == Some(SCREENING_ROUNDS) {
                    cancel2.set(true);
                }
            })),
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Cancelled);
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原原始策略");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 確認階段某 finalist 的 PresentMon spawn 失敗 → Failed 且還原。
    #[test]
    fn error_during_confirmation_fails_and_restores() {
        let root = temp_root("err_confirm");
        let journal = root.join("journal.json");
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)]));
        let baseline = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot::dword(4),
            assignment_set_override: RegistryValueSnapshot::binary(vec![0x08]),
        };
        backend.set_policy(baseline.clone());
        let processes = Arc::new(FakeProcessRunner::new());
        processes
            .presentmon_csv
            .lock()
            .unwrap()
            .push_str(&csv_for_lp(0));
        // 只有 round 3（確認階段）的 PresentMon spawn 失敗
        processes.fail_presentmon_rounds.lock().unwrap().insert(3);
        let cancel = FakeCancel::new();
        let config = base_config(); // candidate_lps [1,2,3]

        let mut ctx = build_ctx(
            &root,
            backend.clone() as Arc<dyn GpuBackend>,
            processes.clone() as Arc<dyn ProcessRunner>,
            Arc::new(cancel) as Arc<dyn CancelSignal>,
            Arc::new(NoopSleeper) as Arc<dyn Sleep>,
            config,
            &journal,
            None,
        );
        let result = run_benchmark(&mut ctx);
        assert_eq!(result.status, SessionStatus::Failed);
        assert_eq!(result.error.as_deref(), Some(codes::BENCHMARK_PRESENTMON_FAILED));
        assert_eq!(result.best_lp, None, "失敗不該有推薦");
        assert_eq!(result.detail.results.len(), 3, "篩選 3 round 的部分結果應保留");
        assert_eq!(backend.current_policy(GPU_A), baseline, "必須還原原始策略");
        assert!(!journal.exists());
        // 確認階段確實被觸及（round 3 的 PresentMon spawn 曾發生並失敗）
        let reached_round3 = processes.spawn_log().iter().any(|(n, _, args)| {
            n.contains("PresentMon") && args.iter().any(|a| a.contains("round-3-lp-"))
        });
        assert!(reached_round3, "應在確認階段 round 3 觸發失敗");
        let _ = std::fs::remove_dir_all(&root);
    }
}
