//! 依 PID 找 workload 的 top-level visible window，做兩件事：
//! 1. 停用關閉能力（`guard_close`）：僅內建 Vulkan workload（windowed 與
//!    fullscreen 都做）。用 `GetSystemMenu` + `EnableMenuItem(SC_CLOSE,
//!    MF_GRAYED)` 停用關閉鈕，保留 minimize/maximize，再 `DrawMenuBar` 重繪。
//! 2. 把 client area 調整成使用者設定的 width×height（`find_and_resize`，
//!    僅 Vulkan windowed）。fullscreen 或 D3D9 不調整尺寸。
//!
//! 尺寸調整作法：用 `GetWindowRect` / `GetClientRect` 量出 non-client frame 差異，
//! 再以 `SetWindowPos` 把 outer window 設成 `(width + dx, height + dy)`。
//! 此 delta 法與 DPI 無關，PerMonitorV2 或跨 monitor 都正確；不依賴
//! `AdjustWindowRect*ForDpi`（需另猜 DPI）。
//!
//! `SetWindowPos` 成功不代表 client 真的達標（frame 可能在調整後改變），
//! 因此調整後一律 remeasure client；未達標做 bounded 補正，最終仍不符回 Err
//! 由 caller（runner）log warn。

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawMenuBar, EnableMenuItem, EnumWindows, GetClientRect, GetSystemMenu, GetWindowRect,
    GetWindowThreadProcessId, IsWindowVisible, SetWindowPos, HWND_TOP, MF_BYCOMMAND, MF_GRAYED,
    SC_CLOSE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
};

/// workload 視窗操作的注入介面。runner 在內建 Vulkan 時呼叫。
pub trait WorkloadWindow: Send + Sync {
    /// 單次嘗試：依 pid 找 visible top-level window，把 client area 調成
    /// `width`×`height`。
    /// - `Ok(true)`：找到並調整成功。
    /// - `Ok(false)`：尚未找到（window 可能還在建立，可稍後重試）。
    /// - `Err(e)`：找到但調整失敗（重試無益）。
    fn find_and_resize(&self, pid: u32, width: u32, height: u32) -> Result<bool, String>;

    /// 單次嘗試：依 pid 找 visible top-level window，停用其關閉能力
    /// （SC_CLOSE），防使用者誤關 workload。
    /// - `Ok(true)`：找到並已停用關閉。
    /// - `Ok(false)`：尚未找到（window 可能還在建立，可稍後重試）。
    /// - `Err(e)`：找到但停用失敗（重試無益）。
    fn guard_close(&self, pid: u32) -> Result<bool, String>;
}

/// 由目前 outer/client 尺寸算出「使 client area 變成 (width,height)」所需
/// 的 outer window 尺寸。純函式，獨立測試。
pub fn outer_size_for_client(
    window_w: i32,
    window_h: i32,
    client_w: i32,
    client_h: i32,
    width: u32,
    height: u32,
) -> (i32, i32) {
    let frame_w = window_w - client_w;
    let frame_h = window_h - client_h;
    (width as i32 + frame_w, height as i32 + frame_h)
}

/// 生產實作：EnumWindows + GetClientRect/SetWindowPos。
pub struct RealWorkloadWindow;

impl RealWorkloadWindow {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RealWorkloadWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkloadWindow for RealWorkloadWindow {
    fn find_and_resize(&self, pid: u32, width: u32, height: u32) -> Result<bool, String> {
        let Some(hwnd) = find_top_level_window(pid) else {
            return Ok(false);
        };
        resize_to_client(hwnd, width, height)?;
        Ok(true)
    }

    fn guard_close(&self, pid: u32) -> Result<bool, String> {
        let Some(hwnd) = find_top_level_window(pid) else {
            return Ok(false);
        };
        disable_close(hwnd)?;
        Ok(true)
    }
}

struct EnumCtx {
    pid: u32,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut EnumCtx);
    if ctx.found.is_some() {
        return BOOL(0);
    }
    if !IsWindowVisible(hwnd).as_bool() {
        return BOOL(1);
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == ctx.pid {
        ctx.found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}

/// 找 pid 的「第一個」visible top-level window。
fn find_top_level_window(pid: u32) -> Option<HWND> {
    let mut ctx = EnumCtx { pid, found: None };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found
}

/// 停用 `SC_CLOSE`：`GetSystemMenu` + `EnableMenuItem(SC_CLOSE, MF_GRAYED)` 停用
/// 關閉鈕（保留 minimize/maximize），再 `DrawMenuBar` 重繪標題列。idempotent：
/// 重複停用無害。不可移除整個 `WS_SYSMENU`。
fn disable_close(hwnd: HWND) -> Result<(), String> {
    unsafe {
        let menu = GetSystemMenu(hwnd, false);
        if menu.0.is_null() {
            return Err("GetSystemMenu 傳回 null（視窗無系統選單）".to_string());
        }
        let prev = EnableMenuItem(menu, SC_CLOSE, MF_BYCOMMAND | MF_GRAYED);
        if prev.0 == -1 {
            return Err("EnableMenuItem(SC_CLOSE) 失敗".to_string());
        }
        // 重繪讓停用狀態反映到標題列 X；失敗不影響已完成的停用（cosmetic）
        let _ = DrawMenuBar(hwnd);
        Ok(())
    }
}

/// 初次調整後 client area 仍未達指定值時，最多再補正的次數（bounded correction）。
const RESIZE_MAX_CORRECTIONS: u32 = 3;

/// 驅動「量測 → 調整 → 驗證」的收斂迴圈（純邏輯，不含 Win32）。`measure` 回傳
/// `(outer_w, outer_h, client_w, client_h)`；`set` 設定 outer 尺寸。初次量測若
/// 已達標即成功；否則調整後 remeasure，未達標依最新 frame delta 補正，最多
/// `max_corrections` 次；最終仍不符回 Err。
fn resize_verify_loop(
    measure: &dyn Fn() -> Result<(i32, i32, i32, i32), String>,
    set: &dyn Fn(i32, i32) -> Result<(), String>,
    width: u32,
    height: u32,
    max_corrections: u32,
) -> Result<(), String> {
    let target_w = width as i32;
    let target_h = height as i32;
    // 初次調整 + max_corrections 次補正 = 最多 max_corrections+1 次 set
    let mut sets_left = max_corrections.saturating_add(1);
    loop {
        let (ow, oh, cw, ch) = measure()?;
        if cw == target_w && ch == target_h {
            return Ok(());
        }
        if sets_left == 0 {
            return Err(format!(
                "client area 未達 {width}×{height}（實際 {cw}×{ch}）"
            ));
        }
        let (new_ow, new_oh) = outer_size_for_client(ow, oh, cw, ch, width, height);
        set(new_ow, new_oh)?;
        sets_left -= 1;
    }
}

fn resize_to_client(hwnd: HWND, width: u32, height: u32) -> Result<(), String> {
    let measure = || -> Result<(i32, i32, i32, i32), String> {
        let mut wr = RECT::default();
        let mut cr = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut wr).map_err(|e| format!("GetWindowRect: {e}"))?;
            GetClientRect(hwnd, &mut cr).map_err(|e| format!("GetClientRect: {e}"))?;
        }
        Ok((
            wr.right - wr.left,
            wr.bottom - wr.top,
            cr.right - cr.left,
            cr.bottom - cr.top,
        ))
    };
    let set = |w: i32, h: i32| -> Result<(), String> {
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOP),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .map_err(|e| format!("SetWindowPos: {e}"))
        }
    };
    resize_verify_loop(&measure, &set, width, height, RESIZE_MAX_CORRECTIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_size_preserves_frame_delta() {
        // 假設 outer 800×600、client 784×561（frame 寬 16、高 39）
        let (w, h) = outer_size_for_client(800, 600, 784, 561, 640, 480);
        assert_eq!(w, 640 + 16);
        assert_eq!(h, 480 + 39);
    }

    #[test]
    fn outer_size_no_frame_returns_exact() {
        // borderless：client == outer
        assert_eq!(outer_size_for_client(640, 480, 640, 480, 640, 480), (640, 480));
    }

    /// 依序餵給 `resize_verify_loop` 的 measure 值，回傳 (result, set 呼叫紀錄)。
    fn run_resize(
        measures: Vec<(i32, i32, i32, i32)>,
        max_corrections: u32,
    ) -> (Result<(), String>, Vec<(i32, i32)>) {
        let idx = std::cell::Cell::new(0usize);
        let sets = std::cell::RefCell::new(Vec::new());
        let measure = || -> Result<(i32, i32, i32, i32), String> {
            let i = idx.get();
            idx.set(i + 1);
            measures
                .get(i)
                .copied()
                .ok_or_else(|| "measure exhausted".to_string())
        };
        let set = |w: i32, h: i32| -> Result<(), String> {
            sets.borrow_mut().push((w, h));
            Ok(())
        };
        let r = resize_verify_loop(&measure, &set, 640, 480, max_corrections);
        (r, sets.into_inner())
    }

    #[test]
    fn resize_verify_exact_success_no_set() {
        // 初次量測 client 即達標：不呼叫 set
        let (r, sets) = run_resize(vec![(800, 600, 640, 480)], 3);
        assert_eq!(r, Ok(()));
        assert!(sets.is_empty());
    }

    #[test]
    fn resize_verify_first_set_achieves_target() {
        // 初次 mismatch → set 一次 → remeasure 達標
        let (r, sets) = run_resize(vec![(800, 600, 784, 561), (656, 519, 640, 480)], 3);
        assert_eq!(r, Ok(()));
        assert_eq!(sets, vec![(656, 519)]);
    }

    #[test]
    fn resize_verify_corrects_after_first_mismatch() {
        // 第一次 set 後 client 仍 mismatch（frame delta 改變）→ 依最新 delta 補正成功
        let measures = vec![
            (800, 600, 784, 561), // frame 16×39 → set(656, 519)
            (656, 519, 624, 441), // frame 變 32×78 → set(672, 558)
            (672, 558, 640, 480), // 達標
        ];
        let (r, sets) = run_resize(measures, 3);
        assert_eq!(r, Ok(()));
        assert_eq!(sets, vec![(656, 519), (672, 558)]);
    }

    #[test]
    fn resize_verify_bounded_correction_failure() {
        // 無補正（max_corrections=0）：初次 set 後仍 mismatch → Err
        let (r, sets) = run_resize(vec![(800, 600, 784, 561), (656, 519, 624, 441)], 0);
        assert!(r.is_err(), "補正用盡仍不符必須回 Err");
        assert_eq!(sets, vec![(656, 519)]);
    }

    #[test]
    fn resize_verify_exhausts_corrections_then_errs() {
        // max_corrections=2：初次 + 2 補正 = 3 次 set，之後仍 mismatch → Err
        let measures = vec![
            (800, 600, 784, 561),
            (656, 519, 624, 441),
            (672, 558, 624, 441),
            (672, 558, 624, 441),
        ];
        let (r, sets) = run_resize(measures, 2);
        assert!(r.is_err());
        assert_eq!(sets.len(), 3, "初次 + 2 次補正共 3 次 set");
    }
}
