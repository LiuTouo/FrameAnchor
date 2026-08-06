//! I/O 優先級與記憶體優先級（PLAN §7.3，進階功能）。
//! I/O：ntdll NtSetInformationProcess(ProcessIoPriority=33)，值 0–3（Critical=4 系統保留）。
//! 記憶體：SetProcessInformation(ProcessMemoryPriority)，值 1–5（最高只能 Normal）。

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    GetProcessInformation, SetProcessInformation, MEMORY_PRIORITY, MEMORY_PRIORITY_INFORMATION,
};

use crate::error::PriorityError;
use crate::model::{IoPriority, MemPriority};

type NTSTATUS = i32;
const PROCESS_INFORMATION_CLASS_IO_PRIORITY: i32 = 33; // ProcessIoPriority

// windows-sys 的 Wdk 模組路徑隨版本變動，直接手動宣告 ntdll 進入點最穩（PLAN §16 附錄 C）
#[link(name = "ntdll")]
extern "system" {
    fn NtSetInformationProcess(process: isize, class: i32, info: *const u32, len: u32) -> NTSTATUS;
    #[allow(dead_code)] // get_io_priority 使用，保留給回讀驗證
    fn NtQueryInformationProcess(
        process: isize,
        class: i32,
        info: *mut u32,
        len: u32,
        ret_len: *mut u32,
    ) -> NTSTATUS;
}

pub fn set_io_priority(h: HANDLE, p: IoPriority) -> Result<(), PriorityError> {
    let value = p.to_raw();
    let status = unsafe {
        NtSetInformationProcess(
            h.0 as isize,
            PROCESS_INFORMATION_CLASS_IO_PRIORITY,
            &value,
            std::mem::size_of::<u32>() as u32,
        )
    };
    if status >= 0 {
        Ok(())
    } else {
        Err(PriorityError::NtStatus(status))
    }
}

/// 回讀 I/O 優先級（手動驗證與未來面板顯示用）
#[allow(dead_code)]
pub fn get_io_priority(h: HANDLE) -> Result<IoPriority, PriorityError> {
    let mut value: u32 = 0;
    let mut ret_len: u32 = 0;
    let status = unsafe {
        NtQueryInformationProcess(
            h.0 as isize,
            PROCESS_INFORMATION_CLASS_IO_PRIORITY,
            &mut value,
            std::mem::size_of::<u32>() as u32,
            &mut ret_len,
        )
    };
    if status >= 0 {
        Ok(IoPriority::from_raw(value))
    } else {
        Err(PriorityError::NtStatus(status))
    }
}

// ProcessMemoryPriority 資訊類別值（部分 windows crate 版本未匯出此常數，直接用數值 5）
const PROCESS_MEMORY_PRIORITY_CLASS: windows::Win32::System::Threading::PROCESS_INFORMATION_CLASS =
    windows::Win32::System::Threading::PROCESS_INFORMATION_CLASS(5);

pub fn set_memory_priority(h: HANDLE, p: MemPriority) -> Result<(), PriorityError> {
    let info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: MEMORY_PRIORITY(p.to_raw()),
    };
    unsafe {
        SetProcessInformation(
            h,
            PROCESS_MEMORY_PRIORITY_CLASS,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
        .map_err(|e| PriorityError::Win32(e.code().0 as u32))
    }
}

/// 回讀記憶體優先級（手動驗證與未來面板顯示用）
#[allow(dead_code)]
pub fn get_memory_priority(h: HANDLE) -> Result<MemPriority, PriorityError> {
    let mut info = MEMORY_PRIORITY_INFORMATION::default();
    unsafe {
        GetProcessInformation(
            h,
            PROCESS_MEMORY_PRIORITY_CLASS,
            &mut info as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
        .map_err(|e| PriorityError::Win32(e.code().0 as u32))?;
    }
    Ok(MemPriority::from_raw(info.MemoryPriority.0))
}
