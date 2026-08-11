//! 基準測試資源驗證（Task 2）：PresentMon 與 liblava Vulkan workload 是
//! 內建資源，執行前固定 SHA-256 驗證（`SHA256SUMS` manifest）。
//! D3D9 workload 是我們自己編譯的 sidecar，只要求存在。
//!
//! 所有資源放在 `src-tauri/resources/benchmark/`，打包進 installer 與
//! portable。測試用暫存目錄 + 假的 SHA256SUMS。

use std::path::{Path, PathBuf};

/// 內建資源目錄底下的相對檔名
pub const PRESENTMON_FILE: &str = "PresentMon-2.5.1-x64.exe";
pub const VULKAN_WORKLOAD_FILE: &str = "lava-triangle.exe";
pub const D3D9_WORKLOAD_FILE: &str = "d3d9-workload.exe";
pub const MANIFEST_FILE: &str = "SHA256SUMS";

/// 執行一個基準測試所需的全部外部工具路徑
#[derive(Clone, Debug)]
pub struct BenchmarkAssets {
    pub presentmon: PathBuf,
    pub vulkan_workload: PathBuf,
    pub d3d9_workload: PathBuf,
    /// SHA256SUMS manifest 路徑
    pub manifest: PathBuf,
}

/// 依資源目錄組出資產路徑（檔案可尚不存在；`verify` 負責檢查）
pub fn load(dir: &Path) -> BenchmarkAssets {
    BenchmarkAssets {
        presentmon: dir.join(PRESENTMON_FILE),
        vulkan_workload: dir.join(VULKAN_WORKLOAD_FILE),
        d3d9_workload: dir.join(D3D9_WORKLOAD_FILE),
        manifest: dir.join(MANIFEST_FILE),
    }
}

/// 資源驗證錯誤：帶穩定代碼（前端查 i18n）+ 詳細訊息
#[derive(Debug)]
pub enum AssetError {
    /// SHA256SUMS 讀取/格式錯誤
    Manifest(String),
    /// manifest 內固定 hash 的檔案缺失
    Missing(String),
    /// manifest 內固定 hash 的檔案 hash 不符
    HashMismatch(String),
    /// 自行編譯的 D3D9 workload 缺失
    MissingD3D9(String),
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::Manifest(e) => write!(f, "{e}"),
            AssetError::Missing(file) => write!(f, "資源缺失: {file}"),
            AssetError::HashMismatch(file) => write!(f, "資源 hash 不符: {file}"),
            AssetError::MissingD3D9(file) => {
                write!(f, "資源缺失: {file}（請先 npm run build:benchmark-assets）")
            }
        }
    }
}

impl AssetError {
    pub fn code(&self) -> &'static str {
        match self {
            AssetError::HashMismatch(_) => crate::error::codes::BENCHMARK_ASSETS_HASH_MISMATCH,
            _ => crate::error::codes::BENCHMARK_ASSETS_MISSING,
        }
    }
}

/// 驗證所有資源：
/// 1. SHA256SUMS manifest 內每個固定 hash 的檔案必須存在且 sha256 相符。
/// 2. D3D9 workload（自行編譯）必須存在（hash 隨 build 變動，不 pin）。
pub fn verify(assets: &BenchmarkAssets) -> Result<(), AssetError> {
    let expected = parse_manifest(&assets.manifest).map_err(AssetError::Manifest)?;
    for (file, want_hash) in &expected {
        let path = assets
            .manifest
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(file);
        let got_hash =
            sha256_of(&path).ok_or_else(|| AssetError::Missing(path.display().to_string()))?;
        if got_hash != *want_hash {
            return Err(AssetError::HashMismatch(format!(
                "{}（應為 {want_hash}，實際 {got_hash}）",
                path.display()
            )));
        }
    }
    if !assets.d3d9_workload.exists() {
        return Err(AssetError::MissingD3D9(
            assets.d3d9_workload.display().to_string(),
        ));
    }
    Ok(())
}

/// 解析 `sha256sum` 格式（`<hex>  <filename>`），只挑需要的檔名
fn parse_manifest(path: &Path) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("讀取 SHA256SUMS 失敗 {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `<hex>  <filename>`
        let mut parts = line.split_whitespace();
        let hash = parts
            .next()
            .ok_or_else(|| format!("SHA256SUMS 第 {} 行缺 hash", lineno + 1))?;
        let file = parts
            .next()
            .ok_or_else(|| format!("SHA256SUMS 第 {} 行缺檔名", lineno + 1))?;
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "SHA256SUMS 第 {} 行 hash 格式錯誤: {hash}",
                lineno + 1
            ));
        }
        out.push((file.to_string(), hash.to_lowercase()));
    }
    if out.is_empty() {
        return Err("SHA256SUMS 沒有內容".to_string());
    }
    Ok(out)
}

fn sha256_of(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Some(format!("{:x}", h.finalize()))
}

/// 計算單一檔案的 sha256（測試更新 manifest 用）
#[cfg(test)]
pub fn file_sha256(path: &Path) -> Result<String, String> {
    sha256_of(path).ok_or_else(|| format!("無法讀取檔案: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "frameanchor_assets_{}_{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn verify_passes_when_files_and_hash_match() {
        let dir = temp_dir("ok");
        let pm = dir.join(PRESENTMON_FILE);
        write(&pm, b"presentmon-bytes");
        let vk = dir.join(VULKAN_WORKLOAD_FILE);
        write(&vk, b"lava-bytes");
        write(&dir.join(D3D9_WORKLOAD_FILE), b"d3d9");
        let manifest = format!(
            "{hash}  {pm}\n{hash2}  {vk}\n",
            hash = file_sha256(&pm).unwrap(),
            pm = PRESENTMON_FILE,
            hash2 = file_sha256(&vk).unwrap(),
            vk = VULKAN_WORKLOAD_FILE,
        );
        write(&dir.join(MANIFEST_FILE), manifest.as_bytes());

        let assets = load(&dir);
        verify(&assets).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_fails_clearly_on_missing_asset() {
        let dir = temp_dir("missing");
        // 不建 PresentMon 檔
        write(&dir.join(D3D9_WORKLOAD_FILE), b"d3d9");
        let manifest = format!(
            "{hash}  {pm}\n",
            hash = "a".repeat(64),
            pm = PRESENTMON_FILE,
        );
        write(&dir.join(MANIFEST_FILE), manifest.as_bytes());

        let err = verify(&load(&dir)).unwrap_err();
        assert!(err.to_string().contains("資源缺失"), "err={err}");
        assert!(err.to_string().contains(PRESENTMON_FILE));
        assert_eq!(err.code(), crate::error::codes::BENCHMARK_ASSETS_MISSING);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_fails_on_hash_mismatch() {
        let dir = temp_dir("mismatch");
        let pm = dir.join(PRESENTMON_FILE);
        write(&pm, b"actual-bytes");
        write(&dir.join(D3D9_WORKLOAD_FILE), b"d3d9");
        let manifest = format!("{}  {}\n", "0".repeat(64), PRESENTMON_FILE);
        write(&dir.join(MANIFEST_FILE), manifest.as_bytes());

        let err = verify(&load(&dir)).unwrap_err();
        assert!(err.to_string().contains("hash 不符"), "err={err}");
        assert_eq!(
            err.code(),
            crate::error::codes::BENCHMARK_ASSETS_HASH_MISMATCH
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_requires_d3d9_workload_present() {
        let dir = temp_dir("nod3d9");
        let pm = dir.join(PRESENTMON_FILE);
        write(&pm, b"presentmon-bytes");
        let manifest = format!("{}  {}\n", file_sha256(&pm).unwrap(), PRESENTMON_FILE);
        write(&dir.join(MANIFEST_FILE), manifest.as_bytes());
        // 不建 d3d9-workload.exe

        let err = verify(&load(&dir)).unwrap_err();
        assert!(err.to_string().contains("d3d9-workload"), "err={err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_with_bad_hash_format_errors() {
        let dir = temp_dir("badhash");
        write(
            &dir.join(MANIFEST_FILE),
            "not-a-hash  file.exe\n".as_bytes(),
        );
        assert!(parse_manifest(&dir.join(MANIFEST_FILE)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
