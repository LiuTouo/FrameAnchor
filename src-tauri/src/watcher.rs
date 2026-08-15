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
use crate::model::{AffinityMode, AffinitySpec, MatchBy, Rule};
use crate::topology::{self, Topology};
use crate::{commands, priority, process, AppState};

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
    /// 套用時記錄的 process creation time（0 = 未知），PID 重用偵測用
    pub created: u64,
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

/// Affinity 套用結果（三層降級：硬綁定 → CPU Sets → 執行緒 ideal）
enum AffinityResult {
    /// 硬綁定成功（SetProcessAffinityMask），攜帶實際設定的核心清單
    HardOk { cores: Vec<u32> },
    /// CPU Sets 成功（SetProcessDefaultCpuSets）
    CpuSetsOk { cores: Vec<u32> },
    /// 執行緒 ideal 全成功（軟成功）
    SoftOk {
        cores: Vec<u32>,
        attempted: usize,
        succeeded: usize,
    },
    /// 執行緒 ideal 部分成功（partial，不算完整成功）
    SoftPartial {
        cores: Vec<u32>,
        attempted: usize,
        succeeded: usize,
    },
    /// 三層全部失敗
    AllFailed,
}

/// 執行緒 ideal 套用結果分類
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadIdealClass {
    /// 零個執行緒成功（或無執行緒）→ 視為失敗
    Zero,
    /// 部分成功
    Partial,
    /// 全部成功
    Full,
}

/// 依 attempted/succeeded 分類執行緒 ideal 結果。
fn classify_thread_ideal(attempted: usize, succeeded: usize) -> ThreadIdealClass {
    if attempted == 0 || succeeded == 0 {
        ThreadIdealClass::Zero
    } else if succeeded >= attempted {
        ThreadIdealClass::Full
    } else {
        ThreadIdealClass::Partial
    }
}

/// cores → affinity mask，過濾 c >= 64 避免 1<<64 未定義行為。
fn mask_from_cores(cores: &[u32]) -> u64 {
    cores
        .iter()
        .fold(0u64, |m, &c| if c < 64 { m | (1u64 << c) } else { m })
}

/// 依模式解析目標核心清單（group 0，最多 64 LP）。
fn cores_for_mode(spec: &AffinitySpec, topo: &Topology) -> Vec<u32> {
    match spec.mode {
        AffinityMode::Prefer => spec
            .cores
            .iter()
            .copied()
            .filter(|&c| c < topo.total_lp && c < 64)
            .collect(),
        _ => topology::mask_to_indices(topology::resolve_mask(spec, topo)),
    }
}

/// 判斷 applied 條目是否因 PID 重用而過期（純邏輯，可測試）。
/// `stored_created`：套用時記錄的 creation time（0 = 未知）；`current_created`：現在查到的（None = 查不到）。
fn is_reused(exe_name_changed: bool, stored_created: u64, current_created: Option<u64>) -> bool {
    if exe_name_changed {
        return true;
    }
    // exe 同名：只有已知基線（stored_created != 0）且現在查得到且不同，才算重用。
    // 查不到（受保護進程）→ 保守保留既有身份，不因查詢失敗就丟棄已證實的緩存。
    match current_created {
        Some(t) => stored_created != 0 && t != stored_created,
        None => false,
    }
}

/// 對進程套用 affinity，逐層降級：硬綁定 → CPU Sets → 執行緒 ideal。
/// All 模式還原是否算成功（純邏輯，可測試）：硬綁定還原 與 CPU Sets 清除 都須成功。
fn all_restore_succeeded(hard_ok: bool, clear_ok: bool) -> bool {
    hard_ok && clear_ok
}

/// 對進程套用 affinity。All 模式 = 還原（硬綁定設回全核心 + 清除 CPU Sets 指派）；
/// 非 All = 三層降級 硬綁定 → CPU Sets → 執行緒 ideal。
fn apply_affinity(
    pid: u32,
    h: Option<HANDLE>,
    spec: &AffinitySpec,
    topo: &Topology,
) -> AffinityResult {
    let cores = cores_for_mode(spec, topo);
    if cores.is_empty() {
        return AffinityResult::AllFailed;
    }

    // All 模式：還原必須「硬綁定設回全核心」與「清除 process-default CPU Sets 指派」
    // 兩者都成功，否則不宣告還原完成（進入 retry，不單回報顯示成功）。
    if spec.mode == AffinityMode::All {
        let hard_ok = h
            .map(|h| process::set_affinity(h, mask_from_cores(&cores)).is_ok())
            .unwrap_or(false);
        let clear_ok = process::clear_cpu_sets(pid).is_ok();
        if !all_restore_succeeded(hard_ok, clear_ok) {
            log::warn!("All 模式還原不完整 PID {pid}: hard={hard_ok} clear={clear_ok}");
            return AffinityResult::AllFailed;
        }
        // 回讀實際有效硬綁定 mask（不單報告期望值）
        if let Some(h) = h {
            if let Ok(actual) = process::get_affinity(h) {
                return AffinityResult::HardOk {
                    cores: topology::mask_to_indices(actual),
                };
            }
        }
        return AffinityResult::HardOk { cores };
    }

    // 非 All：Tier 1 硬綁定（SetProcessAffinityMask）
    if let Some(h) = h {
        let mask = mask_from_cores(&cores);
        if process::set_affinity(h, mask).is_ok() {
            return AffinityResult::HardOk { cores };
        }
        log::info!("硬綁定失敗 PID {pid}，降級到 CPU Sets");
    }

    // Tier 2: CPU Sets（SetProcessDefaultCpuSets）
    match process::set_cpu_sets(pid, &cores) {
        Ok(()) => {
            log::info!("CPU Sets 成功 PID {pid}");
            return AffinityResult::CpuSetsOk { cores };
        }
        Err(e) => log::warn!("CPU Sets 失敗 PID {pid}: {e}"),
    }

    // Tier 3: 執行緒 ideal processor（軟提示）
    let outcome = process::set_threads_ideal(pid, &cores);
    match classify_thread_ideal(outcome.attempted, outcome.succeeded) {
        ThreadIdealClass::Full => {
            log::info!(
                "執行緒 ideal 全成功 PID {pid}: {}/{}",
                outcome.succeeded,
                outcome.attempted
            );
            AffinityResult::SoftOk {
                cores,
                attempted: outcome.attempted,
                succeeded: outcome.succeeded,
            }
        }
        ThreadIdealClass::Partial => {
            log::info!(
                "執行緒 ideal 部分成功 PID {pid}: {}/{}",
                outcome.succeeded,
                outcome.attempted
            );
            AffinityResult::SoftPartial {
                cores,
                attempted: outcome.attempted,
                succeeded: outcome.succeeded,
            }
        }
        ThreadIdealClass::Zero => {
            log::warn!("執行緒 ideal 失敗 PID {pid}: 0/{}", outcome.attempted);
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
    state.handles.write().unwrap().insert(
        pid,
        CachedHandle {
            handle: oh,
            created,
        },
    );
    Ok(h)
}

/// 取得 pid 的 creation time：優先用快取 handle 的 created（反作弊保護後仍可用），
/// 否則現查。查不到回 0（未知，呼叫端保守處理）。
fn fetch_created(state: &Arc<AppState>, pid: u32) -> u64 {
    if let Some(c) = state.handles.read().unwrap().get(&pid) {
        return c.created;
    }
    process::creation_time_by_pid(pid).unwrap_or(0)
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
        let created = fetch_created(state, pid);
        let entry = apply_and_build(
            pid,
            &exe_name,
            &rule,
            &state.topology,
            Instant::now(),
            interval,
            (handle, created),
        );
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
            // created == 0（未知基線）→ 保守保留，不因後續可查就誤判重用。
            let stale: Vec<u32> = handles
                .iter()
                .filter(|(pid, c)| {
                    if c.created == 0 {
                        return false;
                    }
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

        // 2) PID 重用防護：還活著但 exe 變了 或 creation time 變了 → 移除，當新進程重新比對
        let stale: Vec<u32> = applied
            .iter()
            .filter(|(pid, e)| {
                procs.iter().find(|p| p.pid == **pid).is_some_and(|p| {
                    if !p.exe_name.eq_ignore_ascii_case(&e.exe_name) {
                        return true;
                    }
                    // exe 同名：比對 creation time（查不到 = 受保護 → 保守保留）
                    is_reused(false, e.created, process::creation_time_by_pid(p.pid))
                })
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
            .filter(|(_, e)| !rules.iter().any(|r| r.id == e.info.rule_id && r.enabled))
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
            let created = fetch_created(state, p.pid);
            let entry = apply_and_build(
                p.pid,
                &p.exe_name,
                &rule,
                &state.topology,
                now,
                interval_ms,
                (handle, created),
            );
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
                let created = fetch_created(state, pid);
                let mut entry = apply_and_build(
                    pid,
                    &exe_name,
                    &rule,
                    &state.topology,
                    now,
                    interval_ms,
                    (handle, created),
                );
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
/// 三層 affinity fallback：硬綁定 → CPU Sets → 執行緒 ideal。
/// handle 由呼叫端透過 ensure_handle 取得（快取的早期 handle 或現開）。
fn apply_and_build(
    pid: u32,
    exe_name: &str,
    rule: &Rule,
    topo: &Topology,
    now: Instant,
    interval_ms: u64,
    handle: (Result<HANDLE, ProcessError>, u64),
) -> AppliedEntry {
    let (handle_result, created) = handle;
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
        thread_ideal_attempted: None,
        thread_ideal_succeeded: None,
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
        AffinityResult::CpuSetsOk { cores } => {
            info.affinity_ok = true;
            info.current_cores = cores;
            log::info!("CPU Sets fallback PID {pid}");
        }
        AffinityResult::SoftOk {
            cores,
            attempted,
            succeeded,
        } => {
            info.affinity_ok = true;
            info.soft_affinity = true;
            info.current_cores = cores;
            info.thread_ideal_attempted = Some(attempted);
            info.thread_ideal_succeeded = Some(succeeded);
            log::info!("執行緒 ideal fallback PID {pid}: {succeeded}/{attempted}");
        }
        AffinityResult::SoftPartial {
            cores,
            attempted,
            succeeded,
        } => {
            // partial：不設 affinity_ok（不視為完整成功），也不設 error（非失敗需重試）
            info.soft_affinity = true;
            info.current_cores = cores;
            info.thread_ideal_attempted = Some(attempted);
            info.thread_ideal_succeeded = Some(succeeded);
            log::info!("執行緒 ideal 部分套用 PID {pid}: {succeeded}/{attempted}");
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
        created,
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

    fn topo(n: u32) -> Topology {
        topology::build_topology((0..n).map(|c| (vec![c], 0, false)).collect())
    }

    fn spec(mode: AffinityMode, cores: &[u32]) -> AffinitySpec {
        AffinitySpec {
            mode,
            cores: cores.to_vec(),
        }
    }

    #[test]
    fn classify_zero_when_no_threads_or_no_success() {
        assert_eq!(classify_thread_ideal(0, 0), ThreadIdealClass::Zero);
        assert_eq!(classify_thread_ideal(5, 0), ThreadIdealClass::Zero);
    }

    #[test]
    fn classify_full_when_all_succeeded() {
        assert_eq!(classify_thread_ideal(5, 5), ThreadIdealClass::Full);
    }

    #[test]
    fn classify_partial_when_some_succeeded() {
        assert_eq!(classify_thread_ideal(5, 3), ThreadIdealClass::Partial);
        assert_eq!(classify_thread_ideal(5, 1), ThreadIdealClass::Partial);
    }

    #[test]
    fn mask_from_cores_safe_and_no_shift_overflow() {
        assert_eq!(mask_from_cores(&[0, 2, 4]), 0b10101);
        assert_eq!(mask_from_cores(&[63]), 1u64 << 63);
        assert_eq!(mask_from_cores(&[64]), 0); // 過濾掉，避免 1<<64 UB
        assert_eq!(mask_from_cores(&[]), 0);
    }

    #[test]
    fn cores_all_covers_every_lp() {
        let t = topo(8);
        assert_eq!(cores_for_mode(&spec(AffinityMode::All, &[]), &t), (0..8).collect::<Vec<u32>>());
    }

    #[test]
    fn cores_prefer_filters_out_of_range() {
        let t = topo(8);
        assert_eq!(
            cores_for_mode(&spec(AffinityMode::Prefer, &[0, 1, 99]), &t),
            vec![0, 1]
        );
    }

    #[test]
    fn cores_custom_resolves_mask() {
        let t = topo(8);
        assert_eq!(
            cores_for_mode(&spec(AffinityMode::Custom, &[0, 2, 4]), &t),
            vec![0, 2, 4]
        );
    }

    #[test]
    fn reused_when_exe_name_changed() {
        assert!(is_reused(true, 0, None));
    }

    #[test]
    fn reused_when_same_name_but_creation_differs() {
        assert!(is_reused(false, 123, Some(456)));
    }

    #[test]
    fn not_reused_when_same_name_same_creation() {
        assert!(!is_reused(false, 123, Some(123)));
    }

    #[test]
    fn not_reused_when_creation_unknown_at_apply() {
        // stored_created == 0（受保護進程取不到基線）→ 保守保留，即使現在可查到
        assert!(!is_reused(false, 0, Some(456)));
    }

    #[test]
    fn not_reused_when_creation_unqueryable_now() {
        // 現在查不到（受保護）→ 保守保留，不因查詢失敗丟棄已證實身份
        assert!(!is_reused(false, 123, None));
    }

    #[test]
    fn all_clear_failure_is_not_success() {
        // All 還原：硬綁定還原 與 CPU Sets 清除 都須成功才算成功
        assert!(all_restore_succeeded(true, true));
        // CPU Sets 清除失敗 → 不算成功（不得宣告限制已移除）
        assert!(!all_restore_succeeded(true, false));
        // 硬綁定還原失敗 → 不算成功
        assert!(!all_restore_succeeded(false, true));
        assert!(!all_restore_succeeded(false, false));
    }
}
