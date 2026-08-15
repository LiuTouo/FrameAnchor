//! Browse 視窗列舉（PLAN §7.4）：列出「alt-tab 會看到的」視窗供選擇，含 exe 圖示提取。

use base64::Engine;
use serde::Serialize;
use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, EnumWindows, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, DI_NORMAL, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

use crate::process;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub hwnd: u64,
    pub pid: u32,
    pub title: String,
    pub exe_name: String,
    pub exe_path: Option<String>,
    /// base64 PNG 32x32；取不到為 None，前端用預設圖
    pub icon_png: Option<String>,
    pub already_has_rule: bool,
}

/// 列舉可見視窗。`has_rule`：依 exe 路徑判斷是否已有規則。
pub fn list_windows(current_pid: u32, has_rule: impl Fn(&str) -> bool) -> Vec<WindowInfo> {
    let mut hwnds: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut hwnds as *mut _ as isize));
    }

    let mut result = Vec::new();
    for hwnd in hwnds {
        if let Some(info) = inspect_window(hwnd, current_pid, &has_rule) {
            result.push(info);
        }
    }
    result.sort_by_key(|a| a.title.to_lowercase());
    result
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let vec = &mut *(lparam.0 as *mut Vec<HWND>);
    vec.push(hwnd);
    BOOL(1)
}

/// 單一視窗的過濾與資訊收集；不符合回傳 None
fn inspect_window(
    hwnd: HWND,
    current_pid: u32,
    has_rule: &impl Fn(&str) -> bool,
) -> Option<WindowInfo> {
    unsafe {
        // 1) 可見
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        // 2) 有標題
        let title_len = GetWindowTextLengthW(hwnd);
        if title_len <= 0 {
            return None;
        }
        let mut title_buf = vec![0u16; (title_len + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut title_buf);
        if copied <= 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&title_buf[..copied as usize]);

        // 3) 排除 cloaked（隱藏的 UWP 背景視窗）
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return None;
        }

        // 4) 排除 tool window
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            return None;
        }

        // 5) PID → exe
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == current_pid {
            return None; // 排除自己
        }

        let exe_path = process::process_path(pid);
        let exe_name = exe_path
            .as_deref()
            .and_then(|p| p.rsplit('\\').next())
            .unwrap_or("unknown")
            .to_string();

        let already_has_rule = exe_path.as_deref().map(has_rule).unwrap_or(false);
        let icon_png = exe_path.as_deref().and_then(icon_png_base64);

        Some(WindowInfo {
            hwnd: hwnd.0 as u64,
            pid,
            title,
            exe_name,
            exe_path,
            icon_png,
            already_has_rule,
        })
    }
}

/// exe → HICON → 32x32 PNG base64
fn icon_png_base64(exe_path: &str) -> Option<String> {
    unsafe {
        let path_wide: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut shfi = SHFILEINFOW::default();
        let res = SHGetFileInfoW(
            PCWSTR(path_wide.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut shfi),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON,
        );
        if res == 0 || shfi.hIcon.is_invalid() {
            return None;
        }
        let png = hicon_to_png_base64(shfi.hIcon);
        let _ = DestroyIcon(shfi.hIcon);
        png
    }
}

/// HICON 畫進 32x32 DIB section → BGRA 轉 RGBA → PNG → base64
fn hicon_to_png_base64(hicon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<String> {
    const W: i32 = 32;
    const H: i32 = 32;
    unsafe {
        let hdc_screen = GetDC(None);
        let hdc = CreateCompatibleDC(Some(hdc_screen));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: W,
                biHeight: -H, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let hbmp = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let old = SelectObject(hdc, HGDIOBJ(hbmp.0));

        // 先清成透明，再畫圖示
        if !bits.is_null() {
            std::ptr::write_bytes(bits as *mut u8, 0, (W * H * 4) as usize);
        }
        let _ = DrawIconEx(hdc, 0, 0, hicon, W, H, 0, None, DI_NORMAL);

        let px = std::slice::from_raw_parts(bits as *const u8, (W * H * 4) as usize);
        let mut rgba = vec![0u8; (W * H * 4) as usize];
        for i in 0..(W * H) as usize {
            rgba[i * 4] = px[i * 4 + 2]; // B→R
            rgba[i * 4 + 1] = px[i * 4 + 1];
            rgba[i * 4 + 2] = px[i * 4]; // R→B
            rgba[i * 4 + 3] = px[i * 4 + 3];
        }

        SelectObject(hdc, old);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        let _ = DeleteDC(hdc);
        ReleaseDC(None, hdc_screen);

        let img = image::RgbaImage::from_raw(W as u32, H as u32, rgba)?;
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
        Some(base64::engine::general_purpose::STANDARD.encode(buf.into_inner()))
    }
}
