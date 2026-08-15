//! 進程列舉與操作（PLAN §7.2）：Toolhelp snapshot、affinity、priority。

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::Kernel::PROCESSOR_NUMBER;
use windows::Win32::System::SystemInformation::{
    CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
};
use windows::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessTimes, OpenProcess, OpenThread,
    QueryFullProcessImageNameW, SetPriorityClass, SetProcessAffinityMask, SetProcessDefaultCpuSets,
    SetThreadIdealProcessorEx, TerminateProcess, ABOVE_NORMAL_PRIORITY_CLASS,
    BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS, IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_INFORMATION, PROCESS_SET_LIMITED_INFORMATION, PROCESS_TERMINATE, PROCESS_VM_READ,
    THREAD_SET_INFORMATION,
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

/// 走訪 Toolhelp snapshot 內所有進程（pid, exe 名）
fn for_each_process(mut f: impl FnMut(u32, String)) {
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return,
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                f(entry.th32ProcessID, utf16_slice_to_string(&entry.szExeFile));
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
}

/// Toolhelp snapshot 列舉全部進程（逐進程解析路徑，較重；tick 用）
pub fn enumerate_processes() -> Vec<ProcessInfo> {
    let mut result = Vec::new();
    for_each_process(|pid, exe_name| {
        let exe_path = process_path(pid);
        result.push(ProcessInfo {
            pid,
            exe_name,
            exe_path,
        });
    });
    result
}

/// 輕量列舉：只有 pid + exe 名，不解析路徑。discovery 高頻掃描用。
pub fn enumerate_process_names() -> Vec<(u32, String)> {
    let mut result = Vec::new();
    for_each_process(|pid, name| result.push((pid, name)));
    result
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
            PROCESS_SET_INFORMATION | PROCESS_QUERY_INFORMATION,
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
    unsafe {
        SetProcessAffinityMask(h, mask as usize).map_err(|e| ProcessError::Win32(e.code().0 as u32))
    }
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
}

/// 軟綁定：進程所有執行緒設 ideal processor（偏好核心，round-robin）。
/// 單一執行緒失敗跳過（best-effort），回傳 attempted/succeeded 供呼叫端分類。
pub fn set_threads_ideal(pid: u32, cores: &[u32]) -> ThreadIdealOutcome {
    let zero = ThreadIdealOutcome {
        attempted: 0,
        succeeded: 0,
    };
    if cores.is_empty() {
        return zero;
    }
    let mut attempted = 0usize;
    let mut succeeded = 0usize;
    let mut idx = 0usize;
    unsafe {
        let snap = match CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) {
            Ok(h) => h,
            Err(_) => return zero,
        };
        let mut entry = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        if Thread32First(snap, &mut entry).is_ok() {
            loop {
                if entry.th32OwnerProcessID == pid {
                    attempted += 1;
                    let preferred = PROCESSOR_NUMBER {
                        Group: 0, // v1 只支援 group 0（前 64 LP）
                        Number: cores[idx % cores.len()] as u8,
                        Reserved: 0,
                    };
                    if let Ok(t) = OpenThread(THREAD_SET_INFORMATION, false, entry.th32ThreadID) {
                        if SetThreadIdealProcessorEx(t, &preferred, None).is_ok() {
                            succeeded += 1;
                        }
                        let _ = CloseHandle(t);
                    }
                    idx += 1;
                }
                if Thread32Next(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    ThreadIdealOutcome {
        attempted,
        succeeded,
    }
}

// ── CPU Sets API（Windows 10 1703+）──────────────────────────────────────

/// LP index → CPU set ID 映射快取（啟動時列舉一次）
struct CpuSetMap {
    lp_to_set_id: Vec<Option<u32>>,
}

/// 列舉系統 CPU sets，建立 LP index → set ID 映射
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
        Some(CpuSetMap { lp_to_set_id })
    }
}

/// 用 CPU Sets API 設定軟性 affinity（PLAN §15 v2 功能）。
/// 需要 `PROCESS_SET_LIMITED_INFORMATION`，與 `PROCESS_SET_INFORMATION` 不同。
/// 反作弊 kernel driver 較少攔截此權限，成功率比硬綁定高。
pub fn set_cpu_sets(pid: u32, cores: &[u32]) -> Result<(), ProcessError> {
    let map = enumerate_cpu_sets().ok_or(ProcessError::OpenFailed(0))?;
    let set_ids: Vec<u32> = cores
        .iter()
        .filter_map(|&lp| map.lp_to_set_id.get(lp as usize).copied().flatten())
        .collect();
    if set_ids.is_empty() {
        return Err(ProcessError::OpenFailed(0));
    }
    // 只要求 PROCESS_SET_LIMITED_INFORMATION（不要求 PROCESS_SET_INFORMATION）
    let h = unsafe {
        OpenProcess(PROCESS_SET_LIMITED_INFORMATION, false, pid)
            .map_err(|_| ProcessError::from_last_open())?
    };
    let result = unsafe { SetProcessDefaultCpuSets(h, Some(&set_ids)) };
    let _ = unsafe { CloseHandle(h) };
    if result.as_bool() {
        Ok(())
    } else {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
        Err(ProcessError::Win32(code))
    }
}

/// 清除 process-default CPU Sets 指派（All 模式還原用）。
/// Microsoft 合約：SetProcessDefaultCpuSets 傳 CpuSetIds=NULL、count=0 才清除指派；
/// 傳入全部列舉 set 是「指派全部」，不等於清除。
pub fn clear_cpu_sets(pid: u32) -> Result<(), ProcessError> {
    // 只要求 PROCESS_SET_LIMITED_INFORMATION（與 set_cpu_sets 相同）
    let h = unsafe {
        OpenProcess(PROCESS_SET_LIMITED_INFORMATION, false, pid)
            .map_err(|_| ProcessError::from_last_open())?
    };
    let result = unsafe { SetProcessDefaultCpuSets(h, None) };
    let _ = unsafe { CloseHandle(h) };
    if result.as_bool() {
        Ok(())
    } else {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
        Err(ProcessError::Win32(code))
    }
}

pub fn get_affinity(h: HANDLE) -> Result<u64, ProcessError> {
    let mut process_mask: usize = 0;
    let mut system_mask: usize = 0;
    unsafe {
        GetProcessAffinityMask(h, &mut process_mask, &mut system_mask)
            .map_err(|e| ProcessError::Win32(e.code().0 as u32))?;
    }
    Ok(process_mask as u64)
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
    unsafe { SetPriorityClass(h, class).map_err(|e| ProcessError::Win32(e.code().0 as u32)) }
}

pub fn get_priority(h: HANDLE) -> CpuPriority {
    let v = unsafe { GetPriorityClass(h) };
    if v == HIGH_PRIORITY_CLASS.0 {
        CpuPriority::High
    } else if v == ABOVE_NORMAL_PRIORITY_CLASS.0 {
        CpuPriority::AboveNormal
    } else if v == BELOW_NORMAL_PRIORITY_CLASS.0 {
        CpuPriority::BelowNormal
    } else if v == IDLE_PRIORITY_CLASS.0 {
        CpuPriority::Idle
    } else {
        CpuPriority::Normal
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
}
