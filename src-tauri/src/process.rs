//! 進程列舉與操作（PLAN §7.2）：Toolhelp snapshot、affinity、priority。

use windows::core::PWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, FILETIME, HANDLE,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::Kernel::PROCESSOR_NUMBER;
use windows::Win32::System::SystemInformation::{
    CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
};
use windows::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessDefaultCpuSets, GetProcessTimes,
    OpenProcess, OpenThread, QueryFullProcessImageNameW, SetPriorityClass, SetProcessAffinityMask,
    SetProcessDefaultCpuSets, SetThreadIdealProcessorEx, TerminateProcess,
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
    IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_NAME_FORMAT, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, PROCESS_SET_LIMITED_INFORMATION,
    PROCESS_TERMINATE, PROCESS_VM_READ, REALTIME_PRIORITY_CLASS, THREAD_SET_INFORMATION,
};

use crate::error::ProcessError;
use crate::model::CpuPriority;

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub exe_name: String,
    /// 受保護進程取不到路徑時為 None
    pub exe_path: Option<String>,
}

/// 走訪 Toolhelp snapshot 內所有進程（pid, exe 名）。
/// 任一列舉錯誤都回傳 Err，呼叫端不得把失敗誤認為空清單。
fn for_each_process(mut f: impl FnMut(u32, String)) -> Result<(), ProcessError> {
    unsafe {
        let snap =
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(ProcessError::from_windows)?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if let Err(e) = Process32FirstW(snap, &mut entry) {
            let _ = CloseHandle(snap);
            return Err(ProcessError::from_windows(e));
        }
        loop {
            f(entry.th32ProcessID, utf16_slice_to_string(&entry.szExeFile));
            if let Err(e) = Process32NextW(snap, &mut entry) {
                let code = ProcessError::normalize_win32(e.code().0 as u32);
                let _ = CloseHandle(snap);
                return if code == ERROR_NO_MORE_FILES.0 {
                    Ok(())
                } else {
                    Err(ProcessError::from_win32(code))
                };
            }
        }
    }
}

/// Toolhelp snapshot 列舉全部進程（逐進程解析路徑，較重；tick 用）
pub fn enumerate_processes() -> Result<Vec<ProcessInfo>, ProcessError> {
    let mut result = Vec::new();
    for_each_process(|pid, exe_name| {
        let exe_path = process_path(pid);
        result.push(ProcessInfo {
            pid,
            exe_name,
            exe_path,
        });
    })?;
    Ok(result)
}

/// 輕量列舉：只有 pid + exe 名，不解析路徑。discovery 高頻掃描用。
pub fn enumerate_process_names() -> Result<Vec<(u32, String)>, ProcessError> {
    let mut result = Vec::new();
    for_each_process(|pid, name| result.push((pid, name)))?;
    Ok(result)
}

/// exe 完整路徑；受保護進程回傳 None
pub fn process_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(h);
        if ok && size > 0 {
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        } else {
            None
        }
    }
}

fn utf16_slice_to_string(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// RAII handle，自動 CloseHandle
pub struct OwnedHandle(pub HANDLE);

// kernel handle 是 process-wide 資源，可安全跨執行緒傳遞/共享
// （windows crate 的 HANDLE 內部是 *mut c_void，預設不實作 Send/Sync）
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 開啟用於設定的 handle
pub fn open_for_set(pid: u32) -> Result<OwnedHandle, ProcessError> {
    unsafe {
        OpenProcess(
            PROCESS_SET_INFORMATION
                | PROCESS_SET_LIMITED_INFORMATION
                | PROCESS_QUERY_INFORMATION
                | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            pid,
        )
        .map(OwnedHandle)
        .map_err(|_| ProcessError::from_last_open())
    }
}

/// process creation time（FILETIME 100ns ticks），PID 重用偵測用
pub fn process_creation_time(h: HANDLE) -> Option<u64> {
    unsafe {
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u).ok()?;
        Some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }
}

/// 用 QUERY_LIMITED_INFORMATION 開 handle 取 creation time。
/// 受保護進程可能連此權限都被剝 → None（呼叫端應保留既有快取 handle）
pub fn creation_time_by_pid(pid: u32) -> Option<u64> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let t = process_creation_time(h);
        let _ = CloseHandle(h);
        t
    }
}

/// 啟用 SeDebugPrivilege。本 app 以 admin 執行，token 內建此權限但預設停用。
/// 無法繞過反作弊的 ObRegisterCallbacks，但對 ACL 保護的進程有幫助。
pub fn enable_debug_privilege() {
    use windows::Win32::Foundation::LUID;
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_DEBUG_NAME,
        SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            return;
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(None, SE_DEBUG_NAME, &mut luid).is_ok() {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            let _ = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
        }
        let _ = CloseHandle(token);
    }
}

pub fn set_affinity(h: HANDLE, mask: u64) -> Result<(), ProcessError> {
    unsafe { SetProcessAffinityMask(h, mask as usize).map_err(ProcessError::from_windows) }
}

/// 清理孤兒 WebView2 子程序。host（frameanchor.exe）被強殺時，
/// msedgewebview2.exe 會殘留並鎖住 user-data 目錄，導致下一次
/// WebView2 environment 建立失敗（0x8007139F）→ 白畫面。
/// 啟動時呼叫：若沒有其他 live frameanchor 實例，終止所有
/// user-data-dir 指向本 app 的 webview 子程序。
pub fn kill_orphan_webviews() {
    use windows::Wdk::System::Threading::{NtQueryInformationProcess, PROCESSINFOCLASS};
    use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows::Win32::System::Threading::{
        PEB, PROCESS_BASIC_INFORMATION, RTL_USER_PROCESS_PARAMETERS,
    };

    let our_dir = format!(
        "{}\\com.frameanchor.app\\EBWebView",
        std::env::var("LOCALAPPDATA").unwrap_or_default()
    );
    let self_pid = std::process::id();

    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return,
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut webviews: Vec<u32> = Vec::new();
        let mut other_host_alive = false;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let name = utf16_slice_to_string(&entry.szExeFile).to_lowercase();
                if name == "frameanchor.exe" && entry.th32ProcessID != self_pid {
                    other_host_alive = true;
                } else if name == "msedgewebview2.exe" {
                    webviews.push(entry.th32ProcessID);
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        if other_host_alive {
            return; // 有其他實例在跑：它的 webview 是活的，不該動
        }
        for pid in webviews {
            // 讀 cmdline 比對 user-data-dir
            let cmdline = (|| {
                let Ok(h) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid)
                else {
                    return None;
                };
                let mut pbi = PROCESS_BASIC_INFORMATION::default();
                let status = NtQueryInformationProcess(
                    h,
                    PROCESSINFOCLASS(0), // ProcessBasicInformation
                    &mut pbi as *mut _ as *mut core::ffi::c_void,
                    std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                );
                if status.0 < 0 || pbi.PebBaseAddress.is_null() {
                    let _ = CloseHandle(h);
                    return None;
                }
                let mut peb = PEB::default();
                let mut read: usize = 0;
                if ReadProcessMemory(
                    h,
                    pbi.PebBaseAddress as *const _,
                    &mut peb as *mut _ as *mut _,
                    std::mem::size_of::<PEB>(),
                    Some(&mut read),
                )
                .is_err()
                {
                    let _ = CloseHandle(h);
                    return None;
                }
                let mut rtl = RTL_USER_PROCESS_PARAMETERS::default();
                if ReadProcessMemory(
                    h,
                    peb.ProcessParameters as *const _,
                    &mut rtl as *mut _ as *mut _,
                    std::mem::size_of::<RTL_USER_PROCESS_PARAMETERS>(),
                    Some(&mut read),
                )
                .is_err()
                {
                    let _ = CloseHandle(h);
                    return None;
                }
                let len = rtl.CommandLine.Length as usize;
                if rtl.CommandLine.Buffer.is_null() || len == 0 {
                    let _ = CloseHandle(h);
                    return None;
                }
                let mut buf = vec![0u16; len / 2];
                if ReadProcessMemory(
                    h,
                    rtl.CommandLine.Buffer.0 as *const core::ffi::c_void,
                    buf.as_mut_ptr() as *mut _,
                    len,
                    Some(&mut read),
                )
                .is_err()
                {
                    let _ = CloseHandle(h);
                    return None;
                }
                let _ = CloseHandle(h);
                Some(String::from_utf16_lossy(&buf))
            })();
            if let Some(cmd) = cmdline {
                if cmd.contains(&our_dir) {
                    if let Ok(h) = OpenProcess(
                        PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                        false,
                        pid,
                    ) {
                        let _ = TerminateProcess(h, 0);
                        let _ = CloseHandle(h);
                    }
                    log::info!("已清理孤兒 WebView2 子程序 PID {pid}");
                }
            }
        }
    }
}

/// 執行緒 ideal processor 套用結果（attempted = 隸屬該 pid 的全部執行緒，succeeded = 成功設定數）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadIdealOutcome {
    pub attempted: usize,
    pub succeeded: usize,
    pub first_error: Option<ProcessError>,
    pub access_denied: bool,
}

/// 軟綁定：進程所有執行緒設 ideal processor（偏好核心，round-robin）。
/// 單一執行緒失敗跳過（best-effort），回傳 attempted/succeeded 供呼叫端分類。
pub fn set_threads_ideal(pid: u32, cores: &[u32]) -> ThreadIdealOutcome {
    let zero = ThreadIdealOutcome {
        attempted: 0,
        succeeded: 0,
        first_error: None,
        access_denied: false,
    };
    if cores.is_empty() {
        return zero;
    }
    let mut attempted = 0usize;
    let mut succeeded = 0usize;
    let mut first_error = None;
    let mut access_denied = false;
    let mut idx = 0usize;
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) {
            Ok(h) => h,
            Err(e) => {
                let error = ProcessError::from_windows(e);
                return ThreadIdealOutcome {
                    first_error: Some(error),
                    access_denied: error.is_access_denied(),
                    ..zero
                };
            }
        };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        match Thread32First(snap, &mut entry) {
            Ok(()) => loop {
                if entry.th32OwnerProcessID == pid {
                    attempted += 1;
                    let preferred = PROCESSOR_NUMBER {
                        Group: 0, // v1 只支援 group 0（前 64 LP）
                        Number: cores[idx % cores.len()] as u8,
                        Reserved: 0,
                    };
                    match OpenThread(THREAD_SET_INFORMATION, false, entry.th32ThreadID) {
                        Ok(t) => {
                            match SetThreadIdealProcessorEx(t, &preferred, None) {
                                Ok(_) => succeeded += 1,
                                Err(e) => {
                                    let error = ProcessError::from_windows(e);
                                    first_error.get_or_insert(error);
                                    access_denied |= error.is_access_denied();
                                }
                            }
                            let _ = CloseHandle(t);
                        }
                        Err(e) => {
                            let error = ProcessError::from_windows(e);
                            first_error.get_or_insert(error);
                            access_denied |= error.is_access_denied();
                        }
                    }
                    idx += 1;
                }
                if let Err(e) = Thread32Next(snap, &mut entry) {
                    let code = ProcessError::normalize_win32(e.code().0 as u32);
                    if code != ERROR_NO_MORE_FILES.0 {
                        let error = ProcessError::from_win32(code);
                        first_error.get_or_insert(error);
                        access_denied |= error.is_access_denied();
                    }
                    break;
                }
            },
            Err(e) => {
                let code = ProcessError::normalize_win32(e.code().0 as u32);
                if code != ERROR_NO_MORE_FILES.0 {
                    let error = ProcessError::from_win32(code);
                    first_error.get_or_insert(error);
                    access_denied |= error.is_access_denied();
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    ThreadIdealOutcome {
        attempted,
        succeeded,
        first_error,
        access_denied,
    }
}

// ── CPU Sets API（Windows 10 1703+）──────────────────────────────────────

/// LP index ↔ CPU set ID 雙向映射（啟動時列舉一次）
struct CpuSetMap {
    lp_to_set_id: Vec<Option<u32>>,
    /// set ID → LP index 反向映射（回讀用）
    set_id_to_lp: std::collections::HashMap<u32, u32>,
}

/// 由 LP→setID 映射建立 setID→LP 反向映射（純函式，可測試）
fn build_set_id_to_lp(lp_to_set_id: &[Option<u32>]) -> std::collections::HashMap<u32, u32> {
    lp_to_set_id
        .iter()
        .enumerate()
        .filter_map(|(lp, id)| id.map(|id| (id, lp as u32)))
        .collect()
}

/// 列舉系統 CPU sets，建立 LP index ↔ set ID 雙向映射
fn enumerate_cpu_sets() -> Option<CpuSetMap> {
    unsafe {
        let mut needed: u32 = 0;
        // 第一次呼叫取得所需 buffer 長度（process=NULL=system-wide）
        let _ = GetSystemCpuSetInformation(None, 0, &mut needed, None, Some(0));
        if needed == 0 {
            return None;
        }
        let mut buf = vec![0u8; needed as usize];
        let result = GetSystemCpuSetInformation(
            Some(buf.as_mut_ptr() as *mut SYSTEM_CPU_SET_INFORMATION),
            needed,
            &mut needed,
            None,
            Some(0),
        );
        if !result.as_bool() {
            return None;
        }
        let count = needed as usize / std::mem::size_of::<SYSTEM_CPU_SET_INFORMATION>();
        let sets =
            std::slice::from_raw_parts(buf.as_ptr() as *const SYSTEM_CPU_SET_INFORMATION, count);
        let max_lp = sets
            .iter()
            .map(|s| s.Anonymous.CpuSet.LogicalProcessorIndex as u32)
            .max()
            .unwrap_or(0);
        let mut lp_to_set_id = vec![None; max_lp as usize + 1];
        for set in sets {
            // Type == CpuSetInformation(0) 為正常 CPU set；1=CpuSetParked 跳過
            if set.Type == CpuSetInformation {
                let lp = set.Anonymous.CpuSet.LogicalProcessorIndex as usize;
                if lp < lp_to_set_id.len() {
                    lp_to_set_id[lp] = Some(set.Anonymous.CpuSet.Id);
                }
            }
        }
        let set_id_to_lp = build_set_id_to_lp(&lp_to_set_id);
        Some(CpuSetMap {
            lp_to_set_id,
            set_id_to_lp,
        })
    }
}

/// 用 CPU Sets API 設定軟性 affinity（PLAN §15 v2 功能）。
/// 需要 `PROCESS_SET_LIMITED_INFORMATION`，與 `PROCESS_SET_INFORMATION` 不同。
/// 反作弊 kernel driver 較少攔截此權限，成功率比硬綁定高。
pub fn set_cpu_sets_by_handle(h: HANDLE, cores: &[u32]) -> Result<(), ProcessError> {
    let map = enumerate_cpu_sets().ok_or(ProcessError::OpenFailed(0))?;
    let set_ids: Vec<u32> = cores
        .iter()
        .filter_map(|&lp| map.lp_to_set_id.get(lp as usize).copied().flatten())
        .collect();
    if set_ids.is_empty() {
        return Err(ProcessError::OpenFailed(0));
    }
    let result = unsafe { SetProcessDefaultCpuSets(h, Some(&set_ids)) };
    if result.as_bool() {
        Ok(())
    } else {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
        Err(ProcessError::from_win32(code))
    }
}

pub fn set_cpu_sets(pid: u32, cores: &[u32]) -> Result<(), ProcessError> {
    let h = unsafe {
        OpenProcess(PROCESS_SET_LIMITED_INFORMATION, false, pid)
            .map_err(|_| ProcessError::from_last_open())?
    };
    let result = set_cpu_sets_by_handle(h, cores);
    let _ = unsafe { CloseHandle(h) };
    result
}

/// 清除 process-default CPU Sets 指派（All 模式還原用）。
/// Microsoft 合約：SetProcessDefaultCpuSets 傳 CpuSetIds=NULL、count=0 才清除指派；
/// 傳入全部列舉 set 是「指派全部」，不等於清除。
pub fn clear_cpu_sets_by_handle(h: HANDLE) -> Result<(), ProcessError> {
    let result = unsafe { SetProcessDefaultCpuSets(h, None) };
    if result.as_bool() {
        Ok(())
    } else {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
        Err(ProcessError::from_win32(code))
    }
}

pub fn clear_cpu_sets(pid: u32) -> Result<(), ProcessError> {
    let h = unsafe {
        OpenProcess(PROCESS_SET_LIMITED_INFORMATION, false, pid)
            .map_err(|_| ProcessError::from_last_open())?
    };
    let result = clear_cpu_sets_by_handle(h);
    let _ = unsafe { CloseHandle(h) };
    result
}

pub fn get_affinity(h: HANDLE) -> Result<u64, ProcessError> {
    let mut process_mask: usize = 0;
    let mut system_mask: usize = 0;
    unsafe {
        GetProcessAffinityMask(h, &mut process_mask, &mut system_mask)
            .map_err(ProcessError::from_windows)?;
    }
    Ok(process_mask as u64)
}

/// 以 pid 開啟 QUERY_LIMITED handle 回讀硬綁定 mask（revalidation 用，不依賴快取 handle）。
pub fn get_affinity_by_pid(pid: u32) -> Result<u64, ProcessError> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|_| ProcessError::from_last_open())?;
        let result = get_affinity(h);
        let _ = CloseHandle(h);
        result
    }
}

/// 回讀 process-default CPU Sets（GetProcessDefaultCpuSets），轉回 LP indices（已排序去重）。
/// 無指派時 API 成功 → 回傳空 Vec（與「指派空集合」區分：後者不存在）。
/// 接受既有 handle：revalidation 優先用快取 handle（反作弊保護後仍可用），避免重開。
pub fn get_cpu_sets_by_handle(h: HANDLE) -> Result<Vec<u32>, ProcessError> {
    let map = enumerate_cpu_sets().ok_or(ProcessError::OpenFailed(0))?;
    let ids = unsafe { read_cpu_sets_raw(h) }?;
    let mut lps: Vec<u32> = ids
        .iter()
        .filter_map(|&id| map.set_id_to_lp.get(&id).copied())
        .collect();
    lps.sort_unstable();
    lps.dedup();
    Ok(lps)
}

/// 以 pid 開啟 QUERY_LIMITED handle 回讀 CPU Sets（無快取 handle 時的 fallback）。
pub fn get_cpu_sets(pid: u32) -> Result<Vec<u32>, ProcessError> {
    let h = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|_| ProcessError::from_last_open())?
    };
    let result = get_cpu_sets_by_handle(h);
    let _ = unsafe { CloseHandle(h) };
    result
}

/// 第一段 GetProcessDefaultCpuSets probe 結果分類（純函式，可測試）。
/// 成功 = 無指派；ERROR_INSUFFICIENT_BUFFER = 需 buffer 走第二段；其他錯誤 = 讀取失敗。
#[derive(Debug, PartialEq, Eq)]
enum CpuSetsProbe {
    /// 成功 → 無指派，空集合
    Empty,
    /// ERROR_INSUFFICIENT_BUFFER → 需分配 buffer 進行第二段讀取
    NeedBuffer { count: u32 },
    /// 其他錯誤 → 回傳該錯誤（不得誤報為空集合）
    Error(u32),
}

fn classify_cpu_sets_probe(ok: bool, required: u32, last_error: u32) -> CpuSetsProbe {
    if ok {
        CpuSetsProbe::Empty
    } else if last_error == ERROR_INSUFFICIENT_BUFFER.0 {
        CpuSetsProbe::NeedBuffer { count: required }
    } else {
        CpuSetsProbe::Error(last_error)
    }
}

/// 兩段式讀取 process-default CPU Sets：先 probe 長度，再取 ID 清單。
/// 第一段 probe 的 BOOL 與 GetLastError 決定語義（詳見 classify_cpu_sets_probe），
/// 不得把 API 失敗當成「空集合」。
unsafe fn read_cpu_sets_raw(h: HANDLE) -> Result<Vec<u32>, ProcessError> {
    let mut required: u32 = 0;
    let ok = GetProcessDefaultCpuSets(h, None, &mut required).as_bool();
    let last_error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
    match classify_cpu_sets_probe(ok, required, last_error) {
        CpuSetsProbe::Empty => Ok(Vec::new()),
        CpuSetsProbe::Error(code) => Err(ProcessError::from_win32(code)),
        CpuSetsProbe::NeedBuffer { count } => {
            let mut ids = vec![0u32; count as usize];
            let ok = GetProcessDefaultCpuSets(h, Some(&mut ids), &mut required);
            if !ok.as_bool() {
                let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
                return Err(ProcessError::from_win32(code));
            }
            ids.truncate(required as usize);
            Ok(ids)
        }
    }
}

pub fn set_priority(h: HANDLE, p: CpuPriority) -> Result<(), ProcessError> {
    let class = match p {
        CpuPriority::Idle => IDLE_PRIORITY_CLASS,
        CpuPriority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        CpuPriority::Normal => NORMAL_PRIORITY_CLASS,
        CpuPriority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        CpuPriority::High => HIGH_PRIORITY_CLASS,
        // 刻意不提供 REALTIME_PRIORITY_CLASS：會餓死系統層級執行緒（PLAN §7.2）
    };
    unsafe { SetPriorityClass(h, class).map_err(ProcessError::from_windows) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActualPriorityClass {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl ActualPriorityClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::BelowNormal => "BelowNormal",
            Self::Normal => "Normal",
            Self::AboveNormal => "AboveNormal",
            Self::High => "High",
            Self::Realtime => "Realtime",
        }
    }
}

pub fn get_priority(h: HANDLE) -> Result<ActualPriorityClass, ProcessError> {
    let v = unsafe { GetPriorityClass(h) };
    if v == 0 {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
        Err(ProcessError::from_win32(code))
    } else if v == HIGH_PRIORITY_CLASS.0 {
        Ok(ActualPriorityClass::High)
    } else if v == ABOVE_NORMAL_PRIORITY_CLASS.0 {
        Ok(ActualPriorityClass::AboveNormal)
    } else if v == BELOW_NORMAL_PRIORITY_CLASS.0 {
        Ok(ActualPriorityClass::BelowNormal)
    } else if v == IDLE_PRIORITY_CLASS.0 {
        Ok(ActualPriorityClass::Idle)
    } else if v == REALTIME_PRIORITY_CLASS.0 {
        Ok(ActualPriorityClass::Realtime)
    } else if v == NORMAL_PRIORITY_CLASS.0 {
        Ok(ActualPriorityClass::Normal)
    } else {
        // v 是 priority class 值而非 Win32 錯誤碼，不可包成 Win32(v) 誤導診斷
        Err(ProcessError::UnknownPriorityClass(v))
    }
}

/// 路徑正規化：去 `\\?\` 前綴、統一反斜線、小寫（比對用）
pub fn normalize_path(p: &str) -> String {
    let p = p.trim();
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    p.replace('/', "\\").to_lowercase()
}

/// 系統進程黑名單（PLAN §10）：永遠拒絕套用
pub fn is_blacklisted(pid: u32, exe_name: &str, exe_path: Option<&str>) -> bool {
    if pid < 8 {
        return true; // System Idle / System / Registry 等
    }
    const BLACKLIST: &[&str] = &[
        "system",
        "registry",
        "memory compression",
        "secure system",
        "smss.exe",
        "csrss.exe",
        "wininit.exe",
        "winlogon.exe",
        "lsass.exe",
        "services.exe",
        "svchost.exe",
        "fontdrvhost.exe",
        "dwm.exe",
        "explorer.exe",
        "sihost.exe",
        "taskhostw.exe",
        "msmpeng.exe",
        "frameanchor.exe",
    ];
    let name = exe_name.to_lowercase();
    if BLACKLIST.contains(&name.as_str()) {
        return true;
    }
    // System32 下的進程一律拒絕
    if let Some(path) = exe_path {
        static SYSTEM32: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let sys32 = SYSTEM32.get_or_init(|| {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string());
            format!("{}\\system32", root.to_lowercase())
        });
        if normalize_path(path).starts_with(sys32.as_str()) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_extended_prefix() {
        assert_eq!(
            normalize_path(r"\\?\C:\Games\Game.exe"),
            r"c:\games\game.exe"
        );
    }

    #[test]
    fn normalize_case_and_slashes() {
        assert_eq!(normalize_path("C:/GAMES/Game.EXE"), r"c:\games\game.exe");
    }

    #[test]
    fn blacklist_blocks_system_pids() {
        assert!(is_blacklisted(0, "System Idle Process", None));
        assert!(is_blacklisted(4, "System", None));
    }

    #[test]
    fn blacklist_blocks_critical_names() {
        assert!(is_blacklisted(500, "lsass.exe", None));
        assert!(is_blacklisted(600, "SVCHOST.EXE", None));
        assert!(is_blacklisted(700, "Explorer.Exe", None));
    }

    #[test]
    fn blacklist_blocks_system32_path() {
        assert!(is_blacklisted(
            1234,
            "whatever.exe",
            Some(r"C:\Windows\System32\whatever.exe")
        ));
    }

    #[test]
    fn blacklist_allows_game() {
        assert!(!is_blacklisted(
            1234,
            "game.exe",
            Some(r"C:\Games\game.exe")
        ));
    }

    #[test]
    fn set_id_to_lp_map_builds_reverse() {
        // LP 0 → id 100，LP 1 → id 200，LP 2 無對應（parked）
        let lp_to_set_id = vec![Some(100u32), Some(200u32), None];
        let rev = build_set_id_to_lp(&lp_to_set_id);
        assert_eq!(rev.get(&100), Some(&0));
        assert_eq!(rev.get(&200), Some(&1));
        assert!(!rev.contains_key(&999));
    }

    #[test]
    fn set_id_to_lp_map_empty() {
        assert!(build_set_id_to_lp(&[]).is_empty());
        assert!(build_set_id_to_lp(&[None, None]).is_empty());
    }

    #[test]
    fn cpu_sets_probe_success_is_empty() {
        // 成功 = 無指派（required/last_error 此時無意義，分類只看 BOOL）
        assert_eq!(classify_cpu_sets_probe(true, 0, 0), CpuSetsProbe::Empty);
        assert_eq!(classify_cpu_sets_probe(true, 7, 999), CpuSetsProbe::Empty);
    }

    #[test]
    fn cpu_sets_probe_insufficient_buffer_needs_second_call() {
        assert_eq!(
            classify_cpu_sets_probe(false, 3, ERROR_INSUFFICIENT_BUFFER.0),
            CpuSetsProbe::NeedBuffer { count: 3 }
        );
    }

    #[test]
    fn cpu_sets_probe_failure_is_not_empty() {
        // 存取失敗（ACCESS_DENIED=5）時 required 仍是 0，但不得誤報為空集合
        assert_eq!(classify_cpu_sets_probe(false, 0, 5), CpuSetsProbe::Error(5));
    }
}
