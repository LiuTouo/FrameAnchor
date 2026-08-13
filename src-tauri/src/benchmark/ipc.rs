//! 基準測試與 GPU 控制的 IPC commands。錯誤一律回傳穩定代碼字串（查 i18n）。

use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::error::codes;
use crate::gpu::{AffinityPolicy, GpuDevice};
use crate::AppState;

use super::recommend;
use super::storage;
use super::{
    cpu_fingerprint_with, detect_cpu_identity, ApplyStatus, BenchmarkConfig, BenchmarkState,
    SessionDetail, SessionStatus, SessionSummary, StorageInfo,
};

/// 列舉目前使用的顯示配接器
#[tauri::command]
pub fn enumerate_gpus(state: State<Arc<AppState>>) -> Result<Vec<GpuDevice>, String> {
    state
        .benchmark
        .backend
        .enumerate_present_adapters()
        .map_err(|e| e.code().to_string())
}

/// 目前基準測試狀態（含 recoveryRequired）
#[tauri::command]
pub fn get_benchmark_state(state: State<Arc<AppState>>) -> BenchmarkState {
    state.benchmark.state_snapshot()
}

/// 歷史 session 摘要列表
#[tauri::command]
pub fn list_benchmark_sessions() -> Result<Vec<SessionSummary>, String> {
    storage::list()
}

/// 讀單一 session 完整內容
#[tauri::command]
pub fn get_benchmark_session(id: String) -> Result<SessionDetail, String> {
    storage::get(&id)
}

/// 刪除單一 session（嚴謹 id 驗證；永不自動刪除）
#[tauri::command]
pub fn delete_benchmark_session(id: String) -> Result<(), String> {
    storage::delete(&id)
}

/// 儲存體總位元組數與 session 數
#[tauri::command]
pub fn get_benchmark_storage_info() -> Result<StorageInfo, String> {
    let total_bytes = storage::total_bytes();
    let session_count = storage::list()?.len();
    Ok(StorageInfo {
        total_bytes,
        session_count,
    })
}

/// 查詢目前 GPU 中斷親和性策略
#[tauri::command]
pub fn get_gpu_affinity_policy(
    state: State<Arc<AppState>>,
    instance_id: String,
) -> Result<AffinityPolicy, String> {
    state
        .benchmark
        .backend
        .read_affinity_policy(&instance_id)
        .map_err(|e| e.code().to_string())
}

/// 把完成 session 的最佳 LP 套用到對應 GPU（驗證相容性 + 快照 + 重啟）
#[tauri::command]
pub fn apply_best_gpu_affinity(
    state: State<Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state.benchmark.apply_best(&state.topology, &session_id)
}

/// 手動套用 GPU 中斷親和性到指定 LP（不經 session；前置驗證 recovery/執行中/LP/GPU/BasicDisplay）
#[tauri::command]
pub fn apply_gpu_affinity(
    state: State<Arc<AppState>>,
    instance_id: String,
    lp: u32,
) -> Result<(), String> {
    state
        .benchmark
        .apply_gpu_affinity(&state.topology, &instance_id, lp)
}

/// 查詢歷史 session 現在可否套用（不做變更；相容性判定只在後端）
#[tauri::command]
pub fn get_benchmark_apply_status(state: State<Arc<AppState>>, session_id: String) -> ApplyStatus {
    state.benchmark.check_apply(&state.topology, &session_id)
}

/// 可匯入的已完成 session（已完成 + CPU 相容 + GPU 存在 + 有 bestLp）
#[tauri::command]
pub fn list_importable_sessions(state: State<Arc<AppState>>) -> Vec<SessionSummary> {
    state.benchmark.list_importable(&state.topology)
}

/// 依 best LP + 目前拓撲計算「固定排除 core 0，並排除最佳 LP 所屬實體核心」的推薦核心集合。
/// severe_lps 僅保留作為前後端 IPC 相容參數與結果標註。
#[tauri::command]
pub fn compute_recommended_cores(
    state: State<Arc<AppState>>,
    best_lp: u32,
    severe_lps: Vec<u32>,
) -> Vec<u32> {
    recommend::recommended_cores(&state.topology, best_lp, &severe_lps)
}

/// 目前 CPU 指紋（判斷規則上的推薦是否已過時硬體）
#[tauri::command]
pub fn get_current_cpu_fingerprint(state: State<Arc<AppState>>) -> String {
    cpu_fingerprint_with(&state.topology, &detect_cpu_identity())
}

/// 還原到上次成功套用前的策略
#[tauri::command]
pub fn restore_previous_gpu_affinity(state: State<Arc<AppState>>) -> Result<(), String> {
    state.benchmark.restore_previous()
}

/// 開始 GPU 基準測試（背景 session；驗證通過立即回傳 Ok）
#[tauri::command]
pub fn start_gpu_benchmark(
    app: AppHandle,
    state: State<Arc<AppState>>,
    config: BenchmarkConfig,
) -> Result<(), String> {
    state.benchmark.start(&app, &state.topology, config)
}

/// 取消正在跑的基準測試
#[tauri::command]
pub fn cancel_benchmark(state: State<Arc<AppState>>) -> Result<(), String> {
    let running = state
        .benchmark
        .state
        .read()
        .map(|s| s.status == SessionStatus::Running)
        .unwrap_or(false);
    if !running {
        return Err(codes::BENCHMARK_NOT_ACTIVE.to_string());
    }
    state.benchmark.request_cancel();
    Ok(())
}
