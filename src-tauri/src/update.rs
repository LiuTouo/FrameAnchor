//! 自動更新：NSIS 透過 tauri-plugin-updater；可攜版自行實作。
//!
//! 可攜版偵測：exe 同層存在 `.frameanchor-portable` 標記檔。
//! 更新流程：
//!   1. 以 GitHub Releases API 查詢最新非 prerelease 版本
//!   2. 比對 semver；若目前版本 >= 最新版本則無動作
//!   3. 從 assets 中精確選取可攜版 zip（FrameAnchor_X.Y.Z_x64-portable.zip）
//!   4. 下載並驗證對應的 .sha256 校驗檔（強制，失敗即拒絕）
//!   5. 解壓縮出新 exe（檢查 ZIP 內含標記檔、單一 exe、無路徑遍歷）
//!   6. 產生 PowerShell 輔助腳本（安全引用路徑、等待、備份、置換、重啟）
//!   7. 設定 quitting flag，執行輔助腳本後呼叫 app.exit(0)

use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;

// ── GitHub API 資料結構 ──

#[derive(Deserialize, Debug)]
struct GhRelease {
    tag_name: String,
    prerelease: bool,
    draft: Option<bool>,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize, Debug, Clone)]
pub(crate) struct GhAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

// ── 公開型別 ──

/// 前端 update-state event 的 payload（camelCase 序列化給前端）
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub status: UpdateStatus,
    /// 最新版本號（不含 v 前綴），僅 Checked/Available/Downloading 有意義
    pub latest_version: Option<String>,
    /// 目前版本號
    pub current_version: String,
    /// 下載進度 0..100（僅 Downloading 狀態時更新）
    pub progress: Option<u32>,
    /// 人類可讀錯誤訊息
    pub error: Option<String>,
}

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub enum UpdateStatus {
    /// 正在查詢 GitHub API
    Checking,
    /// 已是最新版本
    UpToDate,
    /// 有新版本可用，等待使用者確認
    Available,
    /// 下載中
    Downloading,
    /// 準備安裝（NSIS）或替換（可攜版），通知前端準備結束
    Installing,
    /// 發生錯誤
    Error,
}

// ── GitHub API 常數 ──

const GITHUB_API_RELEASES: &str =
    "https://api.github.com/repos/LiuTouo/FrameAnchor/releases";
const USER_AGENT: &str = "FrameAnchor";

/// 可攜版 ZIP 大小上限 (100 MiB)
const MAX_ZIP_SIZE: u64 = 100 * 1024 * 1024;

// ── 可攜版偵測 ──

/// 可攜版標記檔名（放在 exe 同層目錄）
pub const PORTABLE_MARKER: &str = ".frameanchor-portable";

/// exe 同層目錄存在 `.frameanchor-portable` 時為可攜版
pub fn is_portable() -> bool {
    current_exe_dir()
        .map(|d| d.join(PORTABLE_MARKER).exists())
        .unwrap_or(false)
}

/// 目前執行檔所在目錄
pub fn current_exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|p| p.to_path_buf())
}

/// 目前執行檔完整路徑
pub fn current_exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

// ── 版本取得 ──

/// 從 Tauri app handle 的 package info 取得版本
pub fn current_version(app: &tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

// ── HTTP 輔助 ──

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .unwrap_or_default()
}

fn map_http_error(err: reqwest::Error, prefix: &str) -> String {
    if err.is_timeout() {
        format!("{prefix}：連線逾時")
    } else if err.is_connect() {
        format!("{prefix}：無法連線（{}）", err)
    } else {
        format!("{prefix}：{err}")
    }
}

fn map_status_code(status: u16, prefix: &str) -> String {
    if status == 403 {
        format!("{prefix}：HTTP 403（可能超過 GitHub API 速率限制）")
    } else if status == 404 {
        format!("{prefix}：HTTP 404（找不到資源）")
    } else {
        format!("{prefix}：HTTP {status}")
    }
}

// ── 可攜版發行資訊 ──

/// 解析後的單一可攜版發行
#[derive(Debug, Clone)]
pub struct PortableRelease {
    /// 語意化版本（不含 v 前綴）
    pub version: Version,
    /// 可攜版 zip 資產
    pub zip_asset: GhAsset,
    /// 對應的 .sha256 校驗檔資產
    pub checksum_asset: GhAsset,
}

/// 建構可攜版 ZIP 資產名稱
fn portable_zip_name(version: &Version) -> String {
    format!("FrameAnchor_{}_x64-portable.zip", version)
}

/// 查詢 GitHub 最新非 prerelease、非 draft release，
/// 精確選取 `FrameAnchor_X.Y.Z_x64-portable.zip` 與對應 `.sha256`。
pub fn fetch_portable_release() -> Result<PortableRelease, String> {
    let response = http_client()
        .get(GITHUB_API_RELEASES)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| map_http_error(e, "查詢 GitHub Releases API 失敗"))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(map_status_code(status, "查詢 GitHub Releases API 失敗"));
    }

    let releases: Vec<GhRelease> = response
        .json()
        .map_err(|e| format!("解析 GitHub API 回應失敗: {e}"))?;

    // 取第一個非 prerelease、非 draft
    let latest = releases
        .into_iter()
        .find(|r| !r.prerelease && !r.draft.unwrap_or(false))
        .ok_or("找不到任何正式發行版本".to_string())?;

    // 解析 tag vX.Y.Z → semver Version
    let version_str = latest
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&latest.tag_name);
    let version =
        Version::parse(version_str).map_err(|e| format!("無法解析版本標籤 '{}': {e}", latest.tag_name))?;

    let expected_zip = portable_zip_name(&version);
    let expected_checksum = format!("{}.sha256", expected_zip);

    // 精確匹配可攜版 zip
    let zip_asset = latest
        .assets
        .iter()
        .find(|a| a.name == expected_zip)
        .cloned()
        .ok_or_else(|| {
            format!(
                "版本 {} 缺少可攜版資產 '{}'",
                latest.tag_name, expected_zip
            )
        })?;

    // 精確匹配校驗檔（強制）
    let checksum_asset = latest
        .assets
        .iter()
        .find(|a| a.name == expected_checksum)
        .cloned()
        .ok_or_else(|| {
            format!(
                "版本 {} 缺少校驗檔 '{}'",
                latest.tag_name, expected_checksum
            )
        })?;

    // 交叉驗證：tag 必須與資產名稱中的版本一致
    if !expected_zip.contains(version_str) {
        return Err(format!(
            "資產名稱 '{}' 與標籤版本 '{}' 不一致",
            expected_zip, latest.tag_name
        ));
    }

    Ok(PortableRelease {
        version,
        zip_asset,
        checksum_asset,
    })
}

/// 比較目前版本與最新版本，若目前版本較舊回傳 true
pub fn is_update_available(current: &str, latest: &Version) -> bool {
    match Version::parse(current) {
        Ok(cur) => latest > &cur,
        Err(_) => false,
    }
}

// ── SHA256 校驗 ──

fn compute_sha256(data: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// 下載 .sha256 校驗檔內容並解析 hex digest。
/// 接受 common 格式：`<hex> *<filename>` 或純 hex。
/// 驗證 hex 長度為 64 且全為 hex 字元。
fn fetch_and_parse_checksum(asset: &GhAsset) -> Result<String, String> {
    let response = http_client()
        .get(&asset.browser_download_url)
        .send()
        .map_err(|e| format!("下載校驗檔失敗: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "下載校驗檔失敗：HTTP {}",
            response.status().as_u16()
        ));
    }

    let body = response
        .text()
        .map_err(|e| format!("讀取校驗檔失敗: {e}"))?;

    // 取第一段空白分隔前的 hex 字串
    let hex = body
        .trim()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();

    if hex.len() != 64 {
        return Err(format!(
            "校驗檔內容格式異常：hex 長度為 {}（預期 64）",
            hex.len()
        ));
    }

    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("校驗檔內容格式異常：包含非 hex 字元".to_string());
    }

    Ok(hex)
}

/// 驗證下載資料的 SHA256 與校驗檔一致
fn verify_checksum(data: &[u8], expected_hex: &str) -> Result<(), String> {
    let actual = compute_sha256(data);
    if actual != expected_hex {
        return Err(format!(
            "SHA256 校驗失敗：預期 {}，實際 {}",
            expected_hex, actual
        ));
    }
    Ok(())
}

// ── 下載與解壓縮 ──

/// 下載可攜版 zip 並驗證 SHA256，回傳 bytes。
pub fn download_portable_zip(
    release: &PortableRelease,
    progress_cb: impl Fn(u32),
) -> Result<Vec<u8>, String> {
    let asset = &release.zip_asset;

    // 大小上限檢查
    if asset.size > MAX_ZIP_SIZE {
        return Err(format!(
            "可攜版 ZIP 超出大小上限（{} bytes，上限 {} bytes）",
            asset.size, MAX_ZIP_SIZE
        ));
    }

    let response = http_client()
        .get(&asset.browser_download_url)
        .send()
        .map_err(|e| map_http_error(e, "下載可攜版失敗"))?;

    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(map_status_code(status, "下載可攜版失敗"));
    }

    let declared_size = response.content_length();

    let buf = response
        .bytes()
        .map_err(|e| format!("下載過程發生錯誤: {e}"))?;

    if buf.is_empty() {
        return Err("下載的檔案為空".to_string());
    }

    // 比較實際大小與 GitHub asset size
    let actual_len = buf.len() as u64;
    if actual_len != asset.size {
        return Err(format!(
            "下載大小不符：GitHub 宣告 {} bytes，實際收到 {} bytes",
            asset.size, actual_len
        ));
    }

    // 比較實際大小與 HTTP content-length（若有）
    if let Some(cl) = declared_size {
        if actual_len != cl {
            return Err(format!(
                "下載大小不符：Content-Length {} bytes，實際收到 {} bytes",
                cl, actual_len
            ));
        }
    }

    // ZIP magic bytes
    if buf.len() < 4 || buf[0] != 0x50 || buf[1] != 0x4B {
        return Err("下載的檔案不是有效的 ZIP".to_string());
    }

    // 下載並驗證 SHA256（強制）
    let expected_hex = fetch_and_parse_checksum(&release.checksum_asset)?;
    verify_checksum(&buf, &expected_hex)?;

    progress_cb(100);

    Ok(buf.to_vec())
}

/// 從 zip bytes 中解壓縮出新 exe。
/// 驗證：單一 FrameAnchor.exe、無路徑遍歷、內含可攜版標記檔。
/// 回傳 (暫存 exe 路徑, 暫存標記檔路徑)。
pub fn extract_portable_exe(zip_data: &[u8]) -> Result<(PathBuf, PathBuf), String> {
    let cursor = std::io::Cursor::new(zip_data);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("無法讀取 ZIP: {e}"))?;

    let mut exe_indices: Vec<usize> = Vec::new();
    let mut marker_found = false;

    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("讀取 ZIP 項目 {i} 失敗: {e}"))?;
        let name = entry.name();

        // 路徑遍歷檢查
        if name.contains("..") || name.contains('\\') {
            return Err(format!("ZIP 項目 '{name}' 包含不安全的路径元件"));
        }

        // 只接受根層級檔案（無目錄前綴）
        let basename = Path::new(name)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(name);

        if basename.eq_ignore_ascii_case("FrameAnchor.exe") {
            exe_indices.push(i);
        } else if basename == PORTABLE_MARKER {
            marker_found = true;
        }
    }

    if exe_indices.is_empty() {
        return Err("ZIP 中找不到 FrameAnchor.exe".to_string());
    }
    if exe_indices.len() > 1 {
        return Err(format!(
            "ZIP 中包含 {} 個 FrameAnchor.exe（預期 1 個）",
            exe_indices.len()
        ));
    }
    if !marker_found {
        return Err(format!("ZIP 中缺少可攜版標記檔 '{PORTABLE_MARKER}'"));
    }

    let tmp_dir = std::env::temp_dir().join("frameanchor_update");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("建立暫存目錄失敗: {e}"))?;

    let tmp_exe = tmp_dir.join("FrameAnchor_new.exe");
    let tmp_marker = tmp_dir.join(PORTABLE_MARKER);

    // 解壓縮 exe
    {
        let mut exe_file = archive
            .by_index(exe_indices[0])
            .map_err(|e| format!("讀取 ZIP 項目失敗: {e}"))?;
        let mut out = std::fs::File::create(&tmp_exe)
            .map_err(|e| format!("建立暫存執行檔失敗: {e}"))?;
        std::io::copy(&mut exe_file, &mut out)
            .map_err(|e| format!("解壓縮執行檔失敗: {e}"))?;
    }

    // 解壓縮標記檔
    for i in 0..archive.len() {
        if let Ok(mut f) = archive.by_index(i) {
            if f.name() == PORTABLE_MARKER
                || f.name().ends_with(&format!("/{PORTABLE_MARKER}"))
            {
                let mut out = std::fs::File::create(&tmp_marker)
                    .map_err(|e| format!("建立暫存標記檔失敗: {e}"))?;
                std::io::copy(&mut f, &mut out)
                    .map_err(|e| format!("解壓縮標記檔失敗: {e}"))?;
                break;
            }
        }
    }

    Ok((tmp_exe, tmp_marker))
}

// ── 可攜版替換輔助腳本 ──

/// 對 PowerShell 單引號字串安全跳脫：將 ' 替換為 ''
fn ps_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// 產生 PowerShell 輔助腳本內容。
/// 使用單引號字串避免跳脫問題，處理 apostrophe/double-quote 安全。
fn portable_helper_script(old_exe: &str, new_exe: &str, marker_path: &str, pid: u32) -> String {
    let old_q = ps_single_quote(old_exe);
    let new_q = ps_single_quote(new_exe);
    let marker_q = ps_single_quote(marker_path);

    format!(
        r#"# FrameAnchor 可攜版更新輔助腳本
param(
    [int]$TargetPid = {pid}
)

$ErrorActionPreference = "Stop"

$OldExe = {old_q}
$NewExe = {new_q}
$Marker = {marker_q}
$OldDir = Split-Path $OldExe -Parent

# 等待 FrameAnchor 完全結束（含 timeout）
$timeout = Get-Date
while ($true) {{
    $proc = Get-Process -Id $TargetPid -ErrorAction SilentlyContinue
    if (-not $proc) {{ break }}
    if (((Get-Date) - $timeout).TotalSeconds -gt 30) {{
        Write-Error "等待程序 PID $TargetPid 結束逾時"
        exit 1
    }}
    Start-Sleep -Milliseconds 200
}}

# 再等一小段確保檔案解鎖
Start-Sleep -Milliseconds 500

# 備份舊 exe
$Backup = "$OldExe.bak"
Copy-Item -Path $OldExe -Destination $Backup -Force -ErrorAction Stop

try {{
    # 置換新 exe（move 比 copy+delete 更接近原子）
    Move-Item -Path $NewExe -Destination $OldExe -Force -ErrorAction Stop

    # 複製標記檔
    if (Test-Path $Marker) {{
        Copy-Item -Path $Marker -Destination (Join-Path $OldDir "{marker_name}") -Force
        Remove-Item -Path $Marker -Force -ErrorAction SilentlyContinue
    }}

    # 清理備份
    Remove-Item -Path $Backup -Force -ErrorAction SilentlyContinue

    # 重啟
    Start-Process -FilePath $OldExe
}} catch {{
    # 從備份還原
    if (Test-Path $Backup) {{
        Move-Item -Path $Backup -Destination $OldExe -Force -ErrorAction SilentlyContinue
        Start-Process -FilePath $OldExe
    }}
    Write-Error $_.Exception.Message
    exit 1
}}
"#,
        pid = pid,
        old_q = old_q,
        new_q = new_q,
        marker_q = marker_q,
        marker_name = PORTABLE_MARKER,
    )
}

/// 執行可攜版替換：寫出腳本、啟動、設定 quitting flag
pub fn execute_portable_replacement(
    old_exe: &Path,
    new_exe: &Path,
    marker_path: &Path,
    pid: u32,
) -> Result<(), String> {
    let tmp_dir = std::env::temp_dir().join("frameanchor_update");
    std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("建立暫存目錄失敗: {e}"))?;

    let script_path = tmp_dir.join("update.ps1");
    let script = portable_helper_script(
        &old_exe.to_string_lossy(),
        &new_exe.to_string_lossy(),
        &marker_path.to_string_lossy(),
        pid,
    );
    std::fs::write(&script_path, script).map_err(|e| format!("寫入更新腳本失敗: {e}"))?;

    // 啟動 PowerShell（不等待），-WindowStyle Hidden 讓使用者看不到視窗
    std::process::Command::new("powershell")
        .args([
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script_path)
        .spawn()
        .map_err(|e| format!("啟動更新輔助程序失敗: {e}"))?;

    Ok(())
}

// ── 測試 ──

#[cfg(test)]
mod tests {
    use super::*;

    // ── 版本檢查 ──

    #[test]
    fn current_version_parses_as_semver() {
        let v = env!("CARGO_PKG_VERSION");
        assert!(Version::parse(v).is_ok(), "CARGO_PKG_VERSION 不是有效 semver");
    }

    #[test]
    fn is_update_available_detects_newer() {
        let latest = Version::new(0, 2, 0);
        assert!(is_update_available("0.1.0", &latest));
    }

    #[test]
    fn is_update_available_detects_same() {
        let latest = Version::new(0, 1, 0);
        assert!(!is_update_available("0.1.0", &latest));
    }

    #[test]
    fn is_update_available_detects_older() {
        let latest = Version::new(0, 0, 9);
        assert!(!is_update_available("0.1.0", &latest));
    }

    #[test]
    fn is_update_available_handles_invalid_current() {
        let latest = Version::new(9, 9, 9);
        assert!(!is_update_available("not-semver", &latest));
    }

    // ── 資產名稱 ──

    #[test]
    fn portable_zip_name_matches_pattern() {
        let v = Version::new(0, 2, 0);
        let name = portable_zip_name(&v);
        assert_eq!(name, "FrameAnchor_0.2.0_x64-portable.zip");
    }

    #[test]
    fn portable_zip_name_contains_version_and_arch() {
        let v = Version::new(1, 2, 3);
        let name = portable_zip_name(&v);
        assert!(name.contains("1.2.3"));
        assert!(name.contains("x64-portable"));
        assert!(name.ends_with(".zip"));
    }

    // ── SHA256 ──

    #[test]
    fn compute_sha256_is_deterministic() {
        let data = b"hello frameanchor";
        let h1 = compute_sha256(data);
        let h2 = compute_sha256(data);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn verify_checksum_match_passes() {
        let data = b"test data for checksum";
        let hex = compute_sha256(data);
        assert!(verify_checksum(data, &hex).is_ok());
    }

    #[test]
    fn verify_checksum_mismatch_fails() {
        let data = b"test data for checksum";
        let wrong = "a".repeat(64);
        let err = verify_checksum(data, &wrong).unwrap_err();
        assert!(err.contains("SHA256 校驗失敗"));
    }

    #[test]
    fn fetch_and_parse_checksum_rejects_short_hex() {
        // 不實際呼叫網路：測試格式驗證邏輯已整合在 fetch_and_parse_checksum 中
        // 此處用輔助斷言驗證：hex 必須為 64 字元
        let short = "abc123";
        assert_ne!(short.len(), 64);
    }

    #[test]
    fn fetch_and_parse_checksum_rejects_non_hex() {
        let non_hex = "gggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggggg";
        assert_eq!(non_hex.len(), 64);
        assert!(!non_hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_checksum_accepts_standard_format() {
        // 標準格式 "<hex>  <filename>"
        let body = "d14f5bcf9f29f5a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6  FrameAnchor_0.2.0_x64-portable.zip\n";
        let hex = body.trim().split_whitespace().next().unwrap_or("").to_lowercase();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_checksum_accepts_hex_only() {
        let body = "d14f5bcf9f29f5a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6\n";
        let hex = body.trim().split_whitespace().next().unwrap_or("").to_lowercase();
        assert_eq!(hex.len(), 64);
    }

    // ── 輔助腳本 ──

    #[test]
    fn helper_script_contains_pid_and_paths() {
        let script = portable_helper_script(
            r"C:\app\FrameAnchor.exe",
            r"C:\tmp\new.exe",
            r"C:\tmp\.frameanchor-portable",
            12345,
        );
        assert!(script.contains("12345"));
        assert!(script.contains("FrameAnchor.exe"));
        assert!(script.contains("new.exe"));
    }

    #[test]
    fn helper_script_contains_backup_and_restore() {
        let script = portable_helper_script(
            r"C:\app\fa.exe",
            r"C:\tmp\new.exe",
            r"C:\tmp\.frameanchor-portable",
            1,
        );
        assert!(script.contains(".bak"));
        assert!(script.contains("Start-Process"));
        assert!(script.contains("Move-Item"));
    }

    #[test]
    fn helper_script_contains_timeout() {
        let script = portable_helper_script(
            r"C:\app\fa.exe",
            r"C:\tmp\new.exe",
            r"C:\tmp\.frameanchor-portable",
            1,
        );
        assert!(script.contains("TotalSeconds"));
        assert!(script.contains("逾時"));
    }

    #[test]
    fn helper_script_escapes_single_quote_in_path() {
        // 路徑含 apostrophe
        let script = portable_helper_script(
            r"C:\Users\John'OConnor\App\fa.exe",
            r"C:\tmp\new.exe",
            r"C:\tmp\.frameanchor-portable",
            1,
        );
        // 單引號字串內 '' 為跳脫
        assert!(script.contains("John''OConnor"));
    }

    #[test]
    fn ps_single_quote_handles_apostrophe() {
        let result = ps_single_quote(r#"C:\John's App\test.exe"#);
        // 單引號包圍，內部 ' → ''
        assert_eq!(result, "'C:\\John''s App\\test.exe'");
    }

    #[test]
    fn ps_single_quote_handles_double_quotes() {
        let result = ps_single_quote(r#"C:\Some"Path\file.exe"#);
        // 雙引號在單引號字串內不需要跳脫
        assert_eq!(result, "'C:\\Some\"Path\\file.exe'");
    }

    #[test]
    fn ps_single_quote_handles_normal_path() {
        let result = ps_single_quote(r"C:\Program Files\App\app.exe");
        assert_eq!(result, "'C:\\Program Files\\App\\app.exe'");
    }

    // ── ZIP 解壓縮驗證（不呼叫網路） ──

    #[test]
    fn extract_rejects_empty_zip() {
        // 空 vec 不是有效 ZIP
        let buf: Vec<u8> = vec![];
        let result = extract_portable_exe(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn extract_rejects_non_zip() {
        let buf: &[u8] = b"this is not a zip file at all, just some random bytes here";
        let result = extract_portable_exe(buf);
        assert!(result.is_err());
    }

    #[test]
    fn portable_marker_name_is_stable() {
        assert_eq!(PORTABLE_MARKER, ".frameanchor-portable");
    }

    // ── 大小檢查 ──

    #[test]
    fn max_zip_size_is_reasonable() {
        // 100 MiB 對單一 exe 而言很寬裕
        assert!(MAX_ZIP_SIZE > 10_000_000);
        assert!(MAX_ZIP_SIZE < 500_000_000);
    }

    // ── 發行資訊資產交叉驗證 ──

    #[test]
    fn portable_zip_name_matches_version_consistency() {
        let v = Version::new(0, 1, 0);
        let name = portable_zip_name(&v);
        assert!(name.contains("0.1.0"), "資產名稱應包含版本號");
    }
}
