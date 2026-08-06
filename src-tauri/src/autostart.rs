//! 開機啟動（PLAN §7.7）：schtasks CLI 建立 ONLOGON + HIGHEST 工作，登入不跳 UAC。
//! 不用 Registry Run key（無法帶最高權限）。

use std::os::windows::process::CommandExt;
use std::process::Command;

use crate::error::codes;

const CREATE_NO_WINDOW: u32 = 0x08000000;
const TASK_NAME: &str = "FrameAnchor";

pub fn set_autostart(enable: bool) -> Result<(), String> {
    if enable {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        let tr = format!("\"{}\" --minimized", exe.display());
        let out = Command::new("schtasks")
            .args([
                "/Create", "/TN", TASK_NAME, "/SC", "ONLOGON", "/RL", "HIGHEST", "/TR", &tr, "/F",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("schtasks: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            log::error!(
                "schtasks /Create 失敗: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            Err(codes::AUTOSTART_FAILED.to_string())
        }
    } else {
        // 工作不存在時 /Delete 會失敗，視為成功（目標狀態已達成）
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        Ok(())
    }
}

pub fn is_enabled() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
