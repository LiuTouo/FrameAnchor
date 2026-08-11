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
        if let Some(mut entry) = self.children.lock().unwrap().remove(&pid) {
            let _ = entry.child.kill();
            let _ = entry.child.wait();
            return Ok(());
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
