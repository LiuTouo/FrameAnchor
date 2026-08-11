//! IPC commands（PLAN §8）。錯誤回傳穩定代碼字串，前端查 i18n 顯示。

use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::model::{Rule, Settings};
use crate::topology::Topology;
use crate::update::{self, UpdateState, UpdateStatus};
use crate::watcher::AppliedProcess;
use crate::windows_enum::WindowInfo;
use crate::{autostart, config, process, tray, windows_enum, AppState};

#[tauri::command]
pub fn get_topology(state: State<Arc<AppState>>) -> Topology {
    state.topology.clone()
}

#[tauri::command]
pub fn list_windows(state: State<Arc<AppState>>) -> Vec<WindowInfo> {
    let rules = state
        .config
        .read()
        .map(|c| c.rules.clone())
        .unwrap_or_default();
    let my_pid = std::process::id();
    windows_enum::list_windows(my_pid, |exe_path| {
        let norm = process::normalize_path(exe_path);
        rules
            .iter()
            .any(|r| process::normalize_path(&r.exe_path) == norm)
    })
}

#[tauri::command]
pub fn get_rules(state: State<Arc<AppState>>) -> Vec<Rule> {
    state
        .config
        .read()
        .map(|c| c.rules.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub fn save_rule(state: State<Arc<AppState>>, app: AppHandle, rule: Rule) -> Result<(), String> {
    {
        let mut cfg = state.config.write().map_err(|e| e.to_string())?;
        if let Some(existing) = cfg.rules.iter_mut().find(|r| r.id == rule.id) {
            *existing = rule;
        } else {
            cfg.rules.push(rule);
        }
        config::save(&cfg)?;
    }
    // 規則變更 → 清空 applied，watcher 下一輪全部重套（PLAN §8）
    state.applied.write().map_err(|e| e.to_string())?.clear();
    emit_applied(&app, &state);
    Ok(())
}

#[tauri::command]
pub fn delete_rule(state: State<Arc<AppState>>, app: AppHandle, id: String) -> Result<(), String> {
    {
        let mut cfg = state.config.write().map_err(|e| e.to_string())?;
        cfg.rules.retain(|r| r.id != id);
        config::save(&cfg)?;
    }
    state.applied.write().map_err(|e| e.to_string())?.clear();
    emit_applied(&app, &state);
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<Arc<AppState>>) -> Settings {
    state
        .config
        .read()
        .map(|c| c.settings.clone())
        .unwrap_or_default()
}

#[tauri::command]
pub fn save_settings(
    state: State<Arc<AppState>>,
    app: AppHandle,
    settings: Settings,
) -> Result<(), String> {
    let lang_changed = {
        let cfg = state.config.read().map_err(|e| e.to_string())?;
        cfg.settings.language != settings.language
    };
    {
        let mut cfg = state.config.write().map_err(|e| e.to_string())?;
        cfg.settings = settings;
        config::save(&cfg)?;
    }
    if lang_changed {
        tray::rebuild_menu(&app);
    }
    Ok(())
}

#[tauri::command]
pub fn set_autostart(
    state: State<Arc<AppState>>,
    app: AppHandle,
    enable: bool,
) -> Result<(), String> {
    autostart::set_autostart(enable)?;
    {
        let mut cfg = state.config.write().map_err(|e| e.to_string())?;
        cfg.settings.start_with_windows = enable;
        config::save(&cfg)?;
    }
    let _ = app; // tray check 由 rebuild/update 處理
    tray::set_autostart_checked(enable);
    Ok(())
}

#[tauri::command]
pub fn get_applied(state: State<Arc<AppState>>) -> Vec<AppliedProcess> {
    collect_applied(&state)
}

#[tauri::command]
pub fn reapply_all(state: State<Arc<AppState>>, app: AppHandle) -> Result<(), String> {
    state.applied.write().map_err(|e| e.to_string())?.clear();
    emit_applied(&app, &state);
    Ok(())
}

#[tauri::command]
pub fn set_usage_streaming(state: State<Arc<AppState>>, active: bool) {
    let _ = state.usage_tx.send(active);
}

#[tauri::command]
pub fn open_data_folder() -> Result<(), String> {
    let dir = config::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // explorer 即使成功開窗也常回非零結束碼，用 spawn 不等候
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 收集 applied 清單（依 exe 名排序）
fn collect_applied(state: &Arc<AppState>) -> Vec<AppliedProcess> {
    let mut list: Vec<AppliedProcess> = state
        .applied
        .read()
        .map(|a| a.values().map(|e| e.info.clone()).collect())
        .unwrap_or_default();
    list.sort_by(|a, b| a.exe_name.to_lowercase().cmp(&b.exe_name.to_lowercase()));
    list
}

/// 對前端廣播 applied 變更 + 更新 tray 計數
pub fn emit_applied(app: &AppHandle, state: &Arc<AppState>) {
    let list = collect_applied(state);
    tray::update_applied_count(app, list.len());
    let _ = app.emit("applied-update", list);
}

// ── 更新相關 commands ──

/// 回傳目前版本、是否為可攜版
#[tauri::command]
pub fn get_update_info(app: AppHandle) -> serde_json::Value {
    let version = update::current_version(&app);
    let portable = update::is_portable();
    serde_json::json!({
        "version": version,
        "portable": portable,
    })
}

/// 可攜版：檢查 GitHub 有無新版，emit update-state 事件
#[tauri::command]
pub async fn check_portable_update(app: AppHandle) -> Result<(), String> {
    let version = update::current_version(&app);

    // 狀態：檢查中
    let _ = app.emit(
        "update-state",
        UpdateState {
            status: UpdateStatus::Checking,
            latest_version: None,
            current_version: version.clone(),
            progress: None,
            error: None,
        },
    );

    let version_for_check = version.clone();
    let version_for_up_to_date = version.clone();
    let version_for_available = version.clone();
    let app_for_error = app.clone();

    let result = tokio::task::spawn_blocking(move || {
        let release = update::fetch_portable_release()?;
        let latest_str = release.version.to_string();

        if !update::is_update_available(&version_for_check, &release.version) {
            let _ = app.emit(
                "update-state",
                UpdateState {
                    status: UpdateStatus::UpToDate,
                    latest_version: Some(latest_str),
                    current_version: version_for_up_to_date,
                    progress: None,
                    error: None,
                },
            );
            return Ok::<_, String>(());
        }

        // 有新版本
        let _ = app.emit(
            "update-state",
            UpdateState {
                status: UpdateStatus::Available,
                latest_version: Some(latest_str),
                current_version: version_for_available,
                progress: None,
                error: None,
            },
        );
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| format!("檢查更新失敗: {e}"))?;

    // 若 result 為 Err，emit Error 狀態
    if let Err(ref err) = result {
        let _ = app_for_error.emit(
            "update-state",
            UpdateState {
                status: UpdateStatus::Error,
                latest_version: None,
                current_version: version,
                progress: None,
                error: Some(err.clone()),
            },
        );
    }

    result
}

/// 可攜版：下載並安裝更新
#[tauri::command]
pub async fn perform_portable_update(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    // 基準測試執行中：拒絕替換/退出，避免 runner 中斷未還原
    state.benchmark.refuse_exit_if_running()?;
    let version = update::current_version(&app);

    // 狀態：下載中
    let _ = app.emit(
        "update-state",
        UpdateState {
            status: UpdateStatus::Downloading,
            latest_version: None,
            current_version: version.clone(),
            progress: Some(0),
            error: None,
        },
    );

    let version_for_download = version.clone();

    // 階段 1：查詢 + 下載 + 校驗
    let (zip_data, latest_str) = tokio::task::spawn_blocking(move || {
        let release = update::fetch_portable_release()?;

        if !update::is_update_available(&version_for_download, &release.version) {
            return Err("已經是最新版本".to_string());
        }

        let latest_str = release.version.to_string();

        let zip_data = update::download_portable_zip(&release, |_pct| {
            // 下載完成時由 download_portable_zip 回報 100%
        })?;

        Ok::<_, String>((zip_data, latest_str))
    })
    .await
    .map_err(|e| format!("更新執行失敗: {e}"))??;

    // emit 下載完成進度
    let _ = app.emit(
        "update-state",
        UpdateState {
            status: UpdateStatus::Downloading,
            latest_version: Some(latest_str.clone()),
            current_version: version.clone(),
            progress: Some(100),
            error: None,
        },
    );

    // 階段 2：解壓縮（含基準測試資源）
    let (new_exe, marker_path, new_resources) =
        tokio::task::spawn_blocking(move || update::extract_portable_exe(&zip_data))
            .await
            .map_err(|e| format!("解壓縮失敗: {e}"))??;

    let old_exe = update::current_exe_path().ok_or("無法取得目前執行檔路徑".to_string())?;
    let pid = std::process::id();

    // 狀態：安裝中
    let _ = app.emit(
        "update-state",
        UpdateState {
            status: UpdateStatus::Installing,
            latest_version: None,
            current_version: version,
            progress: None,
            error: None,
        },
    );

    // 執行可攜版替換輔助腳本（同步更新基準測試資源）
    update::execute_portable_replacement(&old_exe, &new_exe, &marker_path, &new_resources, pid)?;

    // 設定 quitting flag，繞過 close-to-tray，真正結束程序
    state
        .quitting
        .store(true, std::sync::atomic::Ordering::Relaxed);
    app.exit(0);
    Ok(())
}
