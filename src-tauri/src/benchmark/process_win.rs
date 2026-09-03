//! 真實 ProcessRunner：std::process spawn + 直接 Win32
//! （TerminateProcess），不經 cmd.exe。
//! spawn 出的子程序 stdout/stderr 以 bounded tail 擷取（診斷用），
//! 由背景 thread 排乾 pipe，避免子程序寫滿 pipe buffer 而阻塞。

use std::collections::HashMap;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE,
};

use super::runner::{ProcessOutput, ProcessRunner};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 每個 pipe 的 bounded tail 上限（bytes）；超出只保留最後 N bytes
const IO_TAIL_CAP: usize = 4096;

/// kill 後 bounded reap 的固定上限（ms）。子程序收到 TerminateProcess 後
/// 幾乎立即退出，此上限只作為「永不無限阻塞」的保險。
const KILL_REAP_TIMEOUT_MS: u64 = 2000;
/// bounded reap 的輪詢間隔（ms）
const KILL_REAP_POLL_MS: u64 = 20;

/// spawn 出的 Child 與其 stdout/stderr tail buffer
struct ChildEntry {
    child: Child,
    stdout: Arc<Mutex<TailBuf>>,
    stderr: Arc<Mutex<TailBuf>>,
}

/// bounded tail buffer：最多保留最後 `cap` bytes
struct TailBuf {
    buf: Vec<u8>,
    cap: usize,
}

impl TailBuf {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        let overflow = self.buf.len().saturating_sub(self.cap);
        if overflow > 0 {
            self.buf.drain(0..overflow);
        }
    }

    fn as_string(&self) -> String {
        String::from_utf8_lossy(&self.buf).to_string()
    }
}

/// 生產實作：追蹤 spawn 出的 Child，終結時 kill + reap。
pub struct RealProcessRunner {
    children: Mutex<HashMap<u32, ChildEntry>>,
}

impl RealProcessRunner {
    pub fn new() -> Self {
        Self {
            children: Mutex::new(HashMap::new()),
        }
    }

    /// 啟動排乾 thread：讀 pipe 直到 EOF，內容寫入 bounded tail。
    /// thread 是 detached；child 被 kill 後 pipe 關閉 → thread 讀到 EOF 自行結束。
    fn drain<R: Read + Send + 'static>(pipe: Option<R>, tail: Arc<Mutex<TailBuf>>) {
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                loop {
                    match pipe.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => tail.lock().unwrap().push(&buf[..n]),
                        Err(_) => break,
                    }
                }
            });
        }
    }

    /// 對 children map 中的受管 Child：終止 + bounded reap。
    /// - 已退出（reap 前的正常 race）→ Ok，不進 kill/wait。
    /// - kill 失敗 → 不回無限期 wait；再確認一次是否已退出，否則回 Err。
    /// - reap 逾時或查詢失敗 → 回 Err。
    ///
    /// Err 路徑一律把 ChildEntry 放回 map 保留 ownership，供後續 exit_code/
    ///   output_tail 診斷與重試清理，避免靜默遺失 handle。
    fn kill_owned(&self, pid: u32) -> Result<(), String> {
        let Some(mut entry) = self.children.lock().unwrap().remove(&pid) else {
            return Ok(());
        };

        // 1) 已退出 → 直接 reap 成功（避免正常 race 被誤判為取消失敗）
        match entry.child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => log::warn!("kill {pid}: reap 前 try_wait 失敗: {e}"),
        }

        // 2) 發送終止訊號；失敗不得進入無限期 wait，只再確認一次是否已退出
        if let Err(e) = entry.child.kill() {
            match entry.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                _ => {
                    let msg = format!("kill {pid} 失敗: {e}");
                    self.children.lock().unwrap().insert(pid, entry);
                    return Err(msg);
                }
            }
        }

        // 3) bounded reap：輪詢 try_wait，逾時/錯誤回 Err 並保留 entry
        let result = bounded_reap(pid, KILL_REAP_TIMEOUT_MS, KILL_REAP_POLL_MS, &mut || {
            entry
                .child
                .try_wait()
                .map(|s| s.map(|st| st.code().unwrap_or(0)))
        });
        if result.is_err() {
            self.children.lock().unwrap().insert(pid, entry);
        }
        result
    }
}

impl ProcessRunner for RealProcessRunner {
    fn spawn(&self, exe: &Path, args: &[String]) -> Result<u32, String> {
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", exe.display()))?;
        let pid = child.id();
        let stdout_tail = Arc::new(Mutex::new(TailBuf::new(IO_TAIL_CAP)));
        let stderr_tail = Arc::new(Mutex::new(TailBuf::new(IO_TAIL_CAP)));
        Self::drain(child.stdout.take(), stdout_tail.clone());
        Self::drain(child.stderr.take(), stderr_tail.clone());
        self.children.lock().unwrap().insert(
            pid,
            ChildEntry {
                child,
                stdout: stdout_tail,
                stderr: stderr_tail,
            },
        );
        Ok(pid)
    }

    fn is_alive(&self, pid: u32) -> bool {
        unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code).is_ok();
            let _ = CloseHandle(h);
            ok && code == STILL_ACTIVE.0 as u32
        }
    }

    fn kill(&self, pid: u32) -> Result<(), String> {
        // 受管 child：終止 + bounded reap；未在 map 中（非 owned）走既有
        // TerminateProcess fallback，語意不變。
        if self.children.lock().unwrap().contains_key(&pid) {
            return self.kill_owned(pid);
        }
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, false, pid)
                .map_err(|e| format!("OpenProcess(kill): {e}"))?;
            let result = TerminateProcess(h, 1);
            let _ = CloseHandle(h);
            result.map_err(|e| format!("TerminateProcess: {e}"))
        }
    }

    fn wait_exit(&self, pid: u32, timeout_ms: u64) -> Result<bool, String> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            if !self.is_alive(pid) {
                return Ok(true);
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// 已退出程序的 exit code（try_wait reap；未退出/未知 → None）
    fn exit_code(&self, pid: u32) -> Option<i32> {
        let mut guard = self.children.lock().ok()?;
        match guard.get_mut(&pid)?.child.try_wait() {
            Ok(Some(status)) => status.code(),
            _ => None,
        }
    }

    /// bounded stdout/stderr tail（最多 `max_chars` chars，超出從頭截斷）
    fn output_tail(&self, pid: u32, max_chars: usize) -> Option<ProcessOutput> {
        let guard = self.children.lock().ok()?;
        let entry = guard.get(&pid)?;
        let stdout = truncate_chars(&entry.stdout.lock().unwrap().as_string(), max_chars);
        let stderr = truncate_chars(&entry.stderr.lock().unwrap().as_string(), max_chars);
        Some(ProcessOutput { stdout, stderr })
    }
}

/// 從字串開頭截斷到 `max_chars`（char 邊界安全）
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// 輪詢 `poll` 直到回 `Some(exit_code)`（已退出，回收成功）或逾時。
/// - `poll` 回 `Ok(None)` = 仍在執行，持續輪詢。
/// - `poll` 回 `Err` = 查詢失敗，立即回 Err。
///
/// `timeout_ms`/`poll_ms` 參數化以便測試注入短上限，不依賴真實程序。
/// `poll` 採 `&mut dyn FnMut` 以便測試注入 fake，不需真實 `Child`。
fn bounded_reap(
    pid: u32,
    timeout_ms: u64,
    poll_ms: u64,
    poll: &mut dyn FnMut() -> std::io::Result<Option<i32>>,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        match poll() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => return Err(format!("reap {pid} 失敗: {e}")),
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("kill {pid} 後未於 {timeout_ms}ms 內退出"));
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── bounded_reap（純 helper，不依賴真實程序/GPU）──────────────────────

    #[test]
    fn bounded_reap_already_exited_returns_ok() {
        let mut calls = 0;
        let mut poll = || -> std::io::Result<Option<i32>> {
            calls += 1;
            Ok(Some(0))
        };
        bounded_reap(1, 100, 1, &mut poll).unwrap();
        assert_eq!(calls, 1, "已退出應在第一次輪詢即回收");
    }

    #[test]
    fn bounded_reap_exits_midway_returns_ok() {
        let mut n = 0;
        let mut poll = || -> std::io::Result<Option<i32>> {
            n += 1;
            if n < 3 {
                Ok(None)
            } else {
                Ok(Some(0))
            }
        };
        bounded_reap(1, 1000, 1, &mut poll).unwrap();
        assert_eq!(n, 3, "第 3 次輪詢才退出，不該提前逾時");
    }

    #[test]
    fn bounded_reap_times_out_with_error() {
        let mut poll = || -> std::io::Result<Option<i32>> { Ok(None) };
        let err = bounded_reap(7, 1, 1, &mut poll).unwrap_err();
        assert!(err.contains("7"), "錯誤訊息應含 pid: {err}");
        assert!(err.contains("退出"), "錯誤訊息應含逾時語義: {err}");
    }

    #[test]
    fn bounded_reap_poll_error_returns_err() {
        let mut poll = || -> std::io::Result<Option<i32>> { Err(std::io::Error::other("boom")) };
        let err = bounded_reap(1, 1000, 1, &mut poll).unwrap_err();
        assert!(err.contains("boom"), "錯誤訊息應含原因: {err}");
    }

    // ── RealProcessRunner::kill（真實 child，不依賴 GPU）─────────────────

    /// reap 前的正常 race：child 已自行退出，kill 仍應回 Ok 而非取消失敗。
    #[test]
    fn kill_reaps_already_exited_child() {
        let r = RealProcessRunner::new();
        let args: Vec<String> = ["/C", "exit", "0"].iter().map(|s| s.to_string()).collect();
        let pid = r.spawn(Path::new("cmd.exe"), &args).unwrap();
        assert!(r.wait_exit(pid, 5000).unwrap(), "cmd /C exit 應自行退出");
        r.kill(pid).unwrap();
        assert!(!r.is_alive(pid));
    }

    /// 終止存活中的直接子程序（不經 cmd，kill 後無孤兒），並確認已回收。
    #[test]
    fn kill_terminates_live_child_without_leak() {
        let r = RealProcessRunner::new();
        let args: Vec<String> = ["-n", "30", "127.0.0.1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let pid = r.spawn(Path::new("ping.exe"), &args).unwrap();
        assert!(r.is_alive(pid), "child 應存活");
        r.kill(pid).unwrap();
        assert!(!r.is_alive(pid), "kill 後應已回收，不得殘留");
    }
}
