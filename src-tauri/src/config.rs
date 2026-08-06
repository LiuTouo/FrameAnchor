//! 設定持久化（PLAN §7.8）：%APPDATA%\FrameAnchor\config.json，原子寫入 + 壞檔備份。

use std::path::{Path, PathBuf};

use crate::model::Config;

pub fn config_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."))
        });
    base.join("FrameAnchor")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load() -> Config {
    load_from(&config_path())
}

pub fn load_from(path: &Path) -> Config {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Config::default(), // 不存在 → 預設值
    };
    match serde_json::from_str::<Config>(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            // 壞檔：備份為 config.corrupt.json，用預設值，不覆蓋使用者原檔（PLAN §7.8）
            log::error!("config 解析失敗，備份原檔: {e}");
            let backup = path.with_file_name("config.corrupt.json");
            let _ = std::fs::copy(path, &backup);
            Config::default()
        }
    }
}

pub fn save(cfg: &Config) -> Result<(), String> {
    save_to(&config_path(), cfg)
}

/// 原子寫入：先寫 tmp 再 rename（PLAN §5.1）
pub fn save_to(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(cfg).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&tmp, text).map_err(|e| format!("write tmp: {e}"))?;
    // Windows 上 rename 不覆蓋已存在的目標，先刪再改名
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| format!("remove old: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rule;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("frameanchor_test_{}_{}", std::process::id(), name))
    }

    #[test]
    fn roundtrip_preserves_data() {
        let path = temp_path("roundtrip.json");
        let mut cfg = Config::default();
        cfg.settings.poll_interval_ms = 2000;
        cfg.rules.push(Rule::new(r"C:\Games\game.exe".into(), "Game".into()));

        save_to(&path, &cfg).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.settings.poll_interval_ms, 2000);
        assert_eq!(loaded.rules.len(), 1);
        assert_eq!(loaded.rules[0].exe_path, r"C:\Games\game.exe");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_defaulted() {
        let path = temp_path("corrupt.json");
        std::fs::write(&path, "{ not valid json !!!").unwrap();
        let cfg = load_from(&path);
        assert_eq!(cfg.version, 1);
        let backup = path.with_file_name("config.corrupt.json");
        assert!(backup.exists());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
    }

    #[test]
    fn missing_file_gives_default() {
        let path = temp_path("missing.json");
        let _ = std::fs::remove_file(&path);
        let cfg = load_from(&path);
        assert_eq!(cfg.settings.language, "zh-TW");
    }

    #[test]
    fn missing_fields_use_defaults() {
        let path = temp_path("partial.json");
        std::fs::write(&path, r#"{ "version": 1 }"#).unwrap();
        let cfg = load_from(&path);
        assert_eq!(cfg.settings.poll_interval_ms, 1000);
        assert!(cfg.rules.is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
