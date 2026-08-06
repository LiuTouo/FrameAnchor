//! 系統匣（PLAN §7.9）：右鍵選單、左鍵開面板、雙語選單重建。

use std::sync::{Arc, Mutex};

use tauri::menu::{CheckMenuItem, CheckMenuItemBuilder, Menu, MenuItem, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

use crate::{autostart, config, AppState};

const ID_SHOW: &str = "fa_show";
const ID_APPLIED: &str = "fa_applied";
const ID_AUTOSTART: &str = "fa_autostart";
const ID_QUIT: &str = "fa_quit";
const TRAY_ID: &str = "main";

// 選單項目保留參照，供文字/check 狀態即時更新
static APPLIED_ITEM: Mutex<Option<MenuItem<Wry>>> = Mutex::new(None);
static AUTOSTART_ITEM: Mutex<Option<CheckMenuItem<Wry>>> = Mutex::new(None);

struct TrayStrings {
    show: &'static str,
    applied: &'static str, // 含 {n} 佔位
    autostart: &'static str,
    quit: &'static str,
}

fn strings(lang: &str) -> TrayStrings {
    if lang.starts_with("en") {
        TrayStrings {
            show: "Show Dashboard",
            applied: "{n} rules applied",
            autostart: "Launch at startup",
            quit: "Quit FrameAnchor",
        }
    } else {
        TrayStrings {
            show: "顯示面板",
            applied: "已套用 {n} 個規則",
            autostart: "開機自動啟動",
            quit: "結束 FrameAnchor",
        }
    }
}

pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let lang = app
        .state::<Arc<AppState>>()
        .config
        .read()
        .map(|c| c.settings.language.clone())
        .unwrap_or_else(|_| "zh-TW".to_string());
    let menu = build_menu(app, &lang, 0)?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .menu(&menu)
        .tooltip("FrameAnchor")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu_event(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn build_menu(app: &AppHandle, lang: &str, applied_count: usize) -> tauri::Result<Menu<Wry>> {
    let s = strings(lang);
    let show = MenuItemBuilder::with_id(ID_SHOW, s.show).build(app)?;
    let applied = MenuItemBuilder::with_id(ID_APPLIED, s.applied.replace("{n}", &applied_count.to_string()))
        .enabled(false)
        .build(app)?;
    let autostart = CheckMenuItemBuilder::with_id(ID_AUTOSTART, s.autostart)
        .checked(autostart::is_enabled())
        .build(app)?;
    let quit = MenuItemBuilder::with_id(ID_QUIT, s.quit).build(app)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    *APPLIED_ITEM.lock().unwrap() = Some(applied.clone());
    *AUTOSTART_ITEM.lock().unwrap() = Some(autostart.clone());

    Menu::with_items(app, &[&show, &applied, &sep1, &autostart, &sep2, &quit])
}

/// 語言切換時重建整個選單
pub fn rebuild_menu(app: &AppHandle) {
    let state = app.state::<Arc<AppState>>();
    let (lang, count) = {
        let lang = state
            .config
            .read()
            .map(|c| c.settings.language.clone())
            .unwrap_or_else(|_| "zh-TW".to_string());
        let count = state.applied.read().map(|a| a.len()).unwrap_or(0);
        (lang, count)
    };
    if let Ok(menu) = build_menu(app, &lang, count) {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

/// watcher 更新已套用數量
pub fn update_applied_count(app: &AppHandle, count: usize) {
    let state = app.state::<Arc<AppState>>();
    let lang = state
        .config
        .read()
        .map(|c| c.settings.language.clone())
        .unwrap_or_else(|_| "zh-TW".to_string());
    let text = strings(&lang).applied.replace("{n}", &count.to_string());
    if let Some(item) = APPLIED_ITEM.lock().unwrap().as_ref() {
        let _ = item.set_text(text);
    }
}

pub fn set_autostart_checked(checked: bool) {
    if let Some(item) = AUTOSTART_ITEM.lock().unwrap().as_ref() {
        let _ = item.set_checked(checked);
    }
}

fn handle_menu_event(app: &AppHandle, id: &str) {
    match id {
        ID_SHOW => show_main_window(app),
        ID_AUTOSTART => {
            let enable = !autostart::is_enabled();
            if autostart::set_autostart(enable).is_ok() {
                set_autostart_checked(enable);
                let state = app.state::<Arc<AppState>>();
                if let Ok(mut cfg) = state.config.write() {
                    cfg.settings.start_with_windows = enable;
                    let _ = config::save(&cfg);
                };
            }
        }
        ID_QUIT => {
            let state = app.state::<Arc<AppState>>();
            state
                .quitting
                .store(true, std::sync::atomic::Ordering::Relaxed);
            app.exit(0);
        }
        _ => {}
    }
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}
