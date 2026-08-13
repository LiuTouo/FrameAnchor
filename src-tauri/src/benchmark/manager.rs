//! 基準測試管理者（Task 1 骨架）：AppState 持有 `BenchmarkManager`，
//! 提供狀態查詢、取消訊號、以及「套用最佳 LP」「還原先前策略」「啟動還原」。
//!
//! 所有會動系統的協調都寫成接受注入路徑的 free function，
//! 單元測試用 fake backend + 暫存目錄跑完整流程，不碰真實 HKLM/裝置。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use tauri::{AppHandle, Emitter, Manager};

use crate::config;
use crate::error::codes;
use crate::gpu::{
    policy_matches, restore_snapshot, single_lp_mask_bytes, AffinityPolicy, GpuBackend,
    RealSleeper, RegistryValueSnapshot, Sleep, DEVICE_POLICY_SINGLE_PROCESSOR,
};
use crate::topology::Topology;

use super::assets::{self, BenchmarkAssets};
use super::process_win::RealProcessRunner;
use super::recovery::{self, RecoveryJournal, RecoveryStage};
use super::runner::{self, CancelSignal, ProcessRunner, RunContext};
use super::storage;
use super::window_win::RealWorkloadWindow;
use super::{
    cpu_fingerprint_with, detect_cpu_identity, ApplyStatus, BenchmarkConfig, BenchmarkStage,
    BenchmarkState, CpuIdentity, SessionStatus, SessionSummary, WorkloadKind,
};

/// 一層還原記錄檔：`%APPDATA%\FrameAnchor\gpu-restore.json`。
/// 只保留最近一次成功套用的快照（one-level）。
pub fn restore_record_path() -> PathBuf {
    config::config_dir().join("gpu-restore.json")
}

// ── 協調流程（free function，可注入路徑測試）────────────────────────────

/// 把已完成 session 的最佳 LP 套用到對應 GPU。
/// 步驟：相容性驗證 → BasicDisplay 防呆 → 委派到 [`apply_affinity_to_gpu`]。
/// `sleeper` 與 `cpu_identity` 注入，讓測試不真睡、不依賴真實 CPU 身分。
pub fn apply_best_affinity(
    backend: &dyn GpuBackend,
    sleeper: &dyn Sleep,
    cpu_identity: &CpuIdentity,
    topo: &Topology,
    storage_root: &Path,
    journal_path: &Path,
    restore_path: &Path,
    session_id: &str,
) -> Result<(), String> {
    // 1) session 存在且已完成、有最佳 LP
    let detail = storage::get_at(storage_root, session_id)?;
    if detail.summary.status != SessionStatus::Completed {
        return Err(codes::BENCHMARK_SESSION_NOT_COMPLETED.to_string());
    }
    let best_lp = detail
        .summary
        .best_lp
        .ok_or_else(|| codes::BENCHMARK_SESSION_NOT_COMPLETED.to_string())?;
    // AssignmentSetOverride 是 64-bit 單 LP mask（REG_BINARY）；但 LP 必須落在
    // 拓撲實際存在、且 group 0 上限（64）以內
    if best_lp >= topo.total_lp.min(64) {
        return Err(codes::BENCHMARK_SESSION_INCOMPATIBLE.to_string());
    }

    // 2) 相容性：CPU 指紋與 GPU instance 必須與本次環境一致
    if detail.summary.cpu_fingerprint != cpu_fingerprint_with(topo, cpu_identity) {
        return Err(codes::BENCHMARK_SESSION_INCOMPATIBLE.to_string());
    }
    let instance_id = &detail.summary.gpu_instance_id;
    let present = backend
        .enumerate_present_adapters()
        .map_err(|e| e.code().to_string())?
        .iter()
        .any(|d| d.instance_id.eq_ignore_ascii_case(instance_id));
    if !present {
        return Err(codes::GPU_NOT_FOUND.to_string());
    }

    // 3) BasicDisplay 未停用（避免重啟後無顯示 fallback）
    if !backend
        .basic_display_enabled()
        .map_err(|e| e.code().to_string())?
    {
        return Err(codes::GPU_BASIC_DISPLAY_DISABLED.to_string());
    }

    // 4) 委派到共享 mutation 路徑
    apply_affinity_to_gpu(backend, sleeper, instance_id, best_lp, journal_path, restore_path)
}

/// 將 GPU 中斷親和性套用到指定 LP。
/// 步驟：快照 + 還原日誌 → 寫策略 → 重啟裝置 → 驗證 → 持久化還原記錄 → 清除日誌。
/// 任何失敗都嘗試還原「本次快照」；還原成功才清日誌，失敗保留日誌等啟動重試。
/// 呼叫端負責所有前置驗證（LP 範圍、GPU 存在、BasicDisplay、recovery_required）。
pub fn apply_affinity_to_gpu(
    backend: &dyn GpuBackend,
    sleeper: &dyn Sleep,
    instance_id: &str,
    lp: u32,
    journal_path: &Path,
    restore_path: &Path,
) -> Result<(), String> {
    // 1) 快照目前策略 + 寫還原日誌（第一次變更之前）
    let snapshot = backend
        .read_affinity_policy(instance_id)
        .map_err(|e| e.code().to_string())?;
    recovery::begin_at(journal_path, &snapshot)?;

    // 2) 寫入新策略：DevicePolicy=4（DWORD）+ AssignmentSetOverride=單 LP mask（REG_BINARY）
    let override_bytes = single_lp_mask_bytes(lp);
    let new_policy = AffinityPolicy {
        instance_id: instance_id.to_string(),
        device_policy: RegistryValueSnapshot::dword(DEVICE_POLICY_SINGLE_PROCESSOR),
        assignment_set_override: RegistryValueSnapshot::binary(override_bytes.clone()),
    };
    if let Err(_e) = backend.write_affinity_policy(&new_policy) {
        let restored = restore_snapshot(backend, sleeper, &snapshot).is_ok();
        if restored {
            let _ = recovery::clear_at(journal_path);
        }
        return Err(codes::GPU_APPLY_FAILED.to_string());
    }
    let journal = require_journal(journal_path)?;
    recovery::advance_to_at(journal_path, &journal, RecoveryStage::PolicyApplied)?;

    // 3) 重啟裝置（disable→停頓→enable→停頓）
    if let Err(_e) = backend.restart_device(instance_id, sleeper) {
        let restored = restore_snapshot(backend, sleeper, &snapshot).is_ok();
        if restored {
            let _ = recovery::clear_at(journal_path);
        }
        return Err(codes::GPU_RESTART_FAILED.to_string());
    }
    let journal = require_journal(journal_path)?;
    recovery::advance_to_at(journal_path, &journal, RecoveryStage::DeviceRestarted)?;

    // 4) 驗證新策略已生效（AssignmentSetOverride 逐位元組比對）
    let read_back = backend
        .read_affinity_policy(instance_id)
        .map_err(|e| e.code().to_string())?;
    if read_back.device_policy.as_dword() != Some(DEVICE_POLICY_SINGLE_PROCESSOR)
        || read_back.assignment_set_override.bytes.as_deref() != Some(override_bytes.as_slice())
    {
        let restored = restore_snapshot(backend, sleeper, &snapshot).is_ok();
        if restored {
            let _ = recovery::clear_at(journal_path);
        }
        return Err(codes::GPU_APPLY_FAILED.to_string());
    }

    // 5) 持久化一層還原記錄，清除日誌
    write_restore_record(restore_path, &snapshot)?;
    recovery::clear_at(journal_path)?;
    Ok(())
}

/// 還原到「上次成功套用」之前的策略（一層還原記錄）。
pub fn restore_previous_affinity(
    backend: &dyn GpuBackend,
    sleeper: &dyn Sleep,
    restore_path: &Path,
) -> Result<(), String> {
    let snapshot =
        load_restore_record(restore_path)?.ok_or_else(|| codes::GPU_RESTORE_FAILED.to_string())?;
    restore_snapshot(backend, sleeper, &snapshot)?;
    clear_restore_record(restore_path)?;
    Ok(())
}

/// 啟動時呼叫：存在 pending 日誌則依 stage 還原並清除；失敗回傳 Err。
pub fn attempt_startup_recovery(
    backend: &dyn GpuBackend,
    sleeper: &dyn Sleep,
    journal_path: &Path,
) -> Result<(), String> {
    let Some(journal) = recovery::load_from(journal_path)? else {
        return Ok(());
    };
    match journal.stage {
        // 尚未改寫任何策略：驗證目前仍等於快照即可清除
        RecoveryStage::SnapshotTaken => {
            let current = backend
                .read_affinity_policy(&journal.instance_id)
                .map_err(|e| e.code().to_string())?;
            if !policy_matches(&journal.snapshot, &current) {
                return Err(codes::GPU_RESTORE_FAILED.to_string());
            }
        }
        // 已寫入/已重啟：完整還原（寫回 + 重啟 + 驗證）
        RecoveryStage::PolicyApplied | RecoveryStage::DeviceRestarted => {
            restore_snapshot(backend, sleeper, &journal.snapshot)?;
        }
    }
    recovery::clear_at(journal_path)
}

fn require_journal(journal_path: &Path) -> Result<RecoveryJournal, String> {
    // 內部不變量防呆：apply 中途日誌不可能消失；若發生回穩定代碼
    recovery::load_from(journal_path)?.ok_or_else(|| codes::GPU_APPLY_FAILED.to_string())
}

// ── 一層還原記錄 ────────────────────────────────────────────────────────

fn write_restore_record(path: &Path, snapshot: &AffinityPolicy) -> Result<(), String> {
    let text = serde_json::to_string_pretty(snapshot).map_err(|e| format!("序列化: {e}"))?;
    config::atomic_write(path, &text)
}

fn load_restore_record(path: &Path) -> Result<Option<AffinityPolicy>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("還原記錄解析失敗: {e}"))
}

fn clear_restore_record(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("清除還原記錄失敗: {e}")),
    }
}

// ── AppState 持有的管理者 ───────────────────────────────────────────────

/// 基準測試管理者骨架。state 為執行期狀態；recovery_required 標記
/// 啟動還原失敗（封鎖新的 test/apply）。cancel 為 Task 2 runner 用的訊號。
pub struct BenchmarkManager {
    pub state: RwLock<BenchmarkState>,
    pub backend: Arc<dyn GpuBackend>,
    /// 重啟等待策略（Task 2 亦共用）
    sleeper: Arc<dyn Sleep>,
    pub recovery_required: AtomicBool,
    cancel_tx: tokio::sync::watch::Sender<bool>,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
}

impl BenchmarkManager {
    pub fn new(backend: Arc<dyn GpuBackend>) -> Self {
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        Self {
            state: RwLock::new(BenchmarkState::default()),
            backend,
            sleeper: Arc::new(RealSleeper),
            recovery_required: AtomicBool::new(false),
            cancel_tx,
            cancel_rx,
        }
    }

    /// 啟動時呼叫：存在 pending 還原日誌則嘗試還原。
    /// 失敗 → recovery_required=true，封鎖新的 test/apply。
    pub fn attempt_startup_recovery(&self) {
        match attempt_startup_recovery(
            self.backend.as_ref(),
            self.sleeper.as_ref(),
            &recovery::recovery_path(),
        ) {
            Ok(()) => log::info!("啟動還原：無 pending 或已還原"),
            Err(e) => {
                log::error!("啟動還原失敗: {e}；封鎖基準測試與套用操作");
                self.recovery_required.store(true, Ordering::Relaxed);
                if let Ok(mut s) = self.state.write() {
                    s.recovery_required = true;
                }
            }
        }
    }

    pub fn recovery_required(&self) -> bool {
        self.recovery_required.load(Ordering::Relaxed)
    }

    /// 基準測試執行中？
    pub fn is_running(&self) -> bool {
        self.state
            .read()
            .map(|s| s.status == SessionStatus::Running)
            .unwrap_or(false)
    }

    /// 執行中 → 拒絕退出/重啟（讓 runner 完成或安全取消/還原），
    /// 非執行中（Idle/Completed/Failed/Cancelled）→ 允許。
    pub fn refuse_exit_if_running(&self) -> Result<(), String> {
        if self.is_running() {
            Err(codes::BENCHMARK_ALREADY_RUNNING.to_string())
        } else {
            Ok(())
        }
    }

    /// 回傳目前狀態（附 recovery_required）
    pub fn state_snapshot(&self) -> BenchmarkState {
        let mut s = self.state.read().map(|s| s.clone()).unwrap_or_default();
        s.recovery_required = self.recovery_required();
        s
    }

    /// 套用最佳 LP。recovery 未完成時封鎖。
    pub fn apply_best(&self, topo: &Topology, session_id: &str) -> Result<(), String> {
        if self.recovery_required() {
            return Err(codes::BENCHMARK_RECOVERY_REQUIRED.to_string());
        }
        apply_best_affinity(
            self.backend.as_ref(),
            self.sleeper.as_ref(),
            &detect_cpu_identity(),
            topo,
            &storage::benchmarks_root(),
            &recovery::recovery_path(),
            &restore_record_path(),
            session_id,
        )
    }

    /// 只判定某個歷史 session「現在可否套用」（不做任何變更）。
    /// 相容性判定全在後端，前端不重算。
    pub fn check_apply(&self, topo: &Topology, session_id: &str) -> ApplyStatus {
        let cannot = |reason: &str| ApplyStatus {
            can_apply: false,
            reason: Some(reason.to_string()),
        };
        if self.recovery_required() {
            return cannot(codes::BENCHMARK_RECOVERY_REQUIRED);
        }
        let Ok(detail) = storage::get_at(&storage::benchmarks_root(), session_id) else {
            return cannot(codes::BENCHMARK_SESSION_NOT_FOUND);
        };
        if detail.summary.status != SessionStatus::Completed || detail.summary.best_lp.is_none() {
            return cannot(codes::BENCHMARK_SESSION_NOT_COMPLETED);
        }
        let best_lp = detail.summary.best_lp.unwrap();
        if best_lp >= topo.total_lp.min(64) {
            return cannot(codes::BENCHMARK_SESSION_INCOMPATIBLE);
        }
        if detail.summary.cpu_fingerprint != cpu_fingerprint_with(topo, &detect_cpu_identity()) {
            return cannot(codes::BENCHMARK_SESSION_INCOMPATIBLE);
        }
        let present = self
            .backend
            .enumerate_present_adapters()
            .map(|a| {
                a.iter().any(|d| {
                    d.instance_id
                        .eq_ignore_ascii_case(&detail.summary.gpu_instance_id)
                })
            })
            .unwrap_or(false);
        if !present {
            return cannot(codes::GPU_NOT_FOUND);
        }
        if !self.backend.basic_display_enabled().unwrap_or(false) {
            return cannot(codes::GPU_BASIC_DISPLAY_DISABLED);
        }
        ApplyStatus {
            can_apply: true,
            reason: None,
        }
    }

    /// 可匯入的已完成 session（相容：CPU 指紋一致 + GPU 存在 + 有 bestLp）。
    /// 相容性判定只在後端，前端不重算。
    pub fn list_importable(&self, topo: &Topology) -> Vec<SessionSummary> {
        list_importable(
            self.backend.as_ref(),
            topo,
            &detect_cpu_identity(),
            &storage::benchmarks_root(),
        )
    }

    /// 手動套用 GPU 中斷親和性到指定 LP。前置驗證：recovery、執行中、
    /// LP 範圍、GPU 存在、BasicDisplay；驗證後委派到共享 mutation 路徑。
    pub fn apply_gpu_affinity(
        &self,
        topo: &Topology,
        instance_id: &str,
        lp: u32,
    ) -> Result<(), String> {
        self.apply_gpu_affinity_at(
            topo,
            instance_id,
            lp,
            &recovery::recovery_path(),
            &restore_record_path(),
        )
    }

    /// 手動套用 GPU 中斷親和性到指定 LP（可注入還原日誌與還原記錄路徑供測試隔離）。
    /// 前置驗證與 [`apply_gpu_affinity`] 相同。
    pub fn apply_gpu_affinity_at(
        &self,
        topo: &Topology,
        instance_id: &str,
        lp: u32,
        journal_path: &Path,
        restore_path: &Path,
    ) -> Result<(), String> {
        if self.recovery_required() {
            return Err(codes::BENCHMARK_RECOVERY_REQUIRED.to_string());
        }
        if self.is_running() {
            return Err(codes::BENCHMARK_ALREADY_RUNNING.to_string());
        }
        if lp >= topo.total_lp.min(64) {
            return Err(codes::BENCHMARK_SESSION_INCOMPATIBLE.to_string());
        }
        let present = self
            .backend
            .enumerate_present_adapters()
            .map_err(|e| e.code().to_string())?
            .iter()
            .any(|d| d.instance_id.eq_ignore_ascii_case(instance_id));
        if !present {
            return Err(codes::GPU_NOT_FOUND.to_string());
        }
        if !self
            .backend
            .basic_display_enabled()
            .map_err(|e| e.code().to_string())?
        {
            return Err(codes::GPU_BASIC_DISPLAY_DISABLED.to_string());
        }
        apply_affinity_to_gpu(
            self.backend.as_ref(),
            self.sleeper.as_ref(),
            instance_id,
            lp,
            journal_path,
            restore_path,
        )
    }

    /// 還原到先前策略。這是使用者顯式還原，不因 recovery_required 封鎖。
    pub fn restore_previous(&self) -> Result<(), String> {
        restore_previous_affinity(
            self.backend.as_ref(),
            self.sleeper.as_ref(),
            &restore_record_path(),
        )
    }

    /// 開始基準測試：單一並行的背景 session。前置驗證（config、資源 hash、
    /// GPU 存在、BasicDisplay、未在 RecoveryRequired）通過後立即回傳 Ok。
    pub fn start(
        self: &Arc<Self>,
        app: &AppHandle,
        topo: &Topology,
        config: BenchmarkConfig,
    ) -> Result<(), String> {
        if self.recovery_required() {
            return Err(codes::BENCHMARK_RECOVERY_REQUIRED.to_string());
        }
        // 前置驗證（先於標記 Running，讓使用者即時拿到錯誤）
        runner::validate_config(&config, topo)?;
        let assets = resolve_assets(app, &config)?;
        assets::verify(&assets).map_err(|e| {
            log::error!("基準測試資源驗證失敗: {e}");
            e.code().to_string()
        })?;
        let instance = config
            .gpu_instance_id
            .clone()
            .ok_or_else(|| codes::BENCHMARK_INVALID_CONFIG.to_string())?;
        let present = self
            .backend
            .enumerate_present_adapters()
            .map_err(|e| e.code().to_string())?
            .iter()
            .any(|d| d.instance_id.eq_ignore_ascii_case(&instance));
        if !present {
            return Err(codes::GPU_NOT_FOUND.to_string());
        }
        if !self
            .backend
            .basic_display_enabled()
            .map_err(|e| e.code().to_string())?
        {
            return Err(codes::GPU_BASIC_DISPLAY_DISABLED.to_string());
        }

        // 並行守衛
        {
            let st = self.state.read().map_err(|e| e.to_string())?;
            if st.status == SessionStatus::Running {
                return Err(codes::BENCHMARK_ALREADY_RUNNING.to_string());
            }
        }
        let sid = uuid::Uuid::new_v4().to_string();
        {
            let mut st = self.state.write().map_err(|e| e.to_string())?;
            st.status = SessionStatus::Running;
            st.session_id = Some(sid.clone());
            st.stage = BenchmarkStage::Init;
            st.progress_pct = 0;
            st.elapsed_secs = 0;
            st.current_lp = None;
            st.cancel_requested = false;
        }

        let process_runner: Arc<dyn ProcessRunner> = Arc::new(RealProcessRunner::new());
        let cancel: Arc<dyn CancelSignal> = Arc::new(ManagerCancel {
            rx: self.cancel_rx.clone(),
        });

        let manager_progress = self.clone();
        let manager_done = self.clone();
        let app_emit = app.clone();
        let started = std::time::Instant::now();
        let mut ctx = RunContext {
            backend: self.backend.clone(),
            sleeper: self.sleeper.clone(),
            processes: process_runner,
            cancel,
            topo: topo.clone(),
            cpu_identity: detect_cpu_identity(),
            assets,
            storage_root: storage::benchmarks_root(),
            journal_path: recovery::recovery_path(),
            session_id: sid.clone(),
            config: config.clone(),
            on_progress: Box::new(move |p| {
                if let Ok(mut st) = manager_progress.state.write() {
                    st.session_id = Some(p.session_id.clone());
                    st.stage = runner_stage_to_enum(&p.stage);
                    st.current_lp = p.lp;
                    st.progress_pct = p.percentage;
                    st.elapsed_secs = started.elapsed().as_secs();
                }
                let _ = app_emit.emit("gpu-benchmark-progress", p);
            }),
            baseline: None,
            owned_processes: Vec::new(),
            window: Arc::new(RealWorkloadWindow::new()),
        };

        tauri::async_runtime::spawn_blocking(move || {
            let result = runner::run_benchmark(&mut ctx);
            log::info!(
                "基準測試 session {} 結束: status={:?}, best_lp={:?}, severe_lps={:?}, recommended={:?}",
                result.detail.summary.id,
                result.status,
                result.best_lp,
                result.severe_lps,
                result.recommended_cores
            );
            if let Some(e) = &result.error {
                log::error!("基準測試失敗原因: {e}");
            }
            if let Ok(mut st) = manager_done.state.write() {
                st.status = result.status;
                st.progress_pct = 100;
                st.stage = BenchmarkStage::Finalizing;
                st.current_lp = None;
                st.elapsed_secs = started.elapsed().as_secs();
            }
            if result.recovery_required {
                manager_done
                    .recovery_required
                    .store(true, Ordering::Relaxed);
                if let Ok(mut st) = manager_done.state.write() {
                    st.recovery_required = true;
                }
            }
        });
        Ok(())
    }

    /// 請求取消正在跑的基準測試（runner 在安全階段邊界檢查）。
    pub fn request_cancel(&self) {
        let _ = self.cancel_tx.send(true);
        if let Ok(mut s) = self.state.write() {
            s.cancel_requested = true;
        }
    }

    pub fn cancel_requested(&self) -> bool {
        *self.cancel_rx.borrow()
    }

    pub fn cancel_receiver(&self) -> tokio::sync::watch::Receiver<bool> {
        self.cancel_rx.clone()
    }
}

/// 可匯入的已完成 session（free function，注入路徑/身分供測試）。
fn list_importable(
    backend: &dyn GpuBackend,
    topo: &Topology,
    cpu_identity: &CpuIdentity,
    storage_root: &Path,
) -> Vec<SessionSummary> {
    let current_fp = cpu_fingerprint_with(topo, cpu_identity);
    let present: Vec<String> = backend
        .enumerate_present_adapters()
        .map(|a| a.iter().map(|d| d.instance_id.clone()).collect())
        .unwrap_or_default();
    storage::list_at(storage_root)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| {
            s.status == SessionStatus::Completed
                && s.best_lp.is_some()
                && s.cpu_fingerprint == current_fp
                && present
                    .iter()
                    .any(|g| g.eq_ignore_ascii_case(&s.gpu_instance_id))
        })
        .collect()
}

/// 由 AppHandle 解析內建資源目錄（tauri.conf.json `bundle.resources`）。
/// Windows 上 `resource_dir()` 等於 exe 所在目錄，而 `resources/**` 會以完整
/// 相對路徑（含 `resources/` 前綴）安裝到該目錄 → 實際位置是 `resources/benchmark`。
fn resolve_assets(app: &AppHandle, config: &BenchmarkConfig) -> Result<BenchmarkAssets, String> {
    let dir = app
        .path()
        .resolve("resources/benchmark", tauri::path::BaseDirectory::Resource)
        .map_err(|e| format!("資源目錄解析失敗: {e}"))?;
    let mut assets = assets::load(&dir);
    // 覆寫（測試/除錯用）
    match config.workload {
        WorkloadKind::Vulkan => {
            if let Some(p) = &config.workload_exe_path {
                assets.vulkan_workload = std::path::PathBuf::from(p);
            }
        }
        WorkloadKind::D3D9 => {
            if let Some(p) = &config.workload_exe_path {
                assets.d3d9_workload = std::path::PathBuf::from(p);
            }
        }
    }
    if let Some(p) = &config.presentmon_path {
        assets.presentmon = std::path::PathBuf::from(p);
    }
    Ok(assets)
}

/// runner 的 stage 字串 → BenchmarkStage（執行期狀態）
fn runner_stage_to_enum(stage: &str) -> BenchmarkStage {
    match stage {
        "collecting" => BenchmarkStage::Collecting,
        "finalizing" => BenchmarkStage::Finalizing,
        "applying" | "launching" | "restarting" => BenchmarkStage::Warmup,
        _ => BenchmarkStage::Init,
    }
}

/// manager 的 cancel watch channel 實作 CancelSignal
struct ManagerCancel {
    rx: tokio::sync::watch::Receiver<bool>,
}

impl CancelSignal for ManagerCancel {
    fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }
}

// ── 測試 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::{cpu_fingerprint_with, CpuIdentity, SessionDetail};
    use crate::gpu::fake::FakeBackend;
    use crate::gpu::{AffinityPolicy, GpuDevice, NoopSleeper, RegistryValueSnapshot};
    use crate::topology::{build_topology, Topology};
    use uuid::Uuid;
    use windows::Win32::System::Registry::{REG_BINARY, REG_DWORD};

    const GPU_A: &str = r"PCI\VEN_FAKE&DEV_1";
    const GPU_B: &str = r"PCI\VEN_FAKE&DEV_2";

    /// 測試用的固定 CPU 身分（x64 Intel），讓指紋完全確定、不依賴真實機器
    fn fixed_identity() -> CpuIdentity {
        CpuIdentity {
            architecture: 9, // PROCESSOR_ARCHITECTURE_AMD64
            family: 6,
            model: 183,
            stepping: 1,
        }
    }

    fn topo() -> Topology {
        build_topology((0..8u32).map(|c| (vec![c], 0, false)).collect())
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("frameanchor_mgr_{}_{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(dir.join("journal.json"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn device(instance: &str) -> GpuDevice {
        GpuDevice {
            instance_id: instance.to_string(),
            friendly_name: format!("GPU {instance}"),
        }
    }

    fn completed_session(storage_root: &Path, topo: &Topology, gpu: &str, best_lp: u32) -> String {
        let id = Uuid::new_v4().to_string();
        let detail = SessionDetail {
            summary: crate::benchmark::SessionSummary {
                id: id.clone(),
                status: SessionStatus::Completed,
                started_at: "2026-08-11T00:00:00Z".into(),
                finished_at: Some("2026-08-11T00:01:00Z".into()),
                gpu_name: "GPU".into(),
                gpu_instance_id: gpu.to_string(),
                cpu_fingerprint: cpu_fingerprint_with(topo, &fixed_identity()),
                best_lp: Some(best_lp),
                severe_lps: vec![],
                sample_count: 5,
                total_bytes: 0,
                config: BenchmarkConfig::default(),
                error: None,
            },
            results: vec![],
            samples: vec![],
        };
        storage::save_session_at(storage_root, &detail).unwrap();
        id
    }

    /// u32 → 精簡 little-endian bytes（尾端零移除），fixture 用
    fn le_trimmed(v: u32) -> Vec<u8> {
        let b = v.to_le_bytes();
        let mut len = b.len();
        while len > 0 && b[len - 1] == 0 {
            len -= 1;
        }
        b[..len].to_vec()
    }

    /// 既有策略 fixture：DevicePolicy 是 DWORD；AssignmentSetOverride 是 REG_BINARY
    fn policy_on(instance: &str, device_policy: u32, override_mask: u32) -> AffinityPolicy {
        AffinityPolicy {
            instance_id: instance.to_string(),
            device_policy: RegistryValueSnapshot::dword(device_policy),
            assignment_set_override: RegistryValueSnapshot::binary(le_trimmed(override_mask)),
        }
    }

    fn assert_dword(policy: &AffinityPolicy, name: &str, expected: u32) {
        let v = if name == "DevicePolicy" {
            policy.device_policy.as_dword()
        } else {
            policy.assignment_set_override.as_dword()
        };
        assert_eq!(v, Some(expected), "{name} 值不符");
    }

    /// 斷言 AssignmentSetOverride 是 REG_BINARY，且位元組等於給定 mask 的精簡 LE
    fn assert_override_mask(policy: &AffinityPolicy, mask: u32) {
        let o = &policy.assignment_set_override;
        assert_eq!(
            o.value_type,
            Some(REG_BINARY.0),
            "AssignmentSetOverride 應為 REG_BINARY"
        );
        assert_eq!(
            o.bytes.as_deref(),
            Some(le_trimmed(mask).as_slice()),
            "AssignmentSetOverride bytes 不符"
        );
    }

    #[test]
    fn apply_success_writes_policy_restarts_and_records() {
        let dir = temp_dir("ok");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        // 模擬既有策略（已鎖定 LP 0）
        backend.set_policy(policy_on(GPU_A, 4, 1));

        let sid = completed_session(&storage_root, &topo(), GPU_A, 5);
        apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid,
        )
        .unwrap();

        let cur = backend.current_policy(GPU_A);
        assert_dword(&cur, "DevicePolicy", DEVICE_POLICY_SINGLE_PROCESSOR);
        assert_override_mask(&cur, 1u32 << 5);
        assert_eq!(backend.restart_count(), 1);
        assert_eq!(backend.disable_attempts(), 1);
        assert_eq!(backend.enable_attempts(), 1); // disable 成功後有嘗試 enable
        assert!(!journal.exists(), "成功後日誌應清除");
        assert!(restore.exists(), "一層還原記錄應寫入");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// GPU 原本沒有 Affinity Policy 值（present=false）：套用成功，
    /// 還原時把兩個值都「刪除」回缺失狀態。
    #[test]
    fn apply_and_restore_missing_registry_values() {
        let dir = temp_dir("missingvals");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        // 不 set_policy → 兩值皆不存在
        assert!(!backend.current_policy(GPU_A).device_policy.present);

        let sid = completed_session(&storage_root, &topo(), GPU_A, 3);
        apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid,
        )
        .unwrap();
        assert_override_mask(&backend.current_policy(GPU_A), 1u32 << 3);

        // 還原 → 回到「不存在」
        restore_previous_affinity(&backend, &NoopSleeper, &restore).unwrap();
        let restored = backend.current_policy(GPU_A);
        assert!(!restored.device_policy.present);
        assert!(!restored.assignment_set_override.present);
        assert!(restored.device_policy.bytes.is_none());
        assert!(!restore.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_requires_completed_session() {
        let dir = temp_dir("notcomp");
        let storage_root = dir.join("benchmarks");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let id = Uuid::new_v4().to_string();
        let mut detail = SessionDetail::default();
        detail.summary.id = id.clone();
        detail.summary.status = SessionStatus::Running;
        detail.summary.gpu_instance_id = GPU_A.into();
        detail.summary.cpu_fingerprint = cpu_fingerprint_with(&topo(), &fixed_identity());
        storage::save_session_at(&storage_root, &detail).unwrap();

        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &dir.join("journal.json"),
            &dir.join("restore.json"),
            &id,
        )
        .unwrap_err();
        assert_eq!(err, codes::BENCHMARK_SESSION_NOT_COMPLETED);
        assert_eq!(backend.restart_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rejects_cpu_fingerprint_mismatch() {
        let dir = temp_dir("cpumismatch");
        let storage_root = dir.join("benchmarks");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        // 用「不同 CPU」建立 session（8C16T vs 8C 指紋不同）
        let id = Uuid::new_v4().to_string();
        let other_topo = build_topology(
            (0..8u32)
                .map(|c| (vec![c * 2, c * 2 + 1], 0, true))
                .collect(),
        );
        let mut detail = SessionDetail::default();
        detail.summary.id = id.clone();
        detail.summary.status = SessionStatus::Completed;
        detail.summary.gpu_instance_id = GPU_A.into();
        detail.summary.best_lp = Some(3);
        detail.summary.cpu_fingerprint = cpu_fingerprint_with(&other_topo, &fixed_identity());
        storage::save_session_at(&storage_root, &detail).unwrap();

        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &dir.join("journal.json"),
            &dir.join("restore.json"),
            &id,
        )
        .unwrap_err();
        assert_eq!(err, codes::BENCHMARK_SESSION_INCOMPATIBLE);
        assert_eq!(backend.restart_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rejects_gpu_not_present() {
        let dir = temp_dir("gpumissing");
        let storage_root = dir.join("benchmarks");
        // 本機只有 GPU_B，session 指向 GPU_A
        let backend = FakeBackend::new(vec![device(GPU_B)]);
        let sid = completed_session(&storage_root, &topo(), GPU_A, 3);
        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &dir.join("journal.json"),
            &dir.join("restore.json"),
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::GPU_NOT_FOUND);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_blocks_when_basic_display_disabled() {
        let dir = temp_dir("basic");
        let storage_root = dir.join("benchmarks");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        backend.basic_display_on.store(false, Ordering::SeqCst);
        let sid = completed_session(&storage_root, &topo(), GPU_A, 3);
        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &dir.join("journal.json"),
            &dir.join("restore.json"),
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::GPU_BASIC_DISPLAY_DISABLED);
        assert_eq!(backend.restart_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 64-LP 拓撲（group 0 上限）
    fn topo_64() -> Topology {
        build_topology(
            (0..32u32)
                .map(|c| (vec![c * 2, c * 2 + 1], 0, true))
                .collect(),
        )
    }

    /// LP 0/31/32/63 都能套用：AssignmentSetOverride 是 REG_BINARY 且位元組精確
    #[test]
    fn apply_lp_boundaries_write_reg_binary_masks() {
        let dir = temp_dir("lpbound");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let topo64 = topo_64();
        assert_eq!(topo64.total_lp, 64);

        for lp in [0u32, 31, 32, 63] {
            let sid = completed_session(&storage_root, &topo64, GPU_A, lp);
            apply_best_affinity(
                &backend,
                &NoopSleeper,
                &fixed_identity(),
                &topo64,
                &storage_root,
                &journal,
                &restore,
                &sid,
            )
            .unwrap();
            let cur = backend.current_policy(GPU_A);
            assert_eq!(
                cur.assignment_set_override.value_type,
                Some(REG_BINARY.0),
                "LP {lp} 應為 REG_BINARY"
            );
            assert_eq!(
                cur.assignment_set_override.bytes.as_deref(),
                Some(single_lp_mask_bytes(lp).as_slice()),
                "LP {lp} mask bytes 不符"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rejects_best_lp_64() {
        let dir = temp_dir("lp64");
        let storage_root = dir.join("benchmarks");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let topo64 = topo_64();
        let sid = completed_session(&storage_root, &topo64, GPU_A, 64);
        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo64,
            &storage_root,
            &dir.join("journal.json"),
            &dir.join("restore.json"),
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::BENCHMARK_SESSION_INCOMPATIBLE);
        assert_eq!(backend.restart_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rejects_best_lp_outside_topology() {
        let dir = temp_dir("lpoutside");
        let storage_root = dir.join("benchmarks");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        // 8-LP 拓撲，best_lp=8 超出實際 LP 範圍
        let sid = completed_session(&storage_root, &topo(), GPU_A, 8);
        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &dir.join("journal.json"),
            &dir.join("restore.json"),
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::BENCHMARK_SESSION_INCOMPATIBLE);
        assert_eq!(backend.restart_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 原本策略含非 DWORD 型別：還原必須逐型別、逐位元組還原
    #[test]
    fn restore_preserves_arbitrary_non_dword_types() {
        let dir = temp_dir("non_dword");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        // DevicePolicy=REG_SZ（值 1）+ AssignmentSetOverride=REG_BINARY
        let original = AffinityPolicy {
            instance_id: GPU_A.to_string(),
            device_policy: RegistryValueSnapshot {
                present: true,
                value_type: Some(1), // REG_SZ
                bytes: Some(vec![b'g', 0, b'p', 0, 0, 0]),
            },
            assignment_set_override: RegistryValueSnapshot {
                present: true,
                value_type: Some(REG_BINARY.0),
                bytes: Some(vec![0x00, 0x00, 0x00, 0x80]),
            },
        };
        backend.set_policy(original.clone());
        let sid = completed_session(&storage_root, &topo(), GPU_A, 4);
        apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid,
        )
        .unwrap();
        // 套用後 DevicePolicy 是 DWORD
        assert_eq!(
            backend.current_policy(GPU_A).device_policy.value_type,
            Some(REG_DWORD.0)
        );
        // 還原 → 型別與位元組都回到原本（含非 DWORD）
        restore_previous_affinity(&backend, &NoopSleeper, &restore).unwrap();
        assert_eq!(backend.current_policy(GPU_A), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_write_failure_restores_and_clears_journal() {
        let dir = temp_dir("writefail");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 2, 0b1);
        backend.set_policy(original.clone());
        let sid = completed_session(&storage_root, &topo(), GPU_A, 4);
        backend.fail_next_write(); // 第一次 write 失敗（apply 的 write）

        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::GPU_APPLY_FAILED);
        // 還原成功 → 策略與原快照逐位元組一致，日誌清除
        assert_eq!(backend.current_policy(GPU_A), original);
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_restart_disable_failure_restores_and_clears_journal() {
        let dir = temp_dir("restartfail");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 1, 0b11);
        backend.set_policy(original.clone());
        let sid = completed_session(&storage_root, &topo(), GPU_A, 4);
        backend.fail_next_restart.store(true, Ordering::SeqCst);

        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::GPU_RESTART_FAILED);
        // 套用 restart 失敗後，還原 restart 成功 → 策略還原、日誌清除
        assert_eq!(backend.current_policy(GPU_A), original);
        assert!(!journal.exists());
        assert!(restore.exists() == false, "套用未完成，不該寫入還原記錄");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_restart_failure_and_restore_failure_keeps_journal() {
        let dir = temp_dir("restorefail");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 1, 0b11);
        backend.set_policy(original.clone());
        let sid = completed_session(&storage_root, &topo(), GPU_A, 4);
        backend.disable_fails.store(true, Ordering::SeqCst); // 一直失敗

        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::GPU_RESTART_FAILED);
        // 策略已寫回（write 成功），但 restart 失敗 → 還原未驗證 → 保留日誌等啟動重試
        assert_eq!(backend.current_policy(GPU_A), original);
        assert!(journal.exists(), "還原失敗時必須保留日誌");
        assert!(!restore.exists(), "還原記錄不該寫入");
        assert_eq!(backend.enable_attempts(), 0, "disable 失敗不該嘗試 enable");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_enable_failure_attempts_enable_after_disable() {
        let dir = temp_dir("enablefail");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 1, 0b11);
        backend.set_policy(original.clone());
        let sid = completed_session(&storage_root, &topo(), GPU_A, 4);
        backend.enable_fails.store(true, Ordering::SeqCst);

        let err = apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &dir.join("restore.json"),
            &sid,
        )
        .unwrap_err();
        assert_eq!(err, codes::GPU_RESTART_FAILED);
        // 每次 restart：disable 成功 → 必嘗試 enable（總共 apply + restore 兩次）
        assert!(
            backend.enable_attempts() >= 2,
            "disable 成功後必須嘗試 enable"
        );
        assert_eq!(backend.current_policy(GPU_A), original);
        assert!(journal.exists(), "還原失敗 → 保留日誌");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_previous_restores_bytes_and_clears_record() {
        let dir = temp_dir("restoreprev");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 0, 0xFFFF);
        backend.set_policy(original.clone());
        let sid = completed_session(&storage_root, &topo(), GPU_A, 5);

        apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid,
        )
        .unwrap();
        // 現在策略已被改寫
        assert_override_mask(&backend.current_policy(GPU_A), 1u32 << 5);

        restore_previous_affinity(&backend, &NoopSleeper, &restore).unwrap();
        assert_eq!(backend.current_policy(GPU_A), original, "必須逐位元組還原");
        assert!(!restore.exists(), "還原後記錄清除");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_previous_without_record_errors() {
        let dir = temp_dir("norecord");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let err = restore_previous_affinity(&backend, &NoopSleeper, &dir.join("restore.json"))
            .unwrap_err();
        assert_eq!(err, codes::GPU_RESTORE_FAILED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_twice_is_one_level_record() {
        let dir = temp_dir("onelevel");
        let storage_root = dir.join("benchmarks");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A), device(GPU_B)]);
        let orig_a = policy_on(GPU_A, 0, 0xFFFF);
        let orig_b = policy_on(GPU_B, 0, 0xFF00);
        backend.set_policy(orig_a.clone());
        backend.set_policy(orig_b.clone());

        let sid_a = completed_session(&storage_root, &topo(), GPU_A, 1);
        apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid_a,
        )
        .unwrap();
        let sid_b = completed_session(&storage_root, &topo(), GPU_B, 2);
        apply_best_affinity(
            &backend,
            &NoopSleeper,
            &fixed_identity(),
            &topo(),
            &storage_root,
            &journal,
            &restore,
            &sid_b,
        )
        .unwrap();

        // 只保留最新一次的快照
        let rec = std::fs::read_to_string(&restore).unwrap();
        let saved: AffinityPolicy = serde_json::from_str(&rec).unwrap();
        assert_eq!(saved.instance_id, GPU_B);
        assert_eq!(saved, orig_b);

        // 還原 → 回到 GPU_B 原狀（GPU_A 維持套用後）
        restore_previous_affinity(&backend, &NoopSleeper, &restore).unwrap();
        assert_eq!(backend.current_policy(GPU_B), orig_b);
        assert_override_mask(&backend.current_policy(GPU_A), 1u32 << 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 啟動還原 ──

    #[test]
    fn startup_recovery_no_journal_ok() {
        let dir = temp_dir("recovery_none");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        assert!(
            attempt_startup_recovery(&backend, &NoopSleeper, &dir.join("journal.json")).is_ok()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_recovery_restores_policy_applied_crash() {
        let dir = temp_dir("recovery_crash");
        let journal = dir.join("journal.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 2, 0b1);
        backend.set_policy(original.clone());

        // 模擬崩潰：快照後已寫入新策略、還未完成（stage=PolicyApplied）
        recovery::begin_at(&journal, &original).unwrap();
        let j = recovery::load_from(&journal).unwrap().unwrap();
        recovery::advance_to_at(&journal, &j, RecoveryStage::PolicyApplied).unwrap();
        backend.set_policy(policy_on(GPU_A, 4, 1u32 << 7)); // 假設套用寫入的新值

        attempt_startup_recovery(&backend, &NoopSleeper, &journal).unwrap();
        assert_eq!(backend.current_policy(GPU_A), original, "必須還原到快照");
        assert!(!journal.exists(), "還原驗證成功後清除日誌");
        assert_eq!(backend.restart_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_recovery_stage_snapshot_only_clears_without_mutation() {
        let dir = temp_dir("recovery_snapshot");
        let journal = dir.join("journal.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 1, 0b11);
        backend.set_policy(original.clone());
        recovery::begin_at(&journal, &original).unwrap(); // stage=SnapshotTaken，尚未變更

        attempt_startup_recovery(&backend, &NoopSleeper, &journal).unwrap();
        assert_eq!(backend.current_policy(GPU_A), original);
        assert_eq!(backend.restart_count(), 0, "未變更不該重啟裝置");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_recovery_snapshot_stage_mismatch_is_error() {
        let dir = temp_dir("recovery_mismatch");
        let journal = dir.join("journal.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 1, 0b11);
        recovery::begin_at(&journal, &original).unwrap();
        // 策略被改了（stage 卻停在 SnapshotTaken → 資料異常）
        backend.set_policy(policy_on(GPU_A, 4, 1));

        assert!(attempt_startup_recovery(&backend, &NoopSleeper, &journal).is_err());
        assert!(journal.exists(), "驗證失敗不該清除日誌");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_recovery_restore_failure_keeps_journal() {
        let dir = temp_dir("recovery_restorefail");
        let journal = dir.join("journal.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 2, 0b1);
        recovery::begin_at(&journal, &original).unwrap();
        let j = recovery::load_from(&journal).unwrap().unwrap();
        recovery::advance_to_at(&journal, &j, RecoveryStage::PolicyApplied).unwrap();
        backend.disable_fails.store(true, Ordering::SeqCst); // 還原 restart 一直失敗

        assert!(attempt_startup_recovery(&backend, &NoopSleeper, &journal).is_err());
        assert!(journal.exists(), "還原失敗必須保留日誌供下次啟動");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── BenchmarkManager 狀態骨架 ──

    #[test]
    fn manager_state_default_and_cancel() {
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)])) as Arc<dyn GpuBackend>;
        let m = BenchmarkManager::new(backend);
        let s = m.state_snapshot();
        assert_eq!(s.status, SessionStatus::Pending);
        assert!(!s.recovery_required);
        assert!(!m.cancel_requested());
        m.request_cancel();
        assert!(m.cancel_requested());
        assert!(m.state_snapshot().cancel_requested);
    }

    /// exit guard：Running 阻擋退出，Idle/Completed 允許
    #[test]
    fn refuse_exit_blocks_running_allows_idle_and_completed() {
        let backend = Arc::new(FakeBackend::new(vec![device(GPU_A)])) as Arc<dyn GpuBackend>;
        let m = BenchmarkManager::new(backend);

        // 初始（Idle）→ 允許
        assert!(m.refuse_exit_if_running().is_ok());
        assert!(!m.is_running());

        // Running → 拒絕
        m.state.write().unwrap().status = SessionStatus::Running;
        assert!(m.is_running());
        assert!(m.refuse_exit_if_running().is_err());

        // Completed / Failed / Cancelled → 允許
        for st in [
            SessionStatus::Completed,
            SessionStatus::Failed,
            SessionStatus::Cancelled,
        ] {
            m.state.write().unwrap().status = st;
            assert!(!m.is_running(), "{st:?} 不該算執行中");
            assert!(m.refuse_exit_if_running().is_ok(), "{st:?} 應允許退出");
        }
    }

    #[test]
    fn validate_config_rejects_bad_settings() {
        let t = topo();
        // sample=0
        let mut c = BenchmarkConfig::default();
        c.sample_secs = 0;
        assert!(runner::validate_config(&c, &t).is_err());
        // repetitions 越界
        let mut c = BenchmarkConfig::default();
        c.repetitions = 4;
        assert!(runner::validate_config(&c, &t).is_err());
        // Vulkan 但無 args
        let mut c = BenchmarkConfig::default();
        c.vulkan_args = vec![];
        assert!(runner::validate_config(&c, &t).is_err());
        // 沒 GPU
        let mut c = BenchmarkConfig::default();
        c.gpu_instance_id = None;
        assert!(runner::validate_config(&c, &t).is_err());
        // 合法預設（gpu 補上）
        let mut c = BenchmarkConfig::default();
        c.gpu_instance_id = Some(GPU_A.to_string());
        assert!(runner::validate_config(&c, &t).is_ok());
    }

    #[test]
    fn effective_lps_defaults_to_all_supported() {
        let t = topo(); // 8 LP
        let c = BenchmarkConfig::default();
        assert_eq!(runner::effective_lps(&c, &t), vec![0, 1, 2, 3, 4, 5, 6, 7]);
        // 候選過濾 + 去重 + 排序
        let mut c = BenchmarkConfig::default();
        c.candidate_lps = vec![5, 1, 5, 99];
        assert_eq!(runner::effective_lps(&c, &t), vec![1, 5]);
    }

    #[test]
    fn round_order_asc_desc_asc() {
        let lps = vec![0, 2, 4, 6];
        assert_eq!(runner::round_order(0, &lps), vec![0, 2, 4, 6]);
        assert_eq!(runner::round_order(1, &lps), vec![6, 4, 2, 0]);
        assert_eq!(runner::round_order(2, &lps), vec![0, 2, 4, 6]);
    }

    /// list_importable 只回傳「已完成 + CPU 相容 + GPU 存在 + 有 bestLp」的 session
    #[test]
    fn list_importable_filters_by_compatibility() {
        let dir = temp_dir("importable");
        let root = dir.join("benchmarks");
        let backend = FakeBackend::new(vec![device(GPU_A)]);

        // 相容已完成（GPU_A + fixed identity 指紋 + bestLp）
        let ok = completed_session(&root, &topo(), GPU_A, 3);

        // 不相容 CPU（不同拓撲指紋）
        let other_topo = build_topology(
            (0..8u32)
                .map(|c| (vec![c * 2, c * 2 + 1], 0, true))
                .collect(),
        );
        let bad_cpu = {
            let id = Uuid::new_v4().to_string();
            let detail = crate::benchmark::SessionDetail {
                summary: crate::benchmark::SessionSummary {
                    id: id.clone(),
                    status: SessionStatus::Completed,
                    started_at: "2026-08-11T00:00:00Z".into(),
                    finished_at: Some("2026-08-11T00:01:00Z".into()),
                    gpu_name: "GPU".into(),
                    gpu_instance_id: GPU_A.to_string(),
                    cpu_fingerprint: cpu_fingerprint_with(&other_topo, &fixed_identity()),
                    best_lp: Some(3),
                    severe_lps: vec![],
                    sample_count: 5,
                    total_bytes: 0,
                    config: BenchmarkConfig::default(),
                    error: None,
                },
                results: vec![],
                samples: vec![],
            };
            storage::save_session_at(&root, &detail).unwrap();
            id
        };

        // GPU 不存在（本機只有 GPU_A，session 指向 GPU_B）
        let bad_gpu = {
            let id = Uuid::new_v4().to_string();
            let mut detail = crate::benchmark::SessionDetail::default();
            detail.summary.id = id.clone();
            detail.summary.status = SessionStatus::Completed;
            detail.summary.gpu_instance_id = GPU_B.to_string();
            detail.summary.cpu_fingerprint = cpu_fingerprint_with(&topo(), &fixed_identity());
            detail.summary.best_lp = Some(1);
            storage::save_session_at(&root, &detail).unwrap();
            id
        };

        // 未完成（Running）
        let running = {
            let id = Uuid::new_v4().to_string();
            let mut detail = crate::benchmark::SessionDetail::default();
            detail.summary.id = id.clone();
            detail.summary.status = SessionStatus::Running;
            detail.summary.gpu_instance_id = GPU_A.to_string();
            detail.summary.cpu_fingerprint = cpu_fingerprint_with(&topo(), &fixed_identity());
            storage::save_session_at(&root, &detail).unwrap();
            id
        };

        // 已完成但無 bestLp
        let no_best = {
            let id = Uuid::new_v4().to_string();
            let mut detail = crate::benchmark::SessionDetail::default();
            detail.summary.id = id.clone();
            detail.summary.status = SessionStatus::Completed;
            detail.summary.gpu_instance_id = GPU_A.to_string();
            detail.summary.cpu_fingerprint = cpu_fingerprint_with(&topo(), &fixed_identity());
            storage::save_session_at(&root, &detail).unwrap();
            id
        };

        let list = list_importable(&backend, &topo(), &fixed_identity(), &root);
        let ids: Vec<String> = list.iter().map(|s| s.id.clone()).collect();
        assert!(ids.contains(&ok), "相容已完成應可匯入");
        assert!(!ids.contains(&bad_cpu));
        assert!(!ids.contains(&bad_gpu));
        assert!(!ids.contains(&running));
        assert!(!ids.contains(&no_best));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── apply_affinity_to_gpu（共享 mutation 路徑）──

    #[test]
    fn shared_apply_writes_policy_and_records() {
        let dir = temp_dir("shared_ok");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        backend.set_policy(policy_on(GPU_A, 2, 0b1));

        apply_affinity_to_gpu(&backend, &NoopSleeper, GPU_A, 3, &journal, &restore).unwrap();

        let cur = backend.current_policy(GPU_A);
        assert_dword(&cur, "DevicePolicy", DEVICE_POLICY_SINGLE_PROCESSOR);
        assert_override_mask(&cur, 1u32 << 3);
        assert_eq!(backend.restart_count(), 1);
        assert!(!journal.exists(), "成功後日誌應清除");
        assert!(restore.exists(), "一層還原記錄應寫入");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shared_apply_write_failure_restores() {
        let dir = temp_dir("shared_wfail");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let backend = FakeBackend::new(vec![device(GPU_A)]);
        let original = policy_on(GPU_A, 2, 0b1);
        backend.set_policy(original.clone());
        backend.fail_next_write();

        let err =
            apply_affinity_to_gpu(&backend, &NoopSleeper, GPU_A, 4, &journal, &restore).unwrap_err();
        assert_eq!(err, codes::GPU_APPLY_FAILED);
        assert_eq!(backend.current_policy(GPU_A), original);
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── BenchmarkManager.apply_gpu_affinity ──

    fn manager_with_gpu(gpu: &str) -> (BenchmarkManager, Arc<FakeBackend>) {
        let fake = Arc::new(FakeBackend::new(vec![device(gpu)]));
        let backend = fake.clone() as Arc<dyn GpuBackend>;
        (BenchmarkManager::new(backend), fake)
    }

    #[test]
    fn apply_gpu_affinity_success_writes_correct_mask() {
        let dir = temp_dir("mgr_ok");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let (m, fake) = manager_with_gpu(GPU_A);
        fake.set_policy(policy_on(GPU_A, 2, 0b1));

        m.apply_gpu_affinity_at(&topo(), GPU_A, 5, &journal, &restore)
            .unwrap();

        let cur = m.backend.read_affinity_policy(GPU_A).unwrap();
        assert_dword(&cur, "DevicePolicy", DEVICE_POLICY_SINGLE_PROCESSOR);
        assert_override_mask(&cur, 1u32 << 5);
        assert!(!journal.exists(), "成功後日誌應清除");
        assert!(restore.exists(), "一層還原記錄應寫入");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_gpu_affinity_rejects_invalid_lp_outside_range() {
        let (m, _fake) = manager_with_gpu(GPU_A);
        // 8-LP 拓撲，lp=8 超出範圍
        let err = m.apply_gpu_affinity(&topo(), GPU_A, 8).unwrap_err();
        assert_eq!(err, codes::BENCHMARK_SESSION_INCOMPATIBLE);
    }

    #[test]
    fn apply_gpu_affinity_rejects_lp_64() {
        let (m, _fake) = manager_with_gpu(GPU_A);
        let topo64 = topo_64();
        // 64-LP 拓撲，lp=64 超出 group 0 上限
        let err = m.apply_gpu_affinity(&topo64, GPU_A, 64).unwrap_err();
        assert_eq!(err, codes::BENCHMARK_SESSION_INCOMPATIBLE);
    }

    #[test]
    fn apply_gpu_affinity_rejects_missing_gpu() {
        let (m, _fake) = manager_with_gpu(GPU_A);
        // 本機只有 GPU_A，GPU_B 不存在
        let err = m.apply_gpu_affinity(&topo(), GPU_B, 3).unwrap_err();
        assert_eq!(err, codes::GPU_NOT_FOUND);
    }

    #[test]
    fn apply_gpu_affinity_blocks_when_running() {
        let (m, _fake) = manager_with_gpu(GPU_A);
        m.state.write().unwrap().status = SessionStatus::Running;
        let err = m.apply_gpu_affinity(&topo(), GPU_A, 3).unwrap_err();
        assert_eq!(err, codes::BENCHMARK_ALREADY_RUNNING);
    }

    #[test]
    fn apply_gpu_affinity_blocks_recovery_required() {
        let (m, _fake) = manager_with_gpu(GPU_A);
        m.recovery_required.store(true, Ordering::Relaxed);
        let err = m.apply_gpu_affinity(&topo(), GPU_A, 3).unwrap_err();
        assert_eq!(err, codes::BENCHMARK_RECOVERY_REQUIRED);
    }

    #[test]
    fn apply_gpu_affinity_blocks_basic_display_disabled() {
        let (m, fake) = manager_with_gpu(GPU_A);
        fake.basic_display_on.store(false, Ordering::SeqCst);
        let err = m.apply_gpu_affinity(&topo(), GPU_A, 3).unwrap_err();
        assert_eq!(err, codes::GPU_BASIC_DISPLAY_DISABLED);
    }

    #[test]
    fn apply_gpu_affinity_write_failure_restores_and_clears_journal() {
        let dir = temp_dir("mgr_writefail");
        let journal = dir.join("journal.json");
        let restore = dir.join("restore.json");
        let (m, fake) = manager_with_gpu(GPU_A);
        let original = policy_on(GPU_A, 2, 0b1);
        fake.set_policy(original.clone());
        fake.fail_next_write();

        let err =
            m.apply_gpu_affinity_at(&topo(), GPU_A, 4, &journal, &restore)
                .unwrap_err();
        assert_eq!(err, codes::GPU_APPLY_FAILED);
        // 還原成功 → 策略回原樣，日誌清除
        assert_eq!(
            m.backend.read_affinity_policy(GPU_A).unwrap(),
            original
        );
        assert!(!journal.exists(), "還原成功後日誌應清除");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
