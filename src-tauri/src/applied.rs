//! 已套用進程的呈現契約與廣播邏輯。
//! commands.rs 與 watcher.rs 都只依賴此模組，彼此不互相 import，避免循環相依：
//! AppliedProcess DTO 原在 watcher.rs、collect/emit 原在 commands.rs，現集中於此。

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::{tray, AppState};

/// 套用策略（呈現契約，前端據此區分「已驗證實際核心」與「未驗證偏好」）。
/// Hard / CpuSets 只在精確回讀驗證後才回報；Prefer 是排程提示、永不宣稱硬性限制。
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AffinityStrategy {
    /// 未套用 affinity（失敗或尚未驗證）
    #[default]
    None,
    /// 硬綁定（SetProcessAffinityMask，已由 GetProcessAffinityMask 回讀驗證）
    Hard,
    /// process-default CPU Sets（已由 GetProcessDefaultCpuSets 回讀驗證）
    CpuSets,
    /// 執行緒 ideal processor 偏好（未驗證提示，非硬性限制）
    Prefer,
}

/// 已套用進程（PLAN §5.4）
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppliedProcess {
    pub pid: u32,
    pub exe_name: String,
    pub rule_id: String,
    pub rule_name: String,
    pub affinity_ok: bool,
    pub priority_ok: bool,
    pub io_ok: Option<bool>, // None = 規則未設定此項
    pub mem_ok: Option<bool>,
    pub error: Option<String>, // 錯誤代碼（前端查 i18n）
    pub applied_at: String,    // RFC3339
    pub current_cores: Vec<u32>,
    pub current_priority: String,
    /// true = 軟綁定（Prefer 模式），current_cores 為偏好清單而非實際 mask
    pub soft_affinity: bool,
    /// 執行緒 ideal 套用統計；None = 未走執行緒 ideal 路徑。partial = succeeded < attempted
    pub thread_ideal_attempted: Option<usize>,
    pub thread_ideal_succeeded: Option<usize>,
    /// 套用策略。Hard / CpuSets = current_cores 為已驗證實際核心；Prefer = 偏好提示；None = 未套用。
    #[serde(default)]
    pub strategy: AffinityStrategy,
}

/// 收集 applied 清單（依 exe 名排序）
pub fn collect_applied(state: &Arc<AppState>) -> Vec<AppliedProcess> {
    let mut list: Vec<AppliedProcess> = state
        .applied
        .read()
        .map(|a| a.values().map(|e| e.info.clone()).collect())
        .unwrap_or_default();
    list.sort_by_key(|a| a.exe_name.to_lowercase());
    list
}

/// 對前端廣播 applied 變更 + 更新 tray 計數
pub fn emit_applied(app: &AppHandle, state: &Arc<AppState>) {
    let list = collect_applied(state);
    tray::update_applied_count(app, successful_count(&list));
    let _ = app.emit("applied-update", list);
}

fn successful_count(list: &[AppliedProcess]) -> usize {
    // 「已套用」= affinity 實際完整生效。SoftPartial（affinity_ok=false、無 error，
    // 執行緒 ideal 部分成功）不算；priority/io/mem 部分失敗不影響（affinity 成功即算）。
    list.iter()
        .filter(|entry| entry.error.is_none() && entry.affinity_ok)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(error: Option<&str>) -> AppliedProcess {
        AppliedProcess {
            pid: 1,
            exe_name: "game.exe".into(),
            rule_id: "rule".into(),
            rule_name: "Rule".into(),
            affinity_ok: error.is_none(),
            priority_ok: true,
            io_ok: None,
            mem_ok: None,
            error: error.map(str::to_string),
            applied_at: String::new(),
            current_cores: vec![],
            current_priority: String::new(),
            soft_affinity: false,
            thread_ideal_attempted: None,
            thread_ideal_succeeded: None,
            strategy: AffinityStrategy::None,
        }
    }

    #[test]
    fn tray_count_excludes_failed_entries() {
        let list = vec![item(None), item(Some("ACCESS_DENIED")), item(None)];
        assert_eq!(successful_count(&list), 2);
    }

    #[test]
    fn tray_count_excludes_soft_partial() {
        // SoftPartial：error=None 但 affinity_ok=false（執行緒 ideal 部分成功）→ 不計入
        let mut partial = item(None);
        partial.affinity_ok = false;
        let list = vec![item(None), partial];
        assert_eq!(successful_count(&list), 1);
    }
}
