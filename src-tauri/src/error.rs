//! 錯誤型別。對前端的錯誤一律轉成穩定代碼字串（前端查 i18n 顯示，PLAN §8）。

use thiserror::Error;

/// 前端 i18n 用的穩定錯誤代碼（errors.<code>）。
/// 部分代碼目前由前端字典保留（對應 errors.* 翻譯），後端尚未全數使用。
#[allow(dead_code)]
pub mod codes {
    pub const ACCESS_DENIED: &str = "ACCESS_DENIED";
    pub const OPEN_FAILED: &str = "OPEN_FAILED";
    pub const SET_AFFINITY_FAILED: &str = "SET_AFFINITY_FAILED";
    pub const SET_PRIORITY_FAILED: &str = "SET_PRIORITY_FAILED";
    pub const IO_FAILED: &str = "IO_FAILED";
    pub const MEM_FAILED: &str = "MEM_FAILED";
    pub const TOPOLOGY_FAILED: &str = "TOPOLOGY_FAILED";
    pub const CONFIG_FAILED: &str = "CONFIG_FAILED";
    pub const AUTOSTART_FAILED: &str = "AUTOSTART_FAILED";
}

#[derive(Error, Debug)]
pub enum ProcessError {
    #[error("access denied")]
    AccessDenied,
    #[error("open process failed: {0}")]
    OpenFailed(u32),
    #[error("win32 error: {0}")]
    Win32(u32),
}

impl ProcessError {
    pub fn code(&self) -> &'static str {
        match self {
            ProcessError::AccessDenied => codes::ACCESS_DENIED,
            ProcessError::OpenFailed(_) => codes::OPEN_FAILED,
            ProcessError::Win32(_) => codes::OPEN_FAILED,
        }
    }

    pub fn from_last_open() -> Self {
        let err = std::io::Error::last_os_error();
        match err.raw_os_error() {
            Some(5) => ProcessError::AccessDenied, // ERROR_ACCESS_DENIED
            Some(code) => ProcessError::OpenFailed(code as u32),
            None => ProcessError::OpenFailed(0),
        }
    }
}

#[derive(Error, Debug)]
pub enum PriorityError {
    #[error("ntstatus: {0:#x}")]
    NtStatus(i32),
    #[error("win32 error: {0}")]
    Win32(u32),
}

#[derive(Error, Debug)]
pub enum TopologyError {
    #[error("topology query failed")]
    QueryFailed,
}
