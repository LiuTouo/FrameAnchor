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
    pub const UPDATE_CHECK_FAILED: &str = "UPDATE_CHECK_FAILED";
    pub const UPDATE_DOWNLOAD_FAILED: &str = "UPDATE_DOWNLOAD_FAILED";
    pub const UPDATE_INSTALL_FAILED: &str = "UPDATE_INSTALL_FAILED";
    // ── GPU 顯示裝置控制 ──
    pub const GPU_ENUM_FAILED: &str = "GPU_ENUM_FAILED";
    pub const GPU_NOT_FOUND: &str = "GPU_NOT_FOUND";
    pub const GPU_REGISTRY_FAILED: &str = "GPU_REGISTRY_FAILED";
    pub const GPU_RESTART_FAILED: &str = "GPU_RESTART_FAILED";
    pub const GPU_BASIC_DISPLAY_DISABLED: &str = "GPU_BASIC_DISPLAY_DISABLED";
    pub const GPU_RESTORE_FAILED: &str = "GPU_RESTORE_FAILED";
    pub const GPU_APPLY_FAILED: &str = "GPU_APPLY_FAILED";
    // ── 基準測試 ──
    pub const BENCHMARK_SESSION_NOT_FOUND: &str = "BENCHMARK_SESSION_NOT_FOUND";
    pub const BENCHMARK_SESSION_NOT_COMPLETED: &str = "BENCHMARK_SESSION_NOT_COMPLETED";
    pub const BENCHMARK_SESSION_INCOMPATIBLE: &str = "BENCHMARK_SESSION_INCOMPATIBLE";
    pub const BENCHMARK_RECOVERY_REQUIRED: &str = "BENCHMARK_RECOVERY_REQUIRED";
    pub const BENCHMARK_NOT_IMPLEMENTED: &str = "BENCHMARK_NOT_IMPLEMENTED";
    pub const BENCHMARK_ALREADY_RUNNING: &str = "BENCHMARK_ALREADY_RUNNING";
    pub const BENCHMARK_NOT_ACTIVE: &str = "BENCHMARK_NOT_ACTIVE";
    pub const BENCHMARK_STORAGE_FAILED: &str = "BENCHMARK_STORAGE_FAILED";
    pub const BENCHMARK_INVALID_SESSION_ID: &str = "BENCHMARK_INVALID_SESSION_ID";
    // ── 基準測試 runner（Task 2）──
    pub const BENCHMARK_ASSETS_MISSING: &str = "BENCHMARK_ASSETS_MISSING";
    pub const BENCHMARK_ASSETS_HASH_MISMATCH: &str = "BENCHMARK_ASSETS_HASH_MISMATCH";
    pub const BENCHMARK_INVALID_CONFIG: &str = "BENCHMARK_INVALID_CONFIG";
    pub const BENCHMARK_WORKLOAD_FAILED: &str = "BENCHMARK_WORKLOAD_FAILED";
    pub const BENCHMARK_PRESENTMON_FAILED: &str = "BENCHMARK_PRESENTMON_FAILED";
    pub const BENCHMARK_CSV_INVALID: &str = "BENCHMARK_CSV_INVALID";
    /// PresentMon 在 sample+margin 內未退出（卡住 / 停不下來）
    pub const BENCHMARK_PRESENTMON_TIMEOUT: &str = "BENCHMARK_PRESENTMON_TIMEOUT";
    /// PresentMon 正常退出但沒有產出輸出檔案（裝置/swapchain 暫態或 workload 未產生可擷取畫面）
    pub const BENCHMARK_CAPTURE_MISSING: &str = "BENCHMARK_CAPTURE_MISSING";
    /// 輸出檔案存在但沒有有效 frametime 資料（空 / 只剩 header）
    pub const BENCHMARK_CAPTURE_EMPTY: &str = "BENCHMARK_CAPTURE_EMPTY";
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

#[cfg(test)]
mod tests {
    use super::codes;

    /// 回歸測試：error.rs 內每個穩定錯誤代碼都必須存在於 en.json 與 zh-TW.json 的
    /// errors.*（前端查 i18n 顯示）。防止新增代碼漏加任一 locale。
    #[test]
    fn every_error_code_is_present_in_both_locales() {
        let src_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/error.rs");
        let src = std::fs::read_to_string(src_path).expect("讀取 error.rs");
        let codes: Vec<String> = src
            .lines()
            .filter_map(|l| {
                l.trim()
                    .strip_prefix("pub const ")
                    .and_then(|r| r.split_once(':'))
                    .map(|(name, _)| name.trim().to_string())
            })
            .collect();
        assert!(!codes.is_empty(), "應能從 error.rs 抽取錯誤代碼");

        for locale in ["en", "zh-TW"] {
            let path = format!("{}/../src/i18n/{locale}.json", env!("CARGO_MANIFEST_DIR"));
            let text = std::fs::read_to_string(&path).expect("讀取 i18n json");
            let json: serde_json::Value = serde_json::from_str(&text).expect("i18n json 解析");
            let errors = json["errors"]
                .as_object()
                .unwrap_or_else(|| panic!("{locale}.json 缺 errors 區塊"));
            for code in &codes {
                assert!(
                    errors.contains_key(code),
                    "錯誤代碼 {code} 缺於 {locale}.json 的 errors.*"
                );
            }
        }
        // 確保 codes 模組本身有內容（引用避免 unused）
        let _ = codes::TOPOLOGY_FAILED;
    }
}
