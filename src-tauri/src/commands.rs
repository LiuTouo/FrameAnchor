//! IPC commands（PLAN §8）。錯誤回傳穩定代碼字串，前端查 i18n 顯示。

use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::model::{Rule, Settings};
use crate::topology::Topology;
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
    state.config.read().map(|c| c.rules.clone()).unwrap_or_default()
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
