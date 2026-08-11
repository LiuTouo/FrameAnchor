//! FrameAnchor 主程式（PLAN §4 架構）。
//! 單一 exe、requireAdministrator、tray 常駐、watcher + usage 兩個背景 task。
//! Release 用 GUI subsystem 避免 CMD 閃爍；debug 保留 console。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod benchmark;
mod commands;
mod config;
mod error;
mod gpu;
mod model;
mod priority;
mod process;
mod topology;
mod tray;
mod update;
mod usage;
mod watcher;
mod windows_enum;

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use tauri::Manager;

use model::Config;
use topology::Topology;
use watcher::{AppliedEntry, CachedHandle};

/// 全域共享狀態（PLAN §4）
pub struct AppState {
    pub config: RwLock<Config>,
    pub topology: Topology,
    pub applied: RwLock<HashMap<u32, AppliedEntry>>,
    /// PID → 早期快取的 process handle（反作弊保護生效前開啟，終生重用）
    pub handles: RwLock<HashMap<u32, CachedHandle>>,
    /// usage streaming 開關（Dashboard 開啟且至少有 applied 規則程序時 true）
    pub usage_tx: tokio::sync::watch::Sender<bool>,
    /// tray「結束」設定，用來繞過 closeToTray 攔截
    pub quitting: AtomicBool,
    /// 基準測試管理者（GPU 控制、還原、狀態）
    pub benchmark: Arc<benchmark::manager::BenchmarkManager>,
}

fn main() {
    // 強殺殘留的 WebView2 孤兒（鎖 user-data 目錄會導致白畫面）
    process::kill_orphan_webviews();
    // SeDebugPrivilege：對 ACL 保護的進程有幫助（無法繞過反作弊 kernel callback）
    process::enable_debug_privilege();

    // GUI subsystem 看不到 panic 輸出，寫到暫存檔方便診斷
    std::panic::set_hook(Box::new(|info| {
        let path = std::env::temp_dir().join("frameanchor-panic.log");
        let _ = std::fs::write(&path, format!("{info}\n"));
    }));

    let topology = match topology::enumerate_topology() {
        Ok(t) => {
            if t.total_lp > 64 {
                log::warn!(
                    "偵測到 {} 個邏輯處理器：v1 只支援 group 0（前 64 個）",
                    t.total_lp
                );
            }
            log::info!(
                "拓撲：{} LP / {} 核心, SMT={}, Hybrid={}",
                t.total_lp,
                t.physical_cores.len(),
                t.has_smt,
                t.has_hybrid
            );
            t
        }
        Err(e) => {
            log::error!("拓撲列舉失敗: {e}");
            Topology::default()
        }
    };

    let cfg = config::load();
    let (usage_tx, _) = tokio::sync::watch::channel(false);

    // 基準測試管理者：GPU 控制一律透過注入的 backend（啟動時嘗試 pending 還原）
    let backend: Arc<dyn gpu::GpuBackend> = Arc::new(gpu::RealGpuBackend::new());
    let benchmark = Arc::new(benchmark::manager::BenchmarkManager::new(backend));
    benchmark.attempt_startup_recovery();

    let state = Arc::new(AppState {
        config: RwLock::new(cfg),
        topology,
        applied: RwLock::new(HashMap::new()),
        handles: RwLock::new(HashMap::new()),
        usage_tx,
        quitting: AtomicBool::new(false),
        benchmark,
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 第二個實例啟動 → 喚醒既有視窗後退出
            tray::show_main_window(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .build(),
        )
        .manage(state.clone())
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::build_tray(&handle)?;

            // --minimized（Task Scheduler 帶入）→ 不開主視窗直接常駐 tray
            let minimized = std::env::args().any(|a| a == "--minimized");
            let start_min = state
                .config
                .read()
                .map(|c| c.settings.start_minimized)
                .unwrap_or(false);
            if !minimized && !start_min {
                tray::show_main_window(&handle);
            }

            watcher::spawn(handle.clone(), state.clone());
            usage::spawn(handle.clone(), state.clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_topology,
            commands::list_windows,
            commands::get_rules,
            commands::save_rule,
            commands::delete_rule,
            commands::get_settings,
            commands::save_settings,
            commands::set_autostart,
            commands::get_applied,
            commands::reapply_all,
            commands::set_usage_streaming,
            commands::open_data_folder,
            commands::get_update_info,
            commands::check_portable_update,
            commands::perform_portable_update,
            benchmark::ipc::enumerate_gpus,
            benchmark::ipc::get_benchmark_state,
            benchmark::ipc::list_benchmark_sessions,
            benchmark::ipc::get_benchmark_session,
            benchmark::ipc::delete_benchmark_session,
            benchmark::ipc::get_benchmark_storage_info,
            benchmark::ipc::get_gpu_affinity_policy,
            benchmark::ipc::apply_best_gpu_affinity,
            benchmark::ipc::get_benchmark_apply_status,
            benchmark::ipc::list_importable_sessions,
            benchmark::ipc::compute_recommended_cores,
            benchmark::ipc::get_current_cpu_fingerprint,
            benchmark::ipc::restore_previous_gpu_affinity,
            benchmark::ipc::start_gpu_benchmark,
            benchmark::ipc::cancel_benchmark,
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let state = app.state::<Arc<AppState>>();
                if state.quitting.load(std::sync::atomic::Ordering::Relaxed) {
                    return; // tray「結束」→ 真正關閉
                }
                let close_to_tray = state
                    .config
                    .read()
                    .map(|c| c.settings.close_to_tray)
                    .unwrap_or(true);
                if close_to_tray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running FrameAnchor");
}
