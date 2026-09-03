//! 規則引擎（PLAN §7.6）：高頻 discovery 搶先開 handle → 比對規則 → 套用 → 維護 applied 狀態表。
//! 反作弊對策：EAC 等用 ObRegisterCallbacks 剝奪「新開啟」handle 的權限，但保護生效前
//! 已持有的 handle 不受影響 — 因此 discovery 以 100ms 掃描，在遊戲啟動後立刻開 handle
//! 並快取至進程結束；之後所有套用/重試都走快取 handle。
//! 退避策略：ACCESS_DENIED（反作弊）每 30 秒重試；其他錯誤重試 3 次。

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::AppHandle;
use windows::Win32::Foundation::HANDLE;

use crate::applied::{emit_applied, AffinityStrategy, AppliedProcess};
use crate::error::{codes, ProcessError};
use crate::model::{AffinityMode, AffinitySpec, MatchBy, Rule};
use crate::topology::{self, Topology};
use crate::{priority, process, AppState};

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
    /// 所有允許的策略都失敗，攜帶對外穩定錯誤碼。
    Failed { code: &'static str },
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

/// 正規化核心清單：過濾不支援 LP（>= 64，group 0 上限）、排序、去重。
/// 期望值與回讀值都先正規化再精確比對（純函式，可測試）。
fn normalize_cores(cores: &[u32]) -> Vec<u32> {
    let mut v: Vec<u32> = cores.iter().copied().filter(|&c| c < 64).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// 回讀驗證結果分類（純邏輯，可測試）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Revalidate {
    /// 實際值與期望值（正規化後）精確相等
    Match,
    /// 讀到值但不符
    Mismatch,
    /// 回讀失敗（讀不到，狀態未知）
    ReadFailed,
}

/// 比對期望核心與回讀值（皆正規化）。None = 回讀失敗。
fn compare_verified(expected: &[u32], actual: Option<Vec<u32>>) -> Revalidate {
    match actual {
        Some(a) if normalize_cores(&a) == normalize_cores(expected) => Revalidate::Match,
        Some(_) => Revalidate::Mismatch,
        None => Revalidate::ReadFailed,
    }
}

/// CPU Sets 套用後的處置（純邏輯，可測試）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum CpuSetsOutcome {
    /// 已驗證：設定後回讀相符
    Verified(Vec<u32>),
    /// 可安全降級到執行緒 ideal：setter 失敗（未寫入）或寫入後已清除為空
    Fallback,
    /// 失敗封閉：寫入後未驗證且無法清除為空，不得降級
    FailClosed,
}

/// 決定 CPU Sets 套用結果（純函式，可測試）。
/// `read_after_clear` 只有在 `clear_ok` 為真時才有意義。
fn decide_cpu_sets(
    expected: &[u32],
    set_ok: bool,
    read_after_set: Option<Vec<u32>>,
    clear_ok: bool,
    read_after_clear: Option<Vec<u32>>,
) -> CpuSetsOutcome {
    if !set_ok {
        // setter 失敗：未寫入任何 CPU Sets，可安全降級
        return CpuSetsOutcome::Fallback;
    }
    match read_after_set {
        Some(actual) if normalize_cores(&actual) == normalize_cores(expected) => {
            CpuSetsOutcome::Verified(actual)
        }
        // 回讀不符或失敗：寫入了未知 CPU Sets，必須清除為空才能降級
        _ => {
            let cleared_empty = clear_ok && matches!(&read_after_clear, Some(a) if a.is_empty());
            if cleared_empty {
                CpuSetsOutcome::Fallback
            } else {
                CpuSetsOutcome::FailClosed
            }
        }
    }
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

fn affinity_failure_code(errors: &[ProcessError]) -> &'static str {
    if errors.iter().any(ProcessError::is_access_denied) {
        codes::ACCESS_DENIED
    } else if errors
        .iter()
        .any(|e| matches!(e, ProcessError::OpenFailed(_)))
    {
        codes::OPEN_FAILED
    } else {
        codes::SET_AFFINITY_FAILED
    }
}

fn apply_thread_ideal(pid: u32, cores: Vec<u32>, errors: &mut Vec<ProcessError>) -> AffinityResult {
    let outcome = process::set_threads_ideal(pid, &cores);
    if let Some(error) = outcome.first_error {
        errors.push(error);
    }
    // 第一個 thread 錯誤未必是 ACCESS_DENIED；只要任一 thread 回 5，
    // 整體零成功時就必須進入反作弊的無限退避重試。
    if outcome.access_denied && !errors.iter().any(ProcessError::is_access_denied) {
        errors.push(ProcessError::AccessDenied);
    }
    match classify_thread_ideal(outcome.attempted, outcome.succeeded) {
        ThreadIdealClass::Full => AffinityResult::SoftOk {
            cores,
            attempted: outcome.attempted,
            succeeded: outcome.succeeded,
        },
        ThreadIdealClass::Partial => AffinityResult::SoftPartial {
            cores,
            attempted: outcome.attempted,
            succeeded: outcome.succeeded,
        },
        ThreadIdealClass::Zero => AffinityResult::Failed {
            code: affinity_failure_code(errors),
        },
    }
}

/// 對進程套用 affinity。All 模式 = 還原（硬綁定設回全核心 + 清除 CPU Sets 指派）；
/// 非 All = 三層降級 硬綁定 → CPU Sets → 執行緒 ideal。
fn apply_affinity(
    pid: u32,
    handle: Result<HANDLE, ProcessError>,
    spec: &AffinitySpec,
    topo: &Topology,
) -> AffinityResult {
    if topo.total_lp == 0 {
        return AffinityResult::Failed {
            code: codes::TOPOLOGY_FAILED,
        };
    }
    let cores = normalize_cores(&cores_for_mode(spec, topo));
    if cores.is_empty() {
        return AffinityResult::Failed {
            code: codes::SET_AFFINITY_FAILED,
        };
    }
    let h = handle.ok();
    let mut errors: Vec<ProcessError> = handle.err().into_iter().collect();

    // Prefer 的公開契約是純偏好：只能設定 thread ideal，不得嘗試硬 mask 或 CPU Sets。
    if spec.mode == AffinityMode::Prefer {
        return apply_thread_ideal(pid, cores, &mut errors);
    }

    // All 模式：還原必須「硬綁定設回全核心」與「清除 process-default CPU Sets 指派」
    // 兩者都成功，否則不宣告還原完成（進入 retry，不單回報顯示成功）。
    if spec.mode == AffinityMode::All {
        let hard_ok = h.is_some_and(
            |h| match process::set_affinity(h, mask_from_cores(&cores)) {
                Ok(()) => true,
                Err(e) => {
                    errors.push(e);
                    false
                }
            },
        );
        let clear_result = match h {
            Some(h) => process::clear_cpu_sets_by_handle(h),
            None => process::clear_cpu_sets(pid),
        };
        let clear_ok = match clear_result {
            Ok(()) => true,
            Err(e) => {
                errors.push(e);
                false
            }
        };
        if !all_restore_succeeded(hard_ok, clear_ok) {
            log::warn!("All 模式還原不完整 PID {pid}: hard={hard_ok} clear={clear_ok}");
            return AffinityResult::Failed {
                code: affinity_failure_code(&errors),
            };
        }
        // 回讀實際有效硬綁定 mask 並精確比對（fail-closed：回讀失敗或不符都不宣告成功）
        if let Some(h) = h {
            match process::get_affinity(h) {
                Ok(actual) if normalize_cores(&topology::mask_to_indices(actual)) == cores => {
                    return AffinityResult::HardOk {
                        cores: topology::mask_to_indices(actual),
                    };
                }
                Ok(actual) => {
                    log::warn!(
                        "All 模式還原回讀不符 PID {pid}: 期望 {:?} 實際 {:?}",
                        cores,
                        topology::mask_to_indices(actual)
                    );
                    return AffinityResult::Failed {
                        code: codes::SET_AFFINITY_FAILED,
                    };
                }
                Err(e) => {
                    log::warn!("All 模式還原回讀失敗 PID {pid}: {e}");
                    errors.push(e);
                    return AffinityResult::Failed {
                        code: affinity_failure_code(&errors),
                    };
                }
            }
        }
        return AffinityResult::Failed {
            code: affinity_failure_code(&errors),
        };
    }

    // 非 All：Tier 1 硬綁定（SetProcessAffinityMask），立即回讀驗證
    if let Some(h) = h {
        let mask = mask_from_cores(&cores);
        match process::set_affinity(h, mask) {
            Ok(()) => {
                match process::get_affinity(h) {
                    Ok(actual) if normalize_cores(&topology::mask_to_indices(actual)) == cores => {
                        return AffinityResult::HardOk {
                            cores: topology::mask_to_indices(actual),
                        };
                    }
                    Ok(actual) => {
                        // fail-closed：硬 mask 狀態未知，不得降級疊加 CPU Sets / ideal
                        log::warn!(
                            "硬綁定回讀不符 PID {pid}: 期望 {:?} 實際 {:?}",
                            cores,
                            topology::mask_to_indices(actual)
                        );
                        return AffinityResult::Failed {
                            code: codes::SET_AFFINITY_FAILED,
                        };
                    }
                    Err(e) => {
                        log::warn!("硬綁定回讀失敗 PID {pid}: {e}");
                        errors.push(e);
                        return AffinityResult::Failed {
                            code: affinity_failure_code(&errors),
                        };
                    }
                }
            }
            Err(e) => errors.push(e),
        }
        log::info!("硬綁定失敗 PID {pid}，降級到 CPU Sets");
    }

    // Tier 2: CPU Sets（SetProcessDefaultCpuSets），立即回讀驗證。
    // 寫入後未驗證 → 必須清除為空（並驗證為空）才能降級到執行緒 ideal，否則 fail-closed。
    let set_result = match h {
        Some(h) => process::set_cpu_sets_by_handle(h, &cores),
        None => process::set_cpu_sets(pid, &cores),
    };
    let set_ok = match set_result {
        Ok(()) => true,
        Err(e) => {
            errors.push(e);
            false
        }
    };
    let read_after_set = if set_ok {
        let result = match h {
            Some(h) => process::get_cpu_sets_by_handle(h),
            None => process::get_cpu_sets(pid),
        };
        match result {
            Ok(actual) => Some(actual),
            Err(e) => {
                errors.push(e);
                None
            }
        }
    } else {
        None
    };
    let (clear_ok, read_after_clear) = {
        let needs_clear =
            set_ok && compare_verified(&cores, read_after_set.clone()) != Revalidate::Match;
        if needs_clear {
            let clear_result = match h {
                Some(h) => process::clear_cpu_sets_by_handle(h),
                None => process::clear_cpu_sets(pid),
            };
            let c = match clear_result {
                Ok(()) => true,
                Err(e) => {
                    errors.push(e);
                    false
                }
            };
            (
                c,
                if c {
                    let result = match h {
                        Some(h) => process::get_cpu_sets_by_handle(h),
                        None => process::get_cpu_sets(pid),
                    };
                    match result {
                        Ok(actual) => Some(actual),
                        Err(e) => {
                            errors.push(e);
                            None
                        }
                    }
                } else {
                    None
                },
            )
        } else {
            (false, None)
        }
    };
    match decide_cpu_sets(&cores, set_ok, read_after_set, clear_ok, read_after_clear) {
        CpuSetsOutcome::Verified(actual) => {
            log::info!("CPU Sets 驗證成功 PID {pid}");
            return AffinityResult::CpuSetsOk { cores: actual };
        }
        CpuSetsOutcome::Fallback => {
            if set_ok {
                log::warn!("CPU Sets 未驗證 PID {pid}，已清除為空並降級到執行緒 ideal");
            } else {
                log::warn!("CPU Sets 失敗 PID {pid}，降級到執行緒 ideal");
            }
        }
        CpuSetsOutcome::FailClosed => {
            log::warn!("CPU Sets 清除失敗或仍非空 PID {pid}，fail-closed");
            return AffinityResult::Failed {
                code: affinity_failure_code(&errors),
            };
        }
    }

    // Tier 3: 執行緒 ideal processor（軟提示）
    apply_thread_ideal(pid, cores, &mut errors)
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

/// 回讀來源（純決策，可測試）：有快取 handle 優先走 handle，否則 pid 現開。
enum ReadbackSource {
    Cached(HANDLE),
    FreshOpen,
}

fn readback_source(cached: Option<HANDLE>) -> ReadbackSource {
    match cached {
        Some(h) => ReadbackSource::Cached(h),
        None => ReadbackSource::FreshOpen,
    }
}

/// 週期回讀實際 affinity（revalidation）。優先快取 handle（反作弊保護後仍可用），
/// 無快取才 pid 現開。回傳 None = 回讀失敗（狀態未知）。
fn revalidate_affinity(
    state: &Arc<AppState>,
    pid: u32,
    strategy: AffinityStrategy,
) -> Option<Vec<u32>> {
    // 先拷貝快取 handle，避免在 Windows API 呼叫期間持有 state.handles 鎖
    let cached = state.handles.read().unwrap().get(&pid).map(|c| c.handle.0);
    match strategy {
        AffinityStrategy::Hard => match readback_source(cached) {
            ReadbackSource::Cached(h) => {
                process::get_affinity(h).ok().map(topology::mask_to_indices)
            }
            ReadbackSource::FreshOpen => process::get_affinity_by_pid(pid)
                .ok()
                .map(topology::mask_to_indices),
        },
        AffinityStrategy::CpuSets => match readback_source(cached) {
            ReadbackSource::Cached(h) => process::get_cpu_sets_by_handle(h).ok(),
            ReadbackSource::FreshOpen => process::get_cpu_sets(pid).ok(),
        },
        _ => None,
    }
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
    let (rules, interval, revision): (Vec<Rule>, u64, u64) = state
        .config
        .read()
        .map(|c| {
            (
                c.rules.iter().filter(|r| r.enabled).cloned().collect(),
                c.settings.poll_interval_ms.clamp(200, 60_000),
                state.config_revision.load(Ordering::Acquire),
            )
        })
        .unwrap_or_else(|_| (Vec::new(), 1000, 0));
    if rules.is_empty() {
        return;
    }
    // 檔名過濾集合：名字命中才需要進一步比對（路徑解析很貴，只做候選者）
    let wanted: HashSet<String> = rules
        .iter()
        .map(|r| file_name(&r.exe_path).to_lowercase())
        .collect();

    let names = match process::enumerate_process_names() {
        Ok(names) => names,
        Err(e) => {
            log::warn!("discovery 程序列舉失敗，保留既有狀態: {e}");
            return;
        }
    };
    let candidates: Vec<(u32, String)> = names
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
        // 套用期間若規則已變更，不把舊規則的結果寫回 applied；command 會清表，
        // 下一個 discovery/tick 會依新設定重套。
        if let Ok(cfg) = state.config.read() {
            if state.config_revision.load(Ordering::Acquire) == revision {
                state.applied.write().unwrap().entry(pid).or_insert(entry);
                dirty = true;
            } else {
                log::info!("規則於套用 PID {pid} 期間變更，棄置過期 applied 結果");
            }
            drop(cfg);
        }
    }
    if dirty {
        emit_applied(app, state);
    }
}

fn tick(app: &AppHandle, state: &Arc<AppState>, interval_ms: u64) {
    let procs = match process::enumerate_processes() {
        Ok(procs) => procs,
        Err(e) => {
            log::warn!("watcher 程序列舉失敗，跳過本 tick 並保留 applied/handles: {e}");
            return;
        }
    };
    let (rules, revision): (Vec<Rule>, u64) = state
        .config
        .read()
        .map(|c| {
            (
                c.rules.clone(),
                state.config_revision.load(Ordering::Acquire),
            )
        })
        .unwrap_or_default();
    let now = Instant::now();
    let mut dirty = false;

    // 1) 移除已結束的 PID。所有 Win32 查詢均在 applied/handles 鎖外執行。
    let alive: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
    {
        let mut applied = state.applied.write().unwrap();
        let before = applied.len();
        applied.retain(|pid, _| alive.contains(pid));
        if applied.len() != before {
            dirty = true;
        }
    }

    let handle_snapshot: Vec<(u32, u64)> = state
        .handles
        .read()
        .unwrap()
        .iter()
        .map(|(pid, cached)| (*pid, cached.created))
        .collect();
    let stale_handles: HashSet<u32> = handle_snapshot
        .into_iter()
        .filter(|(pid, created)| {
            !alive.contains(pid)
                || (*created != 0
                    && process::creation_time_by_pid(*pid)
                        .map(|actual| actual != *created)
                        .unwrap_or(false))
        })
        .map(|(pid, _)| pid)
        .collect();
    if !stale_handles.is_empty() {
        state
            .handles
            .write()
            .unwrap()
            .retain(|pid, _| !stale_handles.contains(pid));
    }

    // 2) PID 重用與失效規則清理。creation time 查詢仍在鎖外。
    let applied_snapshot: Vec<(u32, String, u64, String)> = state
        .applied
        .read()
        .unwrap()
        .iter()
        .map(|(pid, e)| (*pid, e.exe_name.clone(), e.created, e.info.rule_id.clone()))
        .collect();
    let stale_applied: HashSet<u32> = applied_snapshot
        .iter()
        .filter(|(pid, exe_name, created, rule_id)| {
            let reused = procs.iter().find(|p| p.pid == *pid).is_some_and(|p| {
                !p.exe_name.eq_ignore_ascii_case(exe_name)
                    || is_reused(false, *created, process::creation_time_by_pid(p.pid))
            });
            let orphaned = !rules.iter().any(|r| r.id == *rule_id && r.enabled);
            reused || orphaned
        })
        .map(|(pid, _, _, _)| *pid)
        .collect();
    if !stale_applied.is_empty() {
        let mut applied = state.applied.write().unwrap();
        let before = applied.len();
        applied.retain(|pid, _| !stale_applied.contains(pid));
        dirty |= applied.len() != before;
    }

    // 3) 新進程套用。昂貴 Win32 操作完成後，以 revision gate 避免寫入舊規則結果。
    for p in &procs {
        if state.applied.read().unwrap().contains_key(&p.pid)
            || process::is_blacklisted(p.pid, &p.exe_name, p.exe_path.as_deref())
        {
            continue;
        }
        let Some(rule) = find_rule(&rules, &p.exe_name, p.exe_path.as_deref()) else {
            continue;
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
        if let Ok(cfg) = state.config.read() {
            if state.config_revision.load(Ordering::Acquire) == revision {
                state.applied.write().unwrap().entry(p.pid).or_insert(entry);
                dirty = true;
            }
            drop(cfg);
        }
    }

    // 4) 週期性回讀驗證；只在快照仍相符時更新該 entry。
    let recheck: Vec<(u32, AffinityStrategy, Vec<u32>)> = state
        .applied
        .read()
        .unwrap()
        .iter()
        .filter(|(_, e)| {
            e.info.error.is_none()
                && matches!(
                    e.info.strategy,
                    AffinityStrategy::Hard | AffinityStrategy::CpuSets
                )
        })
        .map(|(pid, e)| (*pid, e.info.strategy, e.info.current_cores.clone()))
        .collect();
    for (pid, strategy, expected) in recheck {
        let actual = revalidate_affinity(state, pid, strategy);
        match compare_verified(&expected, actual) {
            Revalidate::Match => {}
            // 讀不到 ≠ 不符：反作弊剝 QUERY 權限或暫時性錯誤時狀態未知，
            // 保留已驗證 entry 原狀，下個 tick 自然再驗，不誤標 SET_AFFINITY_FAILED。
            Revalidate::ReadFailed => {
                log::debug!("回讀驗證讀取失敗 PID {pid} (strategy={strategy:?})，保留現狀");
            }
            Revalidate::Mismatch => {
                log::warn!("回讀驗證失敗 PID {pid} (strategy={strategy:?})，排程重套");
                if let Some(entry) = state.applied.write().unwrap().get_mut(&pid) {
                    if entry.info.strategy != strategy || entry.info.current_cores != expected {
                        continue;
                    }
                    entry.info.affinity_ok = false;
                    entry.info.strategy = AffinityStrategy::None;
                    entry.info.soft_affinity = false;
                    entry.info.current_cores.clear();
                    entry.info.error = Some(codes::SET_AFFINITY_FAILED.to_string());
                    entry.access_denied = false;
                    entry.retries_left = MAX_RETRIES;
                    entry.next_retry = Some(now + retry_delay(false, interval_ms));
                    dirty = true;
                }
            }
        }
    }

    // 5) 失敗重試。套用時不持 applied 寫鎖，提交前再確認 entry 與設定 revision。
    let due: Vec<(u32, String, String, u8)> = state
        .applied
        .read()
        .unwrap()
        .iter()
        .filter(|(_, e)| {
            e.info.error.is_some()
                && e.next_retry.map(|t| now >= t).unwrap_or(false)
                && (e.access_denied || e.retries_left > 0)
        })
        .map(|(pid, e)| {
            (
                *pid,
                e.info.rule_id.clone(),
                e.exe_name.clone(),
                e.retries_left,
            )
        })
        .collect();
    for (pid, rule_id, exe_name, previous_retries) in due {
        let Some(rule) = rules.iter().find(|r| r.id == rule_id && r.enabled).cloned() else {
            continue;
        };
        log::info!("重試套用「{}」→ PID {}", rule.name, pid);
        let handle = ensure_handle(state, pid);
        let created = fetch_created(state, pid);
        let mut replacement = apply_and_build(
            pid,
            &exe_name,
            &rule,
            &state.topology,
            now,
            interval_ms,
            (handle, created),
        );
        if replacement.info.error.is_some() && !replacement.access_denied {
            replacement.retries_left = previous_retries.saturating_sub(1);
        }
        if let Ok(cfg) = state.config.read() {
            if state.config_revision.load(Ordering::Acquire) == revision {
                let mut applied = state.applied.write().unwrap();
                if applied.get(&pid).is_some_and(|current| {
                    current.info.rule_id == rule_id && current.info.error.is_some()
                }) {
                    applied.insert(pid, replacement);
                    dirty = true;
                }
            }
            drop(cfg);
        }
    }

    if dirty {
        emit_applied(app, state);
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
        strategy: AffinityStrategy::None,
    };

    // Phase A: handle 由呼叫端 ensure_handle 提供（快取早期 handle 或現開）
    // Phase B: affinity（三層降級，handle 為 None 時跳過硬綁定）
    let aff_result = apply_affinity(pid, handle_result, &rule.affinity, topo);

    match aff_result {
        AffinityResult::HardOk { cores } => {
            info.affinity_ok = true;
            info.strategy = AffinityStrategy::Hard;
            info.current_cores = cores;
        }
        AffinityResult::CpuSetsOk { cores } => {
            info.affinity_ok = true;
            info.strategy = AffinityStrategy::CpuSets;
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
            info.strategy = AffinityStrategy::Prefer;
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
            info.strategy = AffinityStrategy::Prefer;
            info.current_cores = cores;
            info.thread_ideal_attempted = Some(attempted);
            info.thread_ideal_succeeded = Some(succeeded);
            log::info!("執行緒 ideal 部分套用 PID {pid}: {succeeded}/{attempted}");
        }
        AffinityResult::Failed { code } => {
            info.error = Some(code.to_string());
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
        match process::get_priority(h) {
            Ok(priority) => info.current_priority = priority.as_str().to_string(),
            Err(e) => log::warn!("get_priority 失敗 PID {pid}: {e}"),
        }
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
    fn affinity_error_classification_prioritizes_any_access_denied() {
        assert_eq!(
            affinity_failure_code(&[ProcessError::OpenFailed(87), ProcessError::AccessDenied]),
            codes::ACCESS_DENIED
        );
        assert_eq!(
            affinity_failure_code(&[ProcessError::OpenFailed(87), ProcessError::Win32(6)]),
            codes::OPEN_FAILED
        );
        assert_eq!(
            affinity_failure_code(&[ProcessError::Win32(87)]),
            codes::SET_AFFINITY_FAILED
        );
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
        assert_eq!(
            cores_for_mode(&spec(AffinityMode::All, &[]), &t),
            (0..8).collect::<Vec<u32>>()
        );
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

    #[test]
    fn normalize_sorts_dedups_filters() {
        assert_eq!(normalize_cores(&[3, 1, 3, 2, 99, 1]), vec![1, 2, 3]);
        assert_eq!(normalize_cores(&[]), Vec::<u32>::new());
        assert_eq!(normalize_cores(&[64]), Vec::<u32>::new());
        assert_eq!(normalize_cores(&[63, 0]), vec![0, 63]);
    }

    #[test]
    fn compare_verified_matches_after_normalize() {
        // 順序不同但集合相等 → Match（期望值與回讀值都先正規化）
        assert_eq!(
            compare_verified(&[1, 2, 3], Some(vec![3, 2, 1])),
            Revalidate::Match
        );
        assert_eq!(
            compare_verified(&[1, 2], Some(vec![1, 3])),
            Revalidate::Mismatch
        );
        assert_eq!(compare_verified(&[1, 2], None), Revalidate::ReadFailed);
        assert_eq!(
            compare_verified(&[1, 2], Some(vec![])),
            Revalidate::Mismatch
        );
    }

    #[test]
    fn cpu_sets_verified_when_readback_matches() {
        // 設定成功 + 回讀相符（順序不同仍相等）→ Verified，且不需清除
        assert_eq!(
            decide_cpu_sets(&[1, 2], true, Some(vec![2, 1]), false, None),
            CpuSetsOutcome::Verified(vec![2, 1])
        );
    }

    #[test]
    fn cpu_sets_setter_failure_allows_fallback() {
        // setter 失敗 = 未寫入任何 CPU Sets → 安全降級（clear 值被忽略）
        assert_eq!(
            decide_cpu_sets(&[1, 2], false, None, false, None),
            CpuSetsOutcome::Fallback
        );
    }

    #[test]
    fn cpu_sets_mismatch_plus_verified_clear_allows_fallback() {
        // 寫入後回讀不符 + 清除成功且回讀為空 → 允許降級
        assert_eq!(
            decide_cpu_sets(&[1, 2], true, Some(vec![9]), true, Some(vec![])),
            CpuSetsOutcome::Fallback
        );
    }

    #[test]
    fn cpu_sets_readback_failure_plus_verified_clear_allows_fallback() {
        // 寫入後回讀失敗 + 清除成功且回讀為空 → 允許降級
        assert_eq!(
            decide_cpu_sets(&[1, 2], true, None, true, Some(vec![])),
            CpuSetsOutcome::Fallback
        );
    }

    #[test]
    fn cpu_sets_clear_failure_fails_closed() {
        // 清除失敗 → 不得降級
        assert_eq!(
            decide_cpu_sets(&[1, 2], true, Some(vec![9]), false, None),
            CpuSetsOutcome::FailClosed
        );
    }

    #[test]
    fn cpu_sets_clear_readback_failure_fails_closed() {
        // 清除成功但回讀失敗（狀態未知）→ 不得降級
        assert_eq!(
            decide_cpu_sets(&[1, 2], true, Some(vec![9]), true, None),
            CpuSetsOutcome::FailClosed
        );
    }

    #[test]
    fn cpu_sets_clear_nonempty_fails_closed() {
        // 清除成功但回讀仍非空 → 不得降級
        assert_eq!(
            decide_cpu_sets(&[1, 2], true, Some(vec![9]), true, Some(vec![3])),
            CpuSetsOutcome::FailClosed
        );
    }

    fn sample_applied() -> AppliedProcess {
        AppliedProcess {
            pid: 1,
            exe_name: "game.exe".into(),
            rule_id: "r1".into(),
            rule_name: "Game".into(),
            affinity_ok: true,
            priority_ok: true,
            io_ok: None,
            mem_ok: None,
            error: None,
            applied_at: "2026-08-16T00:00:00Z".into(),
            current_cores: vec![0, 1],
            current_priority: "High".into(),
            soft_affinity: false,
            thread_ideal_attempted: None,
            thread_ideal_succeeded: None,
            strategy: AffinityStrategy::Hard,
        }
    }

    #[test]
    fn strategy_serializes_pascal_case() {
        let v = serde_json::to_value(sample_applied()).unwrap();
        assert_eq!(v["strategy"], "Hard");
        assert_eq!(v["softAffinity"], false);
    }

    #[test]
    fn strategy_default_is_none() {
        assert_eq!(AffinityStrategy::default(), AffinityStrategy::None);
    }

    #[test]
    fn readback_source_prefers_cached_handle() {
        let h = HANDLE::default();
        assert!(matches!(
            readback_source(Some(h)),
            ReadbackSource::Cached(_)
        ));
        assert!(matches!(readback_source(None), ReadbackSource::FreshOpen));
    }
}
