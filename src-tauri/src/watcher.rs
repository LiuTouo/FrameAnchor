//! 規則引擎（PLAN §7.6）：高頻 discovery 搶先開 handle → 比對規則 → 套用 → 維護 applied 狀態表。
//! 反作弊對策：EAC 等用 ObRegisterCallbacks 剝奪「新開啟」handle 的權限，但保護生效前
//! 已持有的 handle 不受影響 — 因此 discovery 以 100ms 掃描，在遊戲啟動後立刻開 handle
//! 並快取至進程結束；之後所有套用/重試都走快取 handle。
//! 退避策略：ACCESS_DENIED（反作弊）每 30 秒重試；其他錯誤重試 3 次。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::AppHandle;
use windows::Win32::Foundation::HANDLE;

use crate::error::{codes, ProcessError};
use crate::model::{AffinityMode, MatchBy, Rule};
use crate::topology::{self, Topology};
use crate::{commands, process, priority, AppState};

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
    pub io_ok: Option<bool>,  // None = 規則未設定此項
    pub mem_ok: Option<bool>,
    pub error: Option<String>, // 錯誤代碼（前端查 i18n）
    pub applied_at: String,    // RFC3339
    pub current_cores: Vec<u32>,
    pub current_priority: String,
    /// true = 軟綁定（Prefer 模式），current_cores 為偏好清單而非實際 mask
    pub soft_affinity: bool,
}

/// watcher 內部狀態（含重試資訊，不序列化給前端）
pub struct AppliedEntry {
    pub info: AppliedProcess,
    pub exe_name: String,
    /// ACCESS_DENIED：30 秒退避、無限重試（反作弊可能較晚才放行）
    pub access_denied: bool,
    /// 其他錯誤的剩餘重試次數
    pub retries_left: u8,
    pub next_retry: Option<Instant>,
}

/// 早期取得並快取的 process handle（AppState.handles 的值）。
/// 反作弊保護生效前開啟的 handle 之後仍可正常操作。
pub struct CachedHandle {
    pub handle: process::OwnedHandle,
    /// GetProcessTimes creation time，PID 重用偵測用
    pub created: u64,
}

const ACCESS_DENIED_BACKOFF: Duration = Duration::from_secs(30);
const MAX_RETRIES: u8 = 3;

/// 高頻發現間隔：要搶在反作弊保護生效前開 handle（EAC 通常在遊戲啟動後
/// 數百 ms~數秒才完成保護），1 秒級的 poll 太慢會錯過窗口。
const DISCOVERY_INTERVAL: Duration = Duration::from_millis(100);

/// Affinity 套用結果（三層降級）
enum AffinityResult {
    /// 硬綁定成功（SetProcessAffinityMask），攜帶實際設定的核心清單
    HardOk { cores: Vec<u32> },
    /// 軟綁定成功（SetThreadIdealProcessorEx，逐 thread）
    SoftOk { cores: Vec<u32>, thread_count: usize },
    /// CPU Sets 成功（SetProcessDefaultCpuSets）
    CpuSetsOk { cores: Vec<u32> },
    /// 三層全部失敗
    AllFailed,
}

/// 對進程套用 affinity，逐層降級：硬綁定 → 軟綁定 → CPU Sets
fn apply_affinity(
    pid: u32,
    h: Option<HANDLE>,
    spec: &crate::model::AffinitySpec,
    topo: &Topology,
) -> AffinityResult {
    let cores: Vec<u32> = match spec.mode {
        AffinityMode::All => {
            let all_cores: Vec<u32> = (0..topo.total_lp).collect();
            return AffinityResult::HardOk { cores: all_cores };
        }
        AffinityMode::Prefer => spec
            .cores
            .iter()
            .copied()
            .filter(|&c| c < topo.total_lp)
            .collect(),
        _ => topology::mask_to_indices(topology::resolve_mask(spec, topo)),
    };

    if cores.is_empty() {
        return AffinityResult::AllFailed;
    }

    // Tier 1: 硬綁定（SetProcessAffinityMask）
    if let Some(h) = h {
        let mask = cores.iter().fold(0u64, |m, &c| m | (1u64 << c));
        if process::set_affinity(h, mask).is_ok() {
            return AffinityResult::HardOk { cores };
        }
        log::info!("硬綁定失敗 PID {pid}，降級到軟綁定");
    }

    // Tier 2: 軟綁定（SetThreadIdealProcessorEx，逐 thread）
    let n = process::set_threads_ideal(pid, &cores);
    if n > 0 {
        log::info!("軟綁定成功 PID {pid}: {n} 執行緒");
        return AffinityResult::SoftOk {
            cores,
            thread_count: n,
        };
    }
    log::info!("軟綁定失敗 PID {pid}，降級到 CPU Sets");

    // Tier 3: CPU Sets（SetProcessDefaultCpuSets）
    match process::set_cpu_sets(pid, &cores) {
        Ok(()) => {
            log::info!("CPU Sets 成功 PID {pid}");
            AffinityResult::CpuSetsOk { cores }
        }
        Err(e) => {
            log::warn!("CPU Sets 失敗 PID {pid}: {e}");
            AffinityResult::AllFailed
        }
    }
}

/// 規則比對（純函式，PLAN §7.6）
pub fn matches(rule: &Rule, exe_name: &str, exe_path: Option<&str>) -> bool {
    match rule.match_by {
        MatchBy::FullPath => exe_path
            .map(|p| process::normalize_path(p) == process::normalize_path(&rule.exe_path))
            .unwrap_or(false),
        MatchBy::FileName => file_name(&rule.exe_path).eq_ignore_ascii_case(exe_name),
    }
}

fn file_name(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

/// 比對第一條命中且啟用的規則
fn find_rule(rules: &[Rule], exe_name: &str, exe_path: Option<&str>) -> Option<Rule> {
    rules
        .iter()
        .find(|r| r.enabled && matches(r, exe_name, exe_path))
        .cloned()
}

/// 取得 PID 的 handle：優先用快取的早期 handle，沒有才現開（並存入快取）。
/// 快取 handle 在反作弊保護生效後依然可用；現開則可能被剝權限。
fn ensure_handle(state: &Arc<AppState>, pid: u32) -> Result<HANDLE, ProcessError> {
    if let Some(c) = state.handles.read().unwrap().get(&pid) {
        return Ok(c.handle.0);
    }
    let oh = process::open_for_set(pid)?;
    let created = process::process_creation_time(oh.0).unwrap_or(0);
    let h = oh.0;
    state
        .handles
        .write()
        .unwrap()
        .insert(pid, CachedHandle { handle: oh, created });
    Ok(h)
}

pub fn spawn(app: AppHandle, state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut last_tick = Instant::now();
        loop {
            let interval = state
                .config
                .read()
                .map(|c| c.settings.poll_interval_ms.clamp(200, 60_000))
                .unwrap_or(1000);
            tokio::time::sleep(DISCOVERY_INTERVAL).await;
            discovery_pass(&app, &state);
            if last_tick.elapsed() >= Duration::from_millis(interval) {
                tick(&app, &state, interval);
                last_tick = Instant::now();
            }
        }
    });
}

/// 快速發現 pass：輕量掃描進程名，命中規則的新 PID 立刻開 handle 並套用。
/// 無啟用規則時直接返回，閒置零額外負擔。
fn discovery_pass(app: &AppHandle, state: &Arc<AppState>) {
    let (rules, interval): (Vec<Rule>, u64) = state
        .config
        .read()
        .map(|c| {
            (
                c.rules.iter().filter(|r| r.enabled).cloned().collect(),
                c.settings.poll_interval_ms.clamp(200, 60_000),
            )
        })
        .unwrap_or_else(|_| (Vec::new(), 1000));
    if rules.is_empty() {
        return;
    }
    // 檔名過濾集合：名字命中才需要進一步比對（路徑解析很貴，只做候選者）
    let wanted: HashSet<String> = rules
        .iter()
        .map(|r| file_name(&r.exe_path).to_lowercase())
        .collect();

    let candidates: Vec<(u32, String)> = process::enumerate_process_names()
        .into_iter()
        .filter(|(_, name)| wanted.contains(&name.to_lowercase()))
        .collect();
    if candidates.is_empty() {
        return;
    }

    let mut dirty = false;
    for (pid, exe_name) in candidates {
        if state.applied.read().unwrap().contains_key(&pid) {
            continue;
        }
        let path = process::process_path(pid);
        if process::is_blacklisted(pid, &exe_name, path.as_deref()) {
            continue;
        }
        let Some(rule) = find_rule(&rules, &exe_name, path.as_deref()) else {
            continue;
        };
        log::info!("套用規則「{}」→ {} (PID {})", rule.name, exe_name, pid);
        let handle = ensure_handle(state, pid);
        let entry = apply_and_build(pid, &exe_name, &rule, &state.topology, Instant::now(), interval, handle);
        if let Some(err) = &entry.info.error {
            log::warn!("套用失敗 {} (PID {}): {}", exe_name, pid, err);
        }
        state.applied.write().unwrap().insert(pid, entry);
        dirty = true;
    }
    if dirty {
        commands::emit_applied(app, state);
    }
}

fn tick(app: &AppHandle, state: &Arc<AppState>, interval_ms: u64) {
    let procs = process::enumerate_processes();
    let rules: Vec<Rule> = state
        .config
        .read()
        .map(|c| c.rules.clone())
        .unwrap_or_default();
    let now = Instant::now();
    let mut dirty = false;

    {
        let mut applied = state.applied.write().unwrap();

        // 1) 移除已結束的 PID（含 handle 快取）
        let alive: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
        let before = applied.len();
        applied.retain(|pid, _| alive.contains(pid));
        if applied.len() != before {
            dirty = true;
        }
        {
            let mut handles = state.handles.write().unwrap();
            handles.retain(|pid, _| alive.contains(pid));
            // PID 重用偵測：creation time 不符 → 丟棄舊 handle。
            // 受保護進程連 QUERY_LIMITED 都可能被剝 → None → 保留快取。
            let stale: Vec<u32> = handles
                .iter()
                .filter(|(pid, c)| {
                    process::creation_time_by_pid(**pid)
                        .map(|t| t != c.created)
                        .unwrap_or(false)
                })
                .map(|(pid, _)| *pid)
                .collect();
            for pid in stale {
                handles.remove(&pid);
            }
        }

        // 2) PID 重用防護：還活著但 exe 變了 → 移除，當新進程重新比對
        let stale: Vec<u32> = applied
            .iter()
            .filter(|(pid, e)| {
                procs
                    .iter()
                    .find(|p| p.pid == **pid)
                    .map(|p| !p.exe_name.eq_ignore_ascii_case(&e.exe_name))
                    .unwrap_or(false)
            })
            .map(|(pid, _)| *pid)
            .collect();
        for pid in stale {
            applied.remove(&pid);
            dirty = true;
        }

        // 3) 規則被刪除或停用 → 移除對應 entry
        let orphaned: Vec<u32> = applied
            .iter()
            .filter(|(_, e)| {
                !rules
                    .iter()
                    .any(|r| r.id == e.info.rule_id && r.enabled)
            })
            .map(|(pid, _)| *pid)
            .collect();
        for pid in orphaned {
            applied.remove(&pid);
            dirty = true;
        }

        // 4) 新進程：比對規則並套用（discovery 漏掉的才會走到這，例如 poll 間隔極短）
        for p in &procs {
            if applied.contains_key(&p.pid) {
                continue;
            }
            if process::is_blacklisted(p.pid, &p.exe_name, p.exe_path.as_deref()) {
                continue;
            }
            let rule = match find_rule(&rules, &p.exe_name, p.exe_path.as_deref()) {
                Some(r) => r,
                None => continue,
            };
            log::info!("套用規則「{}」→ {} (PID {})", rule.name, p.exe_name, p.pid);
            let handle = ensure_handle(state, p.pid);
            let entry = apply_and_build(p.pid, &p.exe_name, &rule, &state.topology, now, interval_ms, handle);
            if let Some(err) = &entry.info.error {
                log::warn!("套用失敗 {} (PID {}): {}", p.exe_name, p.pid, err);
            }
            applied.insert(p.pid, entry);
            dirty = true;
        }

        // 5) 失敗重試
        let due: Vec<u32> = applied
            .iter()
            .filter(|(_, e)| {
                e.info.error.is_some()
                    && e.next_retry.map(|t| now >= t).unwrap_or(false)
                    && (e.access_denied || e.retries_left > 0)
            })
            .map(|(pid, _)| *pid)
            .collect();
        for pid in due {
            let (rule, exe_name) = {
                let entry = &applied[&pid];
                let rule = rules.iter().find(|r| r.id == entry.info.rule_id).cloned();
                (rule, entry.exe_name.clone())
            };
            if let Some(rule) = rule {
                log::info!("重試套用「{}」→ PID {}", rule.name, pid);
                let handle = ensure_handle(state, pid);
                let mut entry = apply_and_build(pid, &exe_name, &rule, &state.topology, now, interval_ms, handle);
                if entry.info.error.is_some() && !entry.access_denied {
                    let prev = applied.get(&pid).map(|e| e.retries_left).unwrap_or(1);
                    entry.retries_left = prev.saturating_sub(1);
                }
                applied.insert(pid, entry);
                dirty = true;
            }
        }
    }

    if dirty {
        commands::emit_applied(app, state);
    }
}

/// 對單一進程套用規則，回傳完整狀態（含重試排程）。
/// 三層 affinity fallback：硬綁定 → 軟綁定 → CPU Sets。
/// handle 由呼叫端透過 ensure_handle 取得（快取的早期 handle 或現開）。
fn apply_and_build(
    pid: u32,
    exe_name: &str,
    rule: &Rule,
    topo: &Topology,
    now: Instant,
    interval_ms: u64,
    handle_result: Result<HANDLE, ProcessError>,
) -> AppliedEntry {
    let mut info = AppliedProcess {
        pid,
        exe_name: exe_name.to_string(),
        rule_id: rule.id.clone(),
        rule_name: rule.name.clone(),
        affinity_ok: false,
        priority_ok: false,
        io_ok: None,
        mem_ok: None,
        error: None,
        applied_at: chrono::Utc::now().to_rfc3339(),
        current_cores: Vec::new(),
        current_priority: String::new(),
        soft_affinity: false,
    };

    // Phase A: handle 由呼叫端 ensure_handle 提供（快取早期 handle 或現開）
    // Phase B: affinity（三層降級，handle 為 None 時跳過硬綁定）
    let aff_result = apply_affinity(
        pid,
        handle_result.as_ref().ok().copied(),
        &rule.affinity,
        topo,
    );

    match aff_result {
        AffinityResult::HardOk { cores } => {
            info.affinity_ok = true;
            info.current_cores = cores;
        }
        AffinityResult::SoftOk { cores, thread_count } => {
            info.affinity_ok = true;
            info.soft_affinity = true;
            info.current_cores = cores;
            log::info!("軟綁定 fallback PID {pid}: {thread_count} 執行緒");
        }
        AffinityResult::CpuSetsOk { cores } => {
            info.affinity_ok = true;
            info.current_cores = cores;
            log::info!("CPU Sets fallback PID {pid}");
        }
        AffinityResult::AllFailed => {
            // handle 開啟失敗 → ACCESS_DENIED；否則是一般失敗
            if handle_result.is_err() {
                info.error = Some(codes::ACCESS_DENIED.to_string());
            } else {
                info.error = Some(codes::SET_AFFINITY_FAILED.to_string());
            }
        }
    }

    // Phase C: priority / IO / memory（需 handle）
    if let Ok(h) = handle_result {
        // priority：失敗只記 log + priority_ok=false（UI 顯示警告小字），
        // 不設 info.error — affinity 成功即算成功。EAC 遊戲的 priority 永久
        // 失敗（無早期 handle 時），設 error 只會造成紅字噪音與無謂重試。
        match process::set_priority(h, rule.priority) {
            Ok(()) => info.priority_ok = true,
            Err(e) => log::warn!("set_priority 失敗 PID {pid}: {e}"),
        }

        // 進階：盡力而為，失敗不算整體錯誤（PLAN §7.3）
        if let Some(io) = rule.advanced.io_priority {
            info.io_ok = Some(priority::set_io_priority(h, io).is_ok());
        }
        if let Some(mem) = rule.advanced.memory_priority {
            info.mem_ok = Some(priority::set_memory_priority(h, mem).is_ok());
        }

        // 回讀實際狀態（面板顯示「實際值」而非「期望值」；已從 affinity result
        // 取得 cores 時不覆蓋，只有 current_cores 為空（不該發生）才 fallback 讀回）
        if info.current_cores.is_empty() {
            if let Ok(mask) = process::get_affinity(h) {
                info.current_cores = topology::mask_to_indices(mask);
            }
        }
        info.current_priority = process::get_priority(h).as_str().to_string();
    }

    let failed = info.error.is_some();
    let is_denied = info.error.as_deref() == Some(codes::ACCESS_DENIED);
    AppliedEntry {
        info,
        exe_name: exe_name.to_string(),
        access_denied: is_denied,
        retries_left: MAX_RETRIES,
        next_retry: if failed {
            Some(now + retry_delay(is_denied, interval_ms))
        } else {
            None
        },
    }
}

fn retry_delay(access_denied: bool, interval_ms: u64) -> Duration {
    if access_denied {
        ACCESS_DENIED_BACKOFF
    } else {
        Duration::from_millis(interval_ms.max(500))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rule;

    fn rule(exe_path: &str, match_by: MatchBy) -> Rule {
        let mut r = Rule::new(exe_path.to_string(), "Test".to_string());
        r.match_by = match_by;
        r
    }

    #[test]
    fn fullpath_matches_case_insensitive() {
        let r = rule(r"C:\Games\Game.exe", MatchBy::FullPath);
        assert!(matches(&r, "Game.exe", Some(r"c:\games\game.exe")));
        assert!(!matches(&r, "Game.exe", Some(r"C:\Other\game.exe")));
    }

    #[test]
    fn fullpath_handles_extended_prefix() {
        let r = rule(r"C:\Games\Game.exe", MatchBy::FullPath);
        assert!(matches(&r, "Game.exe", Some(r"\\?\C:\GAMES\GAME.EXE")));
    }

    #[test]
    fn fullpath_fails_without_path() {
        let r = rule(r"C:\Games\Game.exe", MatchBy::FullPath);
        assert!(!matches(&r, "Game.exe", None));
    }

    #[test]
    fn filename_matches_without_path() {
        let r = rule(r"C:\Games\Game.exe", MatchBy::FileName);
        assert!(matches(&r, "game.EXE", None));
        assert!(matches(&r, "GAME.EXE", Some(r"D:\elsewhere\GAME.EXE")));
        assert!(!matches(&r, "other.exe", None));
    }
}
